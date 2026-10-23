//! Small record codecs shared by federation discovery paths.

use iroh_dns::pkarr::SignedPacket;
use nm_transport::SecretKey;

pub(crate) fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn hex_decode_n<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// 变长 hex 解码（P2-13：承载编码后的 AdmissionGrant 等变长字节）。
pub(crate) fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// F2 pkarr group discovery record codec.
pub(crate) mod pkarr_rec {
    use super::{hex_decode_n, hex_encode, SignedPacket, SecretKey};

    pub(crate) const SEED_NAME: &str = "_nmspace";
    pub(crate) const REC_TTL: u32 = 300;
    pub(crate) const MAX_SEEDS: usize = 8;

    /// Sign the group's seed node set as a relay payload.
    pub(crate) fn build(secret: &SecretKey, seeds: &[[u8; 32]]) -> Result<Vec<u8>, String> {
        let values: Vec<String> = seeds.iter().take(MAX_SEEDS).map(|s| hex_encode(s)).collect();
        let pkt = SignedPacket::from_txt_strings(secret, SEED_NAME, values, REC_TTL)
            .map_err(|e| e.to_string())?;
        Ok(pkt.to_relay_payload())
    }

    /// Verify and parse relay payload for a group pubkey into seed node ids.
    pub(crate) fn parse(pubkey: &[u8; 32], payload: &[u8]) -> Result<Vec<[u8; 32]>, String> {
        let pk = nm_transport::Id::from_bytes(pubkey).map_err(|e| e.to_string())?;
        let pkt = SignedPacket::from_relay_payload(&pk, payload).map_err(|e| e.to_string())?;
        Ok(pkt
            .txt_records(SEED_NAME)
            .iter()
            .filter_map(|t| hex_decode_n::<32>(t))
            .collect())
    }
}

/// F6/M1 member bootstrap index record codec.
pub(crate) mod member_rec {
    use super::{hex_decode_n, hex_encode, SignedPacket, SecretKey};

    pub(crate) const NAME: &str = "_nmmember";
    pub(crate) const REC_TTL: u32 = 300;
    pub(crate) const MAX_MEMBERS: usize = 8;

    /// Sign bootstrap member node ids as a relay payload.
    pub(crate) fn build(secret: &SecretKey, members: &[[u8; 32]]) -> Result<Vec<u8>, String> {
        let values: Vec<String> = members.iter().take(MAX_MEMBERS).map(|s| hex_encode(s)).collect();
        let pkt = SignedPacket::from_txt_strings(secret, NAME, values, REC_TTL)
            .map_err(|e| e.to_string())?;
        Ok(pkt.to_relay_payload())
    }

    /// Verify and parse relay payload for an anchor pubkey into bootstrap member ids.
    pub(crate) fn parse(anchor_pubkey: &[u8; 32], payload: &[u8]) -> Result<Vec<[u8; 32]>, String> {
        let pk = nm_transport::Id::from_bytes(anchor_pubkey).map_err(|e| e.to_string())?;
        let pkt = SignedPacket::from_relay_payload(&pk, payload).map_err(|e| e.to_string())?;
        Ok(pkt
            .txt_records(NAME)
            .iter()
            .filter_map(|t| hex_decode_n::<32>(t))
            .collect())
    }
}

// ── P2-12 Phase B 前置实测：iroh_dns 产包 ↔ pkarr crate 的 relay-payload wire 兼容（设计文档 §4B/§7）。
// 两个方向各对应一条桥：发布桥=pkarr 必须能验签解析 iroh 产包(from_relay_payload)；
// 解析桥=pkarr 回吐包必须能被 iroh 验签解析(to_relay_payload → parse)。不兼容则 Phase B 不成立。
#[cfg(test)]
mod dht_bridge_tests {
    use super::pkarr_rec;
    use nm_transport::SecretKey;

    fn sorted(mut v: Vec<[u8; 32]>) -> Vec<[u8; 32]> {
        v.sort();
        v
    }

    /// wire 兼容（纯编解码，无网络，非 ignore）：iroh_dns 产包 → pkarr 验签 → 回吐 → iroh 验签，seeds 不变。
    #[test]
    fn relay_payload_wire_compat_both_directions() {
        let sk = SecretKey::generate();
        let seeds = vec![[7u8; 32], [9u8; 32], [11u8; 32]];
        let payload = pkarr_rec::build(&sk, &seeds).expect("iroh_dns build");
        let pub_bytes = *sk.public().as_bytes();

        // 方向 1（发布桥）：pkarr crate 解析 iroh_dns 产的 relay payload（验签+公钥一致）。
        let pk: pkarr::PublicKey = sk.public().to_z32().parse().expect("z32 → pkarr PublicKey");
        let pkt = pkarr::SignedPacket::from_relay_payload(&pk, &bytes::Bytes::from(payload))
            .expect("wire 兼容：pkarr 必须能验签解析 iroh_dns 产包");
        assert_eq!(pkt.public_key().as_bytes(), &pub_bytes, "公钥穿透一致");

        // 方向 2（解析桥）：pkarr 回吐的 relay payload 被 iroh_dns 验签解析出同样 seeds。
        let back = pkt.to_relay_payload().to_vec();
        let parsed = pkarr_rec::parse(&pub_bytes, &back)
            .expect("wire 兼容：iroh_dns 必须能验签解析 pkarr 回吐包");
        assert_eq!(sorted(parsed), sorted(seeds), "seeds 双向桥后不变");
    }

    /// 本地 Mainline DHT 测试网端到端：publish(经发布桥) → 另一客户端 resolve(经解析桥) → seeds 一致。
    /// 多 UDP 节点、较慢；与其它跨节点 e2e 同列 --ignored 串行跑。
    #[tokio::test]
    #[ignore = "本地 Mainline Testnet(多 UDP 节点,较慢);cargo test -p nm-node --lib -- --ignored dht_testnet"]
    async fn dht_testnet_publish_resolve_roundtrip() {
        let testnet = mainline::Testnet::builder(8).build().expect("local DHT testnet");
        let bootstrap = testnet.bootstrap.clone();
        // 官方测试姿势(pkarr client/tests.rs)：本地测试网须把客户端绑到 127.0.0.1
        // (mainline 默认过滤 loopback 节点)，并用短请求超时加速。
        let mk = |bs: &Vec<String>| {
            let mut b = pkarr::Client::builder();
            b.no_default_network()
                .bootstrap(bs)
                .dht(|c| {
                    c.bind_address = Some(std::net::Ipv4Addr::LOCALHOST);
                    c
                })
                .dht_request_timeout(std::time::Duration::from_millis(500));
            b.build()
        };
        let c1 = mk(&bootstrap).expect("client1");
        let c2 = mk(&bootstrap).expect("client2");

        let sk = SecretKey::generate();
        let seeds = vec![[1u8; 32], [2u8; 32]];
        let payload = pkarr_rec::build(&sk, &seeds).expect("build");
        let pk: pkarr::PublicKey = sk.public().to_z32().parse().unwrap();
        let pkt = pkarr::SignedPacket::from_relay_payload(&pk, &bytes::Bytes::from(payload)).unwrap();

        // mainline 后台 bootstrap 进路由表需片刻(本地测试网毫秒级);就绪前 publish 报
        // NoDhtNodesQueried → 小步重试而非 sleep 猜时长。
        let mut published = None;
        let mut last_err = String::new();
        for _ in 0..40 {
            match c1.publish(&pkt).await {
                Ok(n) => {
                    published = Some(n);
                    break;
                }
                Err(e) => {
                    last_err = e.to_string();
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
            }
        }
        published.unwrap_or_else(|| panic!("publish to testnet DHT 未就绪;最后错误: {last_err}"));
        let got = c2
            .resolve(&pk, pkarr::ResolvePolicy::NetworkOnly)
            .await
            .expect("resolve from testnet DHT");        let parsed = pkarr_rec::parse(sk.public().as_bytes(), &got.to_relay_payload().to_vec())
            .expect("parse resolved");
        assert_eq!(sorted(parsed), sorted(seeds), "DHT 往返后 seeds 一致");
    }
}

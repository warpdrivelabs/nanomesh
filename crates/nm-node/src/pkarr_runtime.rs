use nm_proto::now_ms;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::records::pkarr_rec;
use crate::{Node, LIVE_TTL_MS};

/// P2-12 Phase A：pkarr 发现观测（多中继解析命中/全失败计数）。Phase B 增 DHT 计数。
#[derive(Default)]
pub(crate) struct PkarrStat {
    pub(crate) resolve_ok: AtomicU64,  // 任一中继解析出非空 seed
    pub(crate) resolve_fail: AtomicU64, // 所有中继均未解析出
    pub(crate) dht_resolve_ok: AtomicU64,   // Phase B：Mainline DHT 兜底解析命中
    pub(crate) dht_resolve_fail: AtomicU64, // Phase B：DHT 兜底也未解析出
    pub(crate) dht_publish_ok: AtomicU64,   // Phase B：DHT best-effort 发布成功次数
}

impl Node {
    /// F2：设置本节点 pkarr relay 端点（nmd 据 infra 透传；`None`=不发布/解析群发现记录）。
    /// 值可为逗号分隔的多中继（P2-12 Phase A：弱锚多副本，发布写全部、解析首成功）。
    pub fn set_pkarr(&self, url: Option<String>) {
        *self.pkarr_url.write().unwrap() = url;
    }

    /// 原始配置串（含逗号多中继）；供「是否配置了 pkarr」的门控判断用。
    pub(crate) fn pkarr_endpoint(&self) -> Option<String> {
        self.pkarr_url.read().unwrap().clone()
    }

    /// P2-12 Phase A：pkarr relay 端点列表（`[dns]` 支持逗号分隔多中继；弱锚多副本）。
    /// F2 群/频道发现与 F6 成员索引统一走此列表，退「单中继即单点」。
    pub(crate) fn pkarr_endpoints(&self) -> Vec<String> {
        self.pkarr_url
            .read()
            .unwrap()
            .as_deref()
            .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default()
    }

    /// 配置的 pkarr 中继数（观测）。
    pub fn pkarr_relay_count(&self) -> usize {
        self.pkarr_endpoints().len()
    }

    /// pkarr 解析观测快照：(resolve_ok, resolve_fail)。
    pub fn pkarr_stat_snapshot(&self) -> (u64, u64) {
        (self.pkarr_stat.resolve_ok.load(Ordering::Relaxed), self.pkarr_stat.resolve_fail.load(Ordering::Relaxed))
    }

    // ── P2-12 Phase B：真 Mainline DHT（trustless 叠加层）──
    // 记录本就是 BEP44 SignedPacket;与 relay 的桥=同一 relay-payload 字节(iroh_dns 产包 ↔ pkarr crate
    // from/to_relay_payload),记录语义零改动。默认关;开后 resolve 兜底 + publish best-effort,relay 仍主路径。

    /// 开/关 Mainline DHT 叠加层（nmd.toml `[dns] dht`；reload 可热改）。
    pub fn set_pkarr_dht(&self, on: bool) {
        self.pkarr_dht_on.store(on, Ordering::Relaxed);
    }
    pub fn pkarr_dht_enabled(&self) -> bool {
        self.pkarr_dht_on.load(Ordering::Relaxed)
    }
    /// DHT 观测快照：(enabled, resolve_ok, resolve_fail, publish_ok)。
    pub fn pkarr_dht_snapshot(&self) -> (bool, u64, u64, u64) {
        (
            self.pkarr_dht_enabled(),
            self.pkarr_stat.dht_resolve_ok.load(Ordering::Relaxed),
            self.pkarr_stat.dht_resolve_fail.load(Ordering::Relaxed),
            self.pkarr_stat.dht_publish_ok.load(Ordering::Relaxed),
        )
    }
    /// 是否存在任一发现信道（relay 列表非空 或 DHT 开）。发布/解析入口的统一门控。
    pub(crate) fn pkarr_any_sink(&self) -> bool {
        !self.pkarr_endpoints().is_empty() || self.pkarr_dht_enabled()
    }

    /// DHT 客户端（懒初始化;构建失败记 warn 并缓存 None,不反复重试）。关着时恒 None。
    async fn dht_client(&self) -> Option<&pkarr::Client> {
        if !self.pkarr_dht_enabled() {
            return None;
        }
        self.pkarr_dht
            .get_or_init(|| async {
                match pkarr::Client::builder().build() {
                    Ok(c) => {
                        tracing::info!("pkarr Mainline DHT 客户端已启动(P2-12 Phase B)");
                        Some(c)
                    }
                    Err(e) => {
                        tracing::warn!("pkarr DHT 客户端构建失败: {e}(DHT 叠加层不可用,仅 relay)");
                        None
                    }
                }
            })
            .await
            .as_ref()
    }

    /// 把一条 relay-payload 记录 best-effort 发布到 Mainline DHT（经 from_relay_payload 桥,验签失败即拒）。
    pub(crate) async fn pkarr_dht_publish(&self, z32_key: &str, payload: &[u8], what: &str) {
        let Some(c) = self.dht_client().await else { return };
        let Ok(pk) = z32_key.parse::<pkarr::PublicKey>() else {
            tracing::debug!("pkarr {what} DHT publish: bad z32 key");
            return;
        };
        let bytes = bytes::Bytes::copy_from_slice(payload);
        match pkarr::SignedPacket::from_relay_payload(&pk, &bytes) {
            Ok(pkt) => match c.publish(&pkt).await {
                Ok(_) => {
                    self.pkarr_stat.dht_publish_ok.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!("pkarr {what} DHT published");
                }
                Err(e) => tracing::debug!("pkarr {what} DHT publish failed: {e}"),
            },
            Err(e) => tracing::debug!("pkarr {what} DHT payload bridge failed: {e}"),
        }
    }

    /// 从 Mainline DHT 解析一条记录,返回 relay-payload 字节（供既有 parse 验签复用）。未命中/关着=None。
    pub(crate) async fn pkarr_dht_resolve_payload(&self, z32_key: &str) -> Option<Vec<u8>> {
        let c = self.dht_client().await?;
        let pk = z32_key.parse::<pkarr::PublicKey>().ok()?;
        match c.resolve(&pk, pkarr::ResolvePolicy::CacheFirst).await {
            Ok(pkt) => {
                self.pkarr_stat.dht_resolve_ok.fetch_add(1, Ordering::Relaxed);
                Some(pkt.to_relay_payload().to_vec())
            }
            Err(e) => {
                self.pkarr_stat.dht_resolve_fail.fetch_add(1, Ordering::Relaxed);
                tracing::debug!("pkarr DHT resolve miss: {e}");
                None
            }
        }
    }

    /// 把一条已签名记录 PUT 到**每个**中继（弱锚多副本）+ Phase B:best-effort 发布到 Mainline DHT。
    async fn pkarr_put_all(&self, z32_key: &str, payload: Vec<u8>, what: &str) {
        let endpoints = self.pkarr_endpoints();
        let mut ok = 0usize;
        for base in &endpoints {
            let endpoint = format!("{}/{}", base.trim_end_matches('/'), z32_key);
            match self.http.put(&endpoint).body(payload.clone()).send().await {
                Ok(r) if r.status().is_success() => ok += 1,
                Ok(r) => tracing::debug!(status = %r.status(), relay = %base, "pkarr {what} publish non-success"),
                Err(e) => tracing::debug!(relay = %base, "pkarr {what} publish failed: {e}"),
            }
        }
        tracing::debug!(relays = endpoints.len(), ok, "pkarr {what} record published");
        self.pkarr_dht_publish(z32_key, &payload, what).await; // Phase B(关着即空操作)
    }

    /// 从**每个**中继依次 GET+验签，首个解析出非空 seed 即返回（弱锚多副本，单中继挂不致失败）。
    /// Phase B:relay 全失败 → Mainline DHT 兜底（trustless;同一 relay-payload 验签路径）。
    async fn pkarr_resolve_key(&self, pubkey: &[u8; 32], z32_key: &str) -> Vec<[u8; 32]> {
        let endpoints = self.pkarr_endpoints();
        if endpoints.is_empty() && !self.pkarr_dht_enabled() {
            return Vec::new();
        }
        for base in &endpoints {
            let endpoint = format!("{}/{}", base.trim_end_matches('/'), z32_key);
            let payload = match self.http.get(&endpoint).send().await {
                Ok(r) if r.status().is_success() => match r.bytes().await {
                    Ok(b) => b.to_vec(),
                    Err(_) => continue,
                },
                _ => continue,
            };
            let seeds = pkarr_rec::parse(pubkey, &payload).unwrap_or_default();
            if !seeds.is_empty() {
                self.pkarr_stat.resolve_ok.fetch_add(1, Ordering::Relaxed);
                return seeds; // 首成功即用（记录已 ed25519 自验，首成功足够；多中继仅做可用性冗余）
            }
        }
        if let Some(payload) = self.pkarr_dht_resolve_payload(z32_key).await {
            let seeds = pkarr_rec::parse(pubkey, &payload).unwrap_or_default();
            if !seeds.is_empty() {
                self.pkarr_stat.resolve_ok.fetch_add(1, Ordering::Relaxed);
                return seeds; // DHT 兜底命中（包同样 ed25519 自验）
            }
        }
        self.pkarr_stat.resolve_fail.fetch_add(1, Ordering::Relaxed);
        Vec::new()
    }

    /// F2：把某「带密钥群」的 seed 节点集合（本节点=home + 该群主题上存活的成员 home）签名后 PUT 到
    /// pkarr relay（P2-12 Phase A：写全部中继），使无锚节点可据群公钥解析入网 seed。
    /// 无 pkarr / 无群私钥 / 非 home 则跳过。pub：供 sweep 周期调用，也便于运维/测试直接触发。
    pub async fn pkarr_publish_group(&self, gid: &[u8]) {
        if !self.pkarr_any_sink() {
            return;
        }
        let Some(store) = &self.store else { return };
        let Ok(Some(secret_bytes)) = store.group_secret(gid) else { return };
        let Ok(sk_arr) = <[u8; 32]>::try_from(secret_bytes.as_slice()) else { return };
        let sk = nm_transport::SecretKey::from_bytes(&sk_arr);
        let mut seeds: Vec<[u8; 32]> = vec![self.ep.id_bytes()]; // 本节点(home) 为首 seed
        if let Some(gl) = self.per_group_live.get(gid) {
            let now = now_ms();
            for e in gl.iter() {
                if now.saturating_sub(*e.value()) < LIVE_TTL_MS {
                    if let Ok(id) = <[u8; 32]>::try_from(e.key().as_slice()) {
                        if !seeds.contains(&id) {
                            seeds.push(id);
                        }
                    }
                }
            }
        }
        let Ok(payload) = pkarr_rec::build(&sk, &seeds) else { return };
        self.pkarr_put_all(&sk.public().to_z32(), payload, "group").await;
    }

    /// F2：据群公钥从 pkarr relay 解析 seed 节点集合（GET + 验签；P2-12 Phase A：多中继首成功）。
    /// 无 pkarr / 解析失败则空。pub：供 sweep 周期调用，也便于运维/测试直接触发。
    pub async fn pkarr_resolve_group(&self, gid: &[u8]) -> Vec<[u8; 32]> {
        let Ok(gid_arr) = <[u8; 32]>::try_from(gid) else { return Vec::new() };
        let Ok(pk) = nm_transport::Id::from_bytes(&gid_arr) else { return Vec::new() };
        self.pkarr_resolve_key(&gid_arr, &pk.to_z32()).await
    }

    /// P4：频道发现——home 把本节点（该频道定序器）签名为该频道的 seed，PUT 到 pkarr relay（多中继）。
    /// 复用 `pkarr_rec` 编解码（按频道公钥签名；channel_id 即公钥、私钥在 CHANNEL_SECRETS）。
    /// 无 pkarr / 非 home（无频道私钥）则跳过。晚加入者据频道 id 解析到 seed 即可入网回填历史。
    pub async fn pkarr_publish_channel(&self, channel_id: &[u8]) {
        if !self.pkarr_any_sink() {
            return;
        }
        let Some(store) = &self.store else { return };
        let Ok(Some(secret_bytes)) = store.channel_secret(channel_id) else { return };
        let Ok(sk_arr) = <[u8; 32]>::try_from(secret_bytes.as_slice()) else { return };
        let sk = nm_transport::SecretKey::from_bytes(&sk_arr);
        let seeds: Vec<[u8; 32]> = vec![self.ep.id_bytes()]; // home 节点为 seed（定序器=权威源）
        let Ok(payload) = pkarr_rec::build(&sk, &seeds) else { return };
        self.pkarr_put_all(&sk.public().to_z32(), payload, "channel").await;
    }

    /// P4：据频道公钥从 pkarr relay 解析 seed 节点集合（GET + 验签）。无 pkarr / 解析失败则空。
    pub async fn pkarr_resolve_channel(&self, channel_id: &[u8]) -> Vec<[u8; 32]> {
        self.pkarr_resolve_group(channel_id).await // 同一 relay/编解码；公钥命名空间天然隔离群与频道
    }
}

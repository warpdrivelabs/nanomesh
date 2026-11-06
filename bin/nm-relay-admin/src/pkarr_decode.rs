//! pkarr 记录解码：把 iroh-dns 里存的签名包还原成可读的 relay / 直连地址列表。
//!
//! 用途：relay 监控台需要看到「某个 nmd 节点在 DNS 里发布了哪些地址」。
//! 节点公钥本身即 pkarr key（各节点用自身 EndpointId 发布），
//! 因此 peers 列表里的 endpoint id 可直接拿来查 pkarr。

use serde::Serialize;

/// 解码后的节点地址信息。
#[derive(Debug, Clone, Serialize, Default)]
pub struct NodeAddrs {
    /// relay 地址（通常 1 个）。
    pub relays: Vec<String>,
    /// 直连 IP 地址。
    pub ips: Vec<String>,
    /// 记录原始字节数（0=无记录）。
    pub bytes: usize,
    /// 解码是否成功。
    pub ok: bool,
    /// 失败原因（ok=false 时）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl NodeAddrs {
    fn err(msg: impl Into<String>) -> Self {
        Self { ok: false, error: Some(msg.into()), ..Default::default() }
    }
}

/// 从 pkarr GET 响应字节解码出 relay + IP 地址。
///
/// 注意：iroh-dns-server 的 `/pkarr/{key}` 返回的是 **relay payload** 格式
/// （`SignedPacket::to_relay_payload()`，即 64B 签名 + 8B 时间戳 + DNS 包），
/// 必须用 `from_relay_payload(公钥, payload)` 解码，而非 `from_bytes`。
/// 故需要节点的 32 字节公钥。
pub fn decode_relay_payload(pubkey: &[u8; 32], bytes: &[u8]) -> NodeAddrs {
    if bytes.is_empty() {
        return NodeAddrs::err("empty packet");
    }
    // iroh-dns 的 /pkarr/{key} 返回 relay payload（签名+时间戳+DNS 包，不含公钥前缀）。
    // SignedPacket::from_relay_payload 的实现就是「公钥32字节 ++ payload」再 from_bytes，
    // 这里直接自拼，避开 iroh_base::PublicKey 的可见性问题。
    let mut full = Vec::with_capacity(32 + bytes.len());
    full.extend_from_slice(pubkey);
    full.extend_from_slice(bytes);
    let packet = match iroh_dns::pkarr::SignedPacket::from_bytes(&full) {
        Ok(p) => p,
        Err(e) => return NodeAddrs::err(format!("verify/decode failed: {e}")),
    };
    let info = match iroh_dns::endpoint_info::EndpointInfo::from_pkarr_signed_packet(&packet) {
        Ok(i) => i,
        Err(e) => return NodeAddrs::err(format!("endpoint_info parse failed: {e}")),
    };

    let mut relays: Vec<String> = info.data.relay_urls().map(|u| u.to_string()).collect();
    let mut ips: Vec<String> = info.data.ip_addrs().map(|sa| sa.to_string()).collect();
    relays.sort();
    relays.dedup();
    ips.sort();
    ips.dedup();

    NodeAddrs { relays, ips, bytes: bytes.len(), ok: true, error: None }
}

/// 由 node id（z32 或 64-hex）推出 pkarr 查询用的 z-base-32 key。
/// 已是 z32 形式则原样返回。
pub fn to_pkarr_key(node_id: &str) -> Option<String> {
    let t = node_id.trim();
    if t.is_empty() {
        return None;
    }
    // 64 位 hex → 32 字节 → z-base-32
    if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        let raw = hex32(t)?;
        return Some(z_base32_encode(&raw));
    }
    // 视为 z32 原样
    Some(t.to_string())
}

/// 由 node id（z32 或 64-hex）解出 32 字节公钥。
pub fn pubkey_bytes(node_id: &str) -> Option<[u8; 32]> {
    let t = node_id.trim();
    if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        return hex32(t);
    }
    // z32 解码
    z_base32_decode(t)
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut raw = [0u8; 32];
    for i in 0..32 {
        raw[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(raw)
}

/// z-base-32 解码（52 字符 → 32 字节）。
fn z_base32_decode(s: &str) -> Option<[u8; 32]> {
    const ALPH: &[u8] = b"ybndrfg8ejkmcpqxot1uwisza345h769";
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::with_capacity(32);
    for c in s.bytes() {
        let v = ALPH.iter().position(|&a| a == c)? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    if out.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    Some(arr)
}

/// z-base-32 编码（pkarr/iroh 使用的字母表，与 RFC4648 base32 不同）。
fn z_base32_encode(data: &[u8]) -> String {
    const ALPH: &[u8] = b"ybndrfg8ejkmcpqxot1uwisza345h769";
    let mut out = String::with_capacity((data.len() * 8 + 4) / 5);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in data {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPH[((buf >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPH[((buf << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_z32_roundtrip_shape() {
        // 32 字节 0 → 全 'y'；长度应为 52
        let s = z_base32_encode(&[0u8; 32]);
        assert_eq!(s.len(), 52);
        assert!(s.chars().all(|c| c == 'y'));

        // 64-hex 转换
        let hex = "00".repeat(32);
        let k = to_pkarr_key(&hex).unwrap();
        assert_eq!(k.len(), 52);
        assert_eq!(k, s);
    }

    #[test]
    fn decode_empty_is_error() {
        let r = decode_relay_payload(&[0u8; 32], &[]);
        assert!(!r.ok);
        assert!(r.error.is_some());
    }

    #[test]
    fn decode_garbage_is_error_not_panic() {
        let r = decode_relay_payload(&[1u8; 32], &[0xff; 200]);
        assert!(!r.ok);
    }

    #[test]
    fn z32_decode_roundtrip() {
        // 全 0 → z32 → 解回全 0
        let z = z_base32_encode(&[0u8; 32]);
        let back = z_base32_decode(&z).unwrap();
        assert_eq!(back, [0u8; 32]);
        // 已知公钥 hex → z32 → pubkey_bytes 应回到原 hex 字节
        let hex = "d2aba10821698104f0b2400fb25ff98f027c09f4ccde6957aeb88cc5eb6c2193";
        let raw = hex32(hex).unwrap();
        let z = to_pkarr_key(hex).unwrap();
        assert_eq!(pubkey_bytes(&z).unwrap(), raw);
        assert_eq!(pubkey_bytes(hex).unwrap(), raw);
    }
}

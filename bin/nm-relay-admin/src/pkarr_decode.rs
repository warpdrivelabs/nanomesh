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

/// 从 pkarr 原始字节（SignedPacket）解码出 relay + IP 地址。
pub fn decode_signed_packet(bytes: &[u8]) -> NodeAddrs {
    if bytes.is_empty() {
        return NodeAddrs::err("empty packet");
    }
    let packet = match iroh_dns::pkarr::SignedPacket::from_bytes(bytes) {
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
        let mut raw = [0u8; 32];
        for i in 0..32 {
            raw[i] = u8::from_str_radix(&t[i * 2..i * 2 + 2], 16).ok()?;
        }
        return Some(z_base32_encode(&raw));
    }
    // 视为 z32 原样
    Some(t.to_string())
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
        let r = decode_signed_packet(&[]);
        assert!(!r.ok);
        assert!(r.error.is_some());
    }

    #[test]
    fn decode_garbage_is_error_not_panic() {
        let r = decode_signed_packet(&[0xff; 200]);
        assert!(!r.ok);
    }
}

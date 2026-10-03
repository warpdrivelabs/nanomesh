//! 设备迁移握手（新设备向已登录的旧设备请求私钥）。
//! 新设备用一次性身份上线，经私聊通道与旧设备交换 X25519 临时公钥；双方由共享密钥派生
//! 会话密钥与 6 位核对码，用户在两边比对一致并在旧设备点允许后，旧设备发出加密的身份种子。
//! 中间节点只见密文；若有人替换临时公钥，两边核对码不一致。

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand::RngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

pub const T_REQUEST: &str = "nmspace.v1/pair.request";
pub const T_OFFER: &str = "nmspace.v1/pair.offer";
pub const T_GRANT: &str = "nmspace.v1/pair.grant";
pub const T_DENY: &str = "nmspace.v1/pair.deny";
pub const PREFIX: &str = "nmspace.v1/pair.";
const TICKET_PREFIX: &str = "nmpair1:";

/// 请求有效期。
pub const TTL_MS: u64 = 5 * 60 * 1000;

#[derive(Serialize, Deserialize, Clone)]
pub struct Request {
    pub v: u32,
    pub rid: String,
    pub xpk: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub name: String,
    pub ts: u64,
    /// v2：新设备的设备公钥。旧设备为它签证书，不再交出账号私钥（除非勾选设为管理设备）。
    #[serde(default)]
    pub dk: String,
}

/// v2 授权正文（加密后放进 Grant.ct）。v1 的正文是 32 字节账号种子。
#[derive(Serialize, Deserialize)]
pub struct GrantBody {
    pub cert: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
}

/// 迁移串：与 Request 相同，外加新设备一次性身份，旧设备据此回信。
#[derive(Serialize, Deserialize)]
pub struct Ticket {
    #[serde(flatten)]
    pub req: Request,
    pub eph: String,
}

#[derive(Serialize, Deserialize)]
pub struct Offer {
    pub v: u32,
    pub rid: String,
    pub xpk: String,
}

#[derive(Serialize, Deserialize)]
pub struct Grant {
    pub v: u32,
    pub rid: String,
    pub ct: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub struct Deny {
    pub v: u32,
    pub rid: String,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn fresh(ts: u64) -> bool {
    let now = now_ms();
    ts <= now + 60_000 && now.saturating_sub(ts) <= TTL_MS
}

pub fn random_rid() -> String {
    let mut b = [0u8; 16];
    OsRng.fill_bytes(&mut b);
    hex(&b)
}

pub fn keypair() -> (StaticSecret, [u8; 32]) {
    let mut raw = [0u8; 32];
    OsRng.fill_bytes(&mut raw);
    let sk = StaticSecret::from(raw);
    let pk = *PublicKey::from(&sk).as_bytes();
    (sk, pk)
}

pub struct Session {
    pub key: [u8; 32],
    pub sas: String,
}

/// 由 ECDH 共享密钥与完整转录派生会话密钥和核对码。双方必须以相同顺序传入参数：
/// `owner` 旧设备身份、`newdev` 新设备一次性身份、`xpk_new`/`xpk_old` 双方临时公钥、
/// `dk` 新设备的设备公钥（v2；纳入转录，中间人替换后核对码不一致）。
#[allow(clippy::too_many_arguments)]
pub fn derive(
    my: &StaticSecret,
    peer_xpk: &[u8; 32],
    rid: &str,
    owner: &[u8; 32],
    newdev: &[u8; 32],
    xpk_new: &[u8; 32],
    xpk_old: &[u8; 32],
    dk: Option<&[u8; 32]>,
) -> Result<Session, String> {
    let shared = my.diffie_hellman(&PublicKey::from(*peer_xpk));
    if !shared.was_contributory() {
        return Err("临时公钥无效".into());
    }
    let mut th = Sha256::new();
    th.update(b"nmspace-pair-v1");
    th.update(rid.as_bytes());
    th.update(owner);
    th.update(newdev);
    th.update(xpk_new);
    th.update(xpk_old);
    if let Some(dk) = dk {
        th.update(b"dk");
        th.update(dk);
    }
    let th = th.finalize();
    let hk = Hkdf::<Sha256>::new(Some(&th), shared.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(b"key", &mut key).map_err(|_| "密钥派生失败".to_string())?;
    let mut s = [0u8; 4];
    hk.expand(b"sas", &mut s).map_err(|_| "密钥派生失败".to_string())?;
    let n = u32::from_be_bytes(s) % 1_000_000;
    Ok(Session { key, sas: format!("{:03} {:03}", n / 1000, n % 1000) })
}

pub fn seal(key: &[u8; 32], pt: &[u8]) -> Result<Vec<u8>, String> {
    let c = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let ct = c.encrypt(XNonce::from_slice(&nonce), pt).map_err(|_| "加密失败".to_string())?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn open(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 24 {
        return Err("密文过短".into());
    }
    let (n, ct) = data.split_at(24);
    XChaCha20Poly1305::new(key.into())
        .decrypt(XNonce::from_slice(n), ct)
        .map_err(|_| "解密失败".to_string())
}

pub fn encode_ticket(t: &Ticket) -> Result<String, String> {
    use base64::Engine;
    let json = serde_json::to_vec(t).map_err(|e| e.to_string())?;
    Ok(format!("{TICKET_PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)))
}

pub fn decode_ticket(s: &str) -> Result<Ticket, String> {
    use base64::Engine;
    let body = s.trim().strip_prefix(TICKET_PREFIX).ok_or("不是迁移串")?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body.trim())
        .map_err(|_| "迁移串格式错误".to_string())?;
    serde_json::from_slice(&raw).map_err(|_| "迁移串格式错误".to_string())
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex32(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    if s.len() != 64 {
        return Err("长度应为 64 个十六进制字符".into());
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| "不是十六进制".to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_agree() {
        let (sn, pn) = keypair();
        let (so, po) = keypair();
        let owner = [1u8; 32];
        let newdev = [2u8; 32];
        let a = derive(&sn, &po, "r", &owner, &newdev, &pn, &po, None).unwrap();
        let b = derive(&so, &pn, "r", &owner, &newdev, &pn, &po, None).unwrap();
        assert_eq!(a.key, b.key);
        assert_eq!(a.sas, b.sas);
        let ct = seal(&b.key, b"seed").unwrap();
        assert_eq!(open(&a.key, &ct).unwrap(), b"seed");
    }

    #[test]
    fn swapped_device_key_changes_sas() {
        let (sn, pn) = keypair();
        let (so, po) = keypair();
        let (owner, newdev) = ([1u8; 32], [2u8; 32]);
        let a = derive(&sn, &po, "r", &owner, &newdev, &pn, &po, Some(&[7u8; 32])).unwrap();
        let b = derive(&so, &pn, "r", &owner, &newdev, &pn, &po, Some(&[8u8; 32])).unwrap();
        assert_ne!(a.sas, b.sas);
    }

    #[test]
    fn swapped_key_changes_sas() {
        let (sn, pn) = keypair();
        let (so, po) = keypair();
        let (_sm, pm) = keypair();
        let owner = [1u8; 32];
        let newdev = [2u8; 32];
        let a = derive(&sn, &po, "r", &owner, &newdev, &pn, &po, None).unwrap();
        let b = derive(&so, &pm, "r", &owner, &newdev, &pm, &po, None).unwrap();
        assert_ne!(a.sas, b.sas);
    }

    #[test]
    fn ticket_roundtrip() {
        let t = Ticket {
            req: Request {
                v: 2,
                rid: "ab".into(),
                xpk: "cd".into(),
                device: "mac".into(),
                name: "a@b".into(),
                ts: 1,
                dk: "ee".into(),
            },
            eph: "ef".into(),
        };
        let s = encode_ticket(&t).unwrap();
        let back = decode_ticket(&s).unwrap();
        assert_eq!(back.eph, "ef");
        assert_eq!(back.req.rid, "ab");
        assert_eq!(back.req.dk, "ee");
    }
}

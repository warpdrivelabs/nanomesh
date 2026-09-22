//! `im-crypto` — 完整性校验 + 能力授权(Grant, UCAN 风格) 的签发/校验。
//! 见 `docs/PLAN_C` §7。身份签名基于 iroh 的 Ed25519（`SecretKey`/`PublicKey`）。
//!
//! 说明：iroh 的 QUIC/TLS 握手已在传输层证明「对端持有其公钥」，故连接身份
//! (`conn.remote_id()`) 是被密码学认证的——"登录"退化为节点强制
//! 「entity_id == 连接公钥」。本模块聚焦 Grant：谁(issuer)授权谁(audience)在
//! `expires` 前对 `resource` 执行 `action`，可校验、可过期。

use im_proto::Grant;
use iroh::{PublicKey, SecretKey, Signature};

/// 逐 gram 完整性校验：blake3(payload)。
pub fn content_hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("{0}")]
    Other(String),
    #[error("grant expired")]
    Expired,
    #[error("bad signature")]
    BadSignature,
    #[error("bad key/signature bytes")]
    BadBytes,
    #[error("grant action/resource mismatch")]
    Scope,
}

pub type Result<T> = std::result::Result<T, CryptoError>;

/// Grant 的规范签名字节（不含 signature 本身；含 proof 以绑定委托链）。
fn grant_signing_bytes(g: &Grant) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&g.issuer);
    b.push(0);
    b.extend_from_slice(&g.audience);
    b.push(0);
    b.extend_from_slice(g.action.as_bytes());
    b.push(0);
    b.extend_from_slice(g.resource.as_bytes());
    b.push(0);
    b.extend_from_slice(&g.expires.to_be_bytes());
    b.push(0);
    b.extend_from_slice(&g.proof);
    b
}

/// 用 issuer 私钥签发一份 Grant：audience 可在 `expires`(Unix 秒) 前对 `resource` 做 `action`。
pub fn issue_grant(
    issuer_sk: &SecretKey,
    audience: [u8; 32],
    action: &str,
    resource: &str,
    expires: i64,
    proof: Vec<u8>,
) -> Grant {
    let mut g = Grant {
        issuer: issuer_sk.public().as_bytes().to_vec(),
        audience: audience.to_vec(),
        action: action.to_string(),
        resource: resource.to_string(),
        expires,
        proof,
        signature: Vec::new(),
    };
    let sig = issuer_sk.sign(&grant_signing_bytes(&g));
    g.signature = sig.to_bytes().to_vec();
    g
}

/// 校验 Grant 的签名与有效期（不含委托链递归；proof 已绑入签名）。
pub fn verify_grant(g: &Grant, now_secs: i64) -> Result<()> {
    if g.expires != 0 && now_secs > g.expires {
        return Err(CryptoError::Expired);
    }
    let issuer = pubkey_from(&g.issuer)?;
    let sig = signature_from(&g.signature)?;
    issuer
        .verify(&grant_signing_bytes(g), &sig)
        .map_err(|_| CryptoError::BadSignature)
}

/// 校验 Grant 确实授权 `audience` 对 `resource` 执行 `action`（且签名/有效期有效）。
/// `action`/`resource` 支持精确匹配或 `*` 通配。
pub fn authorize(
    g: &Grant,
    audience: &[u8],
    action: &str,
    resource: &str,
    now_secs: i64,
) -> Result<()> {
    verify_grant(g, now_secs)?;
    if g.audience != audience {
        return Err(CryptoError::Scope);
    }
    let action_ok = g.action == "*" || g.action == action;
    let resource_ok = g.resource == "*" || g.resource == resource;
    if action_ok && resource_ok {
        Ok(())
    } else {
        Err(CryptoError::Scope)
    }
}

fn pubkey_from(b: &[u8]) -> Result<PublicKey> {
    let arr: [u8; 32] = b.try_into().map_err(|_| CryptoError::BadBytes)?;
    PublicKey::from_bytes(&arr).map_err(|_| CryptoError::BadBytes)
}

fn signature_from(b: &[u8]) -> Result<Signature> {
    let arr: [u8; 64] = b.try_into().map_err(|_| CryptoError::BadBytes)?;
    Ok(Signature::from_bytes(&arr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_roundtrip_and_scope() {
        let issuer = SecretKey::from_bytes(&[5u8; 32]);
        let audience = [9u8; 32];
        let g = issue_grant(&issuer, audience, "infer", "compute.inference", 0, Vec::new());

        // 正确 audience/action/resource → 通过
        assert!(authorize(&g, &audience, "infer", "compute.inference", 100).is_ok());
        // 错误 audience → 拒绝
        assert!(authorize(&g, &[1u8; 32], "infer", "compute.inference", 100).is_err());
        // 错误 action → 拒绝
        assert!(authorize(&g, &audience, "lock", "compute.inference", 100).is_err());
    }

    #[test]
    fn grant_expiry_and_tamper() {
        let issuer = SecretKey::from_bytes(&[6u8; 32]);
        let audience = [9u8; 32];
        let g = issue_grant(&issuer, audience, "*", "*", 50, Vec::new());
        // 未过期
        assert!(verify_grant(&g, 49).is_ok());
        // 过期
        assert!(matches!(verify_grant(&g, 51), Err(CryptoError::Expired)));

        // 篡改 action → 签名失效
        let mut tampered = g.clone();
        tampered.action = "evil".into();
        assert!(matches!(verify_grant(&tampered, 10), Err(CryptoError::BadSignature)));
    }
}

//! 本地访问认证与身份私钥静态加密（P1，见 docs/CLIENT_AUTH_SECURITY.md）。
//! 主口令 ─Argon2id(salt,params)→ 主密钥 MK；MK ─XChaCha20Poly1305→ 封装随机保险库密钥 VK；
//! VK ─XChaCha20Poly1305→ 加密每个身份种子。私钥永不明文落盘；改主口令只重封 VK，不动种子。
//! 本模块为纯逻辑（不依赖 tauri）：入参为应用数据目录 `dir`。

use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

const VERIFY: &[u8] = b"nmspace-vault-v1";
const M_COST: u32 = 65536; // 64 MiB
const T_COST: u32 = 3;
const P_COST: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Kdf {
    alg: String,
    m: u32,
    t: u32,
    p: u32,
    salt: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct VaultMeta {
    version: u32,
    kdf: Kdf,
    verifier: Vec<u8>,
    wrapped_vk: Vec<u8>,
}

fn vault_file(dir: &Path) -> PathBuf {
    dir.join("vault.json")
}
fn ident_dir(dir: &Path) -> PathBuf {
    dir.join("identities")
}
pub fn vault_exists(dir: &Path) -> bool {
    vault_file(dir).exists()
}

fn derive_mk(password: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<[u8; 32], String> {
    let params = Params::new(m, t, p, Some(32)).map_err(|e| e.to_string())?;
    let a = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    a.hash_password_into(password.as_bytes(), salt, &mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

fn seal(key: &[u8; 32], pt: &[u8]) -> Result<Vec<u8>, String> {
    let c = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let ct = c.encrypt(XNonce::from_slice(&nonce), pt).map_err(|_| "加密失败".to_string())?;
    let mut out = Vec::with_capacity(24 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn open(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 24 {
        return Err("密文过短".into());
    }
    let (n, ct) = data.split_at(24);
    let c = XChaCha20Poly1305::new(key.into());
    c.decrypt(XNonce::from_slice(n), ct).map_err(|_| "解密失败（口令错误或数据损坏）".to_string())
}

fn pubkey_of_seed(seed: &[u8; 32]) -> [u8; 32] {
    *nm_transport::SecretKey::from_bytes(seed).public().as_bytes()
}
fn hexstr(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn encrypt_seed(vk: &[u8; 32], seed: &[u8; 32]) -> Result<Vec<u8>, String> {
    seal(vk, seed)
}
pub fn decrypt_seed(vk: &[u8; 32], data: &[u8]) -> Result<[u8; 32], String> {
    let pt = open(vk, data)?;
    if pt.len() != 32 {
        return Err("种子长度异常".into());
    }
    let mut s = [0u8; 32];
    s.copy_from_slice(&pt);
    Ok(s)
}

fn write_meta(dir: &Path, meta: &VaultMeta) -> Result<(), String> {
    let s = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
    std::fs::write(vault_file(dir), s).map_err(|e| e.to_string())
}
fn read_meta(dir: &Path) -> Result<VaultMeta, String> {
    let s = std::fs::read_to_string(vault_file(dir)).map_err(|_| "尚未设置主口令".to_string())?;
    serde_json::from_str(&s).map_err(|e| e.to_string())
}

/// 首次建库：随机 VK，用 MK 封装 + verifier；迁移已有明文种子为 .enc 后抹除明文。返回 VK（解锁态）。
pub fn setup(dir: &Path, password: &str) -> Result<[u8; 32], String> {
    if vault_exists(dir) {
        return Err("已设置主口令".into());
    }
    std::fs::create_dir_all(ident_dir(dir)).map_err(|e| e.to_string())?;
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let mut mk = derive_mk(password, &salt, M_COST, T_COST, P_COST)?;
    let mut vk = [0u8; 32];
    OsRng.fill_bytes(&mut vk);
    let wrapped_vk = seal(&mk, &vk)?;
    let verifier = seal(&mk, VERIFY)?;
    mk.zeroize();
    write_meta(dir, &VaultMeta {
        version: 1,
        kdf: Kdf { alg: "argon2id".into(), m: M_COST, t: T_COST, p: P_COST, salt: salt.to_vec() },
        verifier,
        wrapped_vk,
    })?;
    migrate_plaintext(dir, &vk)?;
    Ok(vk)
}

/// 解锁：口令 → MK → 校验 verifier → 解出 VK。
pub fn unlock(dir: &Path, password: &str) -> Result<[u8; 32], String> {
    let meta = read_meta(dir)?;
    let mut mk = derive_mk(password, &meta.kdf.salt, meta.kdf.m, meta.kdf.t, meta.kdf.p)?;
    let ok = open(&mk, &meta.verifier).map(|x| x == VERIFY).unwrap_or(false);
    if !ok {
        mk.zeroize();
        return Err("主口令错误".into());
    }
    let vkv = open(&mk, &meta.wrapped_vk);
    mk.zeroize();
    let vkv = vkv?;
    if vkv.len() != 32 {
        return Err("保险库损坏".into());
    }
    let mut vk = [0u8; 32];
    vk.copy_from_slice(&vkv);
    Ok(vk)
}

/// 改主口令：用旧口令取 VK，用新口令重封（种子不动）。
pub fn change(dir: &Path, old: &str, new: &str) -> Result<(), String> {
    let vk = unlock(dir, old)?;
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let mut mk = derive_mk(new, &salt, M_COST, T_COST, P_COST)?;
    let wrapped_vk = seal(&mk, &vk)?;
    let verifier = seal(&mk, VERIFY)?;
    mk.zeroize();
    write_meta(dir, &VaultMeta {
        version: 1,
        kdf: Kdf { alg: "argon2id".into(), m: M_COST, t: T_COST, p: P_COST, salt: salt.to_vec() },
        verifier,
        wrapped_vk,
    })
}

/// 迁移已有明文种子（identities/*.seed + 旧 nmspace.identity）→ VK 加密的 .enc，并抹除明文。
fn migrate_plaintext(dir: &Path, vk: &[u8; 32]) -> Result<(), String> {
    let idir = ident_dir(dir);
    let migrate_one = |bytes: &[u8]| -> Result<(), String> {
        if bytes.len() != 32 {
            return Ok(());
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(bytes);
        let pk = pubkey_of_seed(&seed);
        let enc = encrypt_seed(vk, &seed)?;
        std::fs::write(idir.join(format!("{}.enc", hexstr(&pk))), enc).map_err(|e| e.to_string())?;
        seed.zeroize();
        Ok(())
    };
    // 旧单身份
    let legacy = dir.join("nmspace.identity");
    if let Ok(bytes) = std::fs::read(&legacy) {
        migrate_one(&bytes)?;
        let _ = std::fs::write(&legacy, [0u8; 32]);
        let _ = std::fs::remove_file(&legacy);
    }
    // 多身份明文
    if let Ok(rd) = std::fs::read_dir(&idir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().ends_with(".seed") {
                if let Ok(bytes) = std::fs::read(e.path()) {
                    migrate_one(&bytes)?;
                }
                let _ = std::fs::write(e.path(), [0u8; 32]);
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

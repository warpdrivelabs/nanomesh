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
// KDF 内存代价：移动端自适应下调（P3），避免手机上 64 MiB 过重。参数入库，跨设备打开用库内参数。
#[cfg(any(target_os = "ios", target_os = "android"))]
const M_COST: u32 = 19456; // ~19 MiB
#[cfg(not(any(target_os = "ios", target_os = "android")))]
const M_COST: u32 = 65536; // 64 MiB（桌面）
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
    #[serde(default)]
    recovery_vk: Option<Vec<u8>>, // 恢复码 RK 封装的 VK（设置恢复码后有值；P3）
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

// ── 加密备份导出/导入（P2）：独立口令，与主口令解耦，用于换机/找回。 ──

#[derive(Serialize, Deserialize)]
struct BackupSeed {
    pk: String,
    seed: Vec<u8>,
}

fn b64(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}
fn unb64(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|_| "备份数据格式错误".to_string())
}

/// 导出全部身份为「独立口令」加密的备份串（base64）。需已解锁（用 VK 解出种子再重加密）。
pub fn export(dir: &Path, vk: &[u8; 32], password: &str) -> Result<String, String> {
    let idir = ident_dir(dir);
    let mut items: Vec<BackupSeed> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&idir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".enc") {
                if stem.len() == 64 {
                    if let Ok(data) = std::fs::read(e.path()) {
                        if let Ok(seed) = decrypt_seed(vk, &data) {
                            items.push(BackupSeed { pk: stem.to_string(), seed: seed.to_vec() });
                        }
                    }
                }
            }
        }
    }
    if items.is_empty() {
        return Err("没有可导出的身份".into());
    }
    let plain = serde_json::to_vec(&items).map_err(|e| e.to_string())?;
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let mut bk = derive_mk(password, &salt, M_COST, T_COST, P_COST)?;
    let sealed = seal(&bk, &plain)?;
    bk.zeroize();
    // 版本(1) || salt(16) || (nonce+ct)
    let mut blob = Vec::with_capacity(1 + 16 + sealed.len());
    blob.push(1u8);
    blob.extend_from_slice(&salt);
    blob.extend_from_slice(&sealed);
    Ok(b64(&blob))
}

/// 导入备份串：用备份口令解出种子，再用当前 VK 重新加密写入。返回导入条数。
pub fn import(dir: &Path, vk: &[u8; 32], blob_b64: &str, password: &str) -> Result<usize, String> {
    let blob = unb64(blob_b64)?;
    if blob.len() < 1 + 16 + 24 {
        return Err("备份数据不完整".into());
    }
    if blob[0] != 1 {
        return Err("不支持的备份版本".into());
    }
    let salt = &blob[1..17];
    let sealed = &blob[17..];
    let mut bk = derive_mk(password, salt, M_COST, T_COST, P_COST)?;
    let opened = open(&bk, sealed);
    bk.zeroize();
    let plain = opened.map_err(|_| "备份口令错误或数据损坏".to_string())?;
    let items: Vec<BackupSeed> = serde_json::from_slice(&plain).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(ident_dir(dir)).map_err(|e| e.to_string())?;
    let mut n = 0usize;
    for it in items {
        if it.seed.len() != 32 {
            continue;
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&it.seed);
        let pk = pubkey_of_seed(&seed);
        let enc = encrypt_seed(vk, &seed)?;
        std::fs::write(ident_dir(dir).join(format!("{}.enc", hexstr(&pk))), enc).map_err(|e| e.to_string())?;
        seed.zeroize();
        n += 1;
    }
    Ok(n)
}

// ── 恢复码（P3）：24 词助记词编码随机 RK，RK 封装 VK 存入 vault；忘记主口令时用它重置。 ──

pub fn has_recovery(dir: &Path) -> bool {
    read_meta(dir).ok().and_then(|m| m.recovery_vk).is_some()
}

/// 生成恢复码：随机 RK 封装当前 VK 存入 vault，返回助记词（仅此一次可见）。
pub fn generate_recovery(dir: &Path, vk: &[u8; 32]) -> Result<String, String> {
    let mut rk = [0u8; 32];
    OsRng.fill_bytes(&mut rk);
    let recovery_vk = seal(&rk, vk)?;
    let mnemonic = bip39::Mnemonic::from_entropy(&rk).map_err(|e| e.to_string())?.to_string();
    rk.zeroize();
    let mut meta = read_meta(dir)?;
    meta.recovery_vk = Some(recovery_vk);
    write_meta(dir, &meta)?;
    Ok(mnemonic)
}

/// 用恢复码重置主口令：助记词→RK→解出 VK→用新口令重封（保留恢复码）。
pub fn recover(dir: &Path, mnemonic: &str, new_password: &str) -> Result<(), String> {
    let m = bip39::Mnemonic::parse(mnemonic.trim().to_lowercase()).map_err(|_| "恢复码无效".to_string())?;
    let ent = m.to_entropy();
    if ent.len() != 32 {
        return Err("恢复码长度不符（应为 24 词）".into());
    }
    let mut rk = [0u8; 32];
    rk.copy_from_slice(&ent);
    let meta = read_meta(dir)?;
    let rvk = meta.recovery_vk.ok_or("本机未设置恢复码，无法用它恢复")?;
    let opened = open(&rk, &rvk);
    rk.zeroize();
    let vkv = opened.map_err(|_| "恢复码不匹配".to_string())?;
    if vkv.len() != 32 {
        return Err("保险库损坏".into());
    }
    let mut vk = [0u8; 32];
    vk.copy_from_slice(&vkv);
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let mut mk = derive_mk(new_password, &salt, M_COST, T_COST, P_COST)?;
    let wrapped_vk = seal(&mk, &vk)?;
    let verifier = seal(&mk, VERIFY)?;
    mk.zeroize();
    vk.zeroize();
    write_meta(dir, &VaultMeta {
        version: 1,
        kdf: Kdf { alg: "argon2id".into(), m: M_COST, t: T_COST, p: P_COST, salt: salt.to_vec() },
        verifier,
        wrapped_vk,
        recovery_vk: Some(rvk),
    })
}

// ── 审计日志（P3）：追加式记录安全事件（unix 秒 + 事件）。 ──

pub fn audit(dir: &Path, event: &str) {
    use std::io::Write;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("audit.log")) {
        let _ = writeln!(f, "{ts} {event}");
    }
}

/// 读取最近 n 条审计日志（新→旧）。
pub fn read_audit(dir: &Path, n: usize) -> Vec<String> {
    std::fs::read_to_string(dir.join("audit.log"))
        .map(|s| {
            let mut lines: Vec<String> = s.lines().filter(|x| !x.is_empty()).map(str::to_string).collect();
            lines.reverse();
            lines.truncate(n);
            lines
        })
        .unwrap_or_default()
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
        recovery_vk: None,
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
    let recovery_vk = read_meta(dir)?.recovery_vk;
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
        recovery_vk,
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

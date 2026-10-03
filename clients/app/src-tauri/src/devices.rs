//! 多设备子密钥。每台设备有自己的连接密钥（设备密钥），由账号私钥签发证书后以账号身份收发；
//! 丢了一台设备只吊销它的设备密钥，账号不变。
//! 本机按账号存 `devices/<账号公钥>.dev.enc`（保险库密钥加密）：设备种子 + 证书。
//! 账号私钥 `identities/<账号公钥>.enc` 只在管理设备上有；普通设备只有设备密钥。

use nm_crypto::{DEVICE_ROLE_ADMIN, DEVICE_ROLE_MEMBER};
use nm_proto::DeviceCert;
use prost::Message;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use super::*;

pub const EVENT: &str = "device://event";
/// 节点推来的设备事件（吊销等）。
pub const NODE_EVENT_TYPE: &str = "nmspace.v1/device.event";

#[derive(Serialize, Deserialize)]
struct Stored {
    seed: String,
    cert: String,
}

pub struct DeviceKey {
    pub seed: [u8; 32],
    pub cert: DeviceCert,
}

impl Drop for DeviceKey {
    fn drop(&mut self) {
        self.seed.zeroize();
    }
}

fn b64(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn unb64(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s.trim()).map_err(|_| "设备证书格式错误".to_string())
}

pub fn cert_to_b64(c: &DeviceCert) -> String {
    b64(&c.encode_to_vec())
}

pub fn cert_from_b64(s: &str) -> Result<DeviceCert, String> {
    DeviceCert::decode(unb64(s)?.as_slice()).map_err(|_| "设备证书格式错误".to_string())
}

fn devices_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = data_dir(app)?.join("devices");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn path_of(app: &AppHandle, account: &str) -> Result<std::path::PathBuf, String> {
    Ok(devices_dir(app)?.join(format!("{}.dev.enc", account.to_ascii_lowercase())))
}

pub fn load(app: &AppHandle, vk: &[u8; 32], account: &str) -> Result<Option<DeviceKey>, String> {
    let p = path_of(app, account)?;
    let Ok(data) = std::fs::read(&p) else { return Ok(None) };
    let mut pt = auth::decrypt_blob(vk, &data)?;
    let parsed = serde_json::from_slice::<Stored>(&pt);
    pt.zeroize();
    let mut st = parsed.map_err(|_| "设备密钥文件损坏".to_string())?;
    let seed = pair::unhex32(&st.seed);
    st.seed.zeroize();
    let seed = seed.map_err(|_| "设备密钥文件损坏".to_string())?;
    let cert = cert_from_b64(&st.cert)?;
    Ok(Some(DeviceKey { seed, cert }))
}

pub fn save(app: &AppHandle, vk: &[u8; 32], account: &str, key: &DeviceKey) -> Result<(), String> {
    let mut st = Stored { seed: hex(&key.seed), cert: cert_to_b64(&key.cert) };
    let mut pt = serde_json::to_vec(&st).map_err(|e| e.to_string())?;
    st.seed.zeroize();
    let enc = auth::encrypt_blob(vk, &pt);
    pt.zeroize();
    auth::write_private(&path_of(app, account)?, enc?)
}

pub fn remove(app: &AppHandle, account: &str) {
    if let Ok(p) = path_of(app, account) {
        let _ = std::fs::remove_file(p);
    }
}

/// 本机有设备密钥的账号。
pub fn accounts(app: &AppHandle) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = devices_dir(app).and_then(|d| std::fs::read_dir(d).map_err(|e| e.to_string())) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".dev.enc") {
                if stem.len() == 64 {
                    out.push(stem.to_string());
                }
            }
        }
    }
    out
}

/// 本机能否以该账号登录（有账号私钥或设备密钥）。
pub fn has_local(app: &AppHandle, account: &str) -> bool {
    let a = account.to_ascii_lowercase();
    identities_dir(app).is_ok_and(|d| d.join(format!("{a}.enc")).exists())
        || path_of(app, &a).is_ok_and(|p| p.exists())
}

pub fn has_account_key(app: &AppHandle, account: &str) -> bool {
    identities_dir(app).is_ok_and(|d| d.join(format!("{}.enc", account.to_ascii_lowercase())).exists())
}

pub fn new_seed() -> [u8; 32] {
    nm_transport::SecretKey::generate().to_bytes()
}

pub fn certify(account_seed: &[u8; 32], device: [u8; 32], label: &str, admin: bool) -> DeviceCert {
    let role = if admin { DEVICE_ROLE_ADMIN } else { DEVICE_ROLE_MEMBER };
    let sk = nm_crypto::SecretKey::from_bytes(account_seed);
    nm_crypto::sign_device_cert(&sk, device, label, role, pair::now_ms() as i64)
}

/// 取本机该账号的设备密钥；没有而本机持有账号私钥（老用户或首台设备）时，生成并自签管理证书。
fn ensure(app: &AppHandle, vk: &[u8; 32], account: &str, account_seed: Option<&[u8; 32]>) -> Result<Option<DeviceKey>, String> {
    if let Some(k) = load(app, vk, account)? {
        return Ok(Some(k));
    }
    let Some(ak) = account_seed else { return Ok(None) };
    let seed = new_seed();
    let cert = certify(ak, pubkey_of_seed(&seed), &pairing::device_label(), true);
    let key = DeviceKey { seed, cert };
    save(app, vk, account, &key)?;
    auth::audit(&data_dir(app)?, "device:created");
    Ok(Some(key))
}

/// 以账号登录：优先用设备密钥 + 证书；家节点不支持多设备时，持有账号私钥则退回直连。
pub async fn dial_account(
    app: &AppHandle,
    vk: &[u8; 32],
    account: &str,
    mode: &str,
    node: &str,
    relay_urls: Vec<String>,
    pkarr_url: Option<String>,
    dns_origin: Option<String>,
) -> Result<(Client, Session), String> {
    let mut ak = load_identity_seed(app, vk, account).ok();
    let dev = ensure(app, vk, account, ak.as_ref());
    let out = match dev {
        Ok(Some(dev)) => {
            match dial(dev.seed, mode, node, relay_urls.clone(), pkarr_url.clone(), dns_origin.clone(), Some(&dev.cert)).await {
                Err(e) if e.contains("unknown method") => match &ak {
                    Some(seed) => dial(*seed, mode, node, relay_urls, pkarr_url, dns_origin, None).await,
                    None => Err("Home Node 版本过旧，暂不支持用设备密钥登录".into()),
                },
                Err(e) if e.contains("device_revoked") => {
                    remove(app, account);
                    auth::audit(&data_dir(app)?, "device:revoked");
                    Err("device_revoked".into())
                }
                other => other,
            }
        }
        Ok(None) => Err("用户身份不存在（可能已删除）".into()),
        Err(e) => Err(e),
    };
    if let Some(s) = ak.as_mut() {
        s.zeroize();
    }
    out
}

fn device_json(d: &nm_proto::DeviceInfo, current: &[u8]) -> Value {
    let cert = d.cert.clone().unwrap_or_default();
    json!({
        "device": hex(&cert.device),
        "label": if d.label.is_empty() { cert.label.clone() } else { d.label.clone() },
        "role": cert.role,
        "issuedAt": cert.issued_at,
        "lastSeen": d.last_seen,
        "online": d.online,
        "revoked": d.revoked.as_ref().map(|r| json!({
            "at": r.revoked_at,
            "reason": r.reason,
            "byNode": !r.by_node.is_empty(),
        })),
        "current": cert.device.as_slice() == current,
    })
}

/// 当前登录设备的概况：账号、设备公钥、本机是否持有账号私钥（能否管理其它设备）。
#[tauri::command]
pub async fn device_self(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let session = session_of(&state).await?;
    let account = hex(&session.id_bytes());
    Ok(json!({
        "account": account,
        "device": hex(&session.device_id()),
        "multiDevice": session.device_id() != session.id_bytes(),
        "admin": has_account_key(&app, &account),
    }))
}

#[tauri::command]
pub async fn device_list(state: State<'_, AppState>) -> Result<Vec<Value>, String> {
    let session = session_of(&state).await?;
    let list = session.device_list().await.map_err(|e| e.to_string())?;
    let current = session.device_id();
    Ok(list.iter().map(|d| device_json(d, &current)).collect())
}

#[tauri::command]
pub async fn device_rename(state: State<'_, AppState>, device: String, label: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.device_rename(parse_id(&device)?, label.trim()).await.map_err(|e| e.to_string())
}

/// 吊销一台设备：用本机账号私钥签发吊销记录交给家节点。只有管理设备能做。
#[tauri::command]
pub async fn device_revoke(app: AppHandle, state: State<'_, AppState>, device: String, reason: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    let device = parse_id(&device)?;
    if device == session.device_id() {
        return Err("不能吊销当前正在使用的设备".into());
    }
    let account = session.id_bytes();
    let vk = vk_of(&state)?;
    let mut ak = load_identity_seed(&app, &vk, &hex(&account)).map_err(|_| "not_admin_device".to_string())?;
    let sk = nm_crypto::SecretKey::from_bytes(&ak);
    ak.zeroize();
    let reason = reason.trim().chars().take(64).collect::<String>();
    let r = nm_crypto::sign_device_revoke(&sk, account, device, pair::now_ms() as i64, &reason, false);
    session.device_revoke(&r).await.map_err(|e| e.to_string())?;
    auth::audit(&data_dir(&app)?, "device:revoke");
    Ok(())
}

/// 紧急冻结：手边没有管理设备时，凭账号密码请家节点代为吊销。不要求已登录。
/// `device` 为空时冻结该账号全部设备，之后用持有账号私钥的设备或备份重新登录。返回冻结台数。
#[tauri::command]
pub async fn device_freeze(
    app: AppHandle,
    local: String,
    domain: String,
    password: String,
    device: String,
    registry: String,
) -> Result<usize, String> {
    let (local, domain) = norm_account(&local, &domain)?;
    let device = if device.trim().is_empty() { None } else { Some(parse_id(&device)?) };
    let node = resolve_home_node(&registry, &domain).await?;
    let (_client, session) = dial(new_seed(), "nat", &node, Vec::new(), None, None, None).await?;
    let n = session
        .device_freeze(&local, &domain, &password, device)
        .await
        .map_err(|e| registry_phrase(&e.to_string()))?;
    auth::audit(&data_dir(&app)?, "device:freeze");
    Ok(n)
}

pub fn node_event(app: &AppHandle, body: &[u8]) {
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        let _ = app.emit(EVENT, v);
    }
}

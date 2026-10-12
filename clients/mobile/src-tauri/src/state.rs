use std::sync::Arc;

use nm_client::{Client, Session};
use tauri::{AppHandle, Manager, State};
use tokio::sync::Mutex;

use crate::pairing;

/// 已建立的连接（客户端 + 会话共享句柄）。
pub(crate) struct Conn {
    pub(crate) _client: Client, // 保活 endpoint
    pub(crate) session: Arc<Session>,
    pub(crate) my_id: [u8; 32],
}

#[derive(Default)]
pub(crate) struct AppState {
    pub(crate) conn: Mutex<Option<Conn>>,
    /// 解锁后驻留的保险库密钥 VK；None = 已上锁（见 auth.rs / docs/CLIENT_AUTH_SECURITY.md）。
    pub(crate) vault: std::sync::Mutex<Option<[u8; 32]>>,
    pub(crate) pair: pairing::PairState,
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn parse_id(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    if s.len() != 64 {
        return Err("需要 64 位十六进制的公钥/EntityId".into());
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] =
            u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| "非法十六进制".to_string())?;
    }
    Ok(out)
}

/// 应用数据目录（不存在则创建）。
pub(crate) fn data_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// 身份目录（不存在则创建）。
pub(crate) fn identities_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = data_dir(app)?.join("identities");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// 由种子推导公钥（= 用户公钥 / 节点 id）。
pub(crate) fn pubkey_of_seed(seed: &[u8; 32]) -> [u8; 32] {
    *nm_transport::SecretKey::from_bytes(seed)
        .public()
        .as_bytes()
}

/// 取当前保险库密钥 VK（未解锁则报错，作为命令门禁）。
pub(crate) fn vk_of(state: &State<AppState>) -> Result<[u8; 32], String> {
    (*state.vault.lock().unwrap()).ok_or_else(|| "未解锁，请先输入主口令".to_string())
}

pub(crate) async fn session_of(state: &State<'_, AppState>) -> Result<Arc<Session>, String> {
    let g = state.conn.lock().await;
    Ok(g.as_ref().ok_or("尚未连接")?.session.clone())
}

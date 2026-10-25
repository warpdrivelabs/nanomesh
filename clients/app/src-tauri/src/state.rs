use std::sync::Arc;

use nm_client::{Client, Session};
use tauri::{AppHandle, Manager, State};
use tokio::sync::{Mutex, oneshot};

use crate::pairing;

/// 已建立的连接（客户端 + 会话共享句柄）。
pub(crate) struct Conn {
    pub(crate) _client: Client, // 保活 endpoint
    pub(crate) session: Arc<Session>,
    pub(crate) my_id: [u8; 32],
    /// 连接世代：每次（重）连接 +1。供发送时"单飞重连"判重——并发发送只触发一次真正重连。
    pub(crate) conn_gen: u64,
}

/// connect 的入参快照，供待机唤醒后"发送失败→自动重连"复用（不缓存 seed/cert：
/// 重连经 devices::dial_account 从 vault 现解 seed + 现取 device cert，尊重锁库安全模型）。
#[derive(Clone, Default)]
pub(crate) struct ConnectArgs {
    pub(crate) user: String,
    pub(crate) mode: String,
    pub(crate) node: String,
    pub(crate) display_name: String,
    pub(crate) relay_urls: Vec<String>,
    pub(crate) pkarr_url: Option<String>,
    pub(crate) dns_origin: Option<String>,
}

#[derive(Default)]
pub(crate) struct AppState {
    pub(crate) conn: Mutex<Option<Conn>>,
    /// 解锁后驻留的保险库密钥 VK；None = 已上锁（见 auth.rs / docs/CLIENT_AUTH_SECURITY.md）。
    pub(crate) vault: std::sync::Mutex<Option<[u8; 32]>>,
    pub(crate) pair: pairing::PairState,
    /// 前端就绪闸门：连接后消息事件循环先等它，直到 `imStart` 完成（监听器+用户+本地库就绪）才放水，
    /// 避免离线补投的消息早于前端监听器而丢失。每次 connect 重置。
    pub(crate) im_ready: Mutex<Option<oneshot::Sender<()>>>,
    /// 最近一次成功 connect 的入参，供待机唤醒后自动重连复用。
    pub(crate) last_connect: Mutex<Option<ConnectArgs>>,
    /// 单飞守卫：并发发送同时判死时，只有一个协程真正拨号重连，其余复用结果。
    pub(crate) reconnect_lock: Mutex<()>,
    /// 供重连 helper 在任意位置取 AppHandle（setup 时写入）。
    pub(crate) app_handle: std::sync::OnceLock<AppHandle>,
    /// 换网信号通道（P1 入站主动重连）：换网哨兵检测到本端点地址变化即 `send(())`；
    /// setup 里启动的重连驱动收到后主动重连，恢复入站推送——不必等用户下一次发送。
    /// 用通道而非在哨兵里直接重连：重连 future 含 tauri `State`（非 Send），不能进裸 spawn。
    pub(crate) roam_tx: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<()>>,
    /// 云模型共享：本机上线的 model.llm 运行表（local_id → 任务）。
    pub(crate) llm_runtime: crate::llm_share::LlmRuntime,
    /// 消费方：本机为远端模型启动的 OpenAI 兼容代理（provider_hex → 任务）。
    pub(crate) llm_proxies: crate::llm_proxy::LlmProxies,
    /// 聊天直聊模型：进行中的流(streamId → 取消句柄)。
    pub(crate) llm_chats: crate::llm_chat::LlmChats,
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

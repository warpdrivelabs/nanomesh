//! Tauri 桥接：把界面 IPC 调用转发到进程内的 `nm-client`(iroh)。
//! 原生端(desktop / iOS / android)运行本模块；Web 端改走 nm-gateway。
//! 前端资源在 ../ui（静态壳，generate_context! 编译期内嵌；build.rs 声明 rerun-if-changed）。
//! App 图标源 ../../nanomesh-app.png（tauri icon 生成 icons/*，generate_context! 内嵌为窗口图标）。
//! 实体目录、手动添加的实体、群、频道、节点服务和资料存在 app_data_dir/catalog.db。
//! 界面偏好（主题、语言、身份昵称）仍经 ui_kv_* 写到 app_data_dir/ui-state.json。
//! 本地访问认证见 auth.rs / docs/CLIENT_AUTH_SECURITY.md（P1：主口令门 + 身份私钥信封加密；P2：自动锁定 + 加密备份；P3：恢复码 + 失败冷却 + 审计日志）。
//!
//! 连接策略 **同网优先、穿透兜底**：
//! - `nat`（默认，N0）/ `selfhost`（自建 relay+dns）：`node` 给完整地址(JSON)时**先试同网直连**
//!   （Minimal，零基础设施、可离线）；同网失败或只给公钥，则**转穿透 NAT**（发现 + 中继，按公钥拨号）。
//! - `lan`：仅 Minimal 直连（按地址，无 NAT 兜底）。
//!
//! 客户端身份持久化在应用数据目录（`nmspace.identity`，32 字节），启动复用（稳定 EntityId）。

use std::sync::Arc;

use nm_client::{Client, Session};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, State, async_runtime};
use tokio::sync::Mutex;

mod auth;
mod auth_commands;
mod connection;
mod devices;
mod directory;
mod identity;
mod local_store;
mod media;
mod pair;
mod pairing;
mod registry;
mod runtime;
mod social;
mod state;
#[cfg(desktop)]
mod tray;

pub(crate) use connection::{connect, dial};
pub(crate) use identity::{create_identity, list_identities, load_identity_seed};
pub(crate) use state::{
    AppState, Conn, data_dir, hex, identities_dir, parse_id, pubkey_of_seed, session_of, vk_of,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    runtime::run();
}

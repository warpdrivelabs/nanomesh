//! 换网自愈共享原语（P0）。见 `docs/NETWORK_ROAMING_RECOVERY_DESIGN.md` §5.3/§5.4。
//!
//! 背景：持久会话 [`Session`] 只持**一条** iroh 连接；换网 / 睡眠唤醒 / 对端漂移会使其断开
//! （后台收流循环结束 → `next_command()`/`recv()` 返回 `None`）。各 serve 角色（nm-compute、
//! nm-agentd、nm-echo、nm-agent）此前各自实现"断线重连"或干脆退出进程靠外部托管拉起，
//! 范式不一且易漏。本 mod 把"**按 node id 重连 + 指数退避 + 换网日志**"抽成一处复用。
//!
//! 为何按 **node id** 而非缓存的完整地址：换网后对端（或自身出口）的 IP/relay 会变，
//! 只有按公钥经发现服务(N0/selfhost)重新解析，才能拨到**当前**地址；缓存的旧完整地址会把
//! 连接导向死地址（见设计文档 §3 L2）。

use std::time::Duration;

use crate::{Client, ClientError, Session};

/// 重连退避：首个间隔与上限。换网瞬间可能连环触发，指数退避 + 上限避免风暴。
pub const RECONNECT_BACKOFF_START: Duration = Duration::from_secs(2);
pub const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// 按 **node id(公钥)** 持续重连 home node，直到成功返回新的 [`Session`]。
///
/// 语义：
/// - 永不放弃（除非进程被杀）——换网恢复是"迟早会通"，故无限重试；
/// - 指数退避 `2s → … → 30s`（[`RECONNECT_BACKOFF_START`]..=[`RECONNECT_BACKOFF_MAX`]）；
/// - 每次尝试前后打结构化日志，便于定位换网恢复耗时（§7 验收）。
///
/// 仅负责"拿到一条新连接"。重注册 presence / 重发现 provider 等**角色相关**善后由调用方做
/// （见 [`serve_with_reconnect`] 的 `on_online` 回调）。
pub async fn online_by_id_retry(client: &Client, home_id: [u8; 32], tag: &str) -> Session {
    let mut backoff = RECONNECT_BACKOFF_START;
    let mut attempt: u64 = 0;
    loop {
        attempt += 1;
        match client.online_by_id(home_id).await {
            Ok(sess) => {
                tracing::info!(tag, attempt, "重连成功：已按 id 连上 home node");
                return sess;
            }
            Err(e) => {
                tracing::warn!(
                    tag, attempt, backoff_s = backoff.as_secs(),
                    "重连失败（换网未就绪？）：{e}；退避后重试"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
            }
        }
    }
}

/// 通用 **serve-with-reconnect** 循环：进程内自愈，不退出靠外部托管。
///
/// 流程：`on_online(session)` 跑一轮服务（内部循环 `next_command()`，断线后返回）→ 返回即视为
/// 连接已断 → [`online_by_id_retry`] 拿新连接 → 再次 `on_online` …… 永不退出（直到进程被杀）。
///
/// `on_online` 是"**给一条刚连好的 Session，完成本角色的注册/发现并进入服务循环，连接断开后返回**"
/// 的闭包；首个 Session 由本函数建立，故调用方无需在外部先 `online`。
///
/// 这样 nm-compute / nm-agentd 的差异只剩 `on_online` 里的注册与服务逻辑，重连/退避/日志统一。
///
/// 返回类型为 `Result<(), ClientError>` 仅为可组合（`?`）——本函数**永不正常返回**（无限自愈循环）。
pub async fn serve_with_reconnect<F, Fut>(
    client: &Client,
    home_id: [u8; 32],
    tag: &str,
    mut on_online: F,
) -> Result<(), ClientError>
where
    F: FnMut(Session) -> Fut,
    Fut: std::future::Future<Output = Result<(), ClientError>>,
{
    // 首轮连接也走重试（启动即换网/网络未就绪时同样自愈）。
    let mut session = online_by_id_retry(client, home_id, tag).await;
    loop {
        match on_online(session).await {
            Ok(()) => tracing::info!(tag, "serve 轮结束（连接断开），自动重连…"),
            Err(e) => tracing::warn!(tag, "serve 轮出错：{e}，自动重连…"),
        }
        session = online_by_id_retry(client, home_id, tag).await;
    }
}

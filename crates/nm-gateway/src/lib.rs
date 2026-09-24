//! `nm-gateway` — 浏览器网关：WebTransport / WebSocket ↔ iroh。
//! 浏览器无法直接跑 QUIC/iroh，Web 端经本网关接入。见 PLAN_B §13(浏览器)。
//!
//! TODO：axum 提供 `/ws`（先 WebSocket，后升级 WebTransport），把前端 JSON 事件桥接到 `nm_client`。

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("{0}")]
    Other(String),
}

/// 启动网关（骨架）。
pub async fn serve() -> Result<(), GatewayError> {
    tracing::info!("nm-gateway skeleton (WS/WebTransport bridge) — not yet implemented");
    Ok(())
}

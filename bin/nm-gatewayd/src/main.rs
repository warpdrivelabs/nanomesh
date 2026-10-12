//! `nm-gatewayd` — 浏览器 WebSocket 网关守护进程（JSON `/ws` ↔ iroh）。

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    nm_gateway::serve().await?;
    Ok(())
}

//! `im-gatewayd` — 浏览器网关守护进程（WebTransport/WebSocket ↔ iroh）。

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    im_gateway::serve().await?;
    Ok(())
}

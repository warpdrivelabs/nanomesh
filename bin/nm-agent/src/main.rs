//! `nm-agent` — 无界面实体守护进程：注册为某类型实体并响应命令(RPC)。
//! 示例：注册为 `compute.inference`，处理 `infer` 命令。
//!
//! 用法：先 `cargo run -p nmd`（打印 `NM_NODE_ADDR=...`），再
//! `cargo run -p nm-agent -- --node '<那段 JSON>'`。

use std::collections::HashMap;

use clap::Parser;
use nm_entity::kinds;
use nm_proto::{pb::InferenceProfile, Any};

#[derive(Parser)]
#[command(name = "nm-agent", about = "headless nmspace entity agent")]
struct Args {
    /// 节点地址（nmd 启动时打印的 JSON）。
    #[arg(long)]
    node: String,
    /// 身份种子（0-255），决定本 agent 的公钥。
    #[arg(long, default_value_t = 200)]
    seed: u8,
    /// 展示名。
    #[arg(long, default_value = "inference-agent")]
    name: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();

    let addr = nm_transport::addr_from_string(&args.node)?;
    let client = nm_client::Client::bind_local([args.seed; 32]).await?;
    let mut sess = client.online(addr).await?;

    sess.register_as::<kinds::InferenceService>(
        &InferenceProfile {
            models: vec!["demo-llm".into()],
            concurrency: 1,
            ..Default::default()
        },
        &args.name,
        HashMap::from([("status".to_string(), "idle".to_string())]),
    )
    .await?;
    tracing::info!(id = %client.id().fmt_short(), name = %args.name, "agent online; serving commands");

    // 命令服务循环。
    while let Some((req, cmd)) = sess.next_command().await {
        tracing::info!(method = %cmd.method, "command received");
        let (ok, result, error) = match cmd.method.as_str() {
            "infer" => {
                let input = cmd
                    .params
                    .as_ref()
                    .map(|p| String::from_utf8_lossy(&p.value).to_string())
                    .unwrap_or_default();
                let out = format!("inference of: {input}");
                (
                    true,
                    Some(Any {
                        type_url: "nmspace.v1/text".to_string(),
                        value: out.into_bytes(),
                    }),
                    String::new(),
                )
            }
            other => (false, None, format!("unsupported method: {other}")),
        };
        sess.reply(&req, ok, result, &error).await?;
    }
    Ok(())
}

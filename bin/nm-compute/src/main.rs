//! `nm-compute`（C1）：P2P 模型 provider。把一台有模型/算力的机器接入 nmspace 网络，
//! 注册为 `model.llm` 实体，接 `model.infer` 的 OpenAI 兼容请求并回结果。
//!
//! 用法：
//!   # 离线 Demo（无需 Ollama，验证链路）：
//!   cargo run -p nm-compute -- --node '<NM_NODE_ADDR>' --model demo-llm --backend demo
//!   # 接本机 Ollama（真实推理）：
//!   cargo run -p nm-compute -- --node '<NM_NODE_ADDR>' --model qwen2:7b --backend ollama --ollama-url http://127.0.0.1:11434

use std::collections::HashMap;
use std::sync::Arc;

use clap::Parser;
use nm_compute::{Backend, METHOD_INFER, RESP_TYPE_URL};
use nm_entity::kinds;
use nm_proto::pb::InferenceProfile;
use nm_proto::Any;

#[derive(Parser)]
#[command(name = "nm-compute", about = "P2P 模型/算力 provider (C1)")]
struct Args {
    /// 节点地址（nmd 启动时打印的 NM_NODE_ADDR JSON）。
    #[arg(long)]
    node: String,
    /// 身份种子（0-255），决定本 provider 的公钥（固定=稳定 id）。
    #[arg(long, default_value_t = 160)]
    seed: u8,
    /// 对外声明的模型名（写入 attributes，供消费方 directory 过滤）。
    #[arg(long, default_value = "demo-llm")]
    model: String,
    /// 后端：demo（离线回声）| ollama（本机 OpenAI 兼容端点）。
    #[arg(long, default_value = "demo")]
    backend: String,
    /// Ollama 基址（backend=ollama 时用）。
    #[arg(long, default_value = "http://127.0.0.1:11434")]
    ollama_url: String,
    /// 可选发现标签。
    #[arg(long, default_value = "")]
    gpu: String,
    #[arg(long, default_value = "")]
    region: String,
    /// 展示名。
    #[arg(long, default_value = "模型算力")]
    name: String,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();

    let backend = Arc::new(match args.backend.as_str() {
        "ollama" => Backend::ollama(args.ollama_url.as_str()),
        _ => Backend::Demo,
    });

    let addr = nm_transport::addr_from_string(&args.node)?;
    let client = nm_client::Client::bind_local([args.seed; 32]).await?;
    let mut sess = client.online(addr).await?;

    // 发现标签（§13.2）：model / api / status（+ 可选 gpu/region）。
    let mut attrs = HashMap::new();
    attrs.insert("model".to_string(), args.model.clone());
    attrs.insert("api".to_string(), "openai.chat.v1".to_string());
    attrs.insert("status".to_string(), "idle".to_string());
    if !args.gpu.is_empty() {
        attrs.insert("gpu".to_string(), args.gpu.clone());
    }
    if !args.region.is_empty() {
        attrs.insert("region".to_string(), args.region.clone());
    }

    sess.register_as::<kinds::ModelLlm>(
        &InferenceProfile { models: vec![args.model.clone()], ..Default::default() },
        &args.name,
        attrs,
    )
    .await?;
    println!(
        "nm-compute online: kind=model.llm  model={}  backend={}  id={}",
        args.model,
        args.backend,
        hex(&client.id_bytes())
    );
    println!("=> 消费方 directory_query(kind_prefix=\"model.\") 可发现本 provider，call(\"model.infer\") 推理。");

    let model_label = args.model.clone();
    while let Some((req, cmd)) = sess.next_command().await {
        if cmd.method != METHOD_INFER {
            let _ = sess.reply(&req, false, None, &format!("不支持的方法: {}", cmd.method)).await;
            continue;
        }
        let params = cmd.params.as_ref().map(|p| p.value.clone()).unwrap_or_default();
        match backend.handle_infer(&model_label, &params).await {
            Ok(json) => {
                let any = Any { type_url: RESP_TYPE_URL.to_string(), value: json };
                let _ = sess.reply(&req, true, Some(any), "").await;
            }
            Err(e) => {
                let _ = sess.reply(&req, false, None, &format!("推理失败: {e}")).await;
            }
        }
    }
    Ok(())
}

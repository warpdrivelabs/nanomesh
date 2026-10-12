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
use nm_compute::{Backend, CHUNK_TYPE_URL, METHOD_INFER, RESP_TYPE_URL};
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
    // 换网自愈：按 home node 的**公钥 id** 重连（而非缓存的完整地址）——换网后发现服务会重解析
    // 其当前地址。见 docs/NETWORK_ROAMING_RECOVERY_DESIGN.md §5.3/§5.4。
    let home_id: [u8; 32] = *addr.id.as_bytes();
    // N0 预设（n0 公共中继 + DNS/pkarr 发现）：按公钥发现远程 home node、穿透 NAT。
    // （原 bind_local=Infra::Local/Minimal，无中继无发现，仅同网直连——连不上远程服务器。）
    let client = nm_client::Client::bind([args.seed; 32]).await?;
    // 本端点地址变化（换网）结构化日志，便于定位恢复耗时。
    client.watch_addr_log("nm-compute");

    println!(
        "nm-compute online: kind=model.llm  model={}  backend={}  id={}",
        args.model,
        args.backend,
        hex(&client.id_bytes())
    );
    println!("=> 消费方 directory_query(kind_prefix=\"model.\") 可发现本 provider，call(\"model.infer\") 推理。");

    let model_label = args.model.clone();
    // serve-with-reconnect：进程内自愈，断线（含换网）自动按 id 重连 + 重注册，不退出靠外部托管。
    nm_client::serve_with_reconnect(&client, home_id, "nm-compute", |mut sess| {
        let backend = backend.clone();
        let model_label = model_label.clone();
        let args_name = args.name.clone();
        let args_model = args.model.clone();
        let gpu = args.gpu.clone();
        let region = args.region.clone();
        async move {
            // 发现标签（§13.2）：model / api / status（+ 可选 gpu/region）。
            let mut attrs = HashMap::new();
            attrs.insert("model".to_string(), args_model.clone());
            attrs.insert("api".to_string(), "openai.chat.v1".to_string());
            attrs.insert("status".to_string(), "idle".to_string());
            if !gpu.is_empty() {
                attrs.insert("gpu".to_string(), gpu);
            }
            if !region.is_empty() {
                attrs.insert("region".to_string(), region);
            }
            sess.register_as::<kinds::ModelLlm>(
                &InferenceProfile { models: vec![args_model.clone()], ..Default::default() },
                &args_name,
                attrs,
            )
            .await?;

            // 命令循环：连接断开时 next_command() 返回 None → 本轮结束 → 外层自动重连。
            while let Some((req, cmd)) = sess.next_command().await {
                if cmd.method != METHOD_INFER {
                    let _ = sess.reply(&req, false, None, &format!("不支持的方法: {}", cmd.method)).await;
                    continue;
                }
                let params = cmd.params.as_ref().map(|p| p.value.clone()).unwrap_or_default();
                let streaming = serde_json::from_slice::<serde_json::Value>(&params)
                    .ok()
                    .and_then(|v| v["stream"].as_bool())
                    .unwrap_or(false);
                if streaming {
                    // 流式：provider 逐帧回（中途 chunk done=false + 终帧完整 completion done=true）。
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, bool)>();
                    let backend2 = backend.clone();
                    let model2 = model_label.clone();
                    let params2 = params.clone();
                    tokio::spawn(async move {
                        if let Err(e) = backend2.stream_infer(&model2, &params2, tx).await {
                            tracing::warn!("stream_infer: {e}");
                        }
                    });
                    let mut seq = 0u32;
                    while let Some((value, done)) = rx.recv().await {
                        let type_url = if done { RESP_TYPE_URL } else { CHUNK_TYPE_URL }.to_string();
                        if sess.reply_frame(&req, true, Some(Any { type_url, value }), "", seq, done).await.is_err() {
                            break;
                        }
                        seq += 1;
                    }
                } else {
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
            }
            Ok(())
        }
    })
    .await?;
    Ok(())
}

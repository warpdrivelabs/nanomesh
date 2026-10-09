#![cfg(feature = "nm-client")]
//! A1 端到端：真实 P2P 节点上，cmx-agent 内核的 `ModelSeam`（本 crate 的 [`P2pModelSeam`]）
//! 经 [`SessionInferChannel`]（nm-client C2 → `Session::infer`）从 `nm-compute` 的 **Demo provider**
//! 取模型输出。两个用例：
//!   1. 直接 `ModelSeam::complete` 走真实 P2P（证 façade↔P2P 打通，非 A0 测试桩）。
//!   2. 真·**cmx-agent 回合**：`Agent::run_turn` 的模型输出来自 P2P provider（证「agent 回合→seam→P2P」端到端）。
//!
//! 运行：`cargo test --manifest-path crates/nm-agent-bridge/Cargo.toml --features nm-client`

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use cmx_agent_core::{Agent, ModelContext, ModelMessage, ModelSeam, Session, StopReason};
use nm_agent_bridge::{P2pModelSeam, SessionInferChannel};
use nm_compute::{Backend, CHUNK_TYPE_URL, METHOD_INFER, RESP_TYPE_URL};
use nm_entity::kinds;
use nm_proto::pb::{AgentProfile, InferenceProfile, PersonProfile};
use nm_transport::Addr;

/// 起一个本地节点 + 一个 Demo 模型 provider（model.llm）。返回节点地址 + **provider Client**
/// （须由调用方持有存活——`Session` 只持 `conn`，Client 持 iroh endpoint，Client 一 drop 连接即断）。
async fn spawn_node_and_provider(node_seed: u8, prov_seed: u8) -> (Addr, nm_client::Client) {
    let node = nm_node::Node::bind_local([node_seed; 32]).await.expect("bind node");
    let addr = node.addr();
    tokio::spawn(async move {
        let _ = node.serve().await;
    });

    let prov_client = nm_client::Client::bind_local([prov_seed; 32]).await.expect("bind provider");
    let mut provider = tokio::time::timeout(Duration::from_secs(20), prov_client.online(addr.clone()))
        .await
        .expect("provider online timed out")
        .expect("provider online failed");
    let mut attrs = HashMap::new();
    attrs.insert("model".to_string(), "demo-llm".to_string());
    attrs.insert("api".to_string(), "openai.chat.v1".to_string());
    provider
        .register_as::<kinds::ModelLlm>(
            &InferenceProfile { models: vec!["demo-llm".into()], ..Default::default() },
            "模型算力A",
            attrs,
        )
        .await
        .expect("provider register failed");

    tokio::spawn(async move {
        let backend = std::sync::Arc::new(Backend::Demo);
        while let Some((req, cmd)) = provider.next_command().await {
            if cmd.method != METHOD_INFER {
                let _ = provider.reply(&req, false, None, "unsupported method").await;
                continue;
            }
            let params = cmd.params.as_ref().map(|p| p.value.clone()).unwrap_or_default();
            let streaming = serde_json::from_slice::<serde_json::Value>(&params)
                .ok()
                .and_then(|v| v["stream"].as_bool())
                .unwrap_or(false);
            if streaming {
                // 真流式：Backend::stream_infer 产帧 → reply_frame 逐帧回（中途 chunk + 终帧完整）。
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, bool)>();
                let backend2 = backend.clone();
                tokio::spawn(async move {
                    let _ = backend2.stream_infer("demo-llm", &params, tx).await;
                });
                let mut seq = 0u32;
                while let Some((value, done)) = rx.recv().await {
                    let type_url =
                        if done { RESP_TYPE_URL } else { nm_compute::CHUNK_TYPE_URL }.to_string();
                    if provider
                        .reply_frame(&req, true, Some(nm_proto::Any { type_url, value }), "", seq, done)
                        .await
                        .is_err()
                    {
                        break;
                    }
                    seq += 1;
                }
            } else {
                match backend.handle_infer("demo-llm", &params).await {
                    Ok(json) => {
                        let any = nm_proto::Any { type_url: RESP_TYPE_URL.to_string(), value: json };
                        let _ = provider.reply(&req, true, Some(any), "").await;
                    }
                    Err(e) => {
                        let _ = provider.reply(&req, false, None, &format!("infer failed: {e}")).await;
                    }
                }
            }
        }
    });

    (addr, prov_client)
}

/// 消费者上线 → 发现 provider → 用 `SessionInferChannel` 把 `Session` 变成推理通道 → 组装 seam。
/// 返回 seam + **consumer Client**（同样须持有存活，否则 endpoint drop、driver 里的 Session 连接断）。
async fn build_seam(addr: Addr, cons_seed: u8) -> (P2pModelSeam<SessionInferChannel>, nm_client::Client) {
    let cons_client = nm_client::Client::bind_local([cons_seed; 32]).await.expect("bind consumer");
    let consumer = cons_client.online(addr).await.expect("consumer online failed");
    consumer
        .register_as::<kinds::Person>(&PersonProfile::default(), "消费者", HashMap::new())
        .await
        .expect("consumer register failed");

    let found = consumer.find_model_providers(Some("demo-llm")).await.expect("discover failed");
    assert_eq!(found.len(), 1, "应发现 1 个 demo-llm provider");
    let provider_target: [u8; 32] = found[0].entity_id.clone().try_into().expect("id 32 bytes");

    // 关键：Session 非 Sync，交给 driver task；得到 Send+Sync 的推理通道。
    let channel = SessionInferChannel::spawn(consumer, provider_target);
    (P2pModelSeam::new(channel, "demo-llm"), cons_client)
}

/// 用例 1：`ModelSeam::complete` 经真实 P2P 取回 demo 回声 + usage。
#[tokio::test]
async fn model_seam_completes_over_real_p2p() {
    let (addr, _prov_client) = spawn_node_and_provider(51, 52).await;
    let (seam, _cons_client) = build_seam(addr, 53).await;

    let ctx = ModelContext {
        system: Some("你是助手".into()),
        messages: vec![ModelMessage::user("ping over p2p")],
        tools: Vec::new(),
    };
    let m: &dyn ModelSeam = &seam; // 以内核 trait 对象调用，证真是 ModelSeam
    let resp = tokio::time::timeout(Duration::from_secs(15), m.complete(&ctx))
        .await
        .expect("complete timed out")
        .expect("complete failed");

    let text = resp.text.expect("response text");
    assert!(text.contains("ping over p2p"), "demo provider 应回声用户输入；实得: {text}");
    assert!(resp.usage.is_some(), "应带 usage 计量");
}

/// 用例 2：真·cmx-agent 回合——`Agent::run_turn` 的助手输出来自 P2P provider。
/// 纯文本回合（模型无 tool_calls）一步完成、零 I/O（除 seam 自身的 P2P 调用），见内核 run_turn_inner。
#[tokio::test]
async fn agent_turn_uses_model_over_p2p() {
    let (addr, _prov_client) = spawn_node_and_provider(54, 55).await;
    let (seam, _cons_client) = build_seam(addr, 56).await;

    // 注入我们的 P2P seam 作为 agent 的模型缝；其余（工具/守卫/审批）走内核默认。
    let agent = Agent::builder()
        .model(Arc::new(seam) as Arc<dyn ModelSeam>)
        .build()
        .expect("build agent");

    let mut session = Session::new("a1-over-p2p");
    let out = tokio::time::timeout(
        Duration::from_secs(15),
        agent.run_turn(&mut session, "ping through agent"),
    )
    .await
    .expect("run_turn timed out")
    .expect("run_turn failed");

    assert_eq!(out.reason, StopReason::Completed, "纯文本回合应 Completed");
    assert_eq!(out.steps, 1, "一步完成");
    let final_text = out.final_text.expect("assistant final text");
    assert!(
        final_text.contains("ping through agent"),
        "助手最终文本应含 demo provider 对用户输入的回声；实得: {final_text}"
    );
}

/// 用例 3：①agent 作为 P2P bot 服务他人——另一个对端发 DM，bot 经 agent 回合（模型走 P2P）回包。
#[tokio::test]
async fn agent_bot_serves_a_peer_over_p2p() {
    let (addr, _prov_client) = spawn_node_and_provider(61, 62).await;

    // bot：上线 → 注册 agent.assistant → 发现 provider → spawn serve。
    let bot_client = nm_client::Client::bind_local([63u8; 32]).await.expect("bind bot");
    let mut bot = bot_client.online(addr.clone()).await.expect("bot online");
    let mut attrs = HashMap::new();
    attrs.insert("model".to_string(), "demo-llm".to_string());
    bot.register_as::<kinds::AgentAssistant>(
        &AgentProfile { backend: "nmspace-p2p".into(), ..Default::default() },
        "服务 bot",
        attrs,
    )
    .await
    .expect("bot register");
    let bot_id = bot_client.id_bytes();
    let found = bot.find_model_providers(Some("demo-llm")).await.expect("discover");
    let provider_id: [u8; 32] = found[0].entity_id.clone().try_into().expect("id 32 bytes");
    tokio::spawn(async move {
        let _ = nm_agent_bridge::serve::serve(bot, provider_id, "demo-llm", None).await;
    });

    // peer：上线 → 向 bot 发 DM → 收回复。
    let peer_client = nm_client::Client::bind_local([64u8; 32]).await.expect("bind peer");
    let mut peer = peer_client.online(addr).await.expect("peer online");
    peer.register_as::<kinds::Person>(&PersonProfile::default(), "用户", HashMap::new())
        .await
        .expect("peer register");
    peer.send_to(bot_id, "hello bot").await.expect("send to bot");

    let (reply, deltas) = recv_streamed_reply(&mut peer).await;
    assert!(reply.contains("hello bot"), "bot 应经 agent 回合回声用户输入；实得: {reply}");
    assert!(deltas >= 2, "应为多帧 token 流式（打字机），实收中途帧数: {deltas}");
}

/// 收集一个对端收到的流式回复（delta 帧，累计全文），返回 (done 帧的完整文本, 收到的中途帧数)。
async fn recv_streamed_reply(peer: &mut nm_client::Session) -> (String, usize) {
    let mut deltas = 0usize;
    loop {
        let gram = tokio::time::timeout(Duration::from_secs(20), peer.recv())
            .await
            .expect("等回复超时")
            .expect("收件箱关闭");
        if gram.kind() != nm_proto::GramKind::Message {
            continue;
        }
        let Some(p) = gram.payload.as_ref() else { continue };
        if p.type_url != nm_agent_bridge::serve::AGENT_DELTA_TYPE_URL {
            continue;
        }
        let frame: serde_json::Value = serde_json::from_slice(&p.value).expect("delta JSON");
        if frame["done"].as_bool().unwrap_or(false) {
            return (frame["text"].as_str().unwrap_or_default().to_string(), deltas);
        }
        deltas += 1; // 中途（非 done）token 帧
    }
}

/// ③ 并发：一个 bot 同时服务两个对端，各自拿到**自己**消息的回声（per-peer 会话不串味）。
#[tokio::test]
async fn agent_bot_serves_two_peers() {
    let (addr, _prov_client) = spawn_node_and_provider(71, 72).await;

    let bot_client = nm_client::Client::bind_local([73u8; 32]).await.expect("bind bot");
    let mut bot = bot_client.online(addr.clone()).await.expect("bot online");
    let mut attrs = HashMap::new();
    attrs.insert("model".to_string(), "demo-llm".to_string());
    bot.register_as::<kinds::AgentAssistant>(
        &AgentProfile { backend: "nmspace-p2p".into(), ..Default::default() },
        "服务 bot",
        attrs,
    )
    .await
    .expect("bot register");
    let bot_id = bot_client.id_bytes();
    let found = bot.find_model_providers(Some("demo-llm")).await.expect("discover");
    let provider_id: [u8; 32] = found[0].entity_id.clone().try_into().expect("id 32 bytes");
    tokio::spawn(async move {
        let _ = nm_agent_bridge::serve::serve(bot, provider_id, "demo-llm", None).await;
    });

    // 两个对端各发不同消息。
    let a_client = nm_client::Client::bind_local([74u8; 32]).await.expect("bind a");
    let mut a = a_client.online(addr.clone()).await.expect("a online");
    a.register_as::<kinds::Person>(&PersonProfile::default(), "甲", HashMap::new())
        .await
        .expect("a reg");
    let b_client = nm_client::Client::bind_local([75u8; 32]).await.expect("bind b");
    let mut b = b_client.online(addr).await.expect("b online");
    b.register_as::<kinds::Person>(&PersonProfile::default(), "乙", HashMap::new())
        .await
        .expect("b reg");

    a.send_to(bot_id, "from-alice").await.expect("a send");
    b.send_to(bot_id, "from-bob").await.expect("b send");

    // 并发收两路回复（不同对端的 handler 并行）。
    let ((ra, _), (rb, _)) = tokio::join!(recv_streamed_reply(&mut a), recv_streamed_reply(&mut b));
    assert!(ra.contains("from-alice"), "甲应收到对自己输入的回声；实得: {ra}");
    assert!(rb.contains("from-bob"), "乙应收到对自己输入的回声；实得: {rb}");
}

/// 实盘：真 Ollama 流式手验。起节点 + Ollama provider + 消费端，`infer_stream` 逐 token 打印(带时间戳)，
/// 证真·模型 token 流式(provider→P2P→消费端)。需本机/可达 Ollama + 已拉模型。默认跳过。
/// 运行：`OLLAMA_URL=http://127.0.0.1:11434 OLLAMA_MODEL=qwen2:0.5b \
///   cargo test --manifest-path crates/nm-agent-bridge/Cargo.toml --features nm-client \
///   --test e2e_p2p_seam ollama_stream_live -- --ignored --nocapture`
#[tokio::test]
#[ignore = "needs a running Ollama + pulled model; see doc comment"]
async fn ollama_stream_live() {
    let url = std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".into());
    let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen2:0.5b".into());
    eprintln!("ollama_stream_live: url={url} model={model}");

    let node = nm_node::Node::bind_local([81u8; 32]).await.expect("bind node");
    let addr = node.addr();
    tokio::spawn(async move {
        let _ = node.serve().await;
    });

    // Ollama provider（真流式）。
    let prov_client = nm_client::Client::bind_local([82u8; 32]).await.expect("bind provider");
    let mut provider = prov_client.online(addr.clone()).await.expect("provider online");
    let mut attrs = HashMap::new();
    attrs.insert("model".to_string(), model.clone());
    provider
        .register_as::<kinds::ModelLlm>(
            &InferenceProfile { models: vec![model.clone()], ..Default::default() },
            "ollama",
            attrs,
        )
        .await
        .expect("provider register");
    let backend = Arc::new(Backend::ollama(&url));
    let model_label = model.clone();
    tokio::spawn(async move {
        while let Some((req, cmd)) = provider.next_command().await {
            if cmd.method != METHOD_INFER {
                let _ = provider.reply(&req, false, None, "unsupported").await;
                continue;
            }
            let params = cmd.params.as_ref().map(|p| p.value.clone()).unwrap_or_default();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, bool)>();
            let b2 = backend.clone();
            let ml = model_label.clone();
            tokio::spawn(async move {
                let _ = b2.stream_infer(&ml, &params, tx).await;
            });
            let mut seq = 0u32;
            while let Some((value, done)) = rx.recv().await {
                let type_url = if done { RESP_TYPE_URL } else { CHUNK_TYPE_URL }.to_string();
                if provider
                    .reply_frame(&req, true, Some(nm_proto::Any { type_url, value }), "", seq, done)
                    .await
                    .is_err()
                {
                    break;
                }
                seq += 1;
            }
        }
    });

    // 消费端：infer_stream，逐 token 打印到达时间。
    let cons_client = nm_client::Client::bind_local([83u8; 32]).await.expect("bind consumer");
    let consumer = cons_client.online(addr).await.expect("consumer online");
    consumer
        .register_as::<kinds::Person>(&PersonProfile::default(), "user", HashMap::new())
        .await
        .expect("consumer register");
    let found = consumer.find_model_providers(Some(&model)).await.expect("discover");
    let provider_id: [u8; 32] = found[0].entity_id.clone().try_into().expect("id 32 bytes");

    let req = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": "用一句话介绍你自己" }],
        "stream": true,
    });
    let mut stream = consumer
        .infer_stream(provider_id, &serde_json::to_vec(&req).unwrap())
        .await
        .expect("infer_stream");

    let t0 = std::time::Instant::now();
    let mut chunks = 0usize;
    let mut full = String::new();
    while let Some(cr) = tokio::time::timeout(Duration::from_secs(60), stream.next())
        .await
        .expect("流超时")
    {
        assert!(cr.ok, "provider 错误: {}", cr.error);
        let Some(a) = cr.result else { continue };
        if cr.done {
            let v: serde_json::Value = serde_json::from_slice(&a.value).unwrap_or_default();
            let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
            eprintln!(
                "[{:>6}ms] DONE  chunks={chunks}  完整文本({} 字): {content}",
                t0.elapsed().as_millis(),
                content.chars().count()
            );
            break;
        }
        let v: serde_json::Value = serde_json::from_slice(&a.value).unwrap_or_default();
        if let Some(d) = v["delta"].as_str() {
            if !d.is_empty() {
                chunks += 1;
                full.push_str(d);
                eprintln!("[{:>6}ms] +{:?}", t0.elapsed().as_millis(), d);
            }
        }
    }
    eprintln!("累计: {full}");
    assert!(chunks >= 2, "真 Ollama 应多帧 token 流；实收 {chunks} 帧");
    assert!(!full.is_empty(), "流式累计文本不应为空");
}

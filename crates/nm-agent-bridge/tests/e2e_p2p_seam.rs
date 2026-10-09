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
use nm_compute::{Backend, METHOD_INFER, RESP_TYPE_URL};
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
        let backend = Backend::Demo;
        while let Some((req, cmd)) = provider.next_command().await {
            if cmd.method != METHOD_INFER {
                let _ = provider.reply(&req, false, None, "unsupported method").await;
                continue;
            }
            let params = cmd.params.as_ref().map(|p| p.value.clone()).unwrap_or_default();
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

    // bot 的回复（经 agent 回合 + P2P 模型）回到 peer 的收件箱。
    let reply = loop {
        let gram = tokio::time::timeout(Duration::from_secs(20), peer.recv())
            .await
            .expect("等 bot 回复超时")
            .expect("peer 收件箱关闭");
        if gram.kind() != nm_proto::GramKind::Message {
            continue;
        }
        break gram
            .payload
            .as_ref()
            .map(|p| String::from_utf8_lossy(&p.value).to_string())
            .unwrap_or_default();
    };
    assert!(reply.contains("hello bot"), "bot 应经 agent 回合回声用户输入；实得: {reply}");
}

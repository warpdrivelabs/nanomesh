#![cfg(feature = "nm-client")]
//! Bug 2 回归：serve bot 对入站消息按 (sender,gram_id) 去重——一条消息只跑一次回合、只回一次；
//! 不同 gram_id 的不同消息不被误伤。防「同一条消息经多路径到达 agent → 重复回复」。demo 后端确定性。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_compute::{Backend, METHOD_INFER, RESP_TYPE_URL, CHUNK_TYPE_URL};
use nm_entity::kinds;
use nm_proto::pb::{AgentProfile, InferenceProfile, PersonProfile};

async fn node_and_demo_provider(nseed: u8, pseed: u8) -> (nm_transport::Addr, nm_client::Client) {
    let node = nm_node::Node::bind_local([nseed; 32]).await.unwrap();
    let addr = node.addr();
    tokio::spawn(async move { let _ = node.serve().await; });
    let pc = nm_client::Client::bind_local([pseed; 32]).await.unwrap();
    let mut p = pc.online(addr.clone()).await.unwrap();
    let mut a = HashMap::new();
    a.insert("model".into(), "demo-llm".into());
    p.register_as::<kinds::ModelLlm>(&InferenceProfile { models: vec!["demo-llm".into()], ..Default::default() }, "prov", a).await.unwrap();
    tokio::spawn(async move {
        let backend = Arc::new(Backend::Demo);
        while let Some((req, cmd)) = p.next_command().await {
            if cmd.method != METHOD_INFER { let _ = p.reply(&req, false, None, "x").await; continue; }
            let params = cmd.params.as_ref().map(|x| x.value.clone()).unwrap_or_default();
            let stream = serde_json::from_slice::<serde_json::Value>(&params).ok().and_then(|v| v["stream"].as_bool()).unwrap_or(false);
            if stream {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, bool)>();
                let b = backend.clone();
                tokio::spawn(async move { let _ = b.stream_infer("demo-llm", &params, tx).await; });
                let mut seq = 0u32;
                while let Some((val, done)) = rx.recv().await {
                    let tu = if done { RESP_TYPE_URL } else { CHUNK_TYPE_URL }.to_string();
                    if p.reply_frame(&req, true, Some(nm_proto::Any { type_url: tu, value: val }), "", seq, done).await.is_err() { break; }
                    seq += 1;
                }
            } else {
                let json = backend.handle_infer("demo-llm", &params).await.unwrap();
                let _ = p.reply(&req, true, Some(nm_proto::Any { type_url: RESP_TYPE_URL.into(), value: json }), "").await;
            }
        }
    });
    (addr, pc)
}

#[tokio::test]
async fn one_message_one_reply() {
    let (addr, _pc) = node_and_demo_provider(121, 122).await;

    // bot serve
    let bc = nm_client::Client::bind_local([123u8; 32]).await.unwrap();
    let mut bot = bc.online(addr.clone()).await.unwrap();
    let mut attrs = HashMap::new();
    attrs.insert("model".into(), "demo-llm".into());
    bot.register_as::<kinds::AgentAssistant>(&AgentProfile { backend: "p2p".into(), ..Default::default() }, "Bot", attrs).await.unwrap();
    let bot_id = bc.id_bytes();
    let found = bot.find_model_providers(Some("demo-llm")).await.unwrap();
    let pid: [u8; 32] = found[0].entity_id.clone().try_into().unwrap();
    tokio::spawn(async move { let _ = nm_agent_bridge::serve::serve(bot, pid, "demo-llm", None).await; });

    // sender: 发一条
    let sc = nm_client::Client::bind_local([124u8; 32]).await.unwrap();
    let mut send = sc.online(addr).await.unwrap();
    send.register_as::<kinds::Person>(&PersonProfile::default(), "U", HashMap::new()).await.unwrap();
    send.send_to(bot_id, "你好").await.unwrap();
    send.send_to(bot_id, "再见").await.unwrap(); // 第二条不同消息：确认去重不会误伤不同 gram_id

    // 收 6 秒内所有 delta 帧，按 streamId 分组，数有几个 done。
    let mut done_streams = std::collections::HashSet::new();
    let mut total_frames = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while let Ok(Some(g)) = tokio::time::timeout_at(deadline, send.recv()).await {
        if g.kind() != nm_proto::GramKind::Message { continue; }
        let Some(p) = g.payload.as_ref() else { continue };
        if p.type_url != nm_agent_bridge::serve::AGENT_DELTA_TYPE_URL { continue; }
        total_frames += 1;
        let v: serde_json::Value = serde_json::from_slice(&p.value).unwrap();
        if v["done"].as_bool().unwrap_or(false) {
            done_streams.insert(v["streamId"].as_str().unwrap_or("").to_string());
        }
    }
    eprintln!("[repro] sender saw total_frames={} done_streams={}", total_frames, done_streams.len());
    // 两条不同消息 → 恰好 2 个完成流（去重按 gram_id，不误伤不同消息；也不重复回复同一条）。
    assert_eq!(done_streams.len(), 2, "两条不同消息应得 2 个完成的回复流，实得 {}", done_streams.len());
}

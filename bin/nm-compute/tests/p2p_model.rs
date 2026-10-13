//! P2P 模型能力网络端到端（C1 provider + C2 consumer）：
//! provider 以真实 `nm_compute::Backend`（Demo，离线确定性）注册 `model.llm`，
//! 接 `model.infer` 的 OpenAI 兼容请求；consumer 用 nm-client 的 C2 便捷层
//! `find_model_providers` 发现 + `infer` 发一次推理，经节点路由走通整条链路。

use std::collections::HashMap;
use std::time::Duration;

use nm_compute::{Backend, METHOD_INFER, RESP_TYPE_URL};
use nm_entity::kinds;
use nm_proto::pb::{InferenceProfile, PersonProfile};

#[tokio::test]
async fn p2p_model_infer_openai_roundtrip() {
    let node = nm_node::Node::bind_local([41u8; 32]).await.expect("bind node");
    let addr = node.addr();
    tokio::spawn(async move {
        let _ = node.serve().await;
    });

    // ── C1：模型 provider 上线、注册为 model.llm、起 model.infer 命令循环（真实 Backend::Demo）──
    let prov_client = nm_client::Client::bind_local([42u8; 32]).await.expect("bind provider");
    let mut provider =
        tokio::time::timeout(Duration::from_secs(20), prov_client.online(addr.clone()))
            .await
            .expect("provider online timed out")
            .expect("provider online failed");
    let mut attrs = HashMap::new();
    attrs.insert("model".to_string(), "demo-llm".to_string());
    attrs.insert("api".to_string(), "openai.chat.v1".to_string());
    attrs.insert("status".to_string(), "idle".to_string());
    provider
        .register_as::<kinds::ModelLlm>(
            &InferenceProfile { models: vec!["demo-llm".into()], ..Default::default() },
            "模型算力A",
            attrs,
        )
        .await
        .expect("provider register failed");
    let provider_id = provider.id_bytes();

    tokio::spawn(async move {
        let backend = Backend::Demo;
        let model_label = "demo-llm".to_string();
        while let Some((req, cmd)) = provider.next_command().await {
            if cmd.method != METHOD_INFER {
                let _ = provider.reply(&req, false, None, "unsupported method").await;
                continue;
            }
            let params = cmd.params.as_ref().map(|p| p.value.clone()).unwrap_or_default();
            match backend.handle_infer(&model_label, &params).await {
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

    // ── C2：consumer 上线、注册、用便捷层发现 + 推理 ──
    let cons_client = nm_client::Client::bind_local([43u8; 32]).await.expect("bind consumer");
    let consumer = cons_client.online(addr.clone()).await.expect("consumer online failed");
    consumer
        .register_as::<kinds::Person>(&PersonProfile::default(), "消费者", HashMap::new())
        .await
        .expect("consumer register failed");

    // find_model_providers：按 model 名过滤命中 1；不存在的名命中 0。
    let found = consumer.find_model_providers(Some("demo-llm")).await.expect("discover failed");
    assert_eq!(found.len(), 1, "应发现 1 个 demo-llm provider");
    let target: [u8; 32] = found[0].entity_id.clone().try_into().expect("id 32 bytes");
    assert_eq!(target, provider_id);
    let none = consumer.find_model_providers(Some("no-such-model")).await.expect("discover failed");
    assert_eq!(none.len(), 0, "不存在的模型名应命中 0");

    // infer：发 OpenAI 兼容请求，取回 chat.completion，验回声与 usage。
    let req = r#"{"model":"demo-llm","messages":[{"role":"user","content":"你好 世界"}]}"#.as_bytes();
    let out = tokio::time::timeout(Duration::from_secs(15), consumer.infer(target, req))
        .await
        .expect("infer timed out")
        .expect("infer failed");
    let v: serde_json::Value = serde_json::from_slice(&out).expect("response is json");
    assert_eq!(v["object"], "chat.completion");
    assert_eq!(v["model"], "demo-llm");
    assert_eq!(v["choices"][0]["finish_reason"], "stop");
    assert!(
        v["choices"][0]["message"]["content"].as_str().unwrap().contains("你好 世界"),
        "demo 后端应回声用户输入"
    );
    assert!(v["usage"]["total_tokens"].as_u64().unwrap() > 0, "usage 计量应 > 0");
}

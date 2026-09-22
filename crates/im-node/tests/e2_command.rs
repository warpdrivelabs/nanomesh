//! E2：路由命令 RPC。调用方发现推理服务(Agent)，向它 `call("infer")`，节点把命令路由到
//! Agent 的在线会话，Agent 执行并回 `CommandResult`，节点再路由回调用方。

use std::collections::HashMap;
use std::time::Duration;

use im_entity::kinds;
use im_proto::pb::{InferenceProfile, PersonProfile};
use im_proto::{Any, DirectoryQuery};

#[tokio::test]
async fn e2_command_rpc_to_agent() {
    let node = im_node::Node::bind_local([31u8; 32]).await.expect("bind node");
    let addr = node.addr();
    tokio::spawn(async move {
        let _ = node.serve().await;
    });

    // 推理算力 Agent 上线、注册，并起命令服务循环。
    let agent_client = im_client::Client::bind_local([32u8; 32]).await.expect("bind agent");
    let mut agent = tokio::time::timeout(Duration::from_secs(20), agent_client.online(addr.clone()))
        .await
        .expect("agent online timed out")
        .expect("agent online failed");
    agent
        .register_as::<kinds::InferenceService>(
            &InferenceProfile { models: vec!["demo-llm".into()], ..Default::default() },
            "推理A",
            HashMap::new(),
        )
        .await
        .expect("agent register failed");
    let agent_id = agent.id_bytes();

    tokio::spawn(async move {
        while let Some((req, cmd)) = agent.next_command().await {
            let (ok, result, error) = match cmd.method.as_str() {
                "infer" => {
                    let input = cmd
                        .params
                        .as_ref()
                        .map(|p| String::from_utf8_lossy(&p.value).to_string())
                        .unwrap_or_default();
                    (
                        true,
                        Some(Any {
                            type_url: "imspace.v1/text".into(),
                            value: format!("inference of: {input}").into_bytes(),
                        }),
                        String::new(),
                    )
                }
                _ => (false, None, "unsupported".to_string()),
            };
            let _ = agent.reply(&req, ok, result, &error).await;
        }
    });

    // 调用方（人）上线、注册、发现推理服务。
    let caller_client = im_client::Client::bind_local([33u8; 32]).await.expect("bind caller");
    let caller = caller_client.online(addr.clone()).await.expect("caller online failed");
    caller
        .register_as::<kinds::Person>(&PersonProfile::default(), "调用者", HashMap::new())
        .await
        .expect("caller register failed");

    let found = caller
        .directory_query(DirectoryQuery { kind_prefix: "compute.".into(), ..Default::default() })
        .await
        .expect("query failed");
    assert_eq!(found.len(), 1);
    let target: [u8; 32] = found[0].entity_id.clone().try_into().expect("id 32 bytes");
    assert_eq!(target, agent_id);

    // 路由命令 RPC：调用 infer。
    let params = Any { type_url: "imspace.v1/text".into(), value: b"hello".to_vec() };
    let res = tokio::time::timeout(Duration::from_secs(15), caller.call(target, "infer", Some(params)))
        .await
        .expect("call timed out")
        .expect("call failed");

    assert!(res.ok, "command error: {}", res.error);
    let out = String::from_utf8(res.result.expect("result").value).unwrap();
    assert_eq!(out, "inference of: hello");
}

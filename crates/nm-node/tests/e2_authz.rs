//! E2 安全半：能力授权(Grant)。目标实体标记 require_grant=true 后，
//! 无 Grant 的路由命令被拒；持有目标签发的有效 Grant 的命令被放行。

use std::collections::HashMap;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::{InferenceProfile, PersonProfile};
use nm_proto::{Any, DirectoryQuery};

#[tokio::test]
async fn e2_grant_required() {
    let node = nm_node::Node::bind_local([51u8; 32]).await.expect("bind node");
    let addr = node.addr();
    tokio::spawn(async move {
        let _ = node.serve().await;
    });

    // 推理服务上线：标记 require_grant=true，并起命令服务循环。
    let agent_client = nm_client::Client::bind_local([52u8; 32]).await.expect("bind agent");
    let mut agent = tokio::time::timeout(Duration::from_secs(20), agent_client.online(addr.clone()))
        .await
        .expect("agent online timed out")
        .expect("agent online failed");
    agent
        .register_as::<kinds::InferenceService>(
            &InferenceProfile { models: vec!["demo-llm".into()], ..Default::default() },
            "推理A",
            HashMap::from([("require_grant".to_string(), "true".to_string())]),
        )
        .await
        .expect("agent register failed");
    let agent_id = agent.id_bytes();
    tokio::spawn(async move {
        while let Some((req, cmd)) = agent.next_command().await {
            let out = format!("ok:{}", cmd.method);
            let _ = agent
                .reply(&req, true, Some(Any { type_url: "t".into(), value: out.into_bytes() }), "")
                .await;
        }
    });

    // 调用方上线、注册、发现。
    let caller_client = nm_client::Client::bind_local([53u8; 32]).await.expect("bind caller");
    let caller = caller_client.online(addr.clone()).await.expect("caller online failed");
    caller
        .register_as::<kinds::Person>(&PersonProfile::default(), "调用者", HashMap::new())
        .await
        .expect("caller register failed");
    let found = caller
        .directory_query(DirectoryQuery { kind_prefix: "compute.".into(), ..Default::default() })
        .await
        .expect("query failed");
    let target: [u8; 32] = found[0].entity_id.clone().try_into().unwrap();
    assert_eq!(target, agent_id);

    // (1) 无 Grant → 被拒。
    let denied = tokio::time::timeout(
        Duration::from_secs(15),
        caller.call(target, "infer", Some(Any { type_url: "t".into(), value: b"x".to_vec() })),
    )
    .await
    .expect("call timed out")
    .expect("call rpc failed");
    assert!(!denied.ok, "无 Grant 应被拒");
    assert!(denied.error.contains("denied"), "错误应含 denied: {}", denied.error);

    // (2) 由资源所有者(推理服务)签发 Grant 给调用方 → 放行。
    //     真实系统里 Grant 由 owner 侧签发后带外/经协议交付；此处直接用 agent 的密钥签发。
    let grant = agent_client.issue_grant(caller.id_bytes(), "infer", "compute.inference", 0);
    let ok = tokio::time::timeout(
        Duration::from_secs(15),
        caller.call_with_grant(
            target,
            "infer",
            Some(Any { type_url: "t".into(), value: b"x".to_vec() }),
            Some(grant),
        ),
    )
    .await
    .expect("granted call timed out")
    .expect("granted call rpc failed");
    assert!(ok.ok, "持有效 Grant 应放行: {}", ok.error);
    assert_eq!(String::from_utf8(ok.result.unwrap().value).unwrap(), "ok:infer");
}

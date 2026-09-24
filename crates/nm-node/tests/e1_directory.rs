//! E1：不同类型客户端注册为实体 + 按 kind 前缀发现。
//! 验证「客户端 = 可扩展实体（人 / AI Agent / …）」在运行时成立。

use std::collections::HashMap;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::{AgentProfile, PersonProfile};
use nm_proto::DirectoryQuery;

#[tokio::test]
async fn e1_register_and_discover() {
    let node = nm_node::Node::bind_local([11u8; 32]).await.expect("bind node");
    let addr = node.addr();
    tokio::spawn(async move {
        let _ = node.serve().await;
    });

    // 一个 AI Agent 上线并注册。
    let agent = nm_client::Client::bind_local([12u8; 32]).await.expect("bind agent");
    tokio::time::timeout(
        Duration::from_secs(20),
        agent.register_as::<kinds::AgentAssistant>(
            addr.clone(),
            &AgentProfile {
                backend: "claude".into(),
                tools: vec!["search".into()],
                ..Default::default()
            },
            "助手A",
            HashMap::new(),
        ),
    )
    .await
    .expect("register agent timed out")
    .expect("register agent failed");

    // 一个人上线并注册。
    let person = nm_client::Client::bind_local([13u8; 32]).await.expect("bind person");
    person
        .register_as::<kinds::Person>(addr.clone(), &PersonProfile::default(), "小明", HashMap::new())
        .await
        .expect("register person failed");

    // 人查询所有 agent.* 实体，应发现刚上线的 Agent。
    let found = person
        .directory_query(
            addr.clone(),
            DirectoryQuery {
                kind_prefix: "agent.".into(),
                ..Default::default()
            },
        )
        .await
        .expect("query failed");

    assert_eq!(found.len(), 1, "应只发现 1 个 agent.* 实体");
    assert_eq!(found[0].kind, "agent.assistant");
    assert_eq!(found[0].display_name, "助手A");
}

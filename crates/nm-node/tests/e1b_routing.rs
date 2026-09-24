//! E1b：发现 + 投递。A(人) 通过目录发现 B(Agent)，向 B 发消息，节点路由到 B 的在线会话，B 实收。
//! 验证「找到某个实体并向它发消息」端到端跑通（注册 → 发现 → 路由投递）。

use std::collections::HashMap;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::{AgentProfile, PersonProfile};
use nm_proto::{DirectoryQuery, GramKind};

#[tokio::test]
async fn e1b_discover_then_deliver() {
    let node = nm_node::Node::bind_local([21u8; 32]).await.expect("bind node");
    let addr = node.addr();
    tokio::spawn(async move {
        let _ = node.serve().await;
    });

    // B(Agent) 上线并注册；持久会话保持在线以接收推送。
    let b_client = nm_client::Client::bind_local([22u8; 32]).await.expect("bind B");
    let mut b = tokio::time::timeout(Duration::from_secs(20), b_client.online(addr.clone()))
        .await
        .expect("B online timed out")
        .expect("B online failed");
    b.register_as::<kinds::AgentAssistant>(
        &AgentProfile { backend: "claude".into(), ..Default::default() },
        "助手B",
        HashMap::new(),
    )
    .await
    .expect("B register failed");
    let b_id = b.id_bytes();

    // A(人) 上线并注册。
    let a_client = nm_client::Client::bind_local([23u8; 32]).await.expect("bind A");
    let a = a_client.online(addr.clone()).await.expect("A online failed");
    a.register_as::<kinds::Person>(&PersonProfile::default(), "小明", HashMap::new())
        .await
        .expect("A register failed");

    // A 通过目录发现 agent.*，应命中 B。
    let found = a
        .directory_query(DirectoryQuery { kind_prefix: "agent.".into(), ..Default::default() })
        .await
        .expect("query failed");
    assert_eq!(found.len(), 1, "应发现 1 个 agent.*");
    let target: [u8; 32] = found[0].entity_id.clone().try_into().expect("id 32 bytes");
    assert_eq!(target, b_id, "发现的实体应为 B");

    // A 向 B 发消息；节点路由到 B 的在线会话。
    let receipt = a.send_to(target, "你好，B").await.expect("send_to failed");
    assert_eq!(receipt.kind(), GramKind::Receipt);

    // B 实际收到该推送。
    let pushed = tokio::time::timeout(Duration::from_secs(10), b.recv())
        .await
        .expect("B recv timed out")
        .expect("no push received");
    assert_eq!(pushed.kind(), GramKind::Message);
    let text = String::from_utf8(pushed.payload.expect("payload").value).unwrap();
    assert_eq!(text, "你好，B");
}

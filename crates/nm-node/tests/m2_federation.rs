//! M2/E4 联邦（s2s 直连）：两节点互为对等；实体注册在 B，A 侧客户端经目录同步发现它，
//! 向它发跨节点消息 → A 把 Relay 信封转发给 B（归属节点）→ B 本地投递 → 目标实收。

use std::collections::HashMap;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::{AgentProfile, PersonProfile};
use nm_proto::{DirectoryQuery, GramKind};

#[tokio::test]
#[ignore = "flaky: iroh Minimal-mode multi-endpoint LAN connectivity (pre-existing, test-env only; prod uses N0/relay). Run with --ignored."]
async fn cross_node_discover_and_deliver() {
    // 两个节点。
    let node_a = nm_node::Node::bind_local([111u8; 32]).await.unwrap();
    let node_b = nm_node::Node::bind_local([112u8; 32]).await.unwrap();
    let a_id = node_a.id().as_bytes().to_owned();
    let b_id = node_b.id().as_bytes().to_owned();
    let a_addr = node_a.addr();
    let b_addr = node_b.addr();

    // 互为对等。
    node_a.add_peer(b_id, b_addr.clone());
    node_b.add_peer(a_id, a_addr.clone());

    // 启动两节点 serve。
    let a_handle = std::sync::Arc::new(node_a);
    let b_handle = std::sync::Arc::new(node_b);
    { let n = a_handle.clone(); tokio::spawn(async move { let _ = n.serve().await; }); }
    { let n = b_handle.clone(); tokio::spawn(async move { let _ = n.serve().await; }); }

    // B 上的 Agent 上线注册（home_node = B）。
    let agent_c = nm_client::Client::bind_local([113u8; 32]).await.unwrap();
    let mut agent = tokio::time::timeout(Duration::from_secs(20), agent_c.online(b_addr.clone()))
        .await
        .expect("agent online timeout")
        .expect("agent online failed");
    agent
        .register_as::<kinds::AgentAssistant>(
            &AgentProfile { backend: "claude".into(), ..Default::default() },
            "跨节点助手",
            HashMap::new(),
        )
        .await
        .unwrap();
    let agent_id = agent.id_bytes();

    // A 主动同步对等节点目录 → 应并入 B 的 Agent。
    tokio::time::timeout(Duration::from_secs(10), a_handle.sync_peers_once())
        .await
        .expect("sync timeout");

    // A 上的人上线注册。
    let person_c = nm_client::Client::bind_local([114u8; 32]).await.unwrap();
    let person = person_c.online(a_addr.clone()).await.unwrap();
    person
        .register_as::<kinds::Person>(&PersonProfile::default(), "小明", HashMap::new())
        .await
        .unwrap();

    // A 侧发现 agent.*（来自 B 的同步条目）。
    let found = person
        .directory_query(DirectoryQuery { kind_prefix: "agent.".into(), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(found.len(), 1, "应发现来自 B 的 1 个 agent");
    let target: [u8; 32] = found[0].entity_id.clone().try_into().unwrap();
    assert_eq!(target, agent_id);
    assert_eq!(found[0].home_node, b_id.to_vec(), "home_node 应为 B");

    // A 侧的人向该 Agent 发消息 → 跨节点 Relay 到 B → B 投递 → Agent 实收。
    let _ack = person.send_to(target, "跨节点你好").await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(10), agent.recv())
        .await
        .expect("agent recv timeout")
        .expect("no cross-node msg");
    assert_eq!(got.kind(), GramKind::Message);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "跨节点你好");
}

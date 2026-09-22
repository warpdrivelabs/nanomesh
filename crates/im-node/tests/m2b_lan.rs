//! LAN 双服务器互通：两台节点(独立身份+独立 redb)互为对等，靠**后台周期联邦同步**
//! 让 A 发现 B 上注册的实体，再跨服务器把消息 Relay 过去投递。模拟同网两台服务器。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use im_entity::kinds;
use im_proto::pb::{AgentProfile, PersonProfile};
use im_proto::{DirectoryQuery, GramKind};

#[tokio::test]
async fn lan_two_servers_background_sync() {
    let dir = tempfile::tempdir().unwrap();

    // 两台“服务器”：各自独立身份 + 独立 redb（端口 0 = 临时，测试用 addr() 互配）。
    let node_a = Arc::new(
        im_node::Node::bind_local_persistent_on([201u8; 32], dir.path().join("a.redb"), 0)
            .await
            .unwrap(),
    );
    let node_b = Arc::new(
        im_node::Node::bind_local_persistent_on([202u8; 32], dir.path().join("b.redb"), 0)
            .await
            .unwrap(),
    );
    assert_ne!(node_a.id().as_bytes(), node_b.id().as_bytes(), "两节点身份必须不同");

    // 互为对等（按地址；node id 从地址自取）。
    node_a.add_peer_addr(node_b.addr());
    node_b.add_peer_addr(node_a.addr());
    let a_addr = node_a.addr();
    let b_addr = node_b.addr();

    // 两节点各自 serve + 后台联邦同步（1s，测试内快速收敛）。
    {
        let n = node_a.clone();
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    {
        let n = node_b.clone();
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    node_a.clone().spawn_federation_sync(Duration::from_secs(1));
    node_b.clone().spawn_federation_sync(Duration::from_secs(1));

    // B 服务器上的 Agent 上线注册（home_node = B）。
    let agent_c = im_client::Client::bind_local([203u8; 32]).await.unwrap();
    let mut agent = tokio::time::timeout(Duration::from_secs(20), agent_c.online(b_addr.clone()))
        .await
        .expect("agent online timeout")
        .expect("agent online failed");
    agent
        .register_as::<kinds::AgentAssistant>(
            &AgentProfile { backend: "claude".into(), ..Default::default() },
            "B上的助手",
            HashMap::new(),
        )
        .await
        .unwrap();
    let agent_id = agent.id_bytes();

    // A 服务器上的人上线注册。
    let person_c = im_client::Client::bind_local([204u8; 32]).await.unwrap();
    let person = person_c.online(a_addr.clone()).await.unwrap();
    person
        .register_as::<kinds::Person>(&PersonProfile::default(), "A上的人", HashMap::new())
        .await
        .unwrap();

    // 靠**后台同步**：A 侧轮询目录，直到发现来自 B 的 agent（证明周期同步生效）。
    let target: [u8; 32] = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let found = person
                .directory_query(DirectoryQuery { kind_prefix: "agent.".into(), ..Default::default() })
                .await
                .unwrap();
            if let Some(e) = found.into_iter().next() {
                return TryInto::<[u8; 32]>::try_into(e.entity_id).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("A 未能在超时内经后台同步发现 B 的 agent");
    assert_eq!(target, agent_id, "发现的应为 B 上的 agent");

    // A 侧的人 → B 侧的 agent：跨服务器 Relay，agent 实收。
    person.send_to(target, "跨服务器你好").await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(10), agent.recv())
        .await
        .expect("agent recv timeout")
        .expect("no cross-server msg");
    assert_eq!(got.kind(), GramKind::Message);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "跨服务器你好");
}

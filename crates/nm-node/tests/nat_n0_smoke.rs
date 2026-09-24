//! N0(穿透模式) 冒烟测试：两台 N0 预设节点(bind_persistent)按地址互为对等 + 后台同步 +
//! 跨节点投递。本机 localhost 直连路径不依赖 n0 中继/发现，验证 N0 绑定与联邦可用。
//! 注：真实跨 NAT 的中继/打洞/发现需公网可达 n0 设施，见 DEPLOY 文档，此处只验本地路径。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::{AgentProfile, PersonProfile};
use nm_proto::{DirectoryQuery, GramKind};

#[tokio::test]
async fn n0_two_nodes_federate() {
    let dir = tempfile::tempdir().unwrap();

    // 两台 N0 预设节点（穿透模式所用的 bind_persistent），各自独立身份+redb。
    let a = Arc::new(
        tokio::time::timeout(
            Duration::from_secs(30),
            nm_node::Node::bind_persistent([211u8; 32], dir.path().join("a.redb")),
        )
        .await
        .expect("N0 bind A 超时(可能需要公网设施)")
        .unwrap(),
    );
    let b = Arc::new(
        nm_node::Node::bind_persistent([212u8; 32], dir.path().join("b.redb"))
            .await
            .unwrap(),
    );
    assert_ne!(a.id().as_bytes(), b.id().as_bytes());

    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    let a_addr = a.addr();
    let b_addr = b.addr();
    {
        let n = a.clone();
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    {
        let n = b.clone();
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    a.clone().spawn_federation_sync(Duration::from_secs(1));
    b.clone().spawn_federation_sync(Duration::from_secs(1));

    let agent_c = nm_client::Client::bind_local([213u8; 32]).await.unwrap();
    let mut agent = agent_c.online(b_addr.clone()).await.unwrap();
    agent
        .register_as::<kinds::AgentAssistant>(
            &AgentProfile { backend: "claude".into(), ..Default::default() },
            "B-agent",
            HashMap::new(),
        )
        .await
        .unwrap();
    let agent_id = agent.id_bytes();

    let person_c = nm_client::Client::bind_local([214u8; 32]).await.unwrap();
    let person = person_c.online(a_addr.clone()).await.unwrap();
    person
        .register_as::<kinds::Person>(&PersonProfile::default(), "A-person", HashMap::new())
        .await
        .unwrap();

    let target: [u8; 32] = tokio::time::timeout(Duration::from_secs(20), async {
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
    .expect("N0 联邦同步未在超时内发现对端 agent");
    assert_eq!(target, agent_id);

    person.send_to(target, "N0 跨节点").await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(10), agent.recv())
        .await
        .expect("recv 超时")
        .expect("no msg");
    assert_eq!(got.kind(), GramKind::Message);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "N0 跨节点");
}

//! 跨节点群消息（走群联邦 gossip，取代 s2s 中继）：owner 在 A 建群+加 bob，bob 在 B 上线，
//! A 发群消息 → 经 nmspace-groups gossip 扩散到 B → B 投递给本地成员 bob。
//! 复刻 m3_gossip 的双节点 gossip 播种 + m1_group 的客户端流。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;
use nm_proto::GramKind;

async fn online(seed: [u8; 32], addr: &nm_transport::Addr) -> (nm_client::Client, nm_client::Session) {
    let c = nm_client::Client::bind_local(seed).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(20), c.online(addr.clone()))
        .await
        .expect("online timed out")
        .expect("online failed");
    (c, s)
}

#[tokio::test]
async fn cross_node_group_via_gossip() {
    // 两个节点，互相播种地址 + 各自 serve + 各自起群联邦同步。
    let a = Arc::new(nm_node::Node::bind_local([41u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([42u8; 32]).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    let (a_addr, b_addr) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] {
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());

    // owner 连 A，bob 连 B（bob 的 home = 节点 B）。
    let (_oc, owner) = online([43u8; 32], &a_addr).await;
    owner.register_as::<kinds::Person>(&PersonProfile::default(), "Owner", HashMap::new()).await.unwrap();
    let (_bc, mut bob) = online([44u8; 32], &b_addr).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();

    // owner 建群并添加 bob（跨节点成员）。
    let gid = [50u8; 32];
    owner.group_create(gid, "跨节点群").await.unwrap();
    owner.group_add(gid, bob.id_bytes()).await.unwrap();

    // 反复发，直到 gossip 叠加网成型 + 公告扩散到 B（幂等，重复无害）。
    let recv = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(30), bob.recv()).await
    });
    let send = tokio::spawn(async move {
        for _ in 0..40 {
            let _ = owner.send_group(gid, "跨节点群消息").await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    let got = recv.await.unwrap().expect("bob 未在超时内收到跨节点群消息").expect("bob 流结束");
    send.abort();
    assert_eq!(got.kind(), GramKind::GroupMessage);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "跨节点群消息");
}

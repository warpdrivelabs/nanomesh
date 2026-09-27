//! 跨节点私聊（走联邦 gossip Direct，取代 s2s 中继）：A@节点1 发私信给 B@节点2。
//! 两种情形：B 在线即时收到；B 先在节点2 注册后离线、A 发信、B 重连补投。

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

async fn two_nodes() -> (Arc<nm_node::Node>, Arc<nm_node::Node>, nm_transport::Addr, nm_transport::Addr) {
    let a = Arc::new(nm_node::Node::bind_local([71u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([72u8; 32]).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    let (aa, ba) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] {
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());
    (a, b, aa, ba)
}

#[tokio::test]
async fn cross_node_direct_online() {
    let (_a, _b, aa, ba) = two_nodes().await;
    let (_ac, alice) = online([73u8; 32], &aa).await;
    alice.register_as::<kinds::Person>(&PersonProfile::default(), "Alice", HashMap::new()).await.unwrap();
    let (_bc, mut bob) = online([74u8; 32], &ba).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();
    let bob_id = bob.id_bytes();

    let recv = tokio::spawn(async move { tokio::time::timeout(Duration::from_secs(30), bob.recv()).await });
    let send = tokio::spawn(async move {
        for _ in 0..40 { let _ = alice.send_to(bob_id, "跨节点私信").await; tokio::time::sleep(Duration::from_millis(500)).await; }
    });
    let got = recv.await.unwrap().expect("bob 未收到跨节点私信").expect("流结束");
    send.abort();
    assert_eq!(got.kind(), GramKind::Message);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "跨节点私信");
}

#[tokio::test]
async fn cross_node_direct_offline_then_reconnect() {
    let dir2 = tempfile::tempdir().unwrap();
    // 节点2 需持久化(离线库)；节点1 普通即可。
    let a = Arc::new(nm_node::Node::bind_local([75u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local_persistent([76u8; 32], dir2.path().join("b.redb")).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    let (aa, ba) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] { tokio::spawn(async move { let _ = n.serve().await; }); }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());

    // Bob 先在节点2 注册(home=节点2)，随即离线。
    let b_seed = [77u8; 32];
    let bob_id;
    {
        let (_bc, bob) = online(b_seed, &ba).await;
        bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();
        bob_id = bob.id_bytes();
        bob.close().await; // 显式下线
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Alice@节点1 反复给离线的 Bob 发私信 → gossip 到节点2 → 节点2(其 home)入离线库。
    let (_ac, alice) = online([78u8; 32], &aa).await;
    alice.register_as::<kinds::Person>(&PersonProfile::default(), "Alice", HashMap::new()).await.unwrap();
    for _ in 0..10 { let _ = alice.send_to(bob_id, "离线跨节点私信").await; tokio::time::sleep(Duration::from_millis(300)).await; }

    // Bob 重连节点2 → 补投离线消息。
    let b2 = nm_client::Client::bind_local(b_seed).await.unwrap();
    let mut b2s = b2.online(ba.clone()).await.expect("bob 重连失败");
    let pushed = tokio::time::timeout(Duration::from_secs(10), b2s.recv())
        .await
        .expect("bob 重连未收到离线私信")
        .expect("流结束");
    assert_eq!(pushed.kind(), GramKind::Message);
    assert_eq!(String::from_utf8(pushed.payload.unwrap().value).unwrap(), "离线跨节点私信");
}

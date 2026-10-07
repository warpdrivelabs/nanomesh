//! F1 (规模化联邦): with `[federation] per_topic` ON, a cross-node group member receives the
//! group message — exercising the per-group topic path (nmspace-group:<gid>) alongside the
//! firehose (dual-write). In F1 this confirms the flag wires up and doesn't break delivery;
//! true members-only exclusion lands with firehose retirement (F5) and is already proven at the
//! primitive level by per_topic_fed.rs.
//!
//! #[ignore]: cross-node bind_local delivery is subject to the same iroh Minimal-mode env
//! flakiness quarantined elsewhere. Run with: cargo test -p nm-node --test per_topic_group_e2e -- --ignored
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
#[ignore = "flaky: same iroh Minimal-mode cross-node connectivity as group_gossip; validates the F1 per-topic path when connectivity holds. Run with --ignored."]
async fn per_topic_group_cross_node() {
    let a = Arc::new(nm_node::Node::bind_local([71u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([72u8; 32]).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    // F1 开关：两端都开「每群独立主题」。
    a.set_per_topic(true);
    b.set_per_topic(true);
    assert!(a.per_topic_on() && b.per_topic_on());
    let (a_addr, b_addr) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] {
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());

    let (_oc, owner) = online([73u8; 32], &a_addr).await;
    owner.register_as::<kinds::Person>(&PersonProfile::default(), "Owner", HashMap::new()).await.unwrap();
    let (_bc, mut bob) = online([74u8; 32], &b_addr).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();

    let gid = [80u8; 32];
    owner.group_create(gid, "每群主题群").await.unwrap();
    owner.group_add(gid, bob.id_bytes()).await.unwrap();

    let send = tokio::spawn(async move {
        for _ in 0..40 {
            let _ = owner.send_group(gid, "每群主题消息").await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    let got = tokio::time::timeout(Duration::from_secs(30), bob.recv())
        .await
        .expect("bob 未在超时内收到每群主题群消息")
        .expect("bob 流结束");
    send.abort();
    assert_eq!(got.kind(), GramKind::GroupMessage);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "每群主题消息");
}

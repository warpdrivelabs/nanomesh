//! F3 (私聊去火管): with `[federation] per_topic` ON, a cross-node DM reaches the recipient via the
//! per-recipient inbox topic `nmspace-inbox:<pub>` — the recipient's home node subscribes to it, the
//! sender publishes the `Direct` gram there (dual-write with the firehose until retirement; receiver
//! dedups by (sender, gram_id)). This replaces broadcasting every DM on the firehose and sidesteps
//! the NAT-fragile s2s direct dial (the inbox rides the same gossip overlay groups/channels use).
//!
//! Asserts (a) the recipient's home actually subscribed to its inbox topic, and (b) the DM arrives.
//! #[ignore]: same iroh Minimal-mode cross-node connectivity flakiness quarantined elsewhere.
//! Run with: cargo test -p nm-node --test per_topic_inbox_dm -- --ignored
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
#[ignore = "flaky: same iroh Minimal-mode cross-node connectivity as group_gossip; validates the F3 inbox-topic DM path when connectivity holds. Run with --ignored."]
async fn per_topic_inbox_dm_cross_node() {
    let a = Arc::new(nm_node::Node::bind_local([51u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([52u8; 32]).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    a.set_per_topic(true);
    b.set_per_topic(true);
    let (a_addr, b_addr) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] {
        tokio::spawn(async move {
            let _ = n.serve().await;
        });
    }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());
    a.clone().spawn_federation_sync(Duration::from_secs(2));
    b.clone().spawn_federation_sync(Duration::from_secs(2));

    let (_oc, owner) = online([53u8; 32], &a_addr).await;
    owner
        .register_as::<kinds::Person>(&PersonProfile::default(), "Owner", HashMap::new())
        .await
        .unwrap();
    let (_bc, mut bob) = online([54u8; 32], &b_addr).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new())
        .await
        .unwrap();
    let bob_id = bob.id_bytes();

    // (a) B 应在 sweep（~3s）内订阅 bob 的收件箱主题 nmspace-inbox:<bob>。
    let subscribed = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if b.fed_joined().await.iter().any(|l| l.as_slice() == bob_id.as_slice()) {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(subscribed, "B 未在超时内订阅 bob 的收件箱主题");

    // (b) owner → bob 私聊：A 投到 nmspace-inbox:<bob>，B 收并投给在线的 bob。
    let send = tokio::spawn(async move {
        for _ in 0..40 {
            let _ = owner.send_to(bob_id, "收件箱私聊").await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    let got = tokio::time::timeout(Duration::from_secs(30), bob.recv())
        .await
        .expect("bob 未在超时内收到私聊（收件箱主题路径）")
        .expect("bob 流结束");
    send.abort();
    assert_eq!(got.kind(), GramKind::Message);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "收件箱私聊");
}

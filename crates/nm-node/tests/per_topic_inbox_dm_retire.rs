//! F3 退火管 e2e: with `[federation] per_topic` ON, once the recipient's home node is live on the
//! recipient's inbox topic (heartbeat), the sender SUPPRESSES the firehose `Direct` copy and the DM
//! rides `nmspace-inbox:<recipient>` only. Asserts (a) the sender observes the recipient reachable
//! (`dm_firehose_retireable`), and (b) delivery still works with the firehose suppressed.
//!
//! #[ignore]: same iroh Minimal-mode cross-node connectivity flakiness as group_gossip.
//! Run with: cargo test -p nm-node --test per_topic_inbox_dm_retire -- --ignored
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
#[ignore = "flaky: same iroh Minimal-mode cross-node connectivity as group_gossip; validates F3 firehose-Direct suppression when connectivity holds. Run with --ignored."]
async fn per_topic_inbox_dm_firehose_retire() {
    let a = Arc::new(nm_node::Node::bind_local([55u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([56u8; 32]).await.unwrap());
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
    // 目录同步：退火管判据按收件人 home 存活判定，需从对端目录学到 bob 的 home_node。
    a.clone().spawn_federation_sync(Duration::from_secs(2));
    b.clone().spawn_federation_sync(Duration::from_secs(2));

    let (_oc, owner) = online([57u8; 32], &a_addr).await;
    owner
        .register_as::<kinds::Person>(&PersonProfile::default(), "Owner", HashMap::new())
        .await
        .unwrap();
    let (_bc, mut bob) = online([58u8; 32], &b_addr).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new())
        .await
        .unwrap();
    let bob_id = bob.id_bytes();

    // owner 持续发私聊：首条会让 A join bob 收件箱主题，随后 A 听到 B 的存活心跳 → 判定可退火管。
    let send = tokio::spawn(async move {
        for _ in 0..60 {
            let _ = owner.send_to(bob_id, "退火管私聊").await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    // (a) A 侧判定 bob 的私聊火管副本可省略（B 在 bob 收件箱主题存活）。
    let retired = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            if a.dm_firehose_retireable(&bob_id) {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(retired, "A 侧未在超时内判定可退火管（bob@B 在收件箱主题存活）");

    // (b) 退火管后私聊仅经收件箱主题投达 → bob 仍应收到（不漏投）。
    let got = tokio::time::timeout(Duration::from_secs(30), bob.recv())
        .await
        .expect("bob 未在超时内收到私聊（退火管后仅收件箱主题投递）")
        .expect("bob 流结束");
    send.abort();
    assert_eq!(got.kind(), GramKind::Message);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "退火管私聊");
}

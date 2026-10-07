//! F5 (火管退役): with `[federation] per_topic` ON, once every REMOTE member's home node is live on
//! the group's per-topic (heartbeat carried by the member-home periodic announce), the sender
//! SUPPRESSES the firehose copy of the group message — it then rides `nmspace-group:<gid>` only
//! (members-only). This test proves (1) the liveness ledger populates from real topic traffic and
//! the retire decision engages (`group_firehose_retireable`), and (2) delivery still works with the
//! firehose suppressed (no silent drop). Announces still ride the firehose (discovery) — that is how
//! B learns it homes a member of the group in the first place.
//!
//! #[ignore]: cross-node bind_local delivery is subject to the same iroh Minimal-mode env flakiness
//! quarantined elsewhere. Run with: cargo test -p nm-node --test per_topic_group_retire -- --ignored
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
#[ignore = "flaky: same iroh Minimal-mode cross-node connectivity as group_gossip; validates F5 firehose suppression when connectivity holds. Run with --ignored."]
async fn per_topic_group_firehose_retire() {
    let a = Arc::new(nm_node::Node::bind_local([91u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([92u8; 32]).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    // F1/F5 开关：两端都开「每群独立主题」。
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
    // 目录同步：F5 判据按「成员 home 节点在群主题存活」判定，需先从对端目录学到成员的 home_node。
    a.clone().spawn_federation_sync(Duration::from_secs(2));
    b.clone().spawn_federation_sync(Duration::from_secs(2));

    let (_oc, owner) = online([93u8; 32], &a_addr).await;
    owner
        .register_as::<kinds::Person>(&PersonProfile::default(), "Owner", HashMap::new())
        .await
        .unwrap();
    let (_bc, mut bob) = online([94u8; 32], &b_addr).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new())
        .await
        .unwrap();

    let gid = [90u8; 32];
    owner.group_create(gid, "退火管群").await.unwrap();
    owner.group_add(gid, bob.id_bytes()).await.unwrap();

    // 等 A 侧「可退火管」判据成立：A 已从目录学到 bob 的 home=B，且在该群主题上近期收到过 B 的心跳。
    let retired = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            if a.group_firehose_retireable(&gid) {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(retired, "A 侧未在超时内判定可退火管（bob@B 在群主题存活）");

    // 退火管后发群消息：火管副本被省略，bob 只能经每群主题收到 → 验证仍能投递（不漏投）。
    let send = tokio::spawn(async move {
        for _ in 0..40 {
            let _ = owner.send_group(gid, "退火管后的群消息").await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    let got = tokio::time::timeout(Duration::from_secs(30), bob.recv())
        .await
        .expect("bob 未在超时内收到（退火管后仅每群主题投递）")
        .expect("bob 流结束");
    send.abort();
    assert_eq!(got.kind(), GramKind::GroupMessage);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "退火管后的群消息");
}

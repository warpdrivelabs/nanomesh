//! F4 (命名去火管): with `[federation] per_topic` ON, a node subscribes to the per-domain names topic
//! `nmspace-names:<domain>` for domains it hosts or has cached — so a domain's `NameRecord`s replicate
//! only among nodes that care about that domain, instead of the firehose broadcasting every name to
//! every node. Asserts the domain home (A) AND an interested peer (B, which learns the domain via the
//! firehose dual-write) both join the per-domain topic.
//!
//! #[ignore]: cross-node delivery over iroh Minimal-mode is subject to the quarantined env flakiness.
//! Run with: cargo test -p nm-node --test per_topic_names -- --ignored
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;

const DOMAIN: &str = "test.nm";

async fn online(seed: [u8; 32], addr: &nm_transport::Addr) -> (nm_client::Client, nm_client::Session) {
    let c = nm_client::Client::bind_local(seed).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(20), c.online(addr.clone()))
        .await
        .expect("online timed out")
        .expect("online failed");
    (c, s)
}

#[tokio::test]
#[ignore = "flaky: same iroh Minimal-mode cross-node connectivity as group_gossip; validates the F4 per-domain names topic subscription when connectivity holds. Run with --ignored."]
async fn per_topic_names_cross_node() {
    let a = Arc::new(nm_node::Node::bind_local([81u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([82u8; 32]).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    a.set_per_topic(true);
    b.set_per_topic(true);
    a.set_domain(DOMAIN.into()); // A 自声明域名（A 为该域 home）
    let (a_addr, b_addr) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] {
        tokio::spawn(async move {
            let _ = n.serve().await;
        });
    }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());

    // alice 在 A 认领 alice@test.nm（A 的命名缓存持有该记录，home==A）。
    let (_ac, alice) = online([83u8; 32], &a_addr).await;
    alice
        .register_as::<kinds::Person>(&PersonProfile::default(), "Alice", HashMap::new())
        .await
        .unwrap();
    alice.name_claim("alice").await.expect("name_claim 失败");
    // bob 只需在 B 在线即可（无需认领）；B 经火管学到 test.nm 后应订阅该域主题。
    let (_bc, _bob) = online([84u8; 32], &b_addr).await;

    let has_domain = |joined: &[Vec<u8>]| joined.iter().any(|l| l.as_slice() == DOMAIN.as_bytes());
    // A 应订阅自持有域主题；B 学到该域后也应订阅。
    let ok = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            if has_domain(&a.fed_joined().await) && has_domain(&b.fed_joined().await) {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(ok, "A 与 B 未都在超时内订阅 nmspace-names:test.nm");
}

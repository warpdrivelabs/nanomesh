//! P2-11 e2e：presence 按 home 拉取——在 presence overlay **退火**（`presence_scoped=true`）下，
//! A 节点仍能查到 B 节点上在线账号的 presence（证明不依赖 gossip 火管广播）。
//!
//! 场景：A、B 两持久节点互为对等且都退火；owner 登录 A、bob 登录 B 并各自注册（目录经
//! federation_sync 跨节点复制 → A 学到 bob 的 home_node=B）。owner 在 A 上 `presence.query([bob])`：
//! bob 非 A 本地、A 的 presence 缓存因退火而冷 → 必须 resolve_home(bob)=B 并 s2s 拉取，B 本地权威
//! 回 "online"。断言得到 online = 按 home 拉取在退火下成立。
//!
//! 同 cross_group_ack_e2e 一样依赖真跨节点连通（iroh Minimal 模式），故 `#[ignore]`。
//! 运行：cargo test -p nm-node --test presence_pull_e2e -- --ignored
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;

async fn online(seed: [u8; 32], addr: &nm_transport::Addr) -> (nm_client::Client, nm_client::Session) {
    let c = nm_client::Client::bind_local(seed).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(20), c.online(addr.clone()))
        .await
        .expect("online timed out")
        .expect("online failed");
    (c, s)
}

#[tokio::test]
#[ignore = "requires real cross-node connectivity (same iroh Minimal-mode caveat as cross_group_ack_e2e); proves presence resolves by pull-from-home with the presence overlay retired. Run with --ignored."]
async fn presence_pull_works_with_overlay_retired() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = Arc::new(nm_node::Node::bind_local_persistent([71u8; 32], dir_a.path().join("a.redb")).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local_persistent([72u8; 32], dir_b.path().join("b.redb")).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    // 关键：两端 presence overlay 退火——无火管广播，presence 只能靠按 home 拉取（本测即证此）。
    a.set_presence_scoped(true);
    b.set_presence_scoped(true);
    let (a_addr, b_addr) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] {
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    // overlay 任务起着但因退火而空转（证明退火下仍可拉取，而非仅「没起 overlay」）。
    a.clone().spawn_presence("nmspace".into());
    b.clone().spawn_presence("nmspace".into());
    // 目录跨节点复制：A 据此学到 bob 的 home_node=B（resolve_home 的主来源）。
    a.clone().spawn_federation_sync(Duration::from_secs(2));
    b.clone().spawn_federation_sync(Duration::from_secs(2));

    let (_oc, owner) = online([73u8; 32], &a_addr).await;
    owner.register_as::<kinds::Person>(&PersonProfile::default(), "Owner", HashMap::new()).await.unwrap();
    let (_bc, bob) = online([74u8; 32], &b_addr).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();

    let bob_id = bob.id_bytes();
    let bob_hex: String = bob_id.iter().map(|x| format!("{x:02x}")).collect();

    // owner（在 A）查 bob（在 B）的 presence。轮询等目录同步 + 拉取成型；
    // 断言在 overlay 退火下仍得 "online"（= pull-from-home 成立，不依赖火管）。
    let got_online = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            if let Ok(m) = owner.presence_query(&[bob_id]).await
                && m.get(&bob_hex).map(|s| s == "online").unwrap_or(false)
            {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(got_online, "overlay 退火下 A 未能按 home 拉取到 bob 的在线态（pull-from-home 未成立）");
}

//! 根治回归:频道 gossip 不得冲垮单播 mesh。
//!
//! 背景:频道/群/presence/membership 共用同一 iroh endpoint + 单一 Gossip actor;曾因「全部 peers 无上限
//! bootstrap + 0 邻居无退避重注入」的正反馈,建/订阅频道时连接风暴冲垮单播会话(peers→1、发不出消息)。
//! 修复:自愈退避 + 收敛 bootstrap + HTTP 超时 + guard-across-await。本测试守住「频道操作不掉单播会话」。
//!
//! 关键信号:`ctx.sessions` 只由单播 handler 填充,`GossipGate::accept` 不碰它 →
//! `node.online_count()` 是纯单播会话计数,按构造不受 gossip 影响。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;
use nm_proto::GramKind;

async fn connect(seed: [u8; 32], addr: &nm_transport::Addr) -> (nm_client::Client, nm_client::Session) {
    let c = nm_client::Client::bind_local(seed).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(20), c.online(addr.clone()))
        .await
        .expect("online timed out")
        .expect("online failed");
    (c, s)
}

/// 生成 keyed 频道密钥对:返回 (channel_id=公钥, secret=私钥)。
fn new_channel() -> ([u8; 32], [u8; 32]) {
    let sk = nm_transport::SecretKey::generate();
    (*sk.public().as_bytes(), sk.to_bytes())
}

/// 单节点确定性(非 ignore):2 个在线 client → 2 个单播会话;A 建/订阅/发布频道后,单播会话数**不变**
/// (频道 gossip 不冲单播 mesh),且 A→B 同节点 DM 仍送达(单播 mesh 活着)。
#[tokio::test]
async fn channel_ops_do_not_drop_unicast_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        nm_node::Node::bind_local_persistent([61u8; 32], dir.path().join("n.redb")).await.unwrap(),
    );
    let addr = node.addr();
    {
        let n = node.clone();
        tokio::spawn(async move { let _ = n.serve().await; });
    }

    let (_a_c, a_sess) = connect([62u8; 32], &addr).await;
    a_sess.register_as::<kinds::Person>(&PersonProfile::default(), "A", HashMap::new()).await.unwrap();
    let (_b_c, mut b_sess) = connect([63u8; 32], &addr).await;
    b_sess.register_as::<kinds::Person>(&PersonProfile::default(), "B", HashMap::new()).await.unwrap();
    let b_id = b_sess.id_bytes();

    // 两个在线 client → 2 个单播会话(单节点无 peer,故无 s2s 连接干扰)。
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = node.online_count();
    assert_eq!(before, 2, "两个在线 client 应为 2 个单播会话,实际 {before}");

    // A 建频道 + 订阅另一频道 + 发布 —— 频道 gossip 不得冲掉单播会话。
    let (c1, c1_sk) = new_channel();
    a_sess.channel_create(c1, "稳定性频道", "", "", &c1_sk).await.expect("create c1");
    a_sess.channel_sub([0xC2u8; 32], "订阅频道").await.expect("sub c2");
    for i in 1..=3 {
        a_sess.channel_publish(c1, &format!("m{i}")).await.expect("publish");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let after = node.online_count();
    assert_eq!(after, before, "频道建/订阅/发布后单播会话数不得变化(gossip 不得冲单播 mesh):{before}→{after}");

    // 功能性:A→B 同节点 DM 仍送达(单播 mesh 活着)。
    let recv = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(10), b_sess.recv()).await
    });
    let send = tokio::spawn(async move {
        for _ in 0..20 {
            let _ = a_sess.send_to(b_id, "同节点私信").await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    });
    let got = recv.await.unwrap().expect("B 未收到同节点私信").expect("流结束");
    send.abort();
    assert_eq!(got.kind(), GramKind::Message);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "同节点私信");

    node.trigger_shutdown();
}

/// 跨节点(#[ignore],与其它跨节点 e2e 同类——串行跑 scripts/test-crossnode.sh):
/// node1 上建/订阅频道(触发频道 gossip join)后,node1↔node2 的跨节点 DM 仍能送达
/// (频道 join 不破坏单播/联邦投递)。
#[tokio::test]
#[ignore = "flaky: 跨节点 iroh Minimal 叠加网成型不稳;--ignored 串行跑(scripts/test-crossnode.sh)"]
async fn channel_join_does_not_break_cross_node_dm() {
    let a = Arc::new(nm_node::Node::bind_local([64u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([65u8; 32]).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    let (aa, ba) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] {
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());

    let (_ac, alice) = connect([66u8; 32], &aa).await;
    alice.register_as::<kinds::Person>(&PersonProfile::default(), "Alice", HashMap::new()).await.unwrap();
    let (_bc, mut bob) = connect([67u8; 32], &ba).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();
    let bob_id = bob.id_bytes();

    // 扰动:alice 在 node1 建频道 + 订阅另一频道 → 触发频道 gossip join(曾经的「冲垮 mesh」入口)。
    let (c1, c1_sk) = new_channel();
    alice.channel_create(c1, "跨节点稳定性", "", "", &c1_sk).await.expect("create");
    alice.channel_sub([0xC3u8; 32], "订阅").await.expect("sub");

    // 断言:跨节点 DM 仍送达。
    let recv = tokio::spawn(async move { tokio::time::timeout(Duration::from_secs(30), bob.recv()).await });
    let send = tokio::spawn(async move {
        for _ in 0..40 {
            let _ = alice.send_to(bob_id, "频道后跨节点私信").await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    let got = recv.await.unwrap().expect("频道 join 后 bob 未收到跨节点私信").expect("流结束");
    send.abort();
    assert_eq!(got.kind(), GramKind::Message);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "频道后跨节点私信");
}

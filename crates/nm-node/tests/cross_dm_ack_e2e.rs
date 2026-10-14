//! 跨节点单聊可靠投递——端到端 ack 闭环（需真跨节点连通，故 #[ignore]，对齐 per_topic_inbox_dm.rs）。
//!
//! 验证完整闭环：owner(home=A) → bob(home=B) 发私聊：
//!   ① A deliver_direct(track=true) → outbox 落一条待确认；
//!   ② B 收 Direct → 落库/直投 bob → 回 DirectAck 到 nmspace-inbox:<owner>；
//!   ③ A 收 DirectAck → outbox_del → 待确认清零。
//! 断言 bob 收到消息 **且** A 的 outbox_pending 最终回到 0（= 收到了对端 home 的确认）。
//!
//! 不依赖网络的存储层语义（退避/horizon/续传/去重）另见稳定可跑的 cross_dm_outbox.rs。
//! 运行：cargo test -p nm-node --test cross_dm_ack_e2e -- --ignored
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
#[ignore = "flaky: same iroh Minimal-mode cross-node connectivity as per_topic_inbox_dm; validates the E2E ack loop (outbox clears on DirectAck) when connectivity holds. Run with --ignored."]
async fn cross_dm_ack_clears_outbox() {
    let a = Arc::new(nm_node::Node::bind_local([71u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([72u8; 32]).await.unwrap());
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
    // 重传 + outbox 扫描任务（被测主体）。
    a.clone().spawn_pending_flush();
    b.clone().spawn_pending_flush();

    let (_oc, owner) = online([73u8; 32], &a_addr).await;
    owner
        .register_as::<kinds::Person>(&PersonProfile::default(), "Owner", HashMap::new())
        .await
        .unwrap();
    let (_bc, mut bob) = online([74u8; 32], &b_addr).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new())
        .await
        .unwrap();
    let bob_id = bob.id_bytes();

    // B 订阅 bob 收件箱主题（A 发 Direct 的落点）。
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

    // owner → bob：持续重发直到 bob 收到（首投可能丢，靠 outbox 重传补）。
    let send = tokio::spawn(async move {
        for _ in 0..60 {
            let _ = owner.send_to(bob_id, "可靠单聊").await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    let got = tokio::time::timeout(Duration::from_secs(30), bob.recv())
        .await
        .expect("bob 未在超时内收到私聊")
        .expect("bob 流结束");
    assert_eq!(got.kind(), GramKind::Message);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "可靠单聊");

    // 关键断言：A 的待确认发件箱最终清零 = A 收到了 B 回的 DirectAck（E2E ack 闭环成立）。
    let cleared = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if a.outbox_pending() == 0 {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .unwrap_or(false);
    send.abort();
    assert!(cleared, "A 的 outbox 未清零 → 未收到 DirectAck（ack 闭环断裂）");
}

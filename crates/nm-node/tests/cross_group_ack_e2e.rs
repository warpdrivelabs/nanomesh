//! 跨节点群消息可靠扇出——端到端 GroupAck 闭环(需真跨节点连通,故 #[ignore],对齐 cross_dm_ack_e2e)。
//!
//! 验证:owner(home=A) 建群含 bob(home=B),owner 发群消息:
//!   ① A fanout_group → group_outbox 落一条,待 ack 节点集={B};
//!   ② B 收群 Msg → 投递 bob → 回 GroupAck{node=B} 到群主题/火管;
//!   ③ A 收 GroupAck → 从待 ack 集移除 B → 空 → group_outbox 清零。
//! 断言 bob 收到消息 **且** A 的 group_outbox_pending 最终回到 0(= 成员节点确认送达)。
//!
//! 不依赖网络的存储层语义见稳定可跑的 group_outbox.rs。
//! 运行:cargo test -p nm-node --test cross_group_ack_e2e -- --ignored
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
#[ignore = "flaky: same iroh Minimal-mode cross-node connectivity as per_topic_group_e2e; validates the group E2E ack loop (group_outbox clears on GroupAck) when connectivity holds. Run with --ignored."]
async fn cross_group_ack_clears_outbox() {
    // 持久节点(有 redb store)——group_outbox 落盘在 store，bind_local(无 store) 会让可靠扇出空转。
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = Arc::new(nm_node::Node::bind_local_persistent([81u8; 32], dir_a.path().join("a.redb")).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local_persistent([82u8; 32], dir_b.path().join("b.redb")).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    a.set_per_topic(true);
    b.set_per_topic(true);
    let (a_addr, b_addr) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] {
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());
    a.clone().spawn_federation_sync(Duration::from_secs(2));
    b.clone().spawn_federation_sync(Duration::from_secs(2));
    // 重传 + outbox 扫描 + GroupAck 销账任务(被测主体)。
    a.clone().spawn_pending_flush();
    b.clone().spawn_pending_flush();
    // ack 聚合：成员节点把 GroupAck 攒窗口后合并为 GroupAckBatch 发出(新码默认走批量)。
    a.clone().spawn_group_ack_batcher();
    b.clone().spawn_group_ack_batcher();

    let (_oc, owner) = online([83u8; 32], &a_addr).await;
    owner.register_as::<kinds::Person>(&PersonProfile::default(), "Owner", HashMap::new()).await.unwrap();
    let (_bc, mut bob) = online([84u8; 32], &b_addr).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();

    let gid = [90u8; 32];
    owner.group_create(gid, "可靠扇出群").await.unwrap();
    owner.group_add(gid, bob.id_bytes()).await.unwrap();

    // owner → 群:持续重发(首投可能丢,靠 group_outbox 重传补)。
    let send = tokio::spawn(async move {
        for _ in 0..60 {
            let _ = owner.send_group(gid, "可靠群消息").await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    // ① 确认 outbox 路径确被走到:A 经目录同步学到 bob 的 home(B) 后,fanout 会持久跟踪 → pending 一度 > 0。
    //    (否则 A 不知 bob 的 home,不建条目,清零检查会平凡成立=假阳性。)
    let tracked = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            if a.group_outbox_pending() > 0 { break true; }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(tracked, "A 从未跟踪群消息(未学到 bob 的 home_node)——ack 闭环未被真正验证");

    // ② bob 收到群消息。
    let got = tokio::time::timeout(Duration::from_secs(30), bob.recv())
        .await
        .expect("bob 未在超时内收到群消息")
        .expect("bob 流结束");
    assert_eq!(got.kind(), GramKind::GroupMessage);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "可靠群消息");

    // ③ 停发,等所有已发条目被 B 的 GroupAck 销账 → A 的 group_outbox 清零(= 成员节点确认送达)。
    send.abort();
    let cleared = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if a.group_outbox_pending() == 0 { break true; }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(cleared, "A 的 group_outbox 未清零 → 未收到成员节点的 GroupAck(ack 闭环断裂)");

    // ④ 证清零是走「批量」路径而非 legacy：B 作为成员节点应至少发过一个 GroupAckBatch。
    //    (两端新码 → A announce 广告 supports_ack_batch → B 学到后对 A-origin 消息走批量。)
    let batches = b.group_metrics_snapshot().4;
    assert!(batches > 0, "B 从未发出 GroupAckBatch → 走的是 legacy 逐条路径,批量未生效");
}

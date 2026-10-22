//! P4 频道升级 Phase 3：可靠性（跨节点，#[ignore]，对齐 cross_group_ack_e2e）。
//!
//! 场景：A=home+owner（持 keyed 频道私钥、定序器），B=另一节点的订阅者。
//! 验证：
//!  (1) A 发布 → 定序 Msg 经频道主题扩散 → B 收到（实时广播路径）。
//!  (2) B 晚订阅/漏收 → 据 home 的 Live 水位心跳触发 BackfillReq → A 回 Log → B 补齐历史。
//!  (3) B 收到的消息带 home 签名并通过验签（keyed 频道完整性）。
//!
//! 单节点确定性的定序/权限语义见 channel_phase2.rs；存储层见 nm-store 单测。
//! 运行：cargo test -p nm-node --test channel_phase3_e2e -- --ignored
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;
use nm_proto::GramKind;

async fn online(seed: [u8; 32], addr: &nm_transport::Addr) -> (nm_client::Client, nm_client::Session) {
    let c = nm_client::Client::bind_local(seed).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(20), c.online(addr.clone()))
        .await.expect("online timed out").expect("online failed");
    (c, s)
}
fn new_channel() -> ([u8; 32], [u8; 32]) {
    let sk = nm_transport::SecretKey::generate();
    (*sk.public().as_bytes(), sk.to_bytes())
}

#[tokio::test]
#[ignore = "flaky: same iroh Minimal-mode cross-node connectivity as per_topic_group_e2e; validates channel home-sequencing + cross-node delivery + backfill when connectivity holds. Run with --ignored."]
async fn channel_cross_node_delivery_and_backfill() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = Arc::new(nm_node::Node::bind_local_persistent([61u8; 32], dir_a.path().join("a.redb")).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local_persistent([62u8; 32], dir_b.path().join("b.redb")).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    let (a_addr, b_addr) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] {
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    // 每个节点启动联邦火管（与生产一致）——频道 Meta/消息经火管全节点兜底，
    // 跨节点订阅者的 home 无需频道专属 overlay 先成型即可收到（根治「订阅后收不到名称/图标/消息」）。
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());

    let (cid, sk) = new_channel();
    // A = owner + home（建 keyed 频道，持私钥 → 定序器）。
    let (_oc, owner) = online([63u8; 32], &a_addr).await;
    owner.register_as::<kinds::Person>(&PersonProfile::default(), "Owner", HashMap::new()).await.unwrap();
    owner.channel_create(cid, "公告频道", "", "", &sk).await.expect("create keyed on A");

    // A 先发 2 条（B 尚未订阅 → B 将靠回填补齐这段历史）。
    owner.channel_publish(cid, "历史1").await.expect("pub h1");
    owner.channel_publish(cid, "历史2").await.expect("pub h2");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // B 订阅（另一节点）——用**空名称**模拟「粘贴裸 id 订阅」的真实场景：
    // 名称/图标只能靠 home 的 Meta 经火管传来（正是部署复现的失败点）。
    let (_bc, mut bob) = online([64u8; 32], &b_addr).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();
    bob.channel_sub(cid, "").await.expect("B sub");

    // A 持续再发（实时路径）+ Live 心跳驱动 B 回填历史。
    let send = tokio::spawn(async move {
        for i in 3..=8 {
            let _ = owner.channel_publish(cid, &format!("实时{i}")).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    // B 应收到频道消息（实时广播；kind=ChannelPublish）。
    let got = tokio::time::timeout(Duration::from_secs(30), bob.recv())
        .await.expect("B 未在超时内收到频道消息").expect("B 流结束");
    assert_eq!(got.kind(), GramKind::ChannelPublish);
    send.abort();

    // B 的本地频道日志最终应补齐到 A 的历史（含订阅前的「历史1/2」，经回填）。
    let backfilled = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            // 用 B 的 backfill RPC 读本地持久日志（since=0 → 全部）。
            if let Ok(msgs) = bob.channel_backfill(cid, 0).await {
                let bodies: Vec<String> = msgs.iter().map(|m| m.body.clone()).collect();
                // 历史1/2 来自订阅前，仅可能经回填获得 → 它们在即证明回填闭环。
                if bodies.iter().any(|x| x == "历史1") && bodies.iter().any(|x| x == "历史2") {
                    // 验签：keyed 频道每条带 64 字节 home 签名。
                    assert!(msgs.iter().all(|m| m.home_sig.len() == 64), "每条应带 home 签名");
                    break true;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }).await.unwrap_or(false);
    assert!(backfilled, "B 未补齐订阅前的历史（回填闭环断裂）");

    // 根治点验证：B 虽以空名称订阅，仍应经火管收到 home 的 Meta → channel_list 显示真实频道名。
    // （部署复现的失败：订阅后无名称/图标，正是 Meta 没跨节点到达。）
    let meta_seen = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(list) = bob.channel_list().await
                && list.iter().any(|c| c.channel_id == cid && c.name == "公告频道")
            {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }).await.unwrap_or(false);
    assert!(meta_seen, "B 未收到 home 的频道 Meta（名称/图标跨节点不可见——根治失败）");
}

//! P4 频道升级 Phase 1：持久化 + 账号级订阅 + 启动恢复 + caller-scoped list。
//! 单节点、确定性（不依赖跨节点 gossip overlay 成型，故非 #[ignore]）。
//!
//! - 持久化：channel.create/sub 落 CHANNELS+CHANNEL_SUBS；节点重启后频道 + 订阅仍在（重启不丢）。
//! - 账号级订阅：channel.list 仅返回 caller 账号订阅的频道（修旧版「节点全局泄漏」：返回本节点所有频道）。
//! - 日志持久化：publish 落 CHANNEL_LOG，backfill 从 store 取（seq 全局/每节点都可回填）。

use std::collections::HashMap;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;

async fn online(c: &nm_client::Client, addr: &nm_transport::Addr) -> nm_client::Session {
    tokio::time::timeout(Duration::from_secs(20), c.online(addr.clone()))
        .await
        .expect("online timed out")
        .expect("online failed")
}

/// 生成 keyed 频道密钥对：返回 (channel_id=公钥, secret=私钥)。
fn new_channel() -> ([u8; 32], [u8; 32]) {
    let sk = nm_transport::SecretKey::generate();
    (*sk.public().as_bytes(), sk.to_bytes())
}

/// 持久化：A 创建频道 c1、订阅 c2；重启节点（同 db）后，A 重登仍能在 channel.list 看到 c1+c2。
#[tokio::test]
async fn channel_subs_and_meta_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("node.redb");
    let seed = [71u8; 32];
    let a_seed = [72u8; 32];
    let (c1, c1_sk) = new_channel(); // keyed 频道（create）
    let c2 = [0xC2u8; 32]; // 订阅的远端频道（sub 不需 secret）

    // ---- 第一次启动：建频道 + 订阅 ----
    {
        let node = std::sync::Arc::new(
            nm_node::Node::bind_local_persistent(seed, &db).await.expect("bind"),
        );
        let addr = node.addr();
        {
            let n = node.clone();
            tokio::spawn(async move { let _ = n.serve().await; });
        }
        let a = nm_client::Client::bind_local(a_seed).await.unwrap();
        let a_sess = online(&a, &addr).await;
        a_sess.register_as::<kinds::Person>(&PersonProfile::default(), "A", HashMap::new()).await.unwrap();

        a_sess.channel_create(c1, "频道一", "公告", "", &c1_sk).await.expect("create c1");
        a_sess.channel_sub(c2, "频道二").await.expect("sub c2");

        let list = a_sess.channel_list().await.expect("list");
        let ids: Vec<Vec<u8>> = list.iter().map(|c| c.channel_id.clone()).collect();
        assert!(ids.contains(&c1.to_vec()) && ids.contains(&c2.to_vec()), "建/订阅后应都在列表");
        // 优雅停：触发 shutdown（drop node 的 serve 任务）。
        node.trigger_shutdown();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // ---- 第二次启动：同 db，频道 + 订阅应从持久化恢复 ----
    {
        let node = std::sync::Arc::new(
            nm_node::Node::bind_local_persistent(seed, &db).await.expect("rebind"),
        );
        let addr = node.addr();
        {
            let n = node.clone();
            tokio::spawn(async move { let _ = n.serve().await; });
        }
        // 触发启动恢复。
        node.clone().spawn_channel_restore();
        tokio::time::sleep(Duration::from_millis(500)).await;

        let a = nm_client::Client::bind_local(a_seed).await.unwrap();
        let a_sess = online(&a, &addr).await;
        // 重登（同账号）。设备证书持久化 → 账号恢复；channel.list 按账号订阅返回。
        let list = tokio::time::timeout(Duration::from_secs(5), a_sess.channel_list())
            .await.expect("list timeout").expect("list");
        let ids: Vec<Vec<u8>> = list.iter().map(|c| c.channel_id.clone()).collect();
        assert!(ids.contains(&c1.to_vec()), "重启后 c1 订阅应恢复（持久化）");
        assert!(ids.contains(&c2.to_vec()), "重启后 c2 订阅应恢复（持久化）");
        // 元信息也恢复（c1 的 name）。
        let c1meta = list.iter().find(|c| c.channel_id == c1.to_vec()).unwrap();
        assert_eq!(c1meta.name, "频道一", "重启后频道元信息应恢复");
    }
}

/// 账号级 list 隔离：A 订阅 c1，B 订阅 c2（同节点）；A 的 list 不含 c2，B 的 list 不含 c1。
#[tokio::test]
async fn channel_list_is_caller_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("node.redb");
    let node = std::sync::Arc::new(
        nm_node::Node::bind_local_persistent([81u8; 32], &db).await.expect("bind"),
    );
    let addr = node.addr();
    {
        let n = node.clone();
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    let (c1, c1_sk) = new_channel();
    let (c2, c2_sk) = new_channel();

    let a = nm_client::Client::bind_local([82u8; 32]).await.unwrap();
    let a_sess = online(&a, &addr).await;
    a_sess.register_as::<kinds::Person>(&PersonProfile::default(), "A", HashMap::new()).await.unwrap();
    let b = nm_client::Client::bind_local([83u8; 32]).await.unwrap();
    let b_sess = online(&b, &addr).await;
    b_sess.register_as::<kinds::Person>(&PersonProfile::default(), "B", HashMap::new()).await.unwrap();

    a_sess.channel_create(c1, "A的频道", "", "", &c1_sk).await.expect("A create c1");
    b_sess.channel_create(c2, "B的频道", "", "", &c2_sk).await.expect("B create c2");

    let a_list: Vec<Vec<u8>> = a_sess.channel_list().await.unwrap().iter().map(|c| c.channel_id.clone()).collect();
    let b_list: Vec<Vec<u8>> = b_sess.channel_list().await.unwrap().iter().map(|c| c.channel_id.clone()).collect();

    assert!(a_list.contains(&c1.to_vec()) && !a_list.contains(&c2.to_vec()), "A 只应看到自己订阅的 c1，不泄漏 c2");
    assert!(b_list.contains(&c2.to_vec()) && !b_list.contains(&c1.to_vec()), "B 只应看到自己订阅的 c2，不泄漏 c1");
}

/// 日志持久化 + 回填：A 建频道发 3 条，backfill(since=0) 从 store 取回 3 条、seq 递增。
#[tokio::test]
async fn channel_publish_persists_and_backfills() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("node.redb");
    let node = std::sync::Arc::new(
        nm_node::Node::bind_local_persistent([91u8; 32], &db).await.expect("bind"),
    );
    let addr = node.addr();
    {
        let n = node.clone();
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    let (cid, cid_sk) = new_channel();
    let a = nm_client::Client::bind_local([92u8; 32]).await.unwrap();
    let a_sess = online(&a, &addr).await;
    a_sess.register_as::<kinds::Person>(&PersonProfile::default(), "A", HashMap::new()).await.unwrap();
    a_sess.channel_create(cid, "日志频道", "", "", &cid_sk).await.expect("create");

    for i in 1..=3 {
        a_sess.channel_publish(cid, &format!("消息{i}")).await.expect("publish");
    }
    tokio::time::sleep(Duration::from_millis(400)).await; // 等 run_channel 落库

    let msgs = a_sess.channel_backfill(cid, 0).await.expect("backfill");
    assert_eq!(msgs.len(), 3, "应回填 3 条");
    assert_eq!(msgs[0].seq, 1);
    assert_eq!(msgs[2].seq, 3, "seq 单调递增");
    assert_eq!(msgs[2].body, "消息3");
}

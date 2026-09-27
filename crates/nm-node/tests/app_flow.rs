//! App 流程回归：复刻桌面端真实用法——
//! - 群：owner 建群后**主动添加**成员(group_add，而非成员自行 join)，再发群消息，成员应实时收到。
//! - 频道：owner 建频道，第二个客户端 **channel_sub 订阅**后，owner publish，订阅者应实时收到。
//! 单节点、两个在线客户端；用于定位"其他实体收不到群/频道消息"。

use std::collections::HashMap;
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

async fn reg(s: &nm_client::Session, name: &str) {
    s.register_as::<kinds::Person>(&PersonProfile::default(), name, HashMap::new())
        .await
        .unwrap();
}

/// owner 建群 + group_add 成员 → 发群消息 → 成员实时收到。
#[tokio::test]
async fn group_add_then_fanout_delivers() {
    let dir = tempfile::tempdir().unwrap();
    let node = nm_node::Node::bind_local_persistent([80u8; 32], dir.path().join("n.redb"))
        .await
        .unwrap();
    let addr = node.addr();
    tokio::spawn(async move { let _ = node.serve().await; });

    let (_ac, owner) = online([81u8; 32], &addr).await;
    reg(&owner, "群主").await;
    let (_bc, mut bob) = online([82u8; 32], &addr).await;
    reg(&bob, "Bob").await;
    let bob_id = bob.id_bytes();

    let gid = [90u8; 32];
    owner.group_create(gid, "工作群").await.unwrap();
    // 关键：owner 主动添加 bob（app 用法），而非 bob 自行 join。
    owner.group_add(gid, bob_id).await.unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;
    let ack = owner.send_group(gid, "在吗").await.unwrap();
    assert_eq!(ack.kind(), GramKind::Receipt);

    let got = tokio::time::timeout(Duration::from_secs(10), bob.recv())
        .await
        .expect("bob recv 超时——成员收不到群消息")
        .expect("bob 无消息");
    assert_eq!(got.kind(), GramKind::GroupMessage);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "在吗");
}

/// owner 建频道 → 第二客户端 channel_sub → owner publish → 订阅者实时收到。
#[tokio::test]
async fn channel_sub_then_publish_delivers() {
    let dir = tempfile::tempdir().unwrap();
    let node = nm_node::Node::bind_local_persistent([83u8; 32], dir.path().join("n.redb"))
        .await
        .unwrap();
    let addr = node.addr();
    tokio::spawn(async move { let _ = node.serve().await; });

    let (_ac, owner) = online([84u8; 32], &addr).await;
    reg(&owner, "频道主").await;
    let (_bc, mut sub) = online([85u8; 32], &addr).await;
    reg(&sub, "订阅者").await;

    let cid = [91u8; 32];
    owner.channel_create(cid, "公告", "频道简介", "").await.unwrap();
    // 第二客户端订阅（app 的 primeChannels/粘贴订阅路径）。
    sub.channel_sub(cid, "公告").await.unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;
    owner.channel_publish(cid, "第一条公告").await.unwrap();

    let got = tokio::time::timeout(Duration::from_secs(10), sub.recv())
        .await
        .expect("订阅者 recv 超时——频道消息收不到")
        .expect("订阅者无消息");
    assert_eq!(got.kind(), GramKind::ChannelPublish);
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "第一条公告");
}

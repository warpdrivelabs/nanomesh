//! 回归：消息丢失 & 在线状态修复（docs/MESSAGE_LOSS_DIAGNOSIS.md）。
//! 单节点、确定性（不依赖跨节点 gossip overlay 成型，故非 #[ignore]）。
//!
//! - 缺陷 B：Session::disconnect() 优雅下线 → 节点即时移除会话 → 发给「刚下线」用户的消息
//!   正确入离线库、重登可 drain（修复「发给刚退出 app 的用户，重登收不到」）。
//! - 缺陷 A：presence.query RPC 按 id 批量返回在线状态（供 app 给跨节点好友盖 presence）。

use std::collections::HashMap;
use std::time::Duration;

use nm_entity::kinds; 
use nm_proto::pb::PersonProfile;
use nm_proto::GramKind;

async fn online(c: &nm_client::Client, addr: &nm_transport::Addr) -> nm_client::Session {
    tokio::time::timeout(Duration::from_secs(20), c.online(addr.clone()))
        .await
        .expect("online timed out")
        .expect("online failed")
}

/// 缺陷 B：优雅 disconnect 后，发给该用户的消息入离线库，重登可收。
#[tokio::test]
async fn graceful_disconnect_routes_next_dm_to_offline_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("node.redb");
    let node = std::sync::Arc::new(nm_node::Node::bind_local_persistent([61u8; 32], &db).await.expect("bind node"));
    let addr = node.addr();
    {
        let n = node.clone();
        tokio::spawn(async move { let _ = n.serve().await; });
    }

    // A、B 都上线并注册。
    let a = nm_client::Client::bind_local([62u8; 32]).await.unwrap();
    let a_sess = online(&a, &addr).await;
    a_sess.register_as::<kinds::Person>(&PersonProfile::default(), "A", HashMap::new()).await.unwrap();

    let b_seed = [63u8; 32];
    let b = nm_client::Client::bind_local(b_seed).await.unwrap();
    let b_sess = online(&b, &addr).await;
    b_sess.register_as::<kinds::Person>(&PersonProfile::default(), "B", HashMap::new()).await.unwrap();
    let b_id = b_sess.id_bytes();
    assert_eq!(node.online_count(), 2, "两端应都在线");

    // B 优雅下线（app 退出/切号路径调的就是这个）→ 节点应即时移除会话（非等 10s idle）。
    b_sess.disconnect().await;
    let removed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if node.online_count() == 1 { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await;
    assert!(removed.is_ok(), "disconnect 后节点应即时移除 B 的会话（Logout），实测仍在线");

    // A 此刻发给「刚下线」的 B → 必须入离线库（而非 fire-and-forget 进死连接丢失）。
    let ack = a_sess.send_to(b_id, "B 刚退出时发的").await.expect("send_to");
    assert_eq!(ack.kind(), GramKind::Receipt);

    // B 重新上线 → 应 drain 到那条离线消息。
    let b2 = nm_client::Client::bind_local(b_seed).await.unwrap();
    let mut b2_sess = online(&b2, &addr).await;
    let pushed = tokio::time::timeout(Duration::from_secs(10), b2_sess.recv())
        .await
        .expect("B 重登 recv 超时")
        .expect("重登未收到离线消息（缺陷 B 未修复）");
    assert_eq!(pushed.kind(), GramKind::Message);
    let text = String::from_utf8(pushed.payload.expect("payload").value).unwrap();
    assert_eq!(text, "B 刚退出时发的");
}

/// 缺陷 A：presence.query 返回本地在线实体为 online、未知实体为 offline。
#[tokio::test]
async fn presence_query_reports_online_and_offline() {
    let node = std::sync::Arc::new(nm_node::Node::bind_local([64u8; 32]).await.expect("bind node"));
    let addr = node.addr();
    {
        let n = node.clone();
        tokio::spawn(async move { let _ = n.serve().await; });
    }

    let a = nm_client::Client::bind_local([65u8; 32]).await.unwrap();
    let a_sess = online(&a, &addr).await;
    a_sess.register_as::<kinds::Person>(&PersonProfile::default(), "A", HashMap::new()).await.unwrap();

    // 一个在线的 B。
    let b = nm_client::Client::bind_local([66u8; 32]).await.unwrap();
    let b_sess = online(&b, &addr).await;
    b_sess.register_as::<kinds::Person>(&PersonProfile::default(), "B", HashMap::new()).await.unwrap();
    let b_id = b_sess.id_bytes();

    // 一个从未上线的 C（随机 id）。
    let c_id = [77u8; 32];

    // A 批量查 B（在线）+ C（离线）。
    let m = tokio::time::timeout(Duration::from_secs(5), a_sess.presence_query(&[b_id, c_id]))
        .await
        .expect("presence_query 超时")
        .expect("presence_query 失败");

    let b_hex: String = b_id.iter().map(|x| format!("{x:02x}")).collect();
    let c_hex: String = c_id.iter().map(|x| format!("{x:02x}")).collect();
    assert_eq!(m.get(b_hex.as_str()).map(String::as_str), Some("online"), "在线的 B 应报 online");
    assert_eq!(m.get(c_hex.as_str()).map(String::as_str), Some("offline"), "未知的 C 应报 offline");
}

//! 回归：单公钥重连速率门（滑动计数窗口）必须放行正常快速重连，只拦持续风暴。
//!
//! 背景：曾用「最小间隔 1s」一刀切速率门（commit dcc671b），导致同一用户 1s 内的正常重连
//! （app 重启 / 切后台回来 / 断线重连）被 close("rate_limited") → client recv 流立即结束收不到消息。
//! 由 git bisect 定位（此前 message_loss_fixes 与 e1_directory 均因此确定性失败）。
//! 改为滑动计数窗口后：窗口内允许突发 N 次，超出才拦——本测试锁住该不变量。
use std::collections::HashMap;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;

async fn try_online(c: &nm_client::Client, addr: &nm_transport::Addr) -> Option<nm_client::Session> {
    tokio::time::timeout(Duration::from_secs(10), c.online(addr.clone()))
        .await
        .ok()
        .and_then(|r| r.ok())
}

/// 同一公钥在 1s 内连续重连 3 次（远低于窗口上限）——每次都应成功建立会话，
/// 且最后一个会话能收到发给它的离线消息（= 没被速率门误杀）。
#[tokio::test]
async fn rapid_same_key_reconnect_allowed() {
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

    // 发送方 A 注册。
    let a = nm_client::Client::bind_local([92u8; 32]).await.unwrap();
    let a_sess = try_online(&a, &addr).await.expect("A online");
    a_sess
        .register_as::<kinds::Person>(&PersonProfile::default(), "A", HashMap::new())
        .await
        .unwrap();

    // B 用同一 seed 快速重连 3 次（每次新建连接，间隔远小于 1s）。前两次建后立即丢弃。
    let b_seed = [93u8; 32];
    let b_id;
    {
        let b0 = nm_client::Client::bind_local(b_seed).await.unwrap();
        let s0 = try_online(&b0, &addr).await.expect("B 第1次重连应成功");
        s0.register_as::<kinds::Person>(&PersonProfile::default(), "B", HashMap::new())
            .await
            .unwrap();
        b_id = s0.id_bytes();
        drop(s0);
        drop(b0);
    }
    {
        let b1 = nm_client::Client::bind_local(b_seed).await.unwrap();
        let _s1 = try_online(&b1, &addr).await.expect("B 第2次快速重连应成功（不被速率门误杀）");
        drop(_s1);
        drop(b1);
    }
    // 第 3 次重连：建立会话并验证能收消息（新会话未被 close）。
    let b2 = nm_client::Client::bind_local(b_seed).await.unwrap();
    let mut b2_sess = try_online(&b2, &addr).await.expect("B 第3次快速重连应成功");

    // A 发给 B（此刻只有 b2_sess 这条新连接）→ 应直投到 b2（会话未被误杀）。
    a_sess.send_to(b_id, "reconnect-ok").await.expect("send_to");
    let got = tokio::time::timeout(Duration::from_secs(8), b2_sess.recv())
        .await
        .expect("b2 recv 超时 → 新会话被速率门误杀（回归）")
        .expect("b2 流结束 → 新会话被 close（回归）");
    let text = String::from_utf8(got.payload.expect("payload").value).unwrap();
    assert_eq!(text, "reconnect-ok", "快速重连后的新会话应正常收消息");
}

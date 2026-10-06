//! 规模化联邦 F0：验证「每群独立 gossip 主题」原语——
//! 成员节点 A、B 各自 `Node::federation()` 并 `join_group(gid)`；A `publish` → B 收到；
//! 第三个未加入该群的节点 C **收不到**该群流量（核心可扩展性/隐私断言）。
//!
//! 复刻 group_gossip.rs 的双节点 gossip 播种 + serve() 模式。本测试不触碰任何现有投递路径。

use std::sync::Arc;
use std::time::Duration;

fn id32(n: &nm_node::Node) -> [u8; 32] {
    *n.id().as_bytes()
}

#[tokio::test]
async fn per_topic_group_members_only() {
    // 三节点：A、B 是群成员，C 不是。互相播种地址 + 各自 serve。
    let a = Arc::new(nm_node::Node::bind_local([61u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([62u8; 32]).await.unwrap());
    let c = Arc::new(nm_node::Node::bind_local([63u8; 32]).await.unwrap());
    for (x, y) in [(&a, &b), (&a, &c), (&b, &c)] {
        x.add_peer_addr(y.addr());
        y.add_peer_addr(x.addr());
    }
    for n in [a.clone(), b.clone(), c.clone()] {
        tokio::spawn(async move {
            let _ = n.serve().await;
        });
    }

    // 各节点构造联邦句柄；A、B 加入同一群主题，C 不加入。
    let (fa, _ra) = a.federation();
    let (fb, mut rb) = b.federation();
    let (_fc, mut rc) = c.federation();
    let gid: &[u8] = b"group-xyz";
    fa.join_group(gid, vec![id32(&b), id32(&c)]).await.unwrap();
    fb.join_group(gid, vec![id32(&a)]).await.unwrap();
    assert_eq!(fa.joined().await, vec![gid.to_vec()]);

    // A 反复向群主题广播，直到叠加网成型、B 收到（幂等，重复无害）。
    let send = {
        let fa = fa.clone();
        tokio::spawn(async move {
            for _ in 0..40 {
                let _ = fa.publish(gid, b"hello-group".to_vec()).await;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        })
    };

    let got = tokio::time::timeout(Duration::from_secs(30), rb.recv())
        .await
        .expect("B 未在超时内收到群主题消息")
        .expect("B 事件流结束");
    assert_eq!(got.group, gid.to_vec());
    assert_eq!(got.content, b"hello-group".to_vec());

    // 核心断言：未加入该群的 C 不应收到任何该群流量。
    let none = tokio::time::timeout(Duration::from_secs(3), rc.recv()).await;
    assert!(none.is_err(), "非成员 C 不应收到群主题流量（可扩展性/隐私）");

    send.abort();
}

/// publish 前未 join → NotJoined；join 幂等；leave 后 publish 再次 NotJoined。
#[tokio::test]
async fn publish_requires_join_and_leave_stops() {
    let a = Arc::new(nm_node::Node::bind_local([64u8; 32]).await.unwrap());
    tokio::spawn({
        let a = a.clone();
        async move {
            let _ = a.serve().await;
        }
    });
    let (fa, _ra) = a.federation();
    let gid: &[u8] = b"g1";

    assert!(matches!(
        fa.publish(gid, b"x".to_vec()).await,
        Err(nm_federation::FederationError::NotJoined)
    ));
    fa.join_group(gid, vec![]).await.unwrap();
    fa.join_group(gid, vec![]).await.unwrap(); // 幂等
    assert_eq!(fa.joined().await.len(), 1);
    fa.publish(gid, b"x".to_vec()).await.unwrap(); // 已加入：发布成功（即便暂无邻居）
    fa.leave_group(gid).await;
    assert!(fa.joined().await.is_empty());
    assert!(matches!(
        fa.publish(gid, b"x".to_vec()).await,
        Err(nm_federation::FederationError::NotJoined)
    ));
}

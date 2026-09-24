//! 节点级 gossip 频道广播：3 个节点接入同一频道，任一节点 `publish` 的消息经叠加网
//! 扩散到其余节点。验证 `nm-gossip` pub/sub 原语 + `nm-node` 的 Router 化 accept
//!（单播与 gossip 共用同一 endpoint、同一身份）。

use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn gossip_channel_broadcast_three_nodes() {
    // 3 个「服务器」：各自独立身份，临时端口（用 addr() 互相播种地址）。
    let a = Arc::new(nm_node::Node::bind_local([31u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([32u8; 32]).await.unwrap());
    let c = Arc::new(nm_node::Node::bind_local([33u8; 32]).await.unwrap());

    let a_id = *a.id().as_bytes();
    let b_id = *b.id().as_bytes();
    let c_id = *c.id().as_bytes();
    let (a_addr, b_addr, c_addr) = (a.addr(), b.addr(), c.addr());

    // 互相播种地址：add_peer_addr 会把地址喂进各自 endpoint 地址簿，供 gossip 按公钥拨号。
    a.add_peer_addr(b_addr.clone());
    a.add_peer_addr(c_addr.clone());
    b.add_peer_addr(a_addr.clone());
    b.add_peer_addr(c_addr.clone());
    c.add_peer_addr(a_addr.clone());
    c.add_peer_addr(b_addr.clone());

    // 各自 serve（起 Router，开始接受单播 + gossip 连接）。
    for n in [a.clone(), b.clone(), c.clone()] {
        tokio::spawn(async move {
            let _ = n.serve().await;
        });
    }

    let channel = [7u8; 32];
    // A 以 B、C 为引导；B、C 以 A 为引导 → 连通同一叠加网。
    let ta = a.join_channel(channel, vec![b_id, c_id]).await.unwrap();
    let mut tb = b.join_channel(channel, vec![a_id]).await.unwrap();
    let mut tc = c.join_channel(channel, vec![a_id]).await.unwrap();

    // B、C 各自等待收到广播。
    let bh = tokio::spawn(async move { tokio::time::timeout(Duration::from_secs(25), tb.recv()).await });
    let ch = tokio::spawn(async move { tokio::time::timeout(Duration::from_secs(25), tc.recv()).await });

    // A 重复广播，直到叠加网成型、对端收到（广播幂等，重复无害）。
    let pubtask = tokio::spawn(async move {
        for _ in 0..80 {
            let _ = ta.publish(b"hello nmspace channel".to_vec()).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    });

    let bmsg = bh
        .await
        .unwrap()
        .expect("B 未在超时内收到频道广播")
        .expect("B 的频道流已结束");
    let cmsg = ch
        .await
        .unwrap()
        .expect("C 未在超时内收到频道广播")
        .expect("C 的频道流已结束");
    pubtask.abort();

    assert_eq!(bmsg.content, b"hello nmspace channel".to_vec(), "B 收到内容应一致");
    assert_eq!(cmsg.content, b"hello nmspace channel".to_vec(), "C 收到内容应一致");
}

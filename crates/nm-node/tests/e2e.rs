//! 端到端最小链路测试：节点接受循环 + 客户端连接 + 发一条 Message，校验收到 Receipt。
//! 全程本地直连（Minimal 预设，无中继/发现），验证 传输 → 分帧 → prost gram → 分发 整条路径。

use std::time::Duration;

#[tokio::test]
async fn e2e_message_receipt() {
    // 起节点并进入接受循环。
    let node = nm_node::Node::bind_local([1u8; 32]).await.expect("bind node");
    let node_addr = node.addr();
    tokio::spawn(async move {
        let _ = node.serve().await;
    });

    // 客户端连接节点并发送一条文本消息。
    let client = nm_client::Client::bind_local([2u8; 32]).await.expect("bind client");
    let receipt = tokio::time::timeout(
        Duration::from_secs(20),
        client.send_message(node_addr, [9u8; 32], "hello nmspace"),
    )
    .await
    .expect("connect/send timed out")
    .expect("send_message failed");

    // 校验：收到的是对刚发消息(gram_id=1)的 Receipt。
    assert_eq!(receipt.kind(), nm_proto::GramKind::Receipt);
    assert_eq!(receipt.ref_gram_id, Some(1));
}

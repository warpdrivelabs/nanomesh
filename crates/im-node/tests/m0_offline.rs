//! M0 收尾：离线投递。目标离线时消息入库(redb inbox)，目标上线后自动补投。

use std::time::Duration;

use im_proto::GramKind;

#[tokio::test]
async fn offline_store_and_forward() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("node.redb");

    let node = im_node::Node::bind_local_persistent([41u8; 32], &db)
        .await
        .expect("bind node");
    let addr = node.addr();
    tokio::spawn(async move {
        let _ = node.serve().await;
    });

    // B 的身份（种子 42 → 固定公钥），此刻尚未上线。
    let b_seed = [42u8; 32];
    let b_id = im_client::Client::bind_local(b_seed).await.unwrap().id_bytes();

    // A 上线，给离线的 B 发消息 → 节点应存入 B 的离线队列。
    let a = im_client::Client::bind_local([43u8; 32]).await.unwrap();
    let a_sess = tokio::time::timeout(Duration::from_secs(20), a.online(addr.clone()))
        .await
        .expect("A online timed out")
        .expect("A online failed");
    let ack = a_sess.send_to(b_id, "留言给离线的B").await.expect("send_to failed");
    assert_eq!(ack.kind(), GramKind::Receipt);

    // B 现在上线 → 应收到补投的离线消息。
    let b = im_client::Client::bind_local(b_seed).await.unwrap();
    let mut b_sess = b.online(addr.clone()).await.expect("B online failed");
    let pushed = tokio::time::timeout(Duration::from_secs(10), b_sess.recv())
        .await
        .expect("B recv timed out")
        .expect("no offline message delivered");
    assert_eq!(pushed.kind(), GramKind::Message);
    let text = String::from_utf8(pushed.payload.expect("payload").value).unwrap();
    assert_eq!(text, "留言给离线的B");
}

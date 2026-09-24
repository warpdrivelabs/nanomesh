//! 黑名单机制：**无白名单，默认放行；名单内公钥被拒。**
//! 覆盖：未封禁客户端可用；封禁后其连接/操作被拒；解封后恢复；连接后被拉黑则会话立即失效。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;
use nm_proto::DirectoryQuery;

#[tokio::test]
async fn blacklist_rejects_then_unban_restores() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        nm_node::Node::bind_local_persistent([71u8; 32], dir.path().join("n.redb"))
            .await
            .unwrap(),
    );
    let node_addr = node.addr();
    {
        let n = node.clone();
        tokio::spawn(async move {
            let _ = n.serve().await;
        });
    }

    // 未封禁客户端：正常注册（短连接）。
    let good = nm_client::Client::bind_local([72u8; 32]).await.unwrap();
    good.register_as::<kinds::Person>(node_addr.clone(), &PersonProfile::default(), "good", HashMap::new())
        .await
        .expect("未封禁客户端应成功");

    // 封禁另一个客户端 → 其操作被拒。
    let bad = nm_client::Client::bind_local([73u8; 32]).await.unwrap();
    node.ban(bad.id_bytes());
    assert!(node.is_banned(&bad.id_bytes()));
    let r = tokio::time::timeout(
        Duration::from_secs(6),
        bad.register_as::<kinds::Person>(node_addr.clone(), &PersonProfile::default(), "bad", HashMap::new()),
    )
    .await;
    assert!(matches!(&r, Ok(Err(_)) | Err(_)), "封禁客户端应被拒(报错或超时)，实际={r:?}");

    // 解封 → 恢复可用（新连接不再被拒）。
    node.unban(bad.id_bytes());
    assert!(!node.is_banned(&bad.id_bytes()));
    bad.register_as::<kinds::Person>(node_addr.clone(), &PersonProfile::default(), "bad-now-ok", HashMap::new())
        .await
        .expect("解封后应成功");
}

#[tokio::test]
async fn ban_cuts_live_session() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        nm_node::Node::bind_local_persistent([74u8; 32], dir.path().join("n.redb"))
            .await
            .unwrap(),
    );
    let node_addr = node.addr();
    {
        let n = node.clone();
        tokio::spawn(async move {
            let _ = n.serve().await;
        });
    }

    let c = nm_client::Client::bind_local([75u8; 32]).await.unwrap();
    let session = c.online(node_addr).await.unwrap();
    // 封禁前：目录查询可用（空前缀=不过滤）。
    session
        .directory_query(DirectoryQuery { kind_prefix: String::new(), ..Default::default() })
        .await
        .expect("封禁前应可查询");

    // 拉黑 → 立即切断会话。
    node.ban(c.id_bytes());
    let r = tokio::time::timeout(
        Duration::from_secs(6),
        session.directory_query(DirectoryQuery { kind_prefix: String::new(), ..Default::default() }),
    )
    .await;
    assert!(matches!(&r, Ok(Err(_)) | Err(_)), "封禁后会话应失效，实际={r:?}");
}

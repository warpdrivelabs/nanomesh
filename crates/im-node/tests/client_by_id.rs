//! 客户端「按 node id(公钥)拨号」端到端：客户端不传节点地址，仅凭 node 公钥 `online_by_id`，
//! 由端点地址簿解析后连上、注册、目录查询往返。
//!
//! 这正是跨 NAT 连接后端节点的连接路径——真实 N0/selfhost 下"公钥→当前地址"由发现服务解析；
//! 此处用 `add_peer_addr` 播种地址以**离线验证同一段代码路径**（connect-by-key + 地址解析）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use im_entity::kinds;
use im_proto::pb::PersonProfile;
use im_proto::DirectoryQuery;

#[tokio::test]
async fn client_dials_node_by_id() {
    // 后端节点上线服务。
    let node = Arc::new(im_node::Node::bind_local([41u8; 32]).await.unwrap());
    let node_id = *node.id().as_bytes();
    let node_addr = node.addr();
    {
        let n = node.clone();
        tokio::spawn(async move {
            let _ = n.serve().await;
        });
    }

    // 客户端：只被告知节点地址以供离线解析（真实环境由发现服务代替这一步）。
    let client = im_client::Client::bind_local([42u8; 32]).await.unwrap();
    client.add_peer_addr(node_addr);

    // 关键：仅用 node 公钥拨号，不传地址。
    let session = tokio::time::timeout(Duration::from_secs(20), client.online_by_id(node_id))
        .await
        .expect("online_by_id 超时")
        .expect("online_by_id 失败");

    session
        .register_as::<kinds::Person>(&PersonProfile::default(), "按id接入的人", HashMap::new())
        .await
        .unwrap();

    let found = session
        .directory_query(DirectoryQuery {
            kind_prefix: "person".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        found
            .iter()
            .any(|e| e.entity_id == session.id_bytes().to_vec()),
        "应能查到刚按 id 接入并注册的人"
    );
}

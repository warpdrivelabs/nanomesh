//! 自建基础设施(selfhost)冒烟：节点以「自定义 iroh-relay + 自建 iroh-dns-server(pkarr)」
//! 预设绑定并 serve；客户端按**地址**直连、注册、目录查询往返，证明 selfhost 绑定/服务链路可用。
//!
//! 说明：这里用不可达的 `.invalid` URL——pkarr 后台发布失败无害；按 addr 直连(loopback)
//! 不依赖 relay/dns。真正的**按公钥 dial-by-key**需要真实的自建 iroh-dns-server，属部署/手测范畴。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;
use nm_proto::DirectoryQuery;

#[tokio::test]
async fn selfhost_bind_serve_and_directory_roundtrip() {
    let dir = tempfile::tempdir().unwrap();

    // 以 selfhost 预设绑定：自定义中继 + 自建 pkarr(dns)。端口 0（测试用 addr() 直连）。
    let node = Arc::new(
        nm_node::Node::bind_persistent_selfhosted(
            [211u8; 32],
            dir.path().join("s.redb"),
            vec!["https://relay.invalid.".into()],
            "https://dns.invalid./pkarr".into(),
            Some("dns.invalid.".into()),
            0,
        )
        .await
        .expect("selfhost bind 应成功（绑定不依赖 relay/dns 可达）"),
    );
    let addr = node.addr();
    {
        let n = node.clone();
        tokio::spawn(async move {
            let _ = n.serve().await;
        });
    }

    // 客户端按地址直连该 selfhost 节点，注册为 Person。
    let person_c = nm_client::Client::bind_local([212u8; 32]).await.unwrap();
    let person = tokio::time::timeout(Duration::from_secs(20), person_c.online(addr))
        .await
        .expect("client online timeout")
        .expect("client online failed");
    person
        .register_as::<kinds::Person>(&PersonProfile::default(), "自建人", HashMap::new())
        .await
        .unwrap();
    let me = person.id_bytes();

    // 目录查询往返：应能查到刚注册的 person（证明 selfhost 节点的 serve/命令链路可用）。
    let found = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let list = person
                .directory_query(DirectoryQuery {
                    kind_prefix: "person".into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            if let Some(e) = list.into_iter().next() {
                return TryInto::<[u8; 32]>::try_into(e.entity_id).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    })
    .await
    .expect("selfhost 节点未能在超时内返回目录查询");
    assert_eq!(found, me, "目录应返回刚注册的 person");
}

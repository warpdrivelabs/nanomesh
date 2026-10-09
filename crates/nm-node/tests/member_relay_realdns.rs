//! F6/M1 真 relay 验证（非 mock）：用真实 iroh-dns-server 的 /pkarr 端点跑通成员索引 publish→resolve。
//! 需环境变量 NM_PKARR_URL 指向活的 iroh-dns-server pkarr 端点。默认 #[ignore]。
//!   NM_PKARR_URL=http://127.0.0.1:8090/pkarr cargo test -p nm-node --test member_relay_realdns -- --ignored --nocapture
use std::sync::Arc;

#[tokio::test]
#[ignore = "needs a running iroh-dns-server; set NM_PKARR_URL"]
async fn member_index_via_real_dns_server() {
    let url = std::env::var("NM_PKARR_URL").expect("NM_PKARR_URL");
    eprintln!("real pkarr relay: {url}");

    let anchor = Arc::new(nm_node::Node::bind_local([71u8; 32]).await.unwrap());
    anchor.set_pkarr(Some(url.clone()));
    let anchor_id = *anchor.id().as_bytes();
    anchor.publish_member_index().await;
    eprintln!("anchor published member index (key = its own pubkey z32)");

    // 新节点：零配置，凭真 relay + 锚点公钥解析。
    let newbie = Arc::new(nm_node::Node::bind_local([72u8; 32]).await.unwrap());
    newbie.set_pkarr(Some(url));
    let learned = newbie.resolve_member_index(&anchor_id).await;
    eprintln!("newbie resolved {learned} bootstrap node(s) from real relay");
    assert_eq!(learned, 1, "真 relay 应往返出 1 个 bootstrap 节点（锚点自身）");
    assert!(newbie.peers_list().contains(&anchor_id), "peers 应回填锚点");

    // 防投毒：真 relay 上未发布过的锚点公钥 → 0。
    assert_eq!(newbie.resolve_member_index(&[9u8; 32]).await, 0, "未发布锚点公钥应为 0");
    eprintln!("OK: real iroh-dns-server pkarr relay roundtrip + verify + peers-seed + poison-resist");
}

//! F6/M1（成员发现 relay 索引）本地验证：用一个 localhost mock pkarr relay（PUT 存 / GET 取）跑通
//! `publish_member_index`（锚点签名发布 bootstrap 成员集合）→ `resolve_member_index`（新节点 GET + 验签 +
//! 把解析到的锚点喂入 `peers`）全链路。单节点 relay + 两个本地节点，不涉及跨节点 gossip 叠加网，稳定可跑。
//!
//! 对应「完成判据」：一个无任何 [[peers]] 预配的新节点，仅凭 relay + 锚点公钥，能发现 bootstrap 并回填 peers。
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    routing::put,
    Router,
};

type RelayStore = Arc<Mutex<HashMap<String, Vec<u8>>>>;

async fn put_rec(State(s): State<RelayStore>, Path(key): Path<String>, body: Bytes) -> StatusCode {
    s.lock().unwrap().insert(key, body.to_vec());
    StatusCode::OK
}
async fn get_rec(State(s): State<RelayStore>, Path(key): Path<String>) -> Result<Vec<u8>, StatusCode> {
    s.lock().unwrap().get(&key).cloned().ok_or(StatusCode::NOT_FOUND)
}

#[tokio::test]
async fn member_index_publish_resolve_via_mock_relay() {
    // 1) mock relay。
    let store: RelayStore = Arc::new(Mutex::new(HashMap::new()));
    let app = Router::new().route("/{key}", put(put_rec).get(get_rec)).with_state(store.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // 2) 锚点节点：指向 relay，发布成员索引（key = 锚点公钥）。
    let anchor = Arc::new(nm_node::Node::bind_local([71u8; 32]).await.unwrap());
    anchor.set_pkarr(Some(base.clone()));
    let anchor_id = *anchor.id().as_bytes();
    anchor.publish_member_index().await;
    assert!(!store.lock().unwrap().is_empty(), "relay 应已存入锚点公钥下的成员索引");

    // 3) 新节点：零 [[peers]] 预配，仅凭 relay + 锚点公钥解析 → peers 应回填锚点。
    let newbie = Arc::new(nm_node::Node::bind_local([72u8; 32]).await.unwrap());
    newbie.set_pkarr(Some(base));
    assert_eq!(newbie.peer_count(), 0, "新节点初始应无 peer");
    let learned = newbie.resolve_member_index(&anchor_id).await;
    assert_eq!(learned, 1, "应从锚点索引学到 1 个 bootstrap 节点（锚点自身）");
    assert!(newbie.peers_list().contains(&anchor_id), "peers 应回填锚点（满足 gossip bootstrap 契约）");

    // 4) 防投毒：用错误锚点公钥解析应得 0（键不同→404；即便拿到也会验签失败）。
    let bogus = [99u8; 32];
    assert_eq!(newbie.resolve_member_index(&bogus).await, 0, "未发布过的锚点公钥应解析为 0");
}

//! F2 (pkarr 群发现) 本地验证：用一个 localhost mock pkarr relay（PUT 存 / GET 取）跑通
//! `pkarr_publish_group` → `pkarr_resolve_group` 全链路——持久化群私钥取出、seed 收集、签名、
//! HTTP PUT、GET、验签、解析。单节点 + 本机回环，不涉及跨节点 gossip 叠加网，故稳定可跑。
//!
//! 真实 iroh-dns-server relay 的联调见环境变量门控用例（需活服务器，故 #[ignore]）。
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// 起一个最小 pkarr relay（PUT 存 / GET 取），返回 (存储句柄, base url)。
async fn spawn_relay() -> (RelayStore, String) {
    let store: RelayStore = Arc::new(Mutex::new(HashMap::new()));
    let app = Router::new().route("/{key}", put(put_rec).get(get_rec)).with_state(store.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { let _ = axum::serve(listener, app).await; });
    (store, base)
}

/// 一个「死」端点：绑定取端口后立即释放 → 连接被拒（模拟中继宕机）。
async fn dead_endpoint() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    drop(l);
    format!("http://{a}")
}

#[tokio::test]
async fn pkarr_publish_resolve_via_mock_relay() {
    // 1) 起一个最小 pkarr relay（PUT 存、GET 取），绑定临时端口。
    let store: RelayStore = Arc::new(Mutex::new(HashMap::new()));
    let app = Router::new()
        .route("/{key}", put(put_rec).get(get_rec))
        .with_state(store.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // 2) 带存储的本地节点（持久化群私钥需 store）。
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        nm_node::Node::bind_local_persistent([61u8; 32], dir.path().join("db"))
            .await
            .unwrap(),
    );
    let addr = node.addr();
    {
        let n = node.clone();
        tokio::spawn(async move {
            let _ = n.serve().await;
        });
    }

    // 3) owner 上线 → 建「带密钥群」（群私钥随 create 存到 node 的 GROUP_SECRETS）。
    let c = nm_client::Client::bind_local([62u8; 32]).await.unwrap();
    let owner = tokio::time::timeout(Duration::from_secs(20), c.online(addr))
        .await
        .expect("online timed out")
        .expect("online failed");
    let gid = owner.group_create_keyed("无锚发现群").await.unwrap();

    // 4) 指向 mock relay → 发布 → 解析，验证全链路。
    node.set_pkarr(Some(base));
    node.pkarr_publish_group(&gid).await;
    let seeds = node.pkarr_resolve_group(&gid).await;

    let me = *node.id().as_bytes();
    assert!(seeds.contains(&me), "解析出的 seed 应含 home 节点自身");
    assert!(
        !store.lock().unwrap().is_empty(),
        "relay 应已存入该群公钥下的签名记录"
    );

    // 5) 用错误公钥向同一 relay 解析应得不到该记录（键不同 → 404 → 空）。
    let other = [99u8; 32];
    assert!(
        node.pkarr_resolve_group(&other).await.is_empty(),
        "未发布过的群公钥应解析为空"
    );
}

/// P2-12 Phase A：多中继弱锚多副本——publish 写全部、resolve 首成功；首个中继宕机仍能 failover 解析。
#[tokio::test]
async fn pkarr_multi_relay_failover() {
    let (store_a, base_a) = spawn_relay().await;
    let (store_b, base_b) = spawn_relay().await;
    let dead = dead_endpoint().await;

    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        nm_node::Node::bind_local_persistent([63u8; 32], dir.path().join("db")).await.unwrap(),
    );
    let addr = node.addr();
    {
        let n = node.clone();
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    let c = nm_client::Client::bind_local([64u8; 32]).await.unwrap();
    let owner = tokio::time::timeout(Duration::from_secs(20), c.online(addr))
        .await
        .expect("online timed out")
        .expect("online failed");
    let gid = owner.group_create_keyed("多中继群").await.unwrap();
    let me = *node.id().as_bytes();

    // publish 写**全部**中继（弱锚多副本）。
    node.set_pkarr(Some(format!("{base_a},{base_b}")));
    node.pkarr_publish_group(&gid).await;
    assert!(!store_a.lock().unwrap().is_empty(), "relay A 应已写入");
    assert!(!store_b.lock().unwrap().is_empty(), "relay B 应已写入（publish 写全部）");

    // 首个中继宕机 → 跳过 → failover 到次个解析成功（= 退单点）。
    node.set_pkarr(Some(format!("{dead},{base_b}")));
    let seeds = node.pkarr_resolve_group(&gid).await;
    assert!(seeds.contains(&me), "首个中继死也应 failover 到次个解析出 seed");
    let (ok, _) = node.pkarr_stat_snapshot();
    assert!(ok >= 1, "成功解析应计入 resolve_ok");

    // 全中继不可达 → 空 + resolve_fail 计数。
    node.set_pkarr(Some(format!("{dead},{dead}")));
    assert!(node.pkarr_resolve_group(&gid).await.is_empty(), "全中继不可达应解析为空");
    let (_, fail) = node.pkarr_stat_snapshot();
    assert!(fail >= 1, "全失败应计入 resolve_fail");
}

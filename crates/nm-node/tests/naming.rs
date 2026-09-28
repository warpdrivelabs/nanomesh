//! 去中心命名 N1：node A 域名 acme.mesh，客户端在 A 认领本地名 → 经联邦 gossip 复制，
//! node B 上另一客户端能解析 <local>@acme.mesh → 该客户端公钥；负例/反向/唯一性均校验。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;

const LOCAL: &str = "ada";
const DOMAIN: &str = "acme.mesh";

async fn online(seed: [u8; 32], addr: &nm_transport::Addr) -> (nm_client::Client, nm_client::Session) {
    let c = nm_client::Client::bind_local(seed).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(20), c.online(addr.clone()))
        .await.expect("online timed out").expect("online failed");
    (c, s)
}

#[tokio::test]
async fn name_claim_resolve_across_nodes() {
    let a = Arc::new(nm_node::Node::bind_local([91u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([92u8; 32]).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    a.set_domain(DOMAIN.into()); // A 自声明域名
    b.set_domain("other.mesh".into());
    let (a_addr, b_addr) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] { tokio::spawn(async move { let _ = n.serve().await; }); }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());

    // alice 连 A，认领本地名（→ alice 公钥）。
    let (_ac, alice) = online([93u8; 32], &a_addr).await;
    alice.register_as::<kinds::Person>(&PersonProfile::default(), "Alice", HashMap::new()).await.unwrap();
    let rec = alice.name_claim(LOCAL).await.expect("claim 失败");
    assert_eq!(rec.domain, DOMAIN);
    assert_eq!(rec.local_part, LOCAL);
    assert_eq!(rec.client_pubkey, alice.id_bytes().to_vec());

    // bob 连 B；轮询解析（等 gossip 复制到 B）。
    let (_bc, bob) = online([94u8; 32], &b_addr).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();

    let mut got = None;
    let full = format!("{}@{}", LOCAL, DOMAIN);   // 计算全名，避免手写 @ 字面量
    let bogus = format!("nobody.{}", DOMAIN);     // 未认领名（local-part 不同）
    for _ in 0..40 {
        if let Ok(Some(r)) = bob.name_resolve(&full).await { got = Some(r); break; }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    let r = got.expect("B 未能解析该名（gossip 复制失败）");
    assert_eq!(r.client_pubkey, alice.id_bytes().to_vec(), "解析到的公钥应为 alice");

    // 负例：未认领的名字必须解析为 None（防“任意查询都返回记录”的假阳性）。
    assert!(bob.name_resolve(&bogus).await.unwrap().is_none(), "未认领名应解析为空");

    // 反向：B 用 alice 公钥反查 → 规范名。
    let rev = bob.name_reverse(alice.id_bytes()).await.expect("reverse 失败").expect("反查无结果");
    assert_eq!(rev.local_part, LOCAL);
    assert_eq!(rev.domain, DOMAIN);

    // 唯一性：另一个客户端在 A 认领同名应被拒。
    let (_cc, carol) = online([95u8; 32], &a_addr).await;
    carol.register_as::<kinds::Person>(&PersonProfile::default(), "Carol", HashMap::new()).await.unwrap();
    assert!(carol.name_claim(LOCAL).await.is_err(), "同域重名应被拒绝");
}

/// 管理端注册商流程：add_domain → admin_set_name（任意公钥）→ 异节点解析 → admin_del_name（墓碑）→ 异节点移除。
#[tokio::test]
async fn admin_registrar_crud_across_nodes() {
    let a = std::sync::Arc::new(nm_node::Node::bind_local([96u8; 32]).await.unwrap());
    let b = std::sync::Arc::new(nm_node::Node::bind_local([97u8; 32]).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    let b_addr = b.addr();
    for n in [a.clone(), b.clone()] { tokio::spawn(async move { let _ = n.serve().await; }); }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());
    tokio::time::sleep(Duration::from_secs(4)).await; // 等 gossip 叠加网成型，令即时广播可达

    // A 申请域名 + 登记两个名字到同一公钥（一个公钥可多名字）。
    assert!(a.add_domain("shop.nm").unwrap(), "首次申请应新增");
    assert!(!a.add_domain("shop.nm").unwrap(), "重复申请应幂等(false)");
    assert!(a.owned_domains().contains(&"shop.nm".to_string()));
    let target = [7u8; 32];
    a.admin_set_name("shop.nm", "store", target).unwrap();
    a.admin_set_name("shop.nm", "shop", target).unwrap(); // 同公钥多名字
    assert_eq!(a.names_owned(Some("shop.nm")).len(), 2);

    // 客户端连 B，跨节点解析。
    let (_bc, bob) = online([98u8; 32], &b_addr).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();
    let store_name = format!("{}@{}", "store", "shop.nm");
    let shop_name = format!("{}@{}", "shop", "shop.nm");
    let mut ok = false;
    for _ in 0..40 {
        if let Ok(Some(r)) = bob.name_resolve(&store_name).await {
            assert_eq!(r.client_pubkey, target.to_vec());
            ok = true; break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    assert!(ok, "B 应能解析 store@shop.nm");

    // 删除（墓碑）→ B 侧最终解析为空。
    a.admin_del_name("shop.nm", "store").unwrap();
    let mut gone = false;
    for _ in 0..40 {
        if bob.name_resolve(&store_name).await.unwrap().is_none() { gone = true; break; }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    assert!(gone, "删除后 B 侧应解析为空（墓碑扩散）");
    // 另一个名字仍在。
    assert!(bob.name_resolve(&shop_name).await.unwrap().is_some(), "未删的名字应仍可解析");
}

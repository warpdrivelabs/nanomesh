//! 真网命名联调（#[ignore]）：客户端连「本机节点(cmx.nm)」认领名字 → 经联邦 gossip 复制 →
//! 另一客户端连 cmxdev18 解析该名 → 得认领者公钥。证明 N1 命名在真实联邦上跨节点可用。
//!   NM_LOCAL_NODE=<本机节点id> NM_SEED_NODE=<cmxdev18 id> \
//!   cargo test -p nm-node --test real_naming -- --ignored --nocapture

use std::collections::HashMap;
use std::time::Duration;
use nm_entity::kinds;
use nm_proto::pb::PersonProfile;

fn id(k: &str, dflt: &str) -> [u8; 32] {
    let h = std::env::var(k).unwrap_or_else(|_| dflt.into());
    let mut o = [0u8; 32];
    for i in 0..32 { o[i] = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap(); }
    o
}

#[tokio::test]
#[ignore]
async fn real_name_claim_and_cross_resolve() {
    let local = id("NM_LOCAL_NODE", "a78000cc1e1a6795441970d3915bca96798ef62fdb954f85d1952a9efb78aaa8");
    let seed = id("NM_SEED_NODE", "018640c437531635a0aa3e33cb8af9c999139c77e182336d309d2229a2b37845");
    // 唯一 local-part，避免与历史认领冲突。
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let local_part = format!("verify{}", n % 100000);

    // 认领者连本机节点（域 cmx.nm）。
    let ac = nm_client::Client::bind([221u8; 32]).await.unwrap();
    let alice = tokio::time::timeout(Duration::from_secs(45), ac.online_by_id(local)).await
        .expect("连本机节点超时").expect("连本机节点失败");
    alice.register_as::<kinds::Person>(&PersonProfile::default(), "Alice", HashMap::new()).await.unwrap();
    let rec = alice.name_claim(&local_part).await.expect("认领失败（本机节点是否配了 domain？）");
    let full = format!("{}@{}", rec.local_part, rec.domain);
    eprintln!("claimed {} -> {}", full, hx(&alice.id_bytes()));
    assert_eq!(rec.domain, "cmx.nm");

    // 解析者连 cmxdev18，跨节点解析该名。
    let bc = nm_client::Client::bind([222u8; 32]).await.unwrap();
    let bob = tokio::time::timeout(Duration::from_secs(45), bc.online_by_id(seed)).await
        .expect("连 cmxdev18 超时").expect("连 cmxdev18 失败");
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();

    let mut got = None;
    for _ in 0..30 {
        if let Ok(Some(r)) = bob.name_resolve(&full).await { got = Some(r); break; }
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    match got {
        Some(r) if r.client_pubkey == alice.id_bytes().to_vec() =>
            eprintln!("✅ 跨节点解析成功：{} = {}", full, hx(&r.client_pubkey)),
        Some(r) => panic!("解析到错误公钥：{}", hx(&r.client_pubkey)),
        None => panic!("❌ cmxdev18 未能解析 {}（命名 gossip 复制失败）", full),
    }
}

fn hx(b: &[u8]) -> String { b.iter().map(|x| format!("{:02x}", x)).collect() }

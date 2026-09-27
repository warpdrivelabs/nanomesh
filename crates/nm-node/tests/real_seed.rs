//! 真网联调（默认 #[ignore]，需外网可达 n0 relay/pkarr + 目标种子在线）：
//! 两个 NAT 客户端连**同一个**种子，验证 group_add 扇出 与 channel_sub 广播 在真实
//! relay/NAT 链路上的「服务端→客户端推送」是否到达。
//!   运行：NMSPACE_SEED=d19e4931... cargo test -p nm-node --test real_seed -- --ignored --nocapture

use std::collections::HashMap;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;
use nm_proto::GramKind;


fn hx(b: &[u8]) -> String { b.iter().map(|x| format!("{:02x}", x)).collect() }

fn seed_id() -> [u8; 32] {
    let hex = std::env::var("NMSPACE_SEED")
        .unwrap_or_else(|_| "d19e49310cfac7a4b0cbad445a017e6911cb315f3929200a192b284a9ce84daa".into());
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}

async fn nat_online(seed: [u8; 32], id: [u8; 32]) -> (nm_client::Client, nm_client::Session) {
    let c = nm_client::Client::bind(seed).await.expect("bind nat");
    let s = tokio::time::timeout(Duration::from_secs(45), c.online_by_id(id))
        .await
        .expect("online_by_id 超时(真网)")
        .expect("online_by_id 失败");
    (c, s)
}

#[tokio::test]
#[ignore]
async fn real_seed_group_and_channel() {
    let sid = seed_id();
    eprintln!("connecting two clients to seed {}", hx(&sid));
    let (_ac, owner) = nat_online([201u8; 32], sid).await;
    owner.register_as::<kinds::Person>(&PersonProfile::default(), "Owner", HashMap::new()).await.unwrap();
    let (_bc, mut bob) = nat_online([202u8; 32], sid).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();
    eprintln!("both online; owner={} bob={}", hx(&owner.id_bytes()), hx(&bob.id_bytes()));

    // ---- 群：owner 建群 + add(bob) + 发消息 ----
    let gid = [210u8; 32];
    owner.group_create(gid, "真网群").await.unwrap();
    owner.group_add(gid, bob.id_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner.send_group(gid, "群消息-真网").await.unwrap();
    match tokio::time::timeout(Duration::from_secs(12), bob.recv()).await {
        Ok(Some(g)) => eprintln!("✅ GROUP recv kind={:?} body={:?}", g.kind(),
            String::from_utf8_lossy(&g.payload.unwrap().value)),
        _ => eprintln!("❌ GROUP: bob 未收到群消息(真网推送失败)"),
    }

    // ---- 频道：owner 建频道 + bob 订阅 + publish ----
    let cid = [211u8; 32];
    owner.channel_create(cid, "真网频道", "简介", "").await.unwrap();
    bob.channel_sub(cid, "真网频道").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner.channel_publish(cid, "频道消息-真网").await.unwrap();
    match tokio::time::timeout(Duration::from_secs(12), bob.recv()).await {
        Ok(Some(g)) => eprintln!("✅ CHANNEL recv kind={:?} body={:?}", g.kind(),
            String::from_utf8_lossy(&g.payload.unwrap().value)),
        _ => eprintln!("❌ CHANNEL: bob 未收到频道消息(真网推送失败)"),
    }
    let _ = GramKind::Message;
}

/// 跨种子：owner 连 SEED_A、bob 连 SEED_B（两台不同种子）。验证联邦是否把群/频道消息
/// 跨节点送达。运行：
///   NMSPACE_SEED_A=d19e4931... NMSPACE_SEED_B=018640c437... \
///   cargo test -p nm-node --test real_seed cross -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn real_cross_seed_group_and_channel() {
    let a = env_id("NMSPACE_SEED_A", "d19e49310cfac7a4b0cbad445a017e6911cb315f3929200a192b284a9ce84daa");
    let b = env_id("NMSPACE_SEED_B", "018640c437531635a0aa3e33cb8af9c999139c77e182336d309d2229a2b37845");
    eprintln!("owner→seedA {} ; bob→seedB {}", hx(&a), hx(&b));
    let (_ac, owner) = nat_online([203u8; 32], a).await;
    owner.register_as::<kinds::Person>(&PersonProfile::default(), "OwnerX", HashMap::new()).await.unwrap();
    let (_bc, mut bob) = nat_online([204u8; 32], b).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "BobX", HashMap::new()).await.unwrap();

    let mut gid = [220u8; 32];
    let mut cid = [221u8; 32];
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos().to_le_bytes();
    gid[..16].copy_from_slice(&n); // 每次运行用新 id，避免与上次残留冲突
    cid[..16].copy_from_slice(&n);
    owner.group_create(gid, "跨种子群").await.unwrap();
    owner.group_add(gid, bob.id_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    owner.send_group(gid, "跨种子群消息").await.unwrap();
    match tokio::time::timeout(Duration::from_secs(15), bob.recv()).await {
        Ok(Some(g)) => eprintln!("✅ X-GROUP recv body={:?}", String::from_utf8_lossy(&g.payload.unwrap().value)),
        _ => eprintln!("❌ X-GROUP: bob(异节点) 未收到群消息 → 跨种子联邦不通"),
    }

    let cid = cid; // 见上：已按时间戳生成唯一 id
    owner.channel_create(cid, "跨种子频道", "简介", "").await.unwrap();
    bob.channel_sub(cid, "跨种子频道").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    owner.channel_publish(cid, "跨种子频道消息").await.unwrap();
    match tokio::time::timeout(Duration::from_secs(15), bob.recv()).await {
        Ok(Some(g)) => eprintln!("✅ X-CHANNEL recv body={:?}", String::from_utf8_lossy(&g.payload.unwrap().value)),
        _ => eprintln!("❌ X-CHANNEL: bob(异节点) 未收到频道消息 → 跨种子 gossip 不通"),
    }
}

fn env_id(k: &str, dflt: &str) -> [u8; 32] {
    let hex = std::env::var(k).unwrap_or_else(|_| dflt.into());
    let mut out = [0u8; 32];
    for i in 0..32 { out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap(); }
    out
}

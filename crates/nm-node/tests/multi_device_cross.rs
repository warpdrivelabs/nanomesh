//! 多设备子密钥 × 跨节点私聊：两个账号各用设备密钥连到不同家节点，互发私信。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_crypto::{sign_device_cert, SecretKey, DEVICE_ROLE_ADMIN};
use nm_entity::kinds;
use nm_proto::pb::PersonProfile;
use nm_proto::GramKind;

async fn device_online(account_seed: [u8; 32], device_seed: [u8; 32], addr: &nm_transport::Addr) -> (nm_client::Client, nm_client::Session) {
    let ak = SecretKey::from_bytes(&account_seed);
    let dk = *SecretKey::from_bytes(&device_seed).public().as_bytes();
    let cert = sign_device_cert(&ak, dk, "dev", DEVICE_ROLE_ADMIN, 1);
    let c = nm_client::Client::bind_local(device_seed).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(20), c.online_as(addr.clone(), &cert))
        .await
        .expect("online_as timed out")
        .expect("online_as failed");
    (c, s)
}

fn env_id(key: &str, default: &str) -> [u8; 32] {
    let hex = std::env::var(key).unwrap_or_else(|_| default.into());
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}

/// 真网：A 连 SEED_A、B 连 SEED_B，分别以老客户端和设备密钥身份互发私信。
///   cargo test -p nm-node --test multi_device_cross real -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn real_cross_node_direct() {
    let sa = env_id("NMSPACE_SEED_A", "018640c437531635a0aa3e33cb8af9c999139c77e182336d309d2229a2b37845");
    let sb = env_id("NMSPACE_SEED_B", "d19e49310cfac7a4b0cbad445a017e6911cb315f3929200a192b284a9ce84daa");
    for (label, use_dev) in [("legacy", false), ("device", true)] {
        let (acc_a, acc_b) = if use_dev { ([231u8; 32], [233u8; 32]) } else { ([221u8; 32], [223u8; 32]) };
        let connect = |acc: [u8; 32], dev: [u8; 32], node: [u8; 32]| async move {
            if use_dev {
                let ak = SecretKey::from_bytes(&acc);
                let cert = sign_device_cert(&ak, *SecretKey::from_bytes(&dev).public().as_bytes(), "probe", DEVICE_ROLE_ADMIN, 1);
                let c = nm_client::Client::bind(dev).await.unwrap();
                let s = tokio::time::timeout(Duration::from_secs(45), c.online_as_by_id(node, &cert)).await.unwrap().unwrap();
                (c, s)
            } else {
                let c = nm_client::Client::bind(acc).await.unwrap();
                let s = tokio::time::timeout(Duration::from_secs(45), c.online_by_id(node)).await.unwrap().unwrap();
                (c, s)
            }
        };
        let (_ac, alice) = connect(acc_a, [acc_a[0] + 1; 32], sa).await;
        alice.register_as::<kinds::Person>(&PersonProfile::default(), "ProbeA", HashMap::new()).await.unwrap();
        let (_bc, mut bob) = connect(acc_b, [acc_b[0] + 1; 32], sb).await;
        bob.register_as::<kinds::Person>(&PersonProfile::default(), "ProbeB", HashMap::new()).await.unwrap();
        let bob_id = bob.id_bytes();
        let send = tokio::spawn(async move {
            for i in 0..20 {
                let r = alice.send_to(bob_id, &format!("probe-{i}")).await;
                if i == 0 { eprintln!("[{label}] first send result ok={}", r.is_ok()); }
                tokio::time::sleep(Duration::from_millis(800)).await;
            }
        });
        let got = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match bob.recv().await {
                    Some(g) if g.kind() == GramKind::Message => return Some(g),
                    Some(_) => continue,
                    None => return None,
                }
            }
        }).await;
        send.abort();
        match got {
            Ok(Some(g)) => eprintln!("✅ [{label}] bob 收到 {:?}", String::from_utf8_lossy(&g.payload.unwrap().value)),
            _ => eprintln!("❌ [{label}] bob 20 秒内未收到跨节点私信"),
        }
    }
}

#[tokio::test]
async fn cross_node_direct_between_device_sessions() {
    let a = Arc::new(nm_node::Node::bind_local([81u8; 32]).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local([82u8; 32]).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    let (aa, ba) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] {
        tokio::spawn(async move { let _ = n.serve().await; });
    }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());

    let (_ac, alice) = device_online([83u8; 32], [84u8; 32], &aa).await;
    alice.register_as::<kinds::Person>(&PersonProfile::default(), "Alice", HashMap::new()).await.unwrap();
    let (_bc, mut bob) = device_online([85u8; 32], [86u8; 32], &ba).await;
    bob.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();
    let bob_id = bob.id_bytes();
    let alice_id = alice.id_bytes();

    let recv = tokio::spawn(async move {
        loop {
            let g = tokio::time::timeout(Duration::from_secs(30), bob.recv()).await.ok()??;
            if g.kind() == GramKind::Message {
                return Some(g);
            }
        }
    });
    let send = tokio::spawn(async move {
        for _ in 0..40 {
            let _ = alice.send_to(bob_id, "设备密钥跨节点私信").await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    let got = recv.await.unwrap().expect("bob 未收到跨节点私信");
    send.abort();
    assert_eq!(got.sender, alice_id.to_vec());
    assert_eq!(String::from_utf8(got.payload.unwrap().value).unwrap(), "设备密钥跨节点私信");
}

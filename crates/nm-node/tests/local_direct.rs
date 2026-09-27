//! 真网：sender 连「本机 nmd」(其身份种子=NM_SENDER_NODE_SEED 的派生 id)，receiver 连某稳定种子
//! (NM_RECEIVER_NODE_ID)。验证本机 nmd 的私聊 gossip Direct 能跨节点送达稳定种子上的收件人。
//!   NM_SENDER_NODE_SEED=<本机nmd身份hex> NM_RECEIVER_NODE_ID=<种子node id hex> \
//!   cargo test -p nm-node --test local_direct -- --ignored --nocapture

use std::collections::HashMap;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;
use nm_proto::GramKind;

fn hexb(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 { out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap(); }
    out
}

#[tokio::test]
#[ignore]
async fn local_node_direct_to_seed() {
    let sender_node_seed = hexb(&std::env::var("NM_SENDER_NODE_SEED").expect("NM_SENDER_NODE_SEED"));
    let recv_node_id = hexb(&std::env::var("NM_RECEIVER_NODE_ID").expect("NM_RECEIVER_NODE_ID"));
    // 本机 nmd 的 node id = 该身份种子派生（与 nmd 同一派生）。
    let sender_node_id = nm_client::Client::bind_local(sender_node_seed).await.unwrap().id_bytes();
    eprintln!("sender→local-node {}", hx(&sender_node_id));

    let sc = nm_client::Client::bind([210u8; 32]).await.unwrap();
    let sender = tokio::time::timeout(Duration::from_secs(45), sc.online_by_id(sender_node_id))
        .await.expect("sender online 超时").expect("sender online 失败");
    sender.register_as::<kinds::Person>(&PersonProfile::default(), "Sender", HashMap::new()).await.unwrap();

    let rc = nm_client::Client::bind([211u8; 32]).await.unwrap();
    let mut recv = tokio::time::timeout(Duration::from_secs(45), rc.online_by_id(recv_node_id))
        .await.expect("receiver online 超时").expect("receiver online 失败");
    recv.register_as::<kinds::Person>(&PersonProfile::default(), "Recv", HashMap::new()).await.unwrap();
    eprintln!("both online; sender={} recv={}", hx(&sender.id_bytes()), hx(&recv.id_bytes()));

    // sender(本机节点) → recv(种子节点) 私聊，反复发直到 gossip 叠加网成型。
    let rid = recv.id_bytes();
    let got = tokio::spawn(async move { tokio::time::timeout(Duration::from_secs(30), recv.recv()).await });
    let snd = tokio::spawn(async move {
        for _ in 0..40 { let _ = sender.send_to(rid, "本机→种子 私信").await; tokio::time::sleep(Duration::from_millis(500)).await; }
    });
    match got.await.unwrap() {
        Ok(Some(g)) => { snd.abort(); assert_eq!(g.kind(), GramKind::Message); eprintln!("✅ DIRECT recv body={:?}", String::from_utf8_lossy(&g.payload.unwrap().value)); }
        _ => { snd.abort(); panic!("❌ 收件人未收到本机→种子私信"); }
    }
}

fn hx(b: &[u8]) -> String { b.iter().map(|x| format!("{:02x}", x)).collect() }

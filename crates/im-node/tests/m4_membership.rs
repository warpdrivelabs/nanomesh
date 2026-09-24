//! 节点级联邦成员自动发现：种子引导 + gossip 成员频道 → 传递式发现。
//! 拓扑：A 为枢纽/种子，B 只认识 A，C 只认识 A（C 不认识 B）。各自广播签名成员卡片后，
//! C 应经 gossip 传递「发现」B（B 的卡片经 A 泛洪到 C），并按验签 + 拨号提示纳入本地对等表。
//! 覆盖：`spawn_membership` 的广播/收播/验签/learn_peer，以及 O(1) 种子配置下的传递发现。

use std::sync::Arc;
use std::time::Duration;

use im_node::{MembershipCfg, Node, PeerInfo};

fn card(name: &str) -> PeerInfo {
    PeerInfo {
        name: name.to_string(),
        ..Default::default()
    }
}

fn mcfg(name: &str) -> MembershipCfg {
    MembershipCfg {
        federation: "test-fed".to_string(),
        announce_interval_secs: 5, // 下限；join 后先立即广播一次
        ttl_secs: 60,
        card: card(name),
    }
}

#[tokio::test]
async fn membership_transitive_discovery() {
    // 3 个「服务器」：各自独立身份，临时端口。
    let a = Arc::new(Node::bind_local([41u8; 32]).await.unwrap());
    let b = Arc::new(Node::bind_local([42u8; 32]).await.unwrap());
    let c = Arc::new(Node::bind_local([43u8; 32]).await.unwrap());

    let b_id = *b.id().as_bytes();
    let b_hex: String = b_id.iter().map(|x| format!("{x:02x}")).collect();
    let (a_addr, b_addr, c_addr) = (a.addr(), b.addr(), c.addr());

    // 种子（手工）：A 认识 B、C（枢纽）；B 认识 A；C 只认识 A（不认识 B）。
    // add_peer_addr 把地址喂进 endpoint 地址簿（Minimal 无发现，gossip 靠它按公钥拨号）。
    a.add_peer_addr(b_addr.clone());
    a.note_manual_peer(*b.id().as_bytes());
    a.add_peer_addr(c_addr.clone());
    a.note_manual_peer(*c.id().as_bytes());
    b.add_peer_addr(a_addr.clone());
    b.note_manual_peer(*a.id().as_bytes());
    c.add_peer_addr(a_addr.clone());
    c.note_manual_peer(*a.id().as_bytes());

    // 各自 serve（起 Router，开始接受单播 + gossip 连接）。
    for n in [a.clone(), b.clone(), c.clone()] {
        tokio::spawn(async move {
            let _ = n.serve().await;
        });
    }
    // 各自启动成员发现（同一 federation）。
    a.clone().spawn_membership(mcfg("A-hub"));
    b.clone().spawn_membership(mcfg("B-node"));
    c.clone().spawn_membership(mcfg("C-node"));

    // 轮询：C 应在超时内发现 B（B 不在 C 的种子里，只能经 A 传递学到）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let mut found: Option<PeerInfo> = None;
    while tokio::time::Instant::now() < deadline {
        if let Some((_, info)) = c
            .peers_detail()
            .into_iter()
            .find(|(id, _)| *id == b_hex)
        {
            found = Some(info);
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let info = found.expect("C 未在超时内经 gossip 传递发现 B");
    assert_eq!(info.source, "discovered", "B 在 C 处应标记为自动发现");
    assert_eq!(info.name, "B-node", "应经签名卡片传播 B 的名称");
    assert!(
        c.peers_list().iter().any(|id| *id == b_id),
        "C 的对等表应已纳入 B（可拨号）"
    );
}

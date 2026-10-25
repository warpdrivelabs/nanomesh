//! B 根治回归:跨节点路由 Command 转发(caller home=A,provider home=B)。
//!
//! 曾经:路由 Command 只投本节点在线会话,provider 会话在别的节点 → 丢/35s 超时(LLM 远端代理探测
//! 即此症)。根治:A 投不到且目录 home_node=B → GroupGossip::Routed 经 nmspace-inbox:<provider>+
//! 火管转发;B 投给本地 provider 会话;CommandResult(receiver=caller)在 B 同样投不到 → 反向转发回 A。
//!
//! 断言:① caller.call() 跨节点拿到 ok 结果;② provider 下线后 call 快速回「provider offline at home」
//! 权威错误(B 合成、经 Routed 回程),而非空等到客户端超时。
//! 运行:cargo test -p nm-node --test cross_command_e2e -- --ignored
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_core::Directory;
use nm_entity::kinds;
use nm_proto::pb::{Any, InferenceProfile, PersonProfile};

async fn online(seed: [u8; 32], addr: &nm_transport::Addr) -> (nm_client::Client, nm_client::Session) {
    let c = nm_client::Client::bind_local(seed).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(20), c.online(addr.clone()))
        .await
        .expect("online timed out")
        .expect("online failed");
    (c, s)
}

#[tokio::test]
#[ignore = "flaky: 跨节点 iroh Minimal 叠加网成型不稳(同 cross_dm_ack_e2e);--ignored 串行跑"]
async fn cross_node_command_roundtrip_and_offline_error() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("nm_node=debug")
        .with_test_writer()
        .try_init();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = Arc::new(nm_node::Node::bind_local_persistent([81u8; 32], dir_a.path().join("a.redb")).await.unwrap());
    let b = Arc::new(nm_node::Node::bind_local_persistent([82u8; 32], dir_b.path().join("b.redb")).await.unwrap());
    a.add_peer_addr(b.addr());
    b.add_peer_addr(a.addr());
    a.set_per_topic(true);
    b.set_per_topic(true);
    let (a_addr, b_addr) = (a.addr(), b.addr());
    for n in [a.clone(), b.clone()] {
        tokio::spawn(async move {
            let _ = n.serve().await;
        });
    }
    a.clone().spawn_group_sync("nmspace".into());
    b.clone().spawn_group_sync("nmspace".into());
    a.clone().spawn_federation_sync(Duration::from_secs(2));
    b.clone().spawn_federation_sync(Duration::from_secs(2));

    // provider 挂 B(home=B),echo 式应答。done 信号让测试能让 worker 优雅退出并显式 close(Logout)
    // ——abort 只 drop Session,B 侧 QUIC close_reason 未置位,半死连接会让 uni-push 假成功。
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
    let (_pc, provider) = online([83u8; 32], &b_addr).await;
    provider
        .register_as::<kinds::InferenceService>(
            &InferenceProfile { models: vec!["demo-llm".into()], ..Default::default() },
            "跨节点推理",
            HashMap::new(),
        )
        .await
        .unwrap();
    let provider_id = provider.id_bytes();
    let worker = tokio::spawn(async move {
        let mut provider = provider;
        loop {
            tokio::select! {
                _ = &mut done_rx => break,
                nc = provider.next_command() => {
                    let Some((req, cmd)) = nc else { break };
                    let (ok, result, error) = match cmd.method.as_str() {
                        "infer" => (
                            true,
                            Some(Any { type_url: "nmspace.v1/text".into(), value: b"pong".to_vec() }),
                            String::new(),
                        ),
                        _ => (false, None, "unsupported".to_string()),
                    };
                    let _ = provider.reply(&req, ok, result, &error).await;
                }
            }
        }
        provider.close().await; // 显式 Logout → B 立即清会话表
    });

    // caller 挂 A(home=A)。
    let (_cc, caller) = online([84u8; 32], &a_addr).await;
    caller
        .register_as::<kinds::Person>(&PersonProfile::default(), "Caller", HashMap::new())
        .await
        .unwrap();

    // 等 A 的目录学到 provider(home_node=B)——联邦同步 ~2s 周期。
    let learned = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if a.directory().get(&provider_id).await.ok().flatten().is_some() {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(learned, "A 应经联邦同步学到 provider 实体(home=B)");

    // ① 跨节点 RPC:A 的 caller call B 上的 provider——经 Routed 转发应拿到 ok 结果。
    // 叠加网成型存在窗口期,带重试(与 direct_gossip 的 send-until-received 同思路)。
    let mut got: Option<nm_proto::CommandResult> = None;
    for _ in 0..8 {
        match caller
            .call_with_grant_timeout(
                provider_id,
                "infer",
                Some(Any { type_url: "text/plain".into(), value: b"ping".to_vec() }),
                None,
                15_000,
            )
            .await
        {
            Ok(cr) if cr.ok => {
                got = Some(cr);
                break;
            }
            _ => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
    let cr = got.expect("跨节点 call 应成功(Routed 转发闭环)");
    assert_eq!(cr.result.unwrap().value, b"pong", "应拿到 provider 的应答");

    // ② provider 显式下线(Logout) → B 清会话表 → 快速权威错误(B 合成 offline 错经 Routed 回程)。
    let _ = done_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(10), worker).await;
    tokio::time::sleep(Duration::from_secs(2)).await; // 等 Logout 处理 + 会话表清理

    let t0 = std::time::Instant::now();
    let r = caller
        .call_with_grant_timeout(
            provider_id,
            "infer",
            Some(Any { type_url: "text/plain".into(), value: b"ping".to_vec() }),
            None,
            30_000,
        )
        .await;
    let waited = t0.elapsed();
    match r {
        Ok(cr) => assert!(!cr.ok, "下线后不应成功:{cr:?}"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("offline") || msg.contains("unreachable"),
                "应为权威 offline 错误,实际:{msg}"
            );
        }
    }
    assert!(
        waited < Duration::from_secs(20),
        "应快速失败(B 合成错误回程),而非空等到客户端超时:{waited:?}"
    );
}

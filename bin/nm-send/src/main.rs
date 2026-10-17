//! `nm-send` — 手动测试：连一个节点，给目标 entity_id 发一条私聊消息，然后等若干秒收回声/回包并打印。
//! 用法：nm-send --node '<NM_NODE_ADDR JSON>' --to <64hex> --text "hi" [--seed 202] [--wait 15]
//! 配合 nm-echo 做跨节点往返验证：echo 挂节点B，nm-send 连节点A 发给 echo.id → 收到回声 = A↔B 私聊通。

use std::time::Duration;

use clap::Parser;

#[derive(Parser)]
#[command(name = "nm-send", about = "一次性私聊发送 + 等回包（手动测试）")]
struct Args {
    /// 节点地址（nmd 打印的 NM_NODE_ADDR JSON）。
    #[arg(long)]
    node: String,
    /// 目标 entity_id（64 hex）。
    #[arg(long)]
    to: String,
    /// 消息内容。
    #[arg(long, default_value = "ping")]
    text: String,
    /// 本客户端身份种子。
    #[arg(long, default_value_t = 202)]
    seed: u8,
    /// 发送后等待回包的秒数。
    #[arg(long, default_value_t = 15)]
    wait: u64,
}

fn parse_id(s: &str) -> anyhow::Result<[u8; 32]> {
    let s = s.trim();
    anyhow::ensure!(s.len() == 64, "需要 64 hex，实际 {}", s.len());
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let target = parse_id(&args.to)?;

    let addr = nm_transport::addr_from_string(&args.node)?;
    let client = nm_client::Client::bind_local([args.seed; 32]).await?;
    let mut sess = client.online(addr).await?;
    println!("nm-send online: id={}  → 发往 {}", hex(&client.id_bytes()), &args.to[..16]);

    // 注册为 person（让对端/目录认得本发送方，presence 一致）。
    sess.register_as::<nm_entity::kinds::Person>(
        &nm_proto::pb::PersonProfile::default(),
        "发送测试",
        std::collections::HashMap::new(),
    )
    .await
    .ok();

    let mut inbox = sess.take_inbox().expect("inbox");

    match sess.send_to(target, &args.text).await {
        Ok(g) => println!("已发送 gram_id={} text={:?}", g.gram_id, args.text),
        Err(e) => {
            println!("发送失败: {e}");
            return Ok(());
        }
    }

    println!("等待回包 {}s …", args.wait);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.wait);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            println!("超时：{}s 内未收到任何回包", args.wait);
            break;
        }
        match tokio::time::timeout(left, inbox.recv()).await {
            Ok(Some(g)) => {
                let body = g
                    .payload
                    .as_ref()
                    .map(|p| String::from_utf8_lossy(&p.value).to_string())
                    .unwrap_or_default();
                println!("✅ 收到回包 from={} text={:?}", hex(&g.sender[..4.min(g.sender.len())]), body);
                break;
            }
            Ok(None) => {
                println!("inbox 关闭（连接断开）");
                break;
            }
            Err(_) => {
                println!("超时：{}s 内未收到任何回包", args.wait);
                break;
            }
        }
    }
    Ok(())
}

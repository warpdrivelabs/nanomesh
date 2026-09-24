//! `nm-echo` — 测试用第二方：注册为 person，把收到的每条消息原样回声给发送方。
//! 用法：`cargo run -p nm-echo -- --node '<nmd 打印的 JSON>'`

use std::collections::HashMap;

use clap::Parser;
use nm_entity::kinds;
use nm_proto::{pb::PersonProfile, GramKind};

#[derive(Parser)]
#[command(name = "nm-echo", about = "echo-bot second party for manual testing")]
struct Args {
    /// 节点地址（nmd 启动时打印的 JSON）。
    #[arg(long)]
    node: String,
    /// 身份种子（0-255），决定本机器人的公钥（固定=稳定 id）。
    #[arg(long, default_value_t = 150)]
    seed: u8,
    /// 展示名。
    #[arg(long, default_value = "回声机器人")]
    name: String,
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

    let addr = nm_transport::addr_from_string(&args.node)?;
    let client = nm_client::Client::bind_local([args.seed; 32]).await?;
    let mut sess = client.online(addr).await?;
    sess.register_as::<kinds::Person>(&PersonProfile::default(), &args.name, HashMap::new())
        .await?;
    println!("echo bot online:  name=\"{}\"  id={}", args.name, hex(&client.id_bytes()));
    println!("=> 在 App 里点“目录/刷新”，选中它发消息，它会回声给你。");

    while let Some(gram) = sess.recv().await {
        if gram.kind() != GramKind::Message {
            continue;
        }
        let text = gram
            .payload
            .as_ref()
            .map(|p| String::from_utf8_lossy(&p.value).to_string())
            .unwrap_or_default();
        let sender: [u8; 32] = match gram.sender.clone().try_into() {
            Ok(s) => s,
            Err(_) => continue,
        };
        println!("recv from {}…: {text}", hex(&sender[..3]));
        if let Err(e) = sess.send_to(sender, &format!("echo: {text}")).await {
            eprintln!("reply failed: {e}");
        }
    }
    Ok(())
}

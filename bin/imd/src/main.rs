//! `imd` — imspace 去中心节点守护进程。

use clap::Parser;

#[derive(Parser)]
#[command(name = "imd", version, about = "imspace decentralized IM node")]
struct Args {
    /// 配置文件路径
    #[arg(short, long, default_value = "imd.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    tracing::info!(config = %args.config, "starting imd");

    // M0/E1/E2：起本地节点(带 redb 持久化)，打印可分享地址，进入接受循环。
    let node = im_node::Node::bind_local_persistent([7u8; 32], "imd.redb").await?;
    println!("IM_NODE_ADDR={}", im_transport::addr_to_string(&node.addr()));
    tracing::info!(id = %node.id().fmt_short(), "node bound; serving (persistent)");
    node.serve().await?;
    Ok(())
}

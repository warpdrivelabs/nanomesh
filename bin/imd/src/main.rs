//! `imd` — imspace 去中心节点守护进程。
//!
//! 读取 `imd.toml`（可选）：持久身份、redb 路径、固定端口、对等节点列表。
//! 有对等节点时启动后台联邦同步循环，实现同网多服务器间客户端互通。
//!
//! ```toml
//! # imd.toml
//! identity  = "imd.identity"   # 32 字节私钥文件；不存在则自动生成（每台一份，勿共用）
//! db        = "imd.redb"
//! bind_port = 9600             # 固定 UDP 端口 → 地址稳定，便于对等配置（0=临时端口）
//! [[peers]]
//! addr = '{"id":"...","addrs":[{"Ip":"192.168.1.50:9600"}]}'  # 对方 imd 打印的 IM_NODE_ADDR
//! ```

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use serde::Deserialize;

#[derive(Parser)]
#[command(name = "imd", version, about = "imspace decentralized IM node")]
struct Args {
    /// 配置文件路径
    #[arg(short, long, default_value = "imd.toml")]
    config: String,
}

#[derive(Deserialize)]
struct Config {
    #[serde(default = "default_identity")]
    identity: String,
    #[serde(default = "default_db")]
    db: String,
    #[serde(default)]
    bind_port: u16,
    #[serde(default)]
    peers: Vec<PeerCfg>,
}

#[derive(Deserialize)]
struct PeerCfg {
    /// 对方节点地址（IM_NODE_ADDR 的 JSON）。
    addr: String,
}

fn default_identity() -> String {
    "imd.identity".to_string()
}
fn default_db() -> String {
    "imd.redb".to_string()
}

/// 加载持久身份；不存在则随机生成并写盘（每台服务器一份，保证 node id 唯一）。
fn load_or_create_identity(path: &str) -> anyhow::Result<[u8; 32]> {
    if let Ok(bytes) = std::fs::read(path) {
        if bytes.len() == 32 {
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&bytes);
            tracing::info!(%path, "loaded node identity");
            return Ok(seed);
        }
        tracing::warn!(%path, "identity file has wrong size, regenerating");
    }
    let seed = im_transport::SecretKey::generate().to_bytes();
    std::fs::write(path, seed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!(%path, "generated new node identity");
    Ok(seed)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "imd=info,im_node=info".into()),
        )
        .init();
    let args = Args::parse();

    // 空/缺失配置 → 全部走默认值。
    let raw = std::fs::read_to_string(&args.config).unwrap_or_default();
    let cfg: Config = toml::from_str(&raw)?;

    let seed = load_or_create_identity(&cfg.identity)?;
    let node = Arc::new(
        im_node::Node::bind_local_persistent_on(seed, &cfg.db, cfg.bind_port).await?,
    );

    // 配置对等节点。
    for p in &cfg.peers {
        match im_transport::addr_from_string(&p.addr) {
            Ok(addr) => node.add_peer_addr(addr),
            Err(e) => tracing::warn!("skip bad peer addr: {e}"),
        }
    }
    if node.peer_count() > 0 {
        node.clone().spawn_federation_sync(Duration::from_secs(15));
        tracing::info!(peers = node.peer_count(), "federation sync started");
    }

    println!("IM_NODE_ADDR={}", im_transport::addr_to_string(&node.addr()));
    tracing::info!(id = %node.id().fmt_short(), bind_port = cfg.bind_port, "node serving");
    node.serve().await?;
    Ok(())
}

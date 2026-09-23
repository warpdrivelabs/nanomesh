//! `imd` — imspace 去中心节点守护进程。
//!
//! 三种连通模式（`imd.toml` 的 `mode`）：
//! - `nat`（默认）：`N0` 预设——**n0 公共中继 + QUIC 打洞 + DNS/pkarr 按公钥发现**，可**穿透 NAT**
//!   （跨公网/不同内网）；同网环境同样可用（会直连）。对等按 **node id(公钥)** 配置。
//! - `selfhost`：`Minimal` 基座 + **自建 iroh-relay + 自建 iroh-dns-server(pkarr)**——同样可穿透
//!   NAT，但中继/发现完全自主可控、不依赖 n0 公共设施。对等按 **node id(公钥)** 配置。
//! - `lan`：`Minimal` 预设——仅直连、免外网依赖；适合能互相直连的同一网络。对等按 **地址** 配置。
//!
//! ```toml
//! # imd.toml
//! mode      = "nat"          # "nat"(n0公共设施) | "selfhost"(自建设施) | "lan"(仅同网)
//! identity  = "imd.identity" # 32 字节私钥；不存在则自动生成（每台一份，勿共用/入库）
//! db        = "imd.redb"
//! bind_port = 0              # lan/selfhost 建议固定端口(如 9600)；nat 一般 0 即可
//!
//! [[peers]]
//! id   = "<对方 node id (公钥 hex, 64 位)>"   # nat/selfhost：按公钥，发现自动解析地址
//! # addr = '<对方 IM_NODE_ADDR JSON>'          # lan：按地址
//!
//! # 仅 mode = "selfhost" 时使用：自建中继 + 自建 dns(pkarr)
//! [relay]
//! url  = "https://relay.example.com"          # 自建 iroh-relay；也可用 urls = [..] 配多个
//! [dns]
//! url    = "https://dns.example.com/pkarr"     # 自建 iroh-dns-server 的 pkarr 端点(必填)
//! # origin = "dns.example.com."                # 可选：额外走 DNS 查询解析的源域
//!
//! # 黑名单（可选，任意 mode 通用）：名单内公钥一律拒绝连接；无白名单，其余默认放行
//! blacklist = ["<被禁公钥 hex, 64 位>"]
//! ```

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use serde::Deserialize;

#[derive(Parser)]
#[command(name = "imd", version, about = "imspace decentralized IM node")]
struct Args {
    #[arg(short, long, default_value = "imd.toml")]
    config: String,
}

#[derive(Deserialize)]
struct Config {
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default = "default_identity")]
    identity: String,
    #[serde(default = "default_db")]
    db: String,
    #[serde(default)]
    bind_port: u16,
    #[serde(default)]
    peers: Vec<PeerCfg>,
    /// 自建中继（仅 selfhost）。
    #[serde(default)]
    relay: RelayCfg,
    /// 自建 dns/pkarr 发现（仅 selfhost）。
    #[serde(default)]
    dns: DnsCfg,
    /// 黑名单：被禁公钥(hex, 64 位)。无白名单——默认放行，仅拒绝名单内公钥。
    /// 配置的封禁会持久化（重启仍生效）；运行时亦可经 API 增删。
    #[serde(default)]
    blacklist: Vec<String>,
}

#[derive(Deserialize)]
struct PeerCfg {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    addr: Option<String>,
}

/// 自建中继配置：`url`（单个）与 `urls`（多个）合并使用。
#[derive(Deserialize, Default)]
struct RelayCfg {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    urls: Vec<String>,
}

impl RelayCfg {
    /// 合并 `url` + `urls`，去重保序。
    fn all(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for u in self.url.iter().cloned().chain(self.urls.iter().cloned()) {
            let u = u.trim().to_string();
            if !u.is_empty() && !out.contains(&u) {
                out.push(u);
            }
        }
        out
    }
}

/// 自建 dns/pkarr 配置。
#[derive(Deserialize, Default)]
struct DnsCfg {
    /// 自建 iroh-dns-server 的 pkarr 端点（发布+解析），如 `https://dns.example.com/pkarr`。
    #[serde(default)]
    url: Option<String>,
    /// 可选：额外走 DNS 查询解析的源域，如 `dns.example.com.`。
    #[serde(default)]
    origin: Option<String>,
}

/// 连通模式。
enum Mode {
    /// n0 公共基础设施（N0 预设）。
    Nat,
    /// 自建 relay + 自建 dns/pkarr。
    SelfHost,
    /// 仅同网直连（Minimal）。
    Lan,
}

fn default_mode() -> String {
    "nat".to_string()
}
fn default_identity() -> String {
    "imd.identity".to_string()
}
fn default_db() -> String {
    "imd.redb".to_string()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn parse_id_hex(s: &str) -> anyhow::Result<[u8; 32]> {
    let s = s.trim();
    anyhow::ensure!(s.len() == 64, "node id 需 64 位十六进制");
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

/// 加载持久身份；不存在则随机生成并写盘（每台一份，保证 node id 唯一）。
fn load_or_create_identity(path: &str) -> anyhow::Result<[u8; 32]> {
    if let Ok(bytes) = std::fs::read(path) {
        if bytes.len() == 32 {
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&bytes);
            tracing::info!(%path, "loaded node identity");
            return Ok(seed);
        }
        tracing::warn!(%path, "identity file wrong size, regenerating");
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

    let raw = std::fs::read_to_string(&args.config).unwrap_or_default();
    let cfg: Config = toml::from_str(&raw)?;
    let mode = match cfg.mode.to_ascii_lowercase().as_str() {
        "nat" | "n0" | "internet" => Mode::Nat,
        "selfhost" | "self" | "custom" | "private" => Mode::SelfHost,
        "lan" | "local" | "direct" => Mode::Lan,
        other => {
            tracing::warn!("未知 mode={other:?}，回退 nat(N0)");
            Mode::Nat
        }
    };

    let seed = load_or_create_identity(&cfg.identity)?;
    let (node, label) = match mode {
        Mode::Nat => {
            // 穿透 NAT + 同网通用（N0：n0 公共中继 + 打洞 + 发现）。
            let n = im_node::Node::bind_persistent(seed, &cfg.db).await?;
            (n, "nat(N0/n0公共设施)")
        }
        Mode::SelfHost => {
            // 自建中继 + 自建 dns/pkarr：自主可控地穿透 NAT。
            let pkarr_url = cfg.dns.url.clone().ok_or_else(|| {
                anyhow::anyhow!("mode=selfhost 需在 [dns] url 配置自建 iroh-dns-server 的 pkarr 端点，如 https://dns.example.com/pkarr")
            })?;
            let relays = cfg.relay.all();
            if relays.is_empty() {
                tracing::warn!("mode=selfhost 未配置 [relay] url——无中继回退，仅当对端可直连(pkarr 发布的地址)时可达，NAT 打洞将不可用");
            }
            let origin = cfg.dns.origin.clone();
            tracing::info!(relays = ?relays, pkarr = %pkarr_url, dns_origin = ?origin, "selfhost 基础设施");
            let n = im_node::Node::bind_persistent_selfhosted(
                seed,
                &cfg.db,
                relays,
                pkarr_url,
                origin,
                cfg.bind_port,
            )
            .await?;
            (n, "selfhost(自建relay+dns)")
        }
        Mode::Lan => {
            // 仅同网直连（Minimal），固定端口便于对等配置。
            let n = im_node::Node::bind_local_persistent_on(seed, &cfg.db, cfg.bind_port).await?;
            (n, "lan(Minimal)")
        }
    };
    let node = Arc::new(node);
    // nat / selfhost 都靠发现服务上线后被按公钥拨号；lan 仅直连。
    let needs_online = matches!(mode, Mode::Nat | Mode::SelfHost);
    tracing::info!(mode = %label, "bound");

    // 配置对等：nat/selfhost 按 id(公钥)，lan 按 addr（每条 peer 按其字段自动选择）。
    for p in &cfg.peers {
        if let Some(id_hex) = &p.id {
            match parse_id_hex(id_hex) {
                Ok(id) => {
                    if let Err(e) = node.add_peer_by_id(id) {
                        tracing::warn!("add peer by id failed: {e}");
                    }
                }
                Err(e) => tracing::warn!("bad peer id: {e}"),
            }
        } else if let Some(addr) = &p.addr {
            match im_transport::addr_from_string(addr) {
                Ok(a) => node.add_peer_addr(a),
                Err(e) => tracing::warn!("bad peer addr: {e}"),
            }
        }
    }

    // 黑名单：封禁配置的公钥（持久化；运行时亦可增删）。无白名单——其余一律放行。
    for hexk in &cfg.blacklist {
        match parse_id_hex(hexk) {
            Ok(k) => node.ban(k),
            Err(e) => tracing::warn!("bad blacklist key: {e}"),
        }
    }
    if node.banned_count() > 0 {
        tracing::info!(banned = node.banned_count(), "blacklist active");
    }

    // nat/selfhost：等待上线（连上中继、发布地址）以便被按公钥发现；限时以免网络不通时卡死。
    if needs_online {
        match tokio::time::timeout(Duration::from_secs(10), node.online()).await {
            Ok(()) => tracing::info!("online — 可被按 node id 发现/拨号"),
            Err(_) => tracing::warn!("online 超时（中继/网络不可达？）——仍继续；直连/同网仍可用"),
        }
    }

    if node.peer_count() > 0 {
        node.clone().spawn_federation_sync(Duration::from_secs(15));
        tracing::info!(peers = node.peer_count(), "federation sync started");
    }

    // 供他方配置：nat/selfhost 对端用 IM_NODE_ID(公钥)；lan 对端用 IM_NODE_ADDR。
    println!("IM_NODE_ID={}", hex(node.id().as_bytes()));
    println!("IM_NODE_ADDR={}", im_transport::addr_to_string(&node.addr()));
    tracing::info!(id = %node.id().fmt_short(), "node serving");
    node.serve().await?;
    Ok(())
}

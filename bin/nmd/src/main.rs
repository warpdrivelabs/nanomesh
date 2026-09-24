//! `nmd` — nmspace 去中心节点守护进程。
//!
//! 三种连通模式（`nmd.toml` 的 `mode`）：
//! - `nat`（默认）：`N0` 预设——**n0 公共中继 + QUIC 打洞 + DNS/pkarr 按公钥发现**，可**穿透 NAT**
//!   （跨公网/不同内网）；**同网同时可用（优先直连）**。`bind_port>0` 时固定 UDP 端口，便于同网直连 +
//!   防火墙放行；对等按 **node id(公钥)** 配置。一个实例即服务同网、也服务跨 NAT。
//! - `selfhost`：`Minimal` 基座 + **自建 iroh-relay + 自建 iroh-dns-server(pkarr)**——同样可穿透
//!   NAT，但中继/发现完全自主可控、不依赖 n0 公共设施。对等按 **node id(公钥)** 配置。
//! - `lan`：`Minimal` 预设——仅直连、免外网依赖；适合能互相直连的同一网络。对等按 **地址** 配置。
//!
//! ```toml
//! # nmd.toml
//! mode      = "nat"          # "nat"(n0公共设施) | "selfhost"(自建设施) | "lan"(仅同网)
//! identity  = "nmd.identity" # 32 字节私钥；不存在则自动生成（每台一份，勿共用/入库）
//! db        = "nmd.redb"
//! bind_port = 0              # nat/lan/selfhost 均可固定端口(如 9600)：同网直连 + 防火墙放行；0=临时端口
//!
//! [[peers]]
//! id   = "<对方 node id (公钥 hex, 64 位)>"   # nat/selfhost：按公钥，发现自动解析地址
//! # addr = '<对方 NM_NODE_ADDR JSON>'          # lan：按地址
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

mod admin;

/// nmd 启动字符画（ANSI Shadow 风格，与 cmx 同族字体）：NANO MESH。
const NANO_MESH_ART: &str = r#"
███╗   ██╗ █████╗ ███╗   ██╗ ██████╗     ███╗   ███╗███████╗███████╗██╗  ██╗
████╗  ██║██╔══██╗████╗  ██║██╔═══██╗    ████╗ ████║██╔════╝██╔════╝██║  ██║
██╔██╗ ██║███████║██╔██╗ ██║██║   ██║    ██╔████╔██║█████╗  ███████╗███████║
██║╚██╗██║██╔══██║██║╚██╗██║██║   ██║    ██║╚██╔╝██║██╔══╝  ╚════██║██╔══██║
██║ ╚████║██║  ██║██║ ╚████║╚██████╔╝    ██║ ╚═╝ ██║███████╗███████║██║  ██║
╚═╝  ╚═══╝╚═╝  ╚═╝╚═╝  ╚═══╝ ╚═════╝     ╚═╝     ╚═╝╚══════╝╚══════╝╚═╝  ╚═╝
"#;

#[derive(Parser)]
#[command(name = "nmd", version, about = "nmspace decentralized mesh node")]
struct Args {
    #[arg(short, long, default_value = "nmd.toml")]
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
    /// 后端管理控制 API（供 nm-admind 取数/下发控制）。缺省不启用。
    #[serde(default)]
    admin: AdminCfg,
    /// 联邦成员自动发现（gossip 成员频道）。缺省启用；[[peers]] 降级为「种子」。
    #[serde(default)]
    membership: MembershipCfg,
}

/// 联邦成员自动发现配置。每台只需配少量种子([[peers]])，其余成员经 gossip 自动发现自维护。
#[derive(Deserialize)]
struct MembershipCfg {
    /// 是否启用成员发现。
    #[serde(default = "default_true")]
    enabled: bool,
    /// 联邦名：同名者组成同一叠加网；改名即隔离出独立联邦。
    #[serde(default = "default_federation")]
    federation: String,
    /// 自身卡片广播间隔（秒）。
    #[serde(default = "default_announce_iv")]
    announce_interval_secs: u64,
    /// 发现节点存活 TTL（秒）：超时未再广播即剔除。
    #[serde(default = "default_ttl")]
    ttl_secs: u64,
    /// 本节点对外广播的成员卡片（名称/物理地址/email/mobile/gps），用于其他节点展示。
    #[serde(default)]
    name: String,
    #[serde(default)]
    address: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    mobile: String,
    #[serde(default)]
    gps: String,
}

impl Default for MembershipCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            federation: default_federation(),
            announce_interval_secs: default_announce_iv(),
            ttl_secs: default_ttl(),
            name: String::new(),
            address: String::new(),
            email: String::new(),
            mobile: String::new(),
            gps: String::new(),
        }
    }
}

/// 后端管理控制 API 配置。绑定本机地址 + 共享 token（同机 nm-admind 调用）。
#[derive(Deserialize, Default)]
struct AdminCfg {
    /// 监听地址，如 `127.0.0.1:9611`；为空则不启用。
    #[serde(default)]
    api_addr: Option<String>,
    /// 访问令牌（`X-Admin-Token` 头）。为空则不启用。
    #[serde(default)]
    api_token: Option<String>,
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
    "nmd.identity".to_string()
}
fn default_db() -> String {
    "nmd.redb".to_string()
}
fn default_true() -> bool {
    true
}
fn default_federation() -> String {
    "nmspace".to_string()
}
fn default_announce_iv() -> u64 {
    30
}
fn default_ttl() -> u64 {
    90
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
    let seed = nm_transport::SecretKey::generate().to_bytes();
    std::fs::write(path, seed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!(%path, "generated new node identity");
    Ok(seed)
}

/// 连通模式的展示名（供 banner 信息面板与「bound」日志复用）。
fn mode_label(mode: &Mode) -> &'static str {
    match mode {
        Mode::Nat => "nat(N0/同网直连+穿透NAT)",
        Mode::SelfHost => "selfhost(自建relay+dns)",
        Mode::Lan => "lan(Minimal)",
    }
}

/// banner 之后：打印 Warp Drive Labs 版本 + 连接/端口信息面板。
fn print_startup_info(cfg: &Config, mode_label: &str) {
    let port = if cfg.bind_port == 0 {
        "临时端口（bind_port=0，可在 nmd.toml 固定）".to_string()
    } else {
        format!("{} · UDP（固定）", cfg.bind_port)
    };
    let admin = match cfg.admin.api_addr.as_deref() {
        Some(a) if !a.trim().is_empty() => a.to_string(),
        _ => "未启用（nmd.toml [admin]）".to_string(),
    };
    println!("  Warp Drive Labs · nmd v{}", env!("CARGO_PKG_VERSION"));
    println!("  联邦      {}", cfg.membership.federation);
    println!("  连接模式  {mode_label}");
    println!("  监听端口  {port}");
    println!("  管理 API  {admin}");
    println!();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nmd=info,nm_node=info".into()),
        )
        .init();
    let args = Args::parse();

    // 启动 banner：NANO MESH（终端渐变；nohup/管道下自动降级纯文本）。放在 arg 解析之后，
    // 使 `--help` / `--version` 保持干净。
    nm_core::banner::print(
        &nm_core::banner::BannerSpec::new(NANO_MESH_ART)
            .tagline("  NANO MESH · 去中心化网格节点 (nmd) ")
            .stops(vec![(16, 185, 129), (34, 211, 238), (99, 102, 241)]),
    );

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
    let label = mode_label(&mode);

    // banner 之后：Warp Drive Labs 版本 + 连接/端口信息。
    print_startup_info(&cfg, label);

    let seed = load_or_create_identity(&cfg.identity)?;
    let node = match mode {
        Mode::Nat => {
            // 穿透 NAT + 同网通用（N0：n0 公共中继 + 打洞 + 发现）。
            // bind_port>0：固定 UDP 端口——同网可按固定端口直连(配合防火墙放行)，同时保留穿透 NAT。
            let n = if cfg.bind_port != 0 {
                nm_node::Node::bind_persistent_on(seed, &cfg.db, cfg.bind_port).await?
            } else {
                nm_node::Node::bind_persistent(seed, &cfg.db).await?
            };
            n
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
            let n = nm_node::Node::bind_persistent_selfhosted(
                seed,
                &cfg.db,
                relays,
                pkarr_url,
                origin,
                cfg.bind_port,
            )
            .await?;
            n
        }
        Mode::Lan => {
            // 仅同网直连（Minimal），固定端口便于对等配置。
            let n = nm_node::Node::bind_local_persistent_on(seed, &cfg.db, cfg.bind_port).await?;
            n
        }
    };
    let node = Arc::new(node);
    // nat / selfhost 都靠发现服务上线后被按公钥拨号；lan 仅直连。
    let needs_online = matches!(mode, Mode::Nat | Mode::SelfHost);
    tracing::info!(mode = %label, "bound");

    // 配置对等（种子）：nat/selfhost 按 id(公钥)，lan 按 addr。标注为「手工」+ 所属联邦（免被自动发现降级/清扫）。
    let fed = cfg.membership.federation.clone();
    for p in &cfg.peers {
        if let Some(id_hex) = &p.id {
            match parse_id_hex(id_hex) {
                Ok(id) => {
                    if let Err(e) = node.add_peer_by_id(id) {
                        tracing::warn!("add peer by id failed: {e}");
                    } else {
                        node.note_manual_peer(id, &fed);
                    }
                }
                Err(e) => tracing::warn!("bad peer id: {e}"),
            }
        } else if let Some(addr) = &p.addr {
            match nm_transport::addr_from_string(addr) {
                Ok(a) => {
                    let id = *a.id.as_bytes();
                    node.add_peer_addr(a);
                    node.note_manual_peer(id, &fed);
                }
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

    // 始终启动联邦同步：即使当前无对等，也便于运行时经管理台动态加对等后立即生效（无需重启）。
    node.clone().spawn_federation_sync(Duration::from_secs(15));
    tracing::info!(peers = node.peer_count(), "federation sync started");

    // 联邦成员自动发现（gossip 成员频道）：每台只配少量种子([[peers]])，其余成员自动发现自维护。
    if cfg.membership.enabled {
        let m = &cfg.membership;
        let card = nm_node::PeerInfo {
            name: m.name.clone(),
            address: m.address.clone(),
            email: m.email.clone(),
            mobile: m.mobile.clone(),
            gps: m.gps.clone(),
            ..Default::default()
        };
        node.clone().spawn_membership(nm_node::MembershipCfg {
            federation: m.federation.clone(),
            announce_interval_secs: m.announce_interval_secs,
            ttl_secs: m.ttl_secs,
            card,
        });
        tracing::info!(
            federation = %cfg.membership.federation,
            "membership auto-discovery started"
        );
    }

    // 后端管理控制 API（若配置了 [admin] api_addr + api_token）：供 nm-admind 取数/下发控制。
    if let (Some(api_addr), Some(api_token)) =
        (cfg.admin.api_addr.clone(), cfg.admin.api_token.clone())
    {
        if !api_addr.trim().is_empty() && !api_token.trim().is_empty() {
            let node_admin = node.clone();
            let db_path = cfg.db.clone();
            let admin_fed = cfg.membership.federation.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    admin::serve(node_admin, api_addr, api_token, db_path, admin_fed).await
                {
                    tracing::warn!("admin api exited: {e}");
                }
            });
            tracing::info!("admin control API enabled");
        }
    }

    // 供他方配置：nat/selfhost 对端用 NM_NODE_ID(公钥)；lan 对端用 NM_NODE_ADDR。
    println!("NM_NODE_ID={}", hex(node.id().as_bytes()));
    println!("NM_NODE_ADDR={}", nm_transport::addr_to_string(&node.addr()));
    tracing::info!(id = %node.id().fmt_short(), "node serving");
    node.serve().await?;
    Ok(())
}

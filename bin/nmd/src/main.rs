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
    /// P0-2 离线恢复：用指定备份文件覆盖配置里的 db 后退出（须在 nmd 停止时执行）。
    #[arg(long)]
    restore: Option<String>,
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
    /// 本节点自声明域名（去中心命名 N1）：如 jeff.nm。空=不签发命名（仍可解析他人）。
    #[serde(default)]
    domain: String,
    /// 规模化联邦（F1–F5）：群/私聊/命名是否额外走「每主题」。**缺省开**（见 FederationCfg）；
    /// 显式 `[federation] per_topic = false` 回退仅火管。
    #[serde(default)]
    federation: FederationCfg,
    /// 优雅关闭：收到 SIGTERM/SIGINT 后等待在途投递发完的最长时间（秒）。
    #[serde(default)]
    shutdown: ShutdownCfg,
    /// 连接限流（DoS 防护）：全局并发连接上限。
    #[serde(default)]
    limits: LimitsCfg,
}

/// 连接限流配置。
#[derive(Deserialize)]
struct LimitsCfg {
    /// 全局并发连接上限：超过则拒绝新连接（防 FD/内存耗尽）。0=不限。默认 10000。
    #[serde(default = "default_max_conns")]
    max_connections: usize,
}

impl Default for LimitsCfg {
    fn default() -> Self {
        Self { max_connections: default_max_conns() }
    }
}

fn default_max_conns() -> usize {
    10_000
}

/// 优雅关闭配置。
#[derive(Deserialize)]
struct ShutdownCfg {
    /// drain 窗口上限（秒）：等在途单播投递清零的最长时间，到点仍未清零则继续关闭。默认 5。
    #[serde(default = "default_drain_secs")]
    drain_timeout_secs: u64,
}

impl Default for ShutdownCfg {
    fn default() -> Self {
        Self { drain_timeout_secs: default_drain_secs() }
    }
}

fn default_drain_secs() -> u64 {
    5
}

/// 规模化联邦（F1–F5）配置。
#[derive(Deserialize)]
struct FederationCfg {
    /// 群/私聊/命名是否额外走「每主题」（nmspace-group/inbox/names）：双写兜底、收端去重、
    /// 对端在主题上存活后退火管。**缺省 true=默认启用**；显式 `per_topic = false` 可回退仅火管。
    #[serde(default = "default_true")]
    per_topic: bool,
    /// P2-11：presence 退火管——开则本节点退出 presence overlay（不 join/发/收），presence 全走按 home 拉取。
    /// **缺省 false**（迁移安全：先全网升级、确认各 home 可被拉取后，再在全自有集群统一开启）。
    #[serde(default)]
    presence_scoped: bool,
}
impl Default for FederationCfg {
    // `[federation]` 整段缺省时也默认启用（保证 default 行为一致，不只是段内缺字段）。
    fn default() -> Self {
        Self { per_topic: true, presence_scoped: false }
    }
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
    /// F6/M1：成员发现 relay 索引灰度开关。开 = 在 gossip 成员频道之上叠加 relay bootstrap 索引
    /// （需 `[dns] url`）；关（默认）= 纯 gossip 成员频道，零行为变化。
    #[serde(default)]
    relay_index: bool,
    /// F6/M1：本节点是否为锚点。锚点周期把本联邦 bootstrap 成员集合 PUT 到 relay（key=本节点公钥）。
    #[serde(default)]
    anchor: bool,
    /// F6/M1：要解析的锚点公钥（64-hex）列表。新节点据此 GET 锚点索引做零配置冷启动 bootstrap。
    #[serde(default)]
    anchors: Vec<String>,
    /// P2-13 成员准入(anti-Sybil)：开=只学习出示「本地信任锚点签发的有效 grant」的成员广播。缺省关（行为中性）。
    #[serde(default)]
    admission: bool,
    /// 信任的准入签发锚点公钥(64-hex)列表。
    #[serde(default)]
    admission_anchors: Vec<String>,
    /// 本节点对外出示的准入凭证(hex of AdmissionGrant)；空=不出示（锚点自身/未受控节点留空）。
    #[serde(default)]
    admission_grant: String,
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
            relay_index: false,
            anchor: false,
            anchors: Vec::new(),
            admission: false,
            admission_anchors: Vec::new(),
            admission_grant: String::new(),
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
    /// P2-12 Phase A：额外的 pkarr 中继列表（弱锚多副本，退单点）。与 `url` 合并去重。
    #[serde(default)]
    urls: Vec<String>,
    /// 可选：额外走 DNS 查询解析的源域，如 `dns.example.com.`。
    #[serde(default)]
    origin: Option<String>,
}

impl DnsCfg {
    /// 合并 `url` + `urls`，去空去重，保序。
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
    /// 逗号拼接的多中继串（喂 set_pkarr 的既有逗号协议，也透传给 selfhost 传输层；空=None）。
    fn joined(&self) -> Option<String> {
        let all = self.all();
        if all.is_empty() { None } else { Some(all.join(",")) }
    }
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

/// 入口：手动构建多线程运行时，worker 线程数可经环境变量 `NMD_WORKER_THREADS` 调优
/// （不设则用 tokio 默认 = CPU 核数）。海量连接/高并发时可据负载显式调大。
fn main() -> anyhow::Result<()> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    if let Some(n) = std::env::var("NMD_WORKER_THREADS").ok().and_then(|s| s.parse::<usize>().ok()) {
        if n > 0 {
            builder.worker_threads(n);
        }
    }
    let rt = builder.build()?;
    rt.block_on(async_main())
}

async fn async_main() -> anyhow::Result<()> {
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

    // P0-2 离线恢复：--restore <备份> → 覆盖 cfg.db 后退出（覆盖前把现有 db 另存一份，可回滚）。须在 nmd 停止时执行。
    if let Some(src) = args.restore.as_deref() {
        let dest = &cfg.db;
        if std::path::Path::new(dest).exists() {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let bak = format!("{dest}.pre-restore.{ts}");
            std::fs::copy(dest, &bak)?;
            println!("已把现有 db 另存为 {bak}");
        }
        std::fs::copy(src, dest)?;
        println!("已从 {src} 恢复到 {dest}；现在不带 --restore 重新启动 nmd 即可。");
        return Ok(());
    }

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
    let mut node = match mode {
        Mode::Nat => {
            // 穿透 NAT + 同网通用（N0：n0 公共中继 + 打洞 + 发现）。
            // bind_port>0：固定 UDP 端口——同网可按固定端口直连(配合防火墙放行)，同时保留穿透 NAT。
            if cfg.bind_port != 0 {
                nm_node::Node::bind_persistent_on(seed, &cfg.db, cfg.bind_port).await?
            } else {
                nm_node::Node::bind_persistent(seed, &cfg.db).await?
            }
        }
        Mode::SelfHost => {
            // 自建中继 + 自建 dns/pkarr：自主可控地穿透 NAT。
            let pkarr_url = cfg.dns.joined().ok_or_else(|| {
                anyhow::anyhow!("mode=selfhost 需在 [dns] url 配置自建 iroh-dns-server 的 pkarr 端点，如 https://dns.example.com/pkarr")
            })?; // P2-12 Phase A(part 3)：逗号多中继透传给传输层（node-addr 发布/解析也多副本）
            let relays = cfg.relay.all();
            if relays.is_empty() {
                tracing::warn!("mode=selfhost 未配置 [relay] url——无中继回退，仅当对端可直连(pkarr 发布的地址)时可达，NAT 打洞将不可用");
            }
            let origin = cfg.dns.origin.clone();
            tracing::info!(relays = ?relays, pkarr = %pkarr_url, dns_origin = ?origin, "selfhost 基础设施");
            nm_node::Node::bind_persistent_selfhosted(
                seed,
                &cfg.db,
                relays,
                pkarr_url,
                origin,
                cfg.bind_port,
            )
            .await?
        }
        Mode::Lan => {
            // 仅同网直连（Minimal），固定端口便于对等配置。
            nm_node::Node::bind_local_persistent_on(seed, &cfg.db, cfg.bind_port).await?
        }
    };
    // 优雅关闭 drain 窗口（nmd.toml [shutdown] drain_timeout_secs，默认 5s）。须在 Arc 包裹前设置。
    node.set_drain_timeout(Duration::from_secs(cfg.shutdown.drain_timeout_secs));
    // 连接限流（nmd.toml [limits] max_connections，默认 10000；0=不限）。须在 Arc 包裹前设置。
    node.set_max_connections(cfg.limits.max_connections);
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
    node.set_domain(cfg.domain.clone()); // 去中心命名 N1：本节点自声明域名（空=不签发）
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
        node.set_presence_scoped(cfg.federation.presence_scoped); // P2-11：presence 退火管（缺省关=维持 overlay 双写兜底；全升级后开 → 纯按 home 拉取）
        node.clone().spawn_presence(m.federation.clone()); // P2：在线状态（关=gossip overlay+TTL；开=退 overlay、按 home 拉取）
        node.clone().spawn_pending_flush(); // Defect C 冷启动补发 + 跨节点可靠投递 outbox 持久重传（2s 扫描）
        node.set_per_topic(cfg.federation.per_topic); // F1–F5：[federation] per_topic（缺省开）启停每主题；火管仍双写兜底、存活后退火管
        node.set_pkarr(cfg.dns.joined()); // F2/P2-12A：群/频道/成员发现 pkarr 中继（逗号多中继=弱锚多副本退单点；否则仅火管/home 锚点发现）
        // P2-13 成员准入(anti-Sybil)：信任锚点 + 本节点 grant + 启停门（缺省关=行为中性）。
        node.set_admission_anchors(cfg.membership.admission_anchors.iter().filter_map(|h| parse_id_hex(h).ok()).collect());
        if !cfg.membership.admission_grant.is_empty() {
            node.set_admission_grant(cfg.membership.admission_grant.clone());
        }
        node.set_admission_enabled(cfg.membership.admission);
        if cfg.membership.admission {
            tracing::info!(anchors = cfg.membership.admission_anchors.len(), "[membership] 成员准入已启用（拒绝无本地信任锚点有效 grant 的成员广播）");
        }
        node.clone().spawn_group_sync(m.federation.clone()); // 群联邦：发现 + 消息扇出走 gossip（免 s2s 中继）
        node.clone().spawn_group_ack_batcher(); // 群 ack 聚合：成员节点按 250ms 窗口合并 GroupAck→GroupAckBatch（仅火管），降低高并发群 ack 数量级
        node.clone().spawn_channel_restore(); // P4：从持久化恢复本节点订阅的频道（重 join 主题 + 回填 subs）
        node.clone().spawn_channel_pkarr(); // P4：频道发现——home 发布 pkarr 记录 / 订阅者解析 seed（需 [dns] url）
        // F6/M1：成员发现 relay 索引（灰度，需 [dns] url）。锚点发布、所有节点解析配置的锚点公钥做冷启动 bootstrap。
        if m.relay_index {
            let anchors: Vec<[u8; 32]> = m
                .anchors
                .iter()
                .filter_map(|h| parse_id_hex(h).ok())
                .collect();
            if cfg.dns.all().is_empty() {
                tracing::warn!("[membership] relay_index=true 但未配 [dns] url —— 成员索引不发布/解析");
            }
            tracing::info!(
                anchor = m.anchor,
                anchors = anchors.len(),
                "F6/M1 member relay index enabled"
            );
            node.clone().spawn_member_index(m.anchor, anchors);
        }
        tracing::info!(
            federation = %cfg.membership.federation,
            "membership auto-discovery started"
        );
    }

    // 后端管理控制 API（若配置了 [admin] api_addr + api_token）：供 nm-admind 取数/下发控制。
    if let (Some(api_addr), Some(api_token)) =
        (cfg.admin.api_addr.clone(), cfg.admin.api_token.clone())
        && !api_addr.trim().is_empty()
        && !api_token.trim().is_empty()
    {
        let node_admin = node.clone();
        let db_path = cfg.db.clone();
        let admin_fed = cfg.membership.federation.clone();
        let cfg_path = args.config.clone();
        tokio::spawn(async move {
            if let Err(e) = admin::serve(node_admin, api_addr, api_token, db_path, admin_fed, cfg_path).await {
                tracing::warn!("admin api exited: {e}");
            }
        });
        tracing::info!("admin control API enabled");
    }

    // 供他方配置：nat/selfhost 对端用 NM_NODE_ID(公钥)；lan 对端用 NM_NODE_ADDR。
    println!("NM_NODE_ID={}", hex(node.id().as_bytes()));
    println!("NM_NODE_ADDR={}", nm_transport::addr_to_string(&node.addr()));
    tracing::info!(id = %node.id().fmt_short(), "node serving");

    // P2 换网自愈：nat/selfhost（有中继/发现）下监听本端点地址变化，换网时：
    //  1) 重新 online()（重连中继、确保地址重发布到发现服务，使对端仍能按公钥找到本节点）；
    //  2) 重新打印 NM_NODE_ADDR（供读完整地址的运维/脚本拿到新地址，不再陈旧）；
    //  3) notify_network_change() 兜底 iroh 漏检的场景（睡眠唤醒 / VPN / 同网卡改 IP）。
    // 见 docs/NETWORK_ROAMING_RECOVERY_DESIGN.md §5.2。lan 模式无中继/发现，换网重发布无意义，跳过。
    if needs_online {
        node.spawn_addr_watch("nmd"); // 地址变化结构化日志（可观测）
        let roam_node = node.clone();
        node.spawn_addr_change(move || {
            let node = roam_node.clone();
            tokio::spawn(async move {
                tracing::info!("nmd 检测到本端点地址变化（疑似换网）——重新 online + 重发布地址");
                node.notify_network_change().await; // 兜底：促使 iroh 重探测/重发布
                // 重新 online（限时，避免网络瞬断时卡住哨兵）。
                match tokio::time::timeout(Duration::from_secs(10), node.online()).await {
                    Ok(()) => tracing::info!("nmd 换网后 online — 地址已重发布，可继续被按 id 发现"),
                    Err(_) => tracing::warn!("nmd 换网后 online 超时（中继/网络暂不可达）——周期任务稍后自愈"),
                }
                // 重打印新地址（NM_NODE_ADDR 不再陈旧）。
                println!("NM_NODE_ADDR={}", nm_transport::addr_to_string(&node.addr()));
            });
        });

        // 换网/休眠自愈兜底：换网后 gossip 的热重 join 无法重建 iroh 到对端的旧连接(half-open/旧地址)，
        // 实测只有【冷启动】(全新 iroh 实例)能恢复动态发现。故换网稳定后(SETTLE 内无新换网，吸收唤醒时
        // 的连环换网)**保留身份自 exec 重启**——PID 原地用新镜像重跑、cwd 不变(identity/db 相对路径照旧)、
        // redb 崩溃安全，等效冷启动，必然恢复。换网频繁时去抖只在网络稳定后触发一次。
        let reexec_node = node.clone();
        tokio::spawn(async move {
            const SETTLE: Duration = Duration::from_secs(30);
            let mut last_epoch = reexec_node.net_epoch();
            let mut pending_since: Option<std::time::Instant> = None;
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let e = reexec_node.net_epoch();
                if e != last_epoch {
                    last_epoch = e;
                    pending_since = Some(std::time::Instant::now()); // 新换网 → 重置去抖计时
                    continue;
                }
                let Some(t) = pending_since else { continue };
                if t.elapsed() < SETTLE {
                    continue;
                }
                tracing::warn!("换网已稳定（{:?} 无新变化）→ 保留身份自重启 nmd 以恢复动态发现（等效冷启动）", SETTLE);
                let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("nmd"));
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;
                    let err = std::process::Command::new(&exe)
                        .args(std::env::args_os().skip(1))
                        .exec(); // 成功则不返回（进程镜像被替换）
                    tracing::error!("自重启 exec 失败: {err}；退出(1) 让 supervisor 拉起");
                    std::process::exit(1);
                }
                #[cfg(not(unix))]
                {
                    let _ = exe;
                    std::process::exit(1); // 非 unix：退出让 supervisor 重启
                }
            }
        });
    }

    // 优雅关闭哨兵：首个 SIGTERM/SIGINT 触发有序关闭；关闭过程中再收到信号则强制退出，
    // 防 drain / iroh close 意外卡死。见 docs 优雅关闭方案。
    {
        let node = node.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            tracing::info!("graceful shutdown initiated");
            node.trigger_shutdown(); // 取消 token → 后台 loop 协作退出 + serve 收尾
            shutdown_signal().await; // 二次信号 = 强退
            tracing::warn!("second signal received — forcing exit");
            std::process::exit(130);
        });
    }

    // P1-10 配置热加载哨兵（SIGHUP）：重读配置文件，热应用可在运行时变更的子集（黑名单/对等/
    // per_topic/pkarr/域名）；需重绑的项（relay URL/端口/身份/db/max_connections）记 WARN 提示需重启。
    #[cfg(unix)]
    {
        let node = node.clone();
        let cfg_path = args.config.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let mut hup = match signal(SignalKind::hangup()) {
                Ok(s) => s,
                Err(e) => { tracing::warn!("install SIGHUP handler failed: {e}"); return; }
            };
            while hup.recv().await.is_some() {
                tracing::info!("SIGHUP → 重载配置");
                match reload_config_from(&cfg_path, &node) {
                    Ok(summary) => tracing::info!(audit = true, actor = "signal", action = "reload", ok = true, detail = %summary, "ADMIN AUDIT"),
                    Err(e) => tracing::warn!(audit = true, actor = "signal", action = "reload", ok = false, detail = %e, "ADMIN AUDIT"),
                }
            }
        });
    }

    node.serve().await?; // 收到取消后：drain 在途投递 → router.shutdown()(CONNECTION_CLOSE) → 返回
    tracing::info!("nmd shutdown complete");
    Ok(())
}

/// P1-10 热加载：读配置文件并把**可运行时变更**的子集应用到活动节点。返回变更摘要（审计用）。
/// 热应用：黑名单(diff ban/unban)、手动对等(diff add/remove)、per_topic、pkarr/dns url、域名。
/// 需重绑（仅告警、不改）：relay URL、bind_port、identity、db、mode、max_connections、membership.*。
pub(crate) fn reload_config_from(cfg_path: &str, node: &std::sync::Arc<nm_node::Node>) -> anyhow::Result<String> {
    let raw = std::fs::read_to_string(cfg_path)?;
    let cfg: Config = toml::from_str(&raw)?;
    let mut changes: Vec<String> = Vec::new();

    // ① 黑名单：配置为期望全集 → 差集 ban/unban（幂等）。
    let want: std::collections::HashSet<[u8; 32]> =
        cfg.blacklist.iter().filter_map(|h| parse_id_hex(h).ok()).collect();
    let have: std::collections::HashSet<[u8; 32]> = node.banned().into_iter().collect();
    let (mut banned, mut unbanned) = (0u32, 0u32);
    for id in want.difference(&have) { node.ban(*id); banned += 1; }
    for id in have.difference(&want) { node.unban(*id); unbanned += 1; }
    if banned + unbanned > 0 { changes.push(format!("blacklist +{banned}/-{unbanned}")); }

    // ② per_topic / pkarr / 域名：直接覆盖（&self 原子/RwLock）。
    node.set_per_topic(cfg.federation.per_topic);
    node.set_presence_scoped(cfg.federation.presence_scoped);
    node.set_pkarr(cfg.dns.joined());
    node.set_admission_anchors(cfg.membership.admission_anchors.iter().filter_map(|h| parse_id_hex(h).ok()).collect());
    node.set_admission_enabled(cfg.membership.admission); // P2-13：准入开关/锚点热加载；本节点 grant 换发需重启
    node.set_domain(cfg.domain.clone());
    changes.push(format!("per_topic={}", cfg.federation.per_topic));
    changes.push(format!("presence_scoped={}", cfg.federation.presence_scoped));

    // ③ 需重绑项：检测到与进程启动时不同则告警（此处只提示；真正生效需重启/自重启）。
    if !cfg.relay.all().is_empty() {
        changes.push("relay(需重启生效)".into());
    }

    let summary = if changes.is_empty() { "无热变更".to_string() } else { changes.join(", ") };
    tracing::info!(%summary, "配置热加载完成");
    Ok(summary)
}

/// 等待关闭信号：unix 同时接 SIGTERM(systemd/launchd 停服)与 SIGINT(ctrl-c)；其它平台用 ctrl_c。
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

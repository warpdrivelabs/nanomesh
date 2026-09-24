//! `im-transport` — 基于 iroh 的节点端点：dial-by-key、流、身份、relay/DNS 发现。
//! 见 `docs/PLAN_B_iroh_decentralized_im.md` §4/§6。
//!
//! 三种基础设施（[`Infra`]）：
//! - [`Infra::N0`]：`N0` 预设——**n0 公共中继 + DNS/pkarr 发现**（`bind*`，生产穿透 NAT 用）。
//! - [`Infra::Local`]：`Minimal` 预设——仅直连、无外部依赖（`bind_local*`，LAN/测试用）。
//! - [`Infra::SelfHosted`]：`Minimal` 基座 + **自建 iroh-relay + 自建 iroh-dns-server(pkarr)**
//!   （`bind_selfhosted*`，自主可控的穿透 NAT）。
//!
//! - `write_gram`/`read_gram`：在 iroh 双向流上收发长度前缀 + prost 编码的 `Gram`。

use im_proto::Gram;
use std::time::Duration;

use iroh::endpoint::{presets, Connection, QuicTransportConfig, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode};
use iroh::address_lookup::memory::MemoryLookup;
use prost::Message;

/// 本系统的 ALPN 协议标识。
pub const ALPN: &[u8] = b"imspace/0";

// 供上层（im-node / im-client）复用而无需直接依赖 iroh。
pub use iroh::endpoint::Connection as IrohConnection;
pub use iroh::{EndpointAddr as Addr, EndpointId as Id};
pub use iroh::SecretKey;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("iroh transport error: {0}")]
    Iroh(String),
}

pub type Result<T> = std::result::Result<T, TransportError>;

fn err(e: impl std::fmt::Display) -> TransportError {
    TransportError::Iroh(e.to_string())
}

/// 连通基础设施：决定中继与发现服务来源。见 crate 级文档。
#[derive(Debug, Clone)]
pub enum Infra {
    /// n0 公共基础设施：`N0` 预设（n0 公共中继 + DNS/pkarr 发现）。可穿透 NAT。
    N0,
    /// 仅本地直连：`Minimal` 预设，无中继/发现。LAN / 测试用。
    Local,
    /// 自建基础设施：`Minimal` 基座 + 自定义 iroh-relay + 自建 iroh-dns-server(pkarr)。
    /// - `relay_urls`：自建中继的 URL（如 `https://relay.example.com`）；为空则不启用中继（仅直连+发现）。
    /// - `pkarr_url`：自建 iroh-dns-server 的 pkarr 端点（如 `https://dns.example.com/pkarr`），
    ///   同时用于**发布**本节点地址与**解析**对端地址（dial-by-key）。
    /// - `dns_origin`：可选，额外走 DNS 查询解析的源域（如 `dns.example.com.`）。
    SelfHosted {
        relay_urls: Vec<String>,
        pkarr_url: String,
        dns_origin: Option<String>,
    },
}

/// QUIC 传输配置：显式设定连接级空闲保活——每 3s 发一次 keep-alive PING，
/// 连接级最大空闲 30s。keep-alive < idle 才有效，从而空闲连接不会被超时掐断。
/// （iroh 默认只设了 per-path 保活，未设连接级 max_idle_timeout。）
fn keepalive_config() -> QuicTransportConfig {
    let mut b = QuicTransportConfig::builder().keep_alive_interval(Duration::from_secs(3));
    if let Ok(idle) = Duration::from_secs(30).try_into() {
        b = b.max_idle_timeout(Some(idle));
    }
    b.build()
}

/// 节点端点：封装 `iroh::Endpoint`，提供按公钥拨号 / 接受连接。
#[derive(Debug, Clone)]
pub struct NodeEndpoint {
    endpoint: Endpoint,
    /// 内存地址簿：供「按公钥拨号」（如 gossip）在 LAN/无发现时解析对端地址。
    mem: MemoryLookup,
}

impl NodeEndpoint {
    /// 按 [`Infra`] 绑定端点：选择中继与发现服务来源，并统一套用 keep-alive / ALPN / 固定端口。
    async fn bind_infra(secret_key: SecretKey, infra: &Infra, port: u16) -> Result<Self> {
        let alpns = vec![ALPN.to_vec()];
        let cfg = keepalive_config();
        let mut builder = match infra {
            Infra::N0 => Endpoint::builder(presets::N0),
            Infra::Local => Endpoint::builder(presets::Minimal),
            Infra::SelfHosted {
                relay_urls,
                pkarr_url,
                dns_origin,
            } => {
                use iroh::address_lookup::{DnsAddressLookup, PkarrPublisher, PkarrResolver};
                // Minimal 基座（不含任何 n0 中继/发现），再叠加自建发现与中继。
                let mut b = Endpoint::builder(presets::Minimal);
                // 自建 iroh-dns-server 的 pkarr：既发布本节点地址、也解析对端（HTTPS /pkarr）。
                b = b.address_lookup(PkarrPublisher::builder(pkarr_url.parse().map_err(err)?));
                b = b.address_lookup(PkarrResolver::builder(pkarr_url.parse().map_err(err)?));
                // 可选：额外用 DNS 查询解析（指向自建 dns 源域）。
                if let Some(origin) = dns_origin {
                    b = b.address_lookup(DnsAddressLookup::builder(origin.clone()));
                }
                // 自建中继（穿透 NAT / 打洞回退）；无中继 URL 则保持 Minimal 的“无中继”。
                if !relay_urls.is_empty() {
                    let relays = relay_urls
                        .iter()
                        .map(|s| s.parse())
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(err)?;
                    b = b.relay_mode(RelayMode::custom(relays));
                }
                b
            }
        };
        builder = builder
            .transport_config(cfg)
            .secret_key(secret_key)
            .alpns(alpns);
        if port != 0 {
            // 固定 UDP 端口：让本节点地址稳定，便于 LAN 对等配置。
            builder = builder
                .bind_addr((std::net::Ipv4Addr::UNSPECIFIED, port))
                .map_err(err)?;
        }
        let endpoint = builder.bind().await.map_err(err)?;
        // 注册一个内存地址簿：供「按公钥拨号」（如 gossip）在 LAN/无发现时解析对端地址。
        // 对 N0/selfhost 是叠加项（discovery 仍照常工作），无害。
        let mem = MemoryLookup::new();
        if let Ok(al) = endpoint.address_lookup() {
            al.add(mem.clone());
        }
        Ok(Self { endpoint, mem })
    }

    async fn bind_with(secret_key: SecretKey, local_only: bool, port: u16) -> Result<Self> {
        let infra = if local_only { Infra::Local } else { Infra::N0 };
        Self::bind_infra(secret_key, &infra, port).await
    }

    /// 生产绑定：`N0` 预设（中继 + 发现）。
    pub async fn bind(secret_key: SecretKey) -> Result<Self> {
        Self::bind_with(secret_key, false, 0).await
    }
    pub async fn bind_from_seed(seed: [u8; 32]) -> Result<Self> {
        Self::bind(SecretKey::from_bytes(&seed)).await
    }
    pub async fn bind_random() -> Result<Self> {
        Self::bind(SecretKey::generate()).await
    }

    /// 生产绑定(`N0`) 到**固定 UDP 端口**：既走 n0 中继 + 发现（穿透 NAT），又让同网可按固定端口
    /// 直连（配合防火墙放行该端口）。一个实例同时服务同网与跨 NAT 两类对端。
    pub async fn bind_on(secret_key: SecretKey, port: u16) -> Result<Self> {
        Self::bind_with(secret_key, false, port).await
    }
    pub async fn bind_from_seed_on(seed: [u8; 32], port: u16) -> Result<Self> {
        Self::bind_on(SecretKey::from_bytes(&seed), port).await
    }

    /// 本地/LAN 绑定：`Minimal` 预设（仅直连，无中继/发现，适合测试）。
    pub async fn bind_local(secret_key: SecretKey) -> Result<Self> {
        Self::bind_with(secret_key, true, 0).await
    }
    pub async fn bind_local_from_seed(seed: [u8; 32]) -> Result<Self> {
        Self::bind_local(SecretKey::from_bytes(&seed)).await
    }

    /// 本地(Minimal)绑定到指定 UDP 端口（0=临时端口）。固定端口=稳定地址，便于 LAN 对等。
    pub async fn bind_local_on(secret_key: SecretKey, port: u16) -> Result<Self> {
        Self::bind_with(secret_key, true, port).await
    }
    pub async fn bind_local_on_from_seed(seed: [u8; 32], port: u16) -> Result<Self> {
        Self::bind_local_on(SecretKey::from_bytes(&seed), port).await
    }
    pub async fn bind_local_random() -> Result<Self> {
        Self::bind_local(SecretKey::generate()).await
    }

    /// 自建基础设施绑定：自定义 iroh-relay + 自建 iroh-dns-server(pkarr)。可穿透 NAT，
    /// 但完全走自主可控的中继/发现，不依赖 n0 公共设施。`port=0` 用临时端口。
    pub async fn bind_selfhosted(
        secret_key: SecretKey,
        relay_urls: Vec<String>,
        pkarr_url: String,
        dns_origin: Option<String>,
        port: u16,
    ) -> Result<Self> {
        let infra = Infra::SelfHosted {
            relay_urls,
            pkarr_url,
            dns_origin,
        };
        Self::bind_infra(secret_key, &infra, port).await
    }
    pub async fn bind_selfhosted_from_seed(
        seed: [u8; 32],
        relay_urls: Vec<String>,
        pkarr_url: String,
        dns_origin: Option<String>,
        port: u16,
    ) -> Result<Self> {
        Self::bind_selfhosted(
            SecretKey::from_bytes(&seed),
            relay_urls,
            pkarr_url,
            dns_origin,
            port,
        )
        .await
    }

    /// 本节点身份（`EndpointId` = Ed25519 公钥）。
    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// 本节点身份的 32 字节表示（用于 `Entity.entity_id` 等）。
    pub fn id_bytes(&self) -> [u8; 32] {
        *self.endpoint.id().as_bytes()
    }

    /// 本节点私钥（用于签发能力授权 Grant / 自签名）。
    pub fn secret_key(&self) -> &SecretKey {
        self.endpoint.secret_key()
    }

    /// 等待端点“上线”（连上中继、地址已发布到发现服务）。N0 模式对外服务前调用，
    /// 以便他方能按公钥 dial-by-key 找到本节点。
    pub async fn online(&self) {
        self.endpoint.online().await;
    }

    /// 本节点当前地址（含直连地址；用于本地直接拨号 / 分享）。
    pub fn addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// 向本端点地址簿播种一个对端地址（供「按公钥拨号」，如 gossip 引导；LAN/无发现时必需）。
    pub fn add_addr(&self, addr: EndpointAddr) {
        self.mem.add_endpoint_info(addr);
    }

    /// 底层 iroh 端点（供 gossip/docs 复用同一 endpoint）。
    pub fn iroh(&self) -> &Endpoint {
        &self.endpoint
    }

    /// 按公钥/地址拨号（dial-by-key）。
    pub async fn connect(&self, to: impl Into<EndpointAddr>) -> Result<Connection> {
        self.endpoint.connect(to, ALPN).await.map_err(err)
    }

    /// 接受下一条入站连接；`None` 表示端点已关闭。
    pub async fn accept(&self) -> Option<Result<Connection>> {
        let incoming = self.endpoint.accept().await?;
        Some(incoming.await.map_err(err))
    }

    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}

/// 全局应用层流量计数（gram 收发条数与字节数，含 4 字节长度前缀）。供后端管理台流量监控。
static GRAMS_IN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GRAMS_OUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BYTES_IN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BYTES_OUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 累计应用层流量快照。
#[derive(Debug, Clone, Copy, Default)]
pub struct TrafficStat {
    pub grams_in: u64,
    pub grams_out: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

/// 读取当前累计流量（进程级全局，自启动累加）。
pub fn traffic_snapshot() -> TrafficStat {
    use std::sync::atomic::Ordering::Relaxed;
    TrafficStat {
        grams_in: GRAMS_IN.load(Relaxed),
        grams_out: GRAMS_OUT.load(Relaxed),
        bytes_in: BYTES_IN.load(Relaxed),
        bytes_out: BYTES_OUT.load(Relaxed),
    }
}

/// 在双向流的发送端写入一个 `Gram`（4 字节大端长度前缀 + prost 编码）。
pub async fn write_gram(send: &mut SendStream, gram: &Gram) -> Result<()> {
    use std::sync::atomic::Ordering::Relaxed;
    let buf = gram.encode_to_vec();
    let len = (buf.len() as u32).to_be_bytes();
    send.write_all(&len).await.map_err(err)?;
    send.write_all(&buf).await.map_err(err)?;
    GRAMS_OUT.fetch_add(1, Relaxed);
    BYTES_OUT.fetch_add((buf.len() + 4) as u64, Relaxed);
    Ok(())
}

/// 从双向流的接收端读取一个 `Gram`。
pub async fn read_gram(recv: &mut RecvStream) -> Result<Gram> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.map_err(err)?;
    let n = u32::from_be_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf).await.map_err(err)?;
    let gram = Gram::decode(buf.as_slice()).map_err(err)?;
    GRAMS_IN.fetch_add(1, Relaxed);
    BYTES_IN.fetch_add((n + 4) as u64, Relaxed);
    Ok(gram)
}

/// 把 `EndpointAddr` 编码为可分享字符串（JSON），供 CLI/配置传递节点地址。
pub fn addr_to_string(addr: &EndpointAddr) -> String {
    serde_json::to_string(addr).unwrap_or_default()
}

/// 从字符串解析 `EndpointAddr`。
pub fn addr_from_string(s: &str) -> Result<EndpointAddr> {
    serde_json::from_str(s).map_err(err)
}

/// 由 node id(公钥 32 字节) 构造“仅含身份”的地址。配合 N0 发现，`connect` 时会
/// 按公钥解析出当前可达地址——跨 NAT 只需交换公钥（稳定），无需交换会变的地址。
pub fn addr_from_id(node_id: [u8; 32]) -> Result<EndpointAddr> {
    EndpointId::from_bytes(&node_id)
        .map(EndpointAddr::from)
        .map_err(err)
}

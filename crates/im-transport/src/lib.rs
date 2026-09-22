//! `im-transport` — 基于 iroh 的节点端点：dial-by-key、流、身份、relay/DNS 发现。
//! 见 `docs/PLAN_B_iroh_decentralized_im.md` §4/§6。
//!
//! - `bind`/`bind_from_seed`/`bind_random`：`N0` 预设（n0 中继 + DNS/pkarr 发现，生产用）。
//! - `bind_local*`：`Minimal` 预设（仅直连，LAN / 测试用，无外部依赖）。
//! - `write_gram`/`read_gram`：在 iroh 双向流上收发长度前缀 + prost 编码的 `Gram`。

use im_proto::Gram;
use std::time::Duration;

use iroh::endpoint::{presets, Connection, QuicTransportConfig, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId};
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
}

impl NodeEndpoint {
    async fn bind_with(secret_key: SecretKey, local_only: bool) -> Result<Self> {
        let alpns = vec![ALPN.to_vec()];
        let cfg = keepalive_config();
        let endpoint = if local_only {
            Endpoint::builder(presets::Minimal)
                .transport_config(cfg)
                .secret_key(secret_key)
                .alpns(alpns)
                .bind()
                .await
        } else {
            Endpoint::builder(presets::N0)
                .transport_config(cfg)
                .secret_key(secret_key)
                .alpns(alpns)
                .bind()
                .await
        }
        .map_err(err)?;
        Ok(Self { endpoint })
    }

    /// 生产绑定：`N0` 预设（中继 + 发现）。
    pub async fn bind(secret_key: SecretKey) -> Result<Self> {
        Self::bind_with(secret_key, false).await
    }
    pub async fn bind_from_seed(seed: [u8; 32]) -> Result<Self> {
        Self::bind(SecretKey::from_bytes(&seed)).await
    }
    pub async fn bind_random() -> Result<Self> {
        Self::bind(SecretKey::generate()).await
    }

    /// 本地/LAN 绑定：`Minimal` 预设（仅直连，无中继/发现，适合测试）。
    pub async fn bind_local(secret_key: SecretKey) -> Result<Self> {
        Self::bind_with(secret_key, true).await
    }
    pub async fn bind_local_from_seed(seed: [u8; 32]) -> Result<Self> {
        Self::bind_local(SecretKey::from_bytes(&seed)).await
    }
    pub async fn bind_local_random() -> Result<Self> {
        Self::bind_local(SecretKey::generate()).await
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

    /// 本节点当前地址（含直连地址；用于本地直接拨号 / 分享）。
    pub fn addr(&self) -> EndpointAddr {
        self.endpoint.addr()
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

/// 在双向流的发送端写入一个 `Gram`（4 字节大端长度前缀 + prost 编码）。
pub async fn write_gram(send: &mut SendStream, gram: &Gram) -> Result<()> {
    let buf = gram.encode_to_vec();
    let len = (buf.len() as u32).to_be_bytes();
    send.write_all(&len).await.map_err(err)?;
    send.write_all(&buf).await.map_err(err)?;
    Ok(())
}

/// 从双向流的接收端读取一个 `Gram`。
pub async fn read_gram(recv: &mut RecvStream) -> Result<Gram> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.map_err(err)?;
    let n = u32::from_be_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf).await.map_err(err)?;
    Gram::decode(buf.as_slice()).map_err(err)
}

/// 把 `EndpointAddr` 编码为可分享字符串（JSON），供 CLI/配置传递节点地址。
pub fn addr_to_string(addr: &EndpointAddr) -> String {
    serde_json::to_string(addr).unwrap_or_default()
}

/// 从字符串解析 `EndpointAddr`。
pub fn addr_from_string(s: &str) -> Result<EndpointAddr> {
    serde_json::from_str(s).map_err(err)
}

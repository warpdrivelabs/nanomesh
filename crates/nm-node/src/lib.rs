//! `nm-node` — 服务端节点：接受连接、维护在线会话表与实体目录、按 gram 类型分发与路由。
//! 见 `docs/PLAN_B` §8.1 与 `docs/PLAN_C` §6。
//!
//! - 会话：`remote_id(对端公钥) → Connection`，连接建立即登记、断开即移除。
//! - 目录(Directory)：`directory.register` / `directory.query`（内存）。
//! - 路由：收到 `Message` 时若 `receiver` 为本机在线会话，经 uni 流推送给它；并回执给发送方。

use std::sync::Arc;

use dashmap::{DashMap, DashSet};
use nm_core::Directory;
use nm_proto::{
    now_ms, Any, BlobData, BlobPut, BlobRef, Channel, ChannelBackfillReq, ChannelGram, ChannelList,
    ChannelLog, ChannelMsg, ChannelOp, ChannelPub, Command, CommandResult, DirectoryQuery, Entity,
    EntityList, FedSyncResp, Gram, GramKind, Group, GroupGossip, GroupList, GroupOp, NameList,
    NameOp, NameQuery, NameRecord, PROTOCOL_VERSION,
};
use nm_store::RedbStore;
use nm_transport::{read_gram, write_gram, IrohConnection, NodeEndpoint};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh_gossip::Gossip;
use prost::Message;

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("{0}")]
    Other(String),
    #[error(transparent)]
    Store(#[from] nm_store::StoreError),
}

type Sessions = DashMap<Vec<u8>, IrohConnection>;
type Peers = DashMap<Vec<u8>, nm_transport::Addr>;

/// 内容寻址 blob 体量上限（头像等小媒体）；超限拒绝，避免撑爆节点存储/带宽。大媒体应分块（后续）。
const MAX_BLOB: usize = 1024 * 1024; // 1 MiB

/// 会话旁挂元数据（不改动 `Sessions` 值类型；用于管理台展示接入时长）。
#[derive(Clone, Copy)]
struct SessionMeta {
    since: std::time::SystemTime,
}
type SessionsMeta = DashMap<Vec<u8>, SessionMeta>;

/// 一条活动会话的管理快照（后端管理台「连接监控」用）。
pub struct SessionRow {
    pub id: [u8; 32],
    pub since_unix_ms: u64,
    pub bytes_tx: u64,
    pub bytes_rx: u64,
    pub alpn: String,
}

/// 存储统计（后端管理台「存储管理」用）。
pub struct StoreStat {
    pub entities: usize,
    pub groups: usize,
}

/// 联邦对等元数据（管理台展示/持久化，可扩展）。连接一律按 node id（公钥）解析。
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    #[serde(default)]
    pub name: String,
    /// 物理所在位置（街道门牌等），仅作展示元数据，与网络连接无关。
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub mobile: String,
    #[serde(default)]
    pub gps: String,
    /// 来源：`"manual"`(手工/种子，常驻不过期) / `"discovered"`(gossip 自动发现，有 TTL)。
    /// 空串按 manual 处理（兼容旧持久化行）。不参与成员卡片签名（本地元数据）。
    #[serde(default)]
    pub source: String,
    /// 最近一次被广播/确认存活的 Unix 秒（0=未知/手工）。不参与签名（本地元数据）。
    #[serde(default)]
    pub last_seen: u64,
    /// 所属联邦名（成员频道）。发现的 peer = 听到它的频道(=本节点联邦)；手工 peer = 本节点联邦。
    /// 空串按本节点联邦呈现。本地元数据，不参与签名。
    #[serde(default)]
    pub federation: String,
}

/// 联邦成员广播卡片（成员频道上以 JSON 广播）。`sig` 覆盖 node_id+ts+info 五个展示字段，
/// 按 `node_id` 验签以自证作者（gossip 无逐条作者签名）；`addr` 为未签名的拨号提示。
#[derive(serde::Serialize, serde::Deserialize)]
struct MembershipAnnounce {
    node_id: String,
    ts: u64,
    #[serde(default)]
    info: PeerInfo,
    #[serde(default)]
    addr: String,
    sig: String,
}

/// 在线状态缓存记录（临时、TTL 过期即离线；不进持久目录，仅内存）。
#[derive(Clone)]
struct PresenceRec {
    status: String, // online / away / busy / dnd
    last_seen: u64, // unix 秒
    #[allow(dead_code)] // 记录来源节点（备调试/未来 presence 回源），当前不读取
    home_node: Vec<u8>,
}
type PresenceMap = DashMap<Vec<u8>, PresenceRec>; // entity_id -> presence
type StatusIntent = DashMap<Vec<u8>, String>; // entity_id -> 用户设定的状态意图

/// 在线状态广播卡片（presence 频道 gossip）；`sig` 覆盖 entity+status+ts，由 home_node 私钥签发。
#[derive(serde::Serialize, serde::Deserialize)]
struct PresenceAnnounce {
    entity: String,
    status: String,
    ts: u64,
    home_node: String,
    sig: String,
}

const PRESENCE_INTERVAL_SECS: u64 = 10; // presence 广播间隔
const PRESENCE_TTL_SECS: u64 = 35; // 超此未刷新即视为离线（≈3× 间隔）

/// 频道运行态（P4）：iroh-gossip 主题上的开放 pub/sub。每个已订阅频道一个后台任务：
/// 收播 gossip 消息 → 追加日志 + 转发给本地订阅会话；发布经 mpsc 交给该任务广播。
struct ChannelEntry {
    meta: Arc<std::sync::Mutex<Channel>>, // 频道元信息（gossip 可更新：晚订阅者据 owner 重播补全）
    subs: Arc<DashSet<Vec<u8>>>, // 本地订阅会话 id
    log: Arc<std::sync::Mutex<std::collections::VecDeque<ChannelMsg>>>, // 近期消息（回填用）
    seq: Arc<std::sync::atomic::AtomicU64>, // 本地发布序号（单调）
    pub_tx: tokio::sync::mpsc::UnboundedSender<ChannelOut>, // → 频道任务：待广播的消息 / 元信息
}
/// 频道后台任务的出站项：一条消息，或一次元信息广播。
enum ChannelOut {
    Msg(ChannelMsg),
    Meta(Channel),
}
type Channels = DashMap<Vec<u8>, ChannelEntry>;
const CHANNEL_LOG_CAP: usize = 300; // 每频道内存日志上限（回填近期）

/// 联邦成员发现配置（nmd 透传）。
pub struct MembershipCfg {
    /// 联邦名：同名者组成同一 gossip 叠加网；改名即隔离独立联邦。
    pub federation: String,
    /// 自身卡片广播间隔（秒）。
    pub announce_interval_secs: u64,
    /// 发现节点的存活 TTL（秒）：超时未再广播即剔除。
    pub ttl_secs: u64,
    /// 本节点对外广播的成员卡片（名称/物理地址/email/mobile/gps）。
    pub card: PeerInfo,
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn hex_decode_n<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for i in 0..N {
        out[i] = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
/// 成员卡片的规范签名字节（node_id||ts||name||address||email||mobile||gps，以 0 分隔）。
/// 只覆盖会广播的展示字段——`source`/`last_seen` 是本地元数据，不参与签名。
fn membership_signing_bytes(node_id: &[u8; 32], ts: u64, info: &PeerInfo) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(node_id);
    b.push(0);
    b.extend_from_slice(&ts.to_be_bytes());
    b.push(0);
    b.extend_from_slice(info.name.as_bytes());
    b.push(0);
    b.extend_from_slice(info.address.as_bytes());
    b.push(0);
    b.extend_from_slice(info.email.as_bytes());
    b.push(0);
    b.extend_from_slice(info.mobile.as_bytes());
    b.push(0);
    b.extend_from_slice(info.gps.as_bytes());
    b
}

/// 处理器共享上下文（含联邦所需的本节点端点/身份/对等表）。
#[derive(Clone)]
struct Ctx {
    ep: NodeEndpoint,
    dir: Arc<MemDirectory>,
    sessions: Arc<Sessions>,
    groups: Arc<DashMap<Vec<u8>, Group>>,
    store: Option<Arc<RedbStore>>,
    peers: Arc<Peers>,
    node_id: [u8; 32],
    blacklist: Arc<DashSet<[u8; 32]>>,
    sessions_meta: Arc<SessionsMeta>,
    presence: Arc<PresenceMap>,      // 在线状态缓存（gossip + 本地会话，TTL 过期即离线）
    status_intent: Arc<StatusIntent>, // 用户设定状态（away/busy/dnd…），广播时采用
    channels: Arc<Channels>,          // P4 频道运行态（gossip pub/sub）
    gossip: Gossip,                   // 频道动态 join 需要
    group_pub: tokio::sync::mpsc::UnboundedSender<Vec<u8>>, // 群消息/群公告 → 联邦 gossip 广播队列
    domains: Arc<std::sync::RwLock<Vec<String>>>, // 本节点自声明域名集合（命名 N1）
    names: Arc<DashMap<String, NameRecord>>,  // 命名缓存 local@domain → NameRecord
}

/// 内存实体目录：`entity_id → Entity`，支持 kind 前缀 / 能力 / 属性过滤。
#[derive(Default)]
pub struct MemDirectory {
    entities: DashMap<Vec<u8>, Entity>,
}

impl MemDirectory {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.entities.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }
    /// LWW（Last-Writer-Wins）合并：仅当传入 `updated_at ≥ 现有`才落库，返回是否被接受。
    /// 统一收敛入口——目录注册 / profile.update / 联邦同步都经此，保证跨节点最终一致、
    /// 且更旧的更新（如联邦回灌的过期副本）不会覆盖较新的本地记录。
    pub fn merge_lww(&self, entity: Entity) -> bool {
        use dashmap::mapref::entry::Entry;
        match self.entities.entry(entity.entity_id.clone()) {
            Entry::Occupied(mut o) => {
                if entity.updated_at >= o.get().updated_at {
                    o.insert(entity);
                    true
                } else {
                    false
                }
            }
            Entry::Vacant(v) => {
                v.insert(entity);
                true
            }
        }
    }
}

impl Directory for MemDirectory {
    async fn upsert(&self, entity: Entity) -> nm_core::Result<()> {
        self.merge_lww(entity); // LWW 收敛（见 merge_lww）
        Ok(())
    }
    async fn get(&self, entity_id: &[u8]) -> nm_core::Result<Option<Entity>> {
        Ok(self.entities.get(entity_id).map(|e| e.clone()))
    }
    async fn query(&self, q: &DirectoryQuery) -> nm_core::Result<Vec<Entity>> {
        let out = self
            .entities
            .iter()
            .filter(|e| entity_matches(e.value(), q))
            .map(|e| e.value().clone())
            .collect();
        Ok(out)
    }
}

fn entity_matches(e: &Entity, q: &DirectoryQuery) -> bool {
    if !q.kind_prefix.is_empty() && !e.kind.starts_with(&q.kind_prefix) {
        return false;
    }
    for cap in &q.require_capabilities {
        // repeated enum 在 prost 里是 Vec<i32>
        if !e.capabilities.contains(cap) {
            return false;
        }
    }
    for (k, v) in &q.match_attributes {
        if e.attributes.get(k).map(String::as_str) != Some(v.as_str()) {
            return false;
        }
    }
    true
}

/// 去中心节点。
pub struct Node {
    ep: NodeEndpoint,
    dir: Arc<MemDirectory>,
    sessions: Arc<Sessions>,
    groups: Arc<DashMap<Vec<u8>, Group>>,
    store: Option<Arc<RedbStore>>,
    peers: Arc<DashMap<Vec<u8>, nm_transport::Addr>>, // 联邦对等节点：node_id -> addr
    gossip: Gossip,                                    // 频道 pub/sub（与单播共用同一 endpoint）
    blacklist: Arc<DashSet<[u8; 32]>>,                 // 黑名单：被禁公钥（无白名单，默认放行）
    sessions_meta: Arc<SessionsMeta>,                  // 会话旁挂元数据（接入时间）
    peer_info: Arc<DashMap<Vec<u8>, PeerInfo>>,        // 对等元数据（名称/地址/email/mobile/gps）
    presence: Arc<PresenceMap>,                        // 在线状态缓存（gossip + 本地会话，TTL）
    status_intent: Arc<StatusIntent>,                  // 用户设定状态（away/busy/dnd…）
    channels: Arc<Channels>,                           // P4 频道运行态（gossip pub/sub）
    // 群联邦 gossip：fanout 把待广播的 GroupGossip 字节丢进 group_pub，由 spawn_group_sync 统一发布。
    group_pub: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    group_pub_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>>,
    // 去中心命名（N1）：本节点自声明的域名集合（可多个）+ 命名缓存（local@domain → NameRecord，含 gossip 学到的）。
    domains: Arc<std::sync::RwLock<Vec<String>>>,
    names: Arc<DashMap<String, NameRecord>>,
}

impl Node {
    pub async fn bind_local(seed: [u8; 32]) -> Result<Self, NodeError> {
        Self::from_ep(
            NodeEndpoint::bind_local_from_seed(seed)
                .await
                .map_err(|e| NodeError::Other(e.to_string()))?,
            None,
        )
    }

    pub async fn bind(seed: [u8; 32]) -> Result<Self, NodeError> {
        Self::from_ep(
            NodeEndpoint::bind_from_seed(seed)
                .await
                .map_err(|e| NodeError::Other(e.to_string()))?,
            None,
        )
    }

    /// 本地绑定 + 持久化（redb）：目录与离线队列落盘，重启回填目录。
    pub async fn bind_local_persistent(
        seed: [u8; 32],
        db_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, NodeError> {
        let store = Arc::new(RedbStore::open(db_path)?);
        let ep = NodeEndpoint::bind_local_from_seed(seed)
            .await
            .map_err(|e| NodeError::Other(e.to_string()))?;
        Self::from_ep(ep, Some(store))
    }

    /// 本地绑定 + 持久化 + 固定端口（LAN 部署：稳定地址便于对等配置）。
    pub async fn bind_local_persistent_on(
        seed: [u8; 32],
        db_path: impl AsRef<std::path::Path>,
        port: u16,
    ) -> Result<Self, NodeError> {
        let store = Arc::new(RedbStore::open(db_path)?);
        let ep = NodeEndpoint::bind_local_on_from_seed(seed, port)
            .await
            .map_err(|e| NodeError::Other(e.to_string()))?;
        Self::from_ep(ep, Some(store))
    }

    /// 生产绑定 + 持久化（N0 预设：中继 + QUIC 打洞 + DNS/pkarr 发现，可穿透 NAT；同网也可用）。
    pub async fn bind_persistent(
        seed: [u8; 32],
        db_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, NodeError> {
        let store = Arc::new(RedbStore::open(db_path)?);
        let ep = NodeEndpoint::bind_from_seed(seed)
            .await
            .map_err(|e| NodeError::Other(e.to_string()))?;
        Self::from_ep(ep, Some(store))
    }

    /// 生产绑定(N0) + **固定 UDP 端口** + 持久化：同一实例**既支持同网直连(固定端口)也支持穿透 NAT**。
    /// 固定端口便于防火墙放行，从而同网走直连、跨网走中继/打洞。
    pub async fn bind_persistent_on(
        seed: [u8; 32],
        db_path: impl AsRef<std::path::Path>,
        port: u16,
    ) -> Result<Self, NodeError> {
        let store = Arc::new(RedbStore::open(db_path)?);
        let ep = NodeEndpoint::bind_from_seed_on(seed, port)
            .await
            .map_err(|e| NodeError::Other(e.to_string()))?;
        Self::from_ep(ep, Some(store))
    }

    /// 自建基础设施绑定 + 持久化（自定义 iroh-relay + 自建 iroh-dns-server(pkarr)：
    /// 自主可控地穿透 NAT，不依赖 n0 公共设施）。`port=0` 用临时端口。
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_persistent_selfhosted(
        seed: [u8; 32],
        db_path: impl AsRef<std::path::Path>,
        relay_urls: Vec<String>,
        pkarr_url: String,
        dns_origin: Option<String>,
        port: u16,
    ) -> Result<Self, NodeError> {
        let store = Arc::new(RedbStore::open(db_path)?);
        let ep =
            NodeEndpoint::bind_selfhosted_from_seed(seed, relay_urls, pkarr_url, dns_origin, port)
                .await
                .map_err(|e| NodeError::Other(e.to_string()))?;
        Self::from_ep(ep, Some(store))
    }

    /// 等待上线（连上中继、发布地址）。N0 模式对外服务前调用，使本节点可被按公钥发现。
    pub async fn online(&self) {
        self.ep.online().await;
    }

    fn from_ep(ep: NodeEndpoint, store: Option<Arc<RedbStore>>) -> Result<Self, NodeError> {
        let dir = Arc::new(MemDirectory::new());
        let groups: Arc<DashMap<Vec<u8>, Group>> = Arc::new(DashMap::new());
        let blacklist: Arc<DashSet<[u8; 32]>> = Arc::new(DashSet::new());
        let peers: Arc<DashMap<Vec<u8>, nm_transport::Addr>> = Arc::new(DashMap::new());
        let peer_info: Arc<DashMap<Vec<u8>, PeerInfo>> = Arc::new(DashMap::new());
        // 从持久层回填内存目录/群组/黑名单/对等（重启恢复）。
        if let Some(s) = &store {
            for e in s.all_entities()? {
                dir.entities.insert(e.entity_id.clone(), e);
            }
            for g in s.all_groups()? {
                groups.insert(g.group_id.clone(), g);
            }
            for b in s.all_banned()? {
                blacklist.insert(b);
            }
            for (id, meta_json) in s.all_peers()? {
                let info: PeerInfo = serde_json::from_str(&meta_json).unwrap_or_default();
                // 连接一律按公钥发现（`address` 是物理位置，与网络无关）。
                if let Ok(a) = nm_transport::addr_from_id(id) {
                    peers.insert(id.to_vec(), a);
                }
                peer_info.insert(id.to_vec(), info);
            }
        }
        // 频道 pub/sub：与单播共用同一 iroh endpoint（accept 侧由 serve() 的 Router 分流）。
        let gossip = Gossip::builder().spawn(ep.iroh().clone());
        let (group_pub, group_pub_rx) = tokio::sync::mpsc::unbounded_channel();
        // 命名缓存：回填本节点持久化的命名记录（重启恢复）。
        let names: Arc<DashMap<String, NameRecord>> = Arc::new(DashMap::new());
        let mut owned_domains: Vec<String> = Vec::new();
        if let Some(s) = &store {
            for r in s.all_names().unwrap_or_default() {
                names.insert(format!("{}@{}", r.local_part, r.domain), r);
            }
            owned_domains = s.all_domains().unwrap_or_default();
        }
        Ok(Self {
            ep,
            dir,
            sessions: Arc::new(Sessions::new()),
            groups,
            store,
            peers,
            gossip,
            blacklist,
            sessions_meta: Arc::new(SessionsMeta::new()),
            peer_info,
            presence: Arc::new(PresenceMap::new()),
            status_intent: Arc::new(StatusIntent::new()),
            channels: Arc::new(Channels::new()),
            group_pub,
            group_pub_rx: std::sync::Mutex::new(Some(group_pub_rx)),
            domains: Arc::new(std::sync::RwLock::new(owned_domains)),
            names,
        })
    }

    /// 添加联邦对等节点（bootstrap）。之后本节点会周期性拉取其目录、并向其转发跨节点消息。
    pub fn add_peer(&self, node_id: [u8; 32], addr: nm_transport::Addr) {
        self.peers.insert(node_id.to_vec(), addr);
    }

    /// 由对等节点地址添加（node id 从地址里取，配置更省事）。
    pub fn add_peer_addr(&self, addr: nm_transport::Addr) {
        let id = *addr.id.as_bytes();
        // 播种到 endpoint 地址簿：让「按公钥拨号」（gossip 引导）在 LAN/无发现时也能解析地址。
        self.ep.add_addr(addr.clone());
        self.peers.insert(id.to_vec(), addr);
    }

    /// 按 node id(公钥) 添加对等（N0 模式：发现服务解析地址；跨 NAT 只需交换公钥）。
    pub fn add_peer_by_id(&self, node_id: [u8; 32]) -> Result<(), NodeError> {
        let addr = nm_transport::addr_from_id(node_id).map_err(|e| NodeError::Other(e.to_string()))?;
        self.peers.insert(node_id.to_vec(), addr);
        Ok(())
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    // ---- 黑名单（无白名单：默认放行，仅拒绝名单内公钥）----

    /// 封禁一个公钥：加入黑名单、持久化，并**立即切断**其在线会话。此后其连接一律被拒。
    pub fn ban(&self, pubkey: [u8; 32]) {
        self.blacklist.insert(pubkey);
        if let Some(s) = &self.store {
            if let Err(e) = s.ban(&pubkey) {
                tracing::warn!("persist ban failed: {e}");
            }
        }
        // 立即切断在线会话（若有），并主动关闭连接。
        if let Some((_, conn)) = self.sessions.remove(&pubkey[..]) {
            self.sessions_meta.remove(&pubkey[..]);
            conn.close(0u32.into(), b"banned");
            tracing::info!(online = self.sessions.len(), "banned peer session cut");
        }
    }

    /// 解封一个公钥（幂等）。
    pub fn unban(&self, pubkey: [u8; 32]) {
        self.blacklist.remove(&pubkey);
        if let Some(s) = &self.store {
            if let Err(e) = s.unban(&pubkey) {
                tracing::warn!("persist unban failed: {e}");
            }
        }
    }

    /// 该公钥是否被封禁。
    pub fn is_banned(&self, pubkey: &[u8; 32]) -> bool {
        self.blacklist.contains(pubkey)
    }

    /// 当前黑名单（全部被禁公钥）。
    pub fn banned(&self) -> Vec<[u8; 32]> {
        self.blacklist.iter().map(|k| *k).collect()
    }

    pub fn banned_count(&self) -> usize {
        self.blacklist.len()
    }

    /// 后台周期性向所有对等节点同步目录，返回任务句柄。
    pub fn spawn_federation_sync(
        self: Arc<Self>,
        interval: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                self.sync_peers_once().await;
                tokio::time::sleep(interval).await;
            }
        })
    }

    /// 主动向所有对等节点拉取一次目录（把远端实体并入本地目录，标注其 home_node）。
    pub async fn sync_peers_once(&self) {
        let ep = self.ep.clone();
        let my = self.ep.id_bytes();
        // 关键：先把对等地址收集成 Vec，再逐个拨号——DashMap 的迭代守卫**不能跨 .await 持有**。
        // 否则某对端在读阶段卡住（fed_pull→s2s_request→read_gram 若无超时会无限等）会把该 shard
        // 的读锁永久占住；此后任何对 peers 的写（learn_peer/sweep_discovered）排队，parking_lot
        // 的公平性会连带阻塞**所有后续读**（peer_count()→管理台、peers.get()→消息投递），
        // worker 线程逐步耗尽 → 整个 runtime 冻死且不恢复（已在 mac + 两台种子复现 3 次）。
        // s2s_request 现另有整体超时兜底，双保险。
        let targets: Vec<nm_transport::Addr> =
            self.peers.iter().map(|e| e.value().clone()).collect();
        for addr in targets {
            match fed_pull(&ep, addr).await {
                Ok(entities) => {
                    for e in entities {
                        if e.entity_id == my.as_slice() {
                            continue;
                        }
                        self.dir.merge_lww(e); // LWW：不让联邦回灌的旧副本覆盖较新的本地记录
                    }
                }
                Err(e) => tracing::warn!("fed sync failed: {e}"),
            }
        }
    }

    pub fn id(&self) -> nm_transport::Id {
        self.ep.id()
    }
    pub fn addr(&self) -> nm_transport::Addr {
        self.ep.addr()
    }
    pub fn directory(&self) -> Arc<MemDirectory> {
        self.dir.clone()
    }
    pub fn online_count(&self) -> usize {
        self.sessions.len()
    }

    // ---- 后端管理台访问器 ----

    /// 活动会话快照（连接监控）。顺带清理陈旧的会话元数据。
    pub fn sessions_snapshot(&self) -> Vec<SessionRow> {
        self.sessions_meta.retain(|k, _| self.sessions.contains_key(k));
        self.sessions
            .iter()
            .map(|e| {
                let key = e.key();
                let conn = e.value();
                let mut id = [0u8; 32];
                if key.len() == 32 {
                    id.copy_from_slice(key);
                }
                let since_unix_ms = self
                    .sessions_meta
                    .get(key)
                    .and_then(|m| m.since.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let stats = conn.stats();
                SessionRow {
                    id,
                    since_unix_ms,
                    bytes_tx: stats.udp_tx.bytes,
                    bytes_rx: stats.udp_rx.bytes,
                    alpn: String::from_utf8_lossy(conn.alpn()).into_owned(),
                }
            })
            .collect()
    }

    /// 群组数量。
    pub fn groups_count(&self) -> usize {
        self.groups.len()
    }

    /// 当前联邦对等节点公钥列表（管理台「对等节点」用）。
    pub fn peers_list(&self) -> Vec<[u8; 32]> {
        self.peers
            .iter()
            .filter_map(|e| <[u8; 32]>::try_from(e.key().as_slice()).ok())
            .collect()
    }

    /// 添加/更新一个带元数据的对等（名称/物理地址/email/mobile/gps）——运行时生效 + 持久化。
    /// 连接一律按 node id（公钥）发现；`info.address` 仅为物理位置元数据。重复 id 即为编辑(覆盖)。
    pub fn add_peer_full(&self, node_id: [u8; 32], info: PeerInfo) -> Result<(), NodeError> {
        let a =
            nm_transport::addr_from_id(node_id).map_err(|e| NodeError::Other(e.to_string()))?;
        self.peers.insert(node_id.to_vec(), a);
        self.peer_info.insert(node_id.to_vec(), info.clone());
        if let Some(s) = &self.store {
            if let Ok(js) = serde_json::to_string(&info) {
                if let Err(e) = s.put_peer(&node_id, &js) {
                    tracing::warn!("persist peer failed: {e}");
                }
            }
        }
        Ok(())
    }

    /// 标注一个对等为手工/种子来源（不改拨号信息，仅登记 peer_info）：
    /// 用于配置文件 `[[peers]]` 种子——使其在管理台显示为「手工」+ 所属联邦，且不被自动发现降级/清扫。
    pub fn note_manual_peer(&self, node_id: [u8; 32], federation: &str) {
        let mut info = self
            .peer_info
            .get(&node_id[..])
            .map(|i| i.clone())
            .unwrap_or_default();
        info.source = "manual".to_string();
        info.federation = federation.to_string();
        self.peer_info.insert(node_id.to_vec(), info);
    }

    /// 移除一个对等（运行时 + 持久化）。返回是否存在过。
    pub fn remove_peer(&self, node_id: [u8; 32]) -> bool {
        let existed = self.peers.remove(&node_id[..]).is_some();
        self.peer_info.remove(&node_id[..]);
        if let Some(s) = &self.store {
            let _ = s.remove_peer(&node_id);
        }
        existed
    }

    /// 对等详情（node id hex + 元数据）——管理台列表用。
    pub fn peers_detail(&self) -> Vec<(String, PeerInfo)> {
        self.peers
            .iter()
            .map(|e| {
                let id = e.key();
                let hexid: String = id.iter().map(|x| format!("{x:02x}")).collect();
                let info = self.peer_info.get(id).map(|i| i.clone()).unwrap_or_default();
                (hexid, info)
            })
            .collect()
    }

    /// 存储统计（实体/群组数，内存计数，与持久层一致）。
    pub fn store_stats(&self) -> StoreStat {
        StoreStat {
            entities: self.dir.len(),
            groups: self.groups.len(),
        }
    }

    /// 踢下线：断开某公钥的会话，但**不**拉黑（区别于 `ban`）。返回是否有会话被断。
    pub fn kick(&self, pubkey: [u8; 32]) -> bool {
        self.sessions_meta.remove(&pubkey[..]);
        if let Some((_, conn)) = self.sessions.remove(&pubkey[..]) {
            conn.close(0u32.into(), b"kicked");
            tracing::info!(online = self.sessions.len(), "peer kicked");
            true
        } else {
            false
        }
    }

    /// 服务：用 iroh `Router` 统一分流 accept —— `nmspace/0` 走单播/命令/联邦处理，
    /// `/iroh-gossip/1` 走 gossip 频道；二者共用同一 endpoint。阻塞至进程结束。
    pub async fn serve(&self) -> Result<(), NodeError> {
        tracing::info!(node = %self.ep.id().fmt_short(), "nm-node serving");
        let nmspace = ImspaceProto {
            ctx: Ctx {
                ep: self.ep.clone(),
                dir: self.dir.clone(),
                sessions: self.sessions.clone(),
                groups: self.groups.clone(),
                store: self.store.clone(),
                peers: self.peers.clone(),
                node_id: self.ep.id_bytes(),
                blacklist: self.blacklist.clone(),
                sessions_meta: self.sessions_meta.clone(),
                presence: self.presence.clone(),
                status_intent: self.status_intent.clone(),
                channels: self.channels.clone(),
                gossip: self.gossip.clone(),
                group_pub: self.group_pub.clone(),
                domains: self.domains.clone(),
                names: self.names.clone(),
            },
        };
        let gossip_gate = GossipGate {
            gossip: self.gossip.clone(),
            blacklist: self.blacklist.clone(),
        };
        // Router::spawn 会把两个 ALPN 一并注册到 endpoint（覆盖 bind 时的单 ALPN）。
        let _router = Router::builder(self.ep.iroh().clone())
            .accept(nm_transport::ALPN, nmspace)
            .accept(iroh_gossip::ALPN, gossip_gate)
            .spawn();
        // 持有 router 并阻塞，保持「serve 跑到进程结束」的既有契约
        //（serve 任务被弃或运行时关停时，_router 析构 → Router 停机）。
        std::future::pending::<()>().await;
        Ok(())
    }

    /// 加入一个频道，返回可 `publish`/`recv` 的句柄。`bootstrap` 为已知对端节点公钥（可空）。
    /// 需先 `serve()`（Router 起来后才能收发 gossip）；LAN/无发现时还需先 `add_peer_addr` 播种地址。
    pub async fn join_channel(
        &self,
        channel: [u8; 32],
        bootstrap: Vec<[u8; 32]>,
    ) -> Result<nm_gossip::ChannelTopic, NodeError> {
        nm_gossip::ChannelHub::new(self.gossip.clone())
            .join(channel, bootstrap)
            .await
            .map_err(|e| NodeError::Other(e.to_string()))
    }

    // ---- 联邦成员自动发现（gossip 成员频道）----

    /// 本节点成员频道的 topic（`blake3("nmspace-federation:<federation>")`）。
    fn membership_channel(federation: &str) -> [u8; 32] {
        nm_crypto::content_hash(format!("nmspace-federation:{federation}").as_bytes())
    }

    /// 用本节点私钥签发一张自身成员卡片，序列化为待广播的 JSON 字节。
    fn membership_card(&self, card: &PeerInfo) -> Vec<u8> {
        let node_id = self.ep.id_bytes();
        let ts = now_secs();
        let sig = nm_crypto::sign_bytes(
            self.ep.secret_key(),
            &membership_signing_bytes(&node_id, ts, card),
        );
        let ann = MembershipAnnounce {
            node_id: hex_encode(&node_id),
            ts,
            info: card.clone(),
            addr: nm_transport::addr_to_string(&self.addr()),
            sig: hex_encode(&sig),
        };
        serde_json::to_vec(&ann).unwrap_or_default()
    }

    /// 处理一条收到的成员广播：验签(按声明的 node_id) → 反重放窗口 → 学习/刷新。
    fn on_membership_msg(&self, content: &[u8], federation: &str) {
        let ann: MembershipAnnounce = match serde_json::from_slice(content) {
            Ok(a) => a,
            Err(_) => return,
        };
        let node_id = match hex_decode_n::<32>(&ann.node_id) {
            Some(x) => x,
            None => return,
        };
        // 不学自己；黑名单（永久排除）直接跳过。
        if node_id == self.ep.id_bytes() || self.blacklist.contains(&node_id) {
            return;
        }
        // 验签：gossip 的 delivered_from 非原作者，必须按声明 node_id 验应用层签名。
        let sig = match hex_decode_n::<64>(&ann.sig) {
            Some(s) => s,
            None => return,
        };
        let signing = membership_signing_bytes(&node_id, ann.ts, &ann.info);
        if nm_crypto::verify_bytes(&node_id, &signing, &sig).is_err() {
            tracing::debug!(peer = %ann.node_id, "membership: bad signature, dropped");
            return;
        }
        // 反重放/防陈旧：拒绝过旧(>1 天)或来自明显未来(>5 分钟)的卡片。
        let now = now_secs();
        if ann.ts + 86_400 < now || ann.ts > now + 300 {
            return;
        }
        // 拨号提示仅当其内嵌 id 与声明 node_id 一致才采用（addr 未签名，防注入错配）。
        let addr_hint = nm_transport::addr_from_string(&ann.addr)
            .ok()
            .filter(|a| *a.id.as_bytes() == node_id);
        self.learn_peer(node_id, ann.info, addr_hint, federation);
    }

    /// 发现专用 upsert：不降级手工/种子 peer（仅刷新存活）；否则登记为 discovered 并持久化。
    /// `federation` 为听到该广播的频道(=本节点联邦)，标注到 peer 的联邦属性。
    fn learn_peer(
        &self,
        node_id: [u8; 32],
        info: PeerInfo,
        addr_hint: Option<nm_transport::Addr>,
        federation: &str,
    ) {
        // 已是手工/种子（source 为空或 "manual"）：保留其元数据，仅刷新 last_seen（联邦补空）。
        if let Some(existing) = self.peer_info.get(&node_id[..]) {
            if existing.source.is_empty() || existing.source == "manual" {
                let mut e = existing.clone();
                drop(existing);
                e.last_seen = now_secs();
                if e.federation.is_empty() {
                    e.federation = federation.to_string();
                }
                self.persist_peer(node_id, e);
                return;
            }
        }
        // 加入 peers（可拨号）：有 addr 提示优先（LAN 也能拨），否则按 id（N0 发现解析）。
        match addr_hint {
            Some(a) => self.add_peer_addr(a),
            None => {
                let _ = self.add_peer_by_id(node_id);
            }
        }
        let mut info = info;
        info.source = "discovered".to_string();
        info.last_seen = now_secs();
        info.federation = federation.to_string();
        self.persist_peer(node_id, info);
    }

    /// 写入 peer_info + 持久化（供 learn_peer 复用）。
    fn persist_peer(&self, node_id: [u8; 32], info: PeerInfo) {
        self.peer_info.insert(node_id.to_vec(), info.clone());
        if let Some(s) = &self.store {
            if let Ok(js) = serde_json::to_string(&info) {
                if let Err(e) = s.put_peer(&node_id, &js) {
                    tracing::warn!("persist discovered peer failed: {e}");
                }
            }
        }
    }

    /// 清扫过期的 discovered peer（超过 TTL 未再广播）；manual/种子 peer 不受影响。
    fn sweep_discovered(&self, ttl_secs: u64) {
        let now = now_secs();
        let stale: Vec<[u8; 32]> = self
            .peer_info
            .iter()
            .filter(|e| {
                let i = e.value();
                i.source == "discovered"
                    && i.last_seen != 0
                    && now.saturating_sub(i.last_seen) > ttl_secs
            })
            .filter_map(|e| <[u8; 32]>::try_from(e.key().as_slice()).ok())
            .collect();
        for id in stale {
            self.remove_peer(id);
            tracing::info!(peer = %hex_encode(&id[..4]), "discovered peer expired (TTL)");
        }
    }

    /// 后台联邦成员发现：加入成员频道，周期广播自身卡片、收播他人卡片(验签后入表)、TTL 清扫。
    /// 镜像 `spawn_federation_sync`。内部对 join 做重试以吸收 `serve()` 顺序（Router 起来后才能收发）。
    pub fn spawn_membership(self: Arc<Self>, cfg: MembershipCfg) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let channel = Self::membership_channel(&cfg.federation);
            let announce_iv = std::time::Duration::from_secs(cfg.announce_interval_secs.max(5));
            let ttl_secs = cfg.ttl_secs.max(announce_iv.as_secs() * 3);
            // 重试 join：serve() 的 Router 起来后 gossip 才能收发。
            let mut topic = loop {
                match self.join_channel(channel, self.peers_list()).await {
                    Ok(t) => break t,
                    Err(e) => {
                        tracing::debug!("membership join retry: {e}");
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                }
            };
            tracing::info!(federation = %cfg.federation, "membership gossip joined");
            // 先立即广播一次，加速被发现。
            let _ = topic.publish(self.membership_card(&cfg.card)).await;
            let mut last_announce = tokio::time::Instant::now();
            let mut last_sweep = tokio::time::Instant::now();
            loop {
                if last_announce.elapsed() >= announce_iv {
                    if let Err(e) = topic.publish(self.membership_card(&cfg.card)).await {
                        tracing::debug!("membership publish failed: {e}");
                    }
                    last_announce = tokio::time::Instant::now();
                }
                // 带 1s 超时的收播：既能及时收，又能周期回到顶部广播/清扫。
                match tokio::time::timeout(std::time::Duration::from_secs(1), topic.recv()).await {
                    Ok(Some(msg)) => self.on_membership_msg(&msg.content, &cfg.federation),
                    Ok(None) => {
                        // 频道关闭：重新加入。
                        match self.join_channel(channel, self.peers_list()).await {
                            Ok(t) => topic = t,
                            Err(_) => {
                                tokio::time::sleep(std::time::Duration::from_secs(2)).await
                            }
                        }
                    }
                    Err(_) => {} // 超时：正常，回到循环顶部。
                }
                if last_sweep.elapsed() >= announce_iv {
                    self.sweep_discovered(ttl_secs);
                    last_sweep = tokio::time::Instant::now();
                }
            }
        })
    }

    // ---- 群联邦 gossip（发现 + 消息扇出，取代跨节点 s2s 中继）----

    /// 群联邦主题（所有节点加入，独立于成员/presence 频道）。
    fn group_channel(federation: &str) -> [u8; 32] {
        nm_crypto::content_hash(format!("nmspace-groups:{federation}").as_bytes())
    }

    /// LWW 合并群状态（联邦发现：各节点据此填充群目录 + 成员表）；较旧不覆盖较新。
    fn merge_group_lww(&self, g: Group) {
        use dashmap::mapref::entry::Entry;
        match self.groups.entry(g.group_id.clone()) {
            Entry::Occupied(mut o) => {
                if g.updated_at > o.get().updated_at {
                    o.insert(g.clone());
                    if let Some(s) = &self.store {
                        let _ = s.put_group(&g);
                    }
                }
            }
            Entry::Vacant(v) => {
                v.insert(g.clone());
                if let Some(s) = &self.store {
                    let _ = s.put_group(&g);
                }
            }
        }
    }

    /// 从配置设置本节点域名（逗号分隔多个）；持久化并去重。nmd 启动时调用。
    pub fn set_domain(&self, csv: String) {
        let list: Vec<String> = csv.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
        if list.is_empty() {
            return;
        }
        let mut g = self.domains.write().unwrap();
        for d in list {
            if !g.contains(&d) {
                g.push(d.clone());
            }
            if let Some(s) = &self.store {
                let _ = s.put_domain(&d);
            }
        }
        tracing::info!(domains = ?*g, "node domains set");
    }

    /// 本节点拥有的域名列表（去中心命名 N1）。
    pub fn owned_domains(&self) -> Vec<String> {
        self.domains.read().unwrap().clone()
    }

    /// 申请（TOFU 认领）一个域名到本节点：校验语法、去重、持久化；返回是否新增。
    /// N1 无全局唯一约束——若命名缓存里已见其它节点为该域签发过记录，软性拒绝（全局唯一见 N2）。
    pub fn add_domain(&self, domain: &str) -> Result<bool, String> {
        let d = domain.trim().to_lowercase();
        if d.is_empty() || d.len() > 253 || !d.contains('.')
            || !d.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        {
            return Err("非法域名（形如 example.nm）".into());
        }
        if self.domains.read().unwrap().contains(&d) {
            return Ok(false);
        }
        let me = self.ep.id_bytes();
        if self.names.iter().any(|r| r.domain == d && r.home_node.as_slice() != me.as_slice()) {
            return Err(format!("域名 {d} 似乎已被联邦中其它节点占用（N1=TOFU；全局唯一见 N2）"));
        }
        self.domains.write().unwrap().push(d.clone());
        if let Some(s) = &self.store {
            let _ = s.put_domain(&d);
        }
        tracing::info!(domain = %d, "domain claimed (TOFU)");
        Ok(true)
    }

    /// 本节点签发的命名记录（可按域名过滤；不含墓碑）。
    pub fn names_owned(&self, domain: Option<&str>) -> Vec<NameRecord> {
        let me = self.ep.id_bytes();
        self.names
            .iter()
            .filter(|r| r.home_node.as_slice() == me.as_slice() && !r.client_pubkey.is_empty())
            .filter(|r| domain.map(|d| r.domain == d).unwrap_or(true))
            .map(|r| r.clone())
            .collect()
    }

    /// 管理端：在本节点某域名下绑定 local_part → pubkey（注册商权威）。签名 + 持久 + 广播。
    pub fn admin_set_name(&self, domain: &str, local_part: &str, pubkey: [u8; 32]) -> Result<NameRecord, String> {
        let d = domain.trim().to_lowercase();
        let lp = local_part.trim().to_lowercase();
        if !self.domains.read().unwrap().contains(&d) {
            return Err(format!("本节点未拥有域名 {d}"));
        }
        if !valid_local_part(&lp) {
            return Err("非法 local-part（仅 a-z 0-9 . - _，≤63）".into());
        }
        let full = format!("{lp}@{d}");
        let serial = self.names.get(&full).map(|r| r.serial + 1).unwrap_or(1);
        let mut rec = NameRecord {
            local_part: lp, domain: d, client_pubkey: pubkey.to_vec(), serial,
            issued_at: now_ms() as i64, ttl: 3600, home_node: self.ep.id_bytes().to_vec(), home_sig: Vec::new(),
        };
        rec.home_sig = nm_crypto::sign_bytes(self.ep.secret_key(), &name_canonical(&rec)).to_vec();
        if let Some(s) = &self.store {
            let _ = s.put_name(&rec);
        }
        self.names.insert(full, rec.clone());
        self.broadcast_name(&rec);
        Ok(rec)
    }

    /// 管理端：删除某域名下的 local_part（墓碑：空公钥 + 更高 serial，经 gossip 扩散移除）。
    pub fn admin_del_name(&self, domain: &str, local_part: &str) -> Result<(), String> {
        let d = domain.trim().to_lowercase();
        let lp = local_part.trim().to_lowercase();
        if !self.domains.read().unwrap().contains(&d) {
            return Err(format!("本节点未拥有域名 {d}"));
        }
        let full = format!("{lp}@{d}");
        let serial = self.names.get(&full).map(|r| r.serial + 1).ok_or("该名字不存在")?;
        let mut rec = NameRecord {
            local_part: lp, domain: d, client_pubkey: Vec::new(), serial, // 空公钥 = 墓碑
            issued_at: now_ms() as i64, ttl: 0, home_node: self.ep.id_bytes().to_vec(), home_sig: Vec::new(),
        };
        rec.home_sig = nm_crypto::sign_bytes(self.ep.secret_key(), &name_canonical(&rec)).to_vec();
        if let Some(s) = &self.store {
            let _ = s.del_name(&full);
        }
        self.names.remove(&full);
        self.broadcast_name(&rec);
        Ok(())
    }

    fn broadcast_name(&self, rec: &NameRecord) {
        let gg = GroupGossip {
            origin: self.ep.id_bytes().to_vec(),
            body: Some(nm_proto::pb::group_gossip::Body::Name(rec.clone())),
        };
        let _ = self.group_pub.send(gg.encode_to_vec());
    }

    /// 合并命名记录到缓存（gossip 学到的他域记录仅入内存；验签通过 + serial 更新才收）。
    fn merge_name_lww(&self, rec: NameRecord) {
        if !verify_name(&rec) {
            tracing::debug!("name: bad signature, dropped");
            return;
        }
        let key = format!("{}@{}", rec.local_part, rec.domain);
        // 墓碑（空公钥）：serial 不更旧则移除缓存/持久。
        if rec.client_pubkey.is_empty() {
            if let Some(e) = self.names.get(&key) {
                if rec.serial < e.serial {
                    return;
                }
            }
            self.names.remove(&key);
            if let Some(s) = &self.store {
                let _ = s.del_name(&key);
            }
            return;
        }
        use dashmap::mapref::entry::Entry;
        match self.names.entry(key) {
            Entry::Occupied(mut o) => {
                if rec.serial >= o.get().serial {
                    o.insert(rec);
                }
            }
            Entry::Vacant(v) => {
                v.insert(rec);
            }
        }
    }

    /// 处理群 gossip：announce→合并群状态；msg→投递本地成员。
    async fn on_group_gossip(&self, bytes: &[u8]) {
        let Ok(gg) = GroupGossip::decode(bytes) else {
            return;
        };
        if gg.origin.as_slice() == self.ep.id_bytes().as_slice() {
            return; // 自回环忽略
        }
        match gg.body {
            Some(nm_proto::pb::group_gossip::Body::Announce(g)) => self.merge_group_lww(g),
            Some(nm_proto::pb::group_gossip::Body::Msg(gram)) => {
                let Some(group) = self.groups.get(&gram.receiver).map(|g| g.clone()) else {
                    return; // 尚未学到该群（announce 未到）→ 跳过；周期公告后续会补
                };
                for m in &group.members {
                    if self.sessions.contains_key(m.as_slice()) {
                        try_push(m, &gram, &self.sessions).await;
                    } else if let Ok(Some(e)) = self.dir.get(m).await {
                        if e.home_node == self.ep.id_bytes().as_slice() {
                            if let Some(s) = &self.store {
                                let _ = s.push_inbox(m, &gram);
                            }
                        }
                    }
                }
            }
            Some(nm_proto::pb::group_gossip::Body::Direct(gram)) => {
                // 私聊单播：仅当本节点负责该收件人时投递（在线直投 / 本节点为其 home 则离线入库）。
                let to = &gram.receiver;
                if self.sessions.contains_key(to.as_slice()) {
                    try_push(to, &gram, &self.sessions).await;
                } else if let Ok(Some(e)) = self.dir.get(to).await {
                    if e.home_node == self.ep.id_bytes().as_slice() {
                        if let Some(s) = &self.store {
                            let _ = s.push_inbox(to, &gram);
                        }
                    }
                }
            }
            Some(nm_proto::pb::group_gossip::Body::Name(rec)) => {
                // 命名记录复制：验签后并入命名缓存（各节点据此解析 local@domain）。
                self.merge_name_lww(rec);
            }
            None => {}
        }
    }

    /// 起群联邦同步：加入 `nmspace-groups:<fed>`；周期广播本节点归属群(发现)；收播 announce/msg；
    /// 并把 fanout 丢来的待广播项(群消息/即时公告)发布出去。需先 `serve()`。
    pub fn spawn_group_sync(self: Arc<Self>, federation: String) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let channel = Self::group_channel(&federation);
            let mut topic = loop {
                match self.join_channel(channel, self.peers_list()).await {
                    Ok(t) => break t,
                    Err(e) => {
                        tracing::debug!("group sync join retry: {e}");
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                }
            };
            tracing::info!(%federation, "group gossip joined");
            let mut pub_rx = match self.group_pub_rx.lock().unwrap().take() {
                Some(rx) => rx,
                None => {
                    tracing::warn!("group sync already running");
                    return;
                }
            };
            let announce_iv = std::time::Duration::from_secs(20);
            let mut last_announce = tokio::time::Instant::now() - announce_iv; // 立即先广播一次
            loop {
                // 发布 fanout 丢来的待广播项（群消息 / 变更即时公告）。
                while let Ok(bytes) = pub_rx.try_recv() {
                    let _ = topic.publish(bytes).await;
                }
                // 周期广播本节点归属群（发现 + 成员表收敛）。
                if last_announce.elapsed() >= announce_iv {
                    let me = self.ep.id_bytes();
                    let mine: Vec<Group> = self
                        .groups
                        .iter()
                        .filter(|g| g.home_node.as_slice() == me.as_slice())
                        .map(|g| g.clone())
                        .collect();
                    for g in mine {
                        let gg = GroupGossip {
                            origin: me.to_vec(),
                            body: Some(nm_proto::pb::group_gossip::Body::Announce(g)),
                        };
                        let _ = topic.publish(gg.encode_to_vec()).await;
                    }
                    // 周期重播本节点签发的命名记录（home_node==自身），令晚加入节点补全命名缓存。
                    let my_names: Vec<NameRecord> = self
                        .names
                        .iter()
                        .filter(|r| r.home_node.as_slice() == me.as_slice())
                        .map(|r| r.clone())
                        .collect();
                    for r in my_names {
                        let gg = GroupGossip {
                            origin: me.to_vec(),
                            body: Some(nm_proto::pb::group_gossip::Body::Name(r)),
                        };
                        let _ = topic.publish(gg.encode_to_vec()).await;
                    }
                    last_announce = tokio::time::Instant::now();
                }
                match tokio::time::timeout(std::time::Duration::from_millis(300), topic.recv()).await {
                    Ok(Some(msg)) => self.on_group_gossip(&msg.content).await,
                    Ok(None) => match self.join_channel(channel, self.peers_list()).await {
                        Ok(t) => topic = t,
                        Err(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
                    },
                    Err(_) => {}
                }
            }
        })
    }

    // ---- 在线状态 presence（gossip + TTL；临时，不进持久目录）----

    /// presence 频道 topic（独立于成员频道）。
    fn presence_channel(federation: &str) -> [u8; 32] {
        nm_crypto::content_hash(format!("nmspace-presence:{federation}").as_bytes())
    }

    /// presence 卡片签名字节：home_node ‖ entity ‖ ts ‖ status（0 分隔）。由 home_node 私钥签发。
    fn presence_signing_bytes(home: &[u8; 32], entity: &[u8], ts: u64, status: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(home);
        b.push(0);
        b.extend_from_slice(entity);
        b.push(0);
        b.extend_from_slice(&ts.to_be_bytes());
        b.push(0);
        b.extend_from_slice(status.as_bytes());
        b
    }

    /// 设置某实体的状态意图（away/busy/dnd…；online/空=清除）。广播时采用；本地即时刷新。
    pub fn set_status(&self, entity: &[u8], status: &str) {
        let s = status.trim();
        if s.is_empty() || s == "online" {
            self.status_intent.remove(entity);
        } else {
            self.status_intent.insert(entity.to_vec(), s.to_string());
        }
        if self.sessions.contains_key(entity) {
            self.presence.insert(
                entity.to_vec(),
                PresenceRec {
                    status: if s.is_empty() { "online".into() } else { s.to_string() },
                    last_seen: now_secs(),
                    home_node: self.ep.id_bytes().to_vec(),
                },
            );
        }
    }

    /// 计算某实体对外状态（管理台/自查用）。
    pub fn presence_of(&self, entity: &[u8]) -> String {
        presence_status(&self.sessions, &self.presence, &self.status_intent, entity)
    }

    /// 处理收到的 presence 广播：验签(按 home_node) → 反陈旧 → LWW 入缓存；不覆盖本地在线。
    fn on_presence_msg(&self, content: &[u8]) {
        let ann: PresenceAnnounce = match serde_json::from_slice(content) {
            Ok(a) => a,
            Err(_) => return,
        };
        let (home, entity) = match (hex_decode_n::<32>(&ann.home_node), hex_decode_n::<32>(&ann.entity)) {
            (Some(h), Some(e)) => (h, e),
            _ => return,
        };
        if self.blacklist.contains(&entity) || self.blacklist.contains(&home) {
            return;
        }
        let sig = match hex_decode_n::<64>(&ann.sig) {
            Some(s) => s,
            None => return,
        };
        let signing = Self::presence_signing_bytes(&home, &entity, ann.ts, &ann.status);
        if nm_crypto::verify_bytes(&home, &signing, &sig).is_err() {
            return;
        }
        let now = now_secs();
        if ann.ts + 300 < now || ann.ts > now + 300 {
            return; // 反陈旧/未来
        }
        if self.sessions.contains_key(&entity[..]) {
            return; // 本地在线会话权威，不被远端覆盖
        }
        if let Some(cur) = self.presence.get(&entity[..]) {
            if cur.last_seen >= ann.ts {
                return; // LWW：仅接受更新的
            }
        }
        self.presence.insert(
            entity.to_vec(),
            PresenceRec { status: ann.status, last_seen: ann.ts, home_node: home.to_vec() },
        );
    }

    /// 后台 presence：周期广播本地在线会话状态；收播他人状态入缓存（TTL 在读时判定）。镜像 spawn_membership。
    pub fn spawn_presence(self: Arc<Self>, federation: String) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let channel = Self::presence_channel(&federation);
            let mut topic = loop {
                match self.join_channel(channel, self.peers_list()).await {
                    Ok(t) => break t,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
                }
            };
            tracing::info!(%federation, "presence gossip joined");
            let interval = std::time::Duration::from_secs(PRESENCE_INTERVAL_SECS);
            let mut last_bcast = tokio::time::Instant::now() - interval;
            loop {
                if last_bcast.elapsed() >= interval {
                    let home = self.ep.id_bytes();
                    let ts = now_secs();
                    // 守卫不跨 await：先收集本地会话公钥。
                    let entities: Vec<Vec<u8>> = self.sessions.iter().map(|e| e.key().clone()).collect();
                    for ent in entities {
                        let status = self
                            .status_intent
                            .get(&ent)
                            .map(|s| s.clone())
                            .unwrap_or_else(|| "online".to_string());
                        self.presence.insert(
                            ent.clone(),
                            PresenceRec { status: status.clone(), last_seen: ts, home_node: home.to_vec() },
                        );
                        let sig = nm_crypto::sign_bytes(
                            self.ep.secret_key(),
                            &Self::presence_signing_bytes(&home, &ent, ts, &status),
                        );
                        let ann = PresenceAnnounce {
                            entity: hex_encode(&ent),
                            status,
                            ts,
                            home_node: hex_encode(&home),
                            sig: hex_encode(&sig),
                        };
                        let _ = topic.publish(serde_json::to_vec(&ann).unwrap_or_default()).await;
                    }
                    last_bcast = tokio::time::Instant::now();
                }
                match tokio::time::timeout(std::time::Duration::from_secs(1), topic.recv()).await {
                    Ok(Some(msg)) => self.on_presence_msg(&msg.content),
                    Ok(None) => match self.join_channel(channel, self.peers_list()).await {
                        Ok(t) => topic = t,
                        Err(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
                    },
                    Err(_) => {}
                }
            }
        })
    }
}

/// 计算实体对外状态：本地在线会话优先（权威，取状态意图或 online）；否则查 presence 缓存并按 TTL 判离线。
fn presence_status(sessions: &Sessions, presence: &PresenceMap, intent: &StatusIntent, entity: &[u8]) -> String {
    if sessions.contains_key(entity) {
        return intent.get(entity).map(|s| s.clone()).unwrap_or_else(|| "online".to_string());
    }
    if let Some(r) = presence.get(entity) {
        if now_secs().saturating_sub(r.last_seen) <= PRESENCE_TTL_SECS {
            return r.status.clone();
        }
    }
    "offline".to_string()
}

// ── 频道 / 主题（P4）：iroh-gossip 上的开放 pub/sub ──

/// 频道 gossip 主题 seed：blake3("nmspace:channel:" + hex(channel_id))。
fn channel_topic(channel_id: &[u8]) -> [u8; 32] {
    nm_crypto::content_hash(format!("nmspace:channel:{}", hex_encode(channel_id)).as_bytes())
}

/// 确保频道已 join gossip 并起收播任务（幂等）。首次订阅时调用。
async fn ensure_channel(ctx: &Ctx, channel_id: &[u8], meta: Option<Channel>) -> bool {
    if ctx.channels.contains_key(channel_id) {
        return true;
    }
    let bootstrap: Vec<[u8; 32]> = ctx
        .peers
        .iter()
        .filter_map(|e| <[u8; 32]>::try_from(e.key().as_slice()).ok())
        .collect();
    let topic = match nm_gossip::ChannelHub::new(ctx.gossip.clone())
        .join(channel_topic(channel_id), bootstrap)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("channel join failed: {e}");
            return false;
        }
    };
    let subs = Arc::new(DashSet::new());
    let log = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let seq = Arc::new(std::sync::atomic::AtomicU64::new(1));
    let (pub_tx, pub_rx) = tokio::sync::mpsc::unbounded_channel();
    let meta = Arc::new(std::sync::Mutex::new(
        meta.unwrap_or(Channel { channel_id: channel_id.to_vec(), ..Default::default() }),
    ));
    tokio::spawn(run_channel(
        channel_id.to_vec(),
        topic,
        subs.clone(),
        log.clone(),
        ctx.sessions.clone(),
        pub_rx,
        meta.clone(),
    ));
    ctx.channels.insert(channel_id.to_vec(), ChannelEntry { meta, subs, log, seq, pub_tx });
    tracing::info!("channel joined");
    true
}

/// 频道后台任务：发布队列 + gossip 收播；两路都「追加日志 + 转发本地订阅会话」。
async fn run_channel(
    channel_id: Vec<u8>,
    mut topic: nm_gossip::ChannelTopic,
    subs: Arc<DashSet<Vec<u8>>>,
    log: Arc<std::sync::Mutex<std::collections::VecDeque<ChannelMsg>>>,
    sessions: Arc<Sessions>,
    mut pub_rx: tokio::sync::mpsc::UnboundedReceiver<ChannelOut>,
    meta: Arc<std::sync::Mutex<Channel>>,
) {
    use nm_proto::pb::channel_gram::Kind;
    let mut last_meta_bcast = std::time::Instant::now();
    let meta_bcast_every = std::time::Duration::from_secs(30);
    loop {
        while let Ok(out) = pub_rx.try_recv() {
            match out {
                ChannelOut::Msg(m) => {
                    let env = ChannelGram { kind: Some(Kind::Msg(m.clone())) };
                    let _ = topic.publish(env.encode_to_vec()).await; // 广播到主题网格
                    channel_ingest(&m, &log, &subs, &sessions).await; // 本地落库 + 转发
                }
                ChannelOut::Meta(c) => {
                    let env = ChannelGram { kind: Some(Kind::Meta(c)) };
                    let _ = topic.publish(env.encode_to_vec()).await;
                    last_meta_bcast = std::time::Instant::now();
                }
            }
        }
        // owner / 已知元信息者周期重播，令晚订阅者补全频道名称/简介/头像。
        if last_meta_bcast.elapsed() >= meta_bcast_every {
            last_meta_bcast = std::time::Instant::now();
            let snap = meta.lock().unwrap().clone();
            if !snap.name.is_empty() || !snap.topic.is_empty() || !snap.avatar_url.is_empty() {
                let env = ChannelGram { kind: Some(Kind::Meta(snap)) };
                let _ = topic.publish(env.encode_to_vec()).await;
            }
        }
        match tokio::time::timeout(std::time::Duration::from_millis(400), topic.recv()).await {
            Ok(Some(cm)) => {
                if let Ok(env) = ChannelGram::decode(cm.content.as_slice()) {
                    match env.kind {
                        Some(Kind::Msg(m)) if m.channel_id == channel_id => {
                            channel_ingest(&m, &log, &subs, &sessions).await;
                        }
                        Some(Kind::Meta(c)) if c.channel_id == channel_id => {
                            merge_channel_meta(&meta, &c);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// 合并收到的频道元信息到本地（仅用非空字段覆盖；owner/created_at 缺省不动）。
fn merge_channel_meta(meta: &std::sync::Mutex<Channel>, incoming: &Channel) {
    let mut m = meta.lock().unwrap();
    if !incoming.name.is_empty() {
        m.name = incoming.name.clone();
    }
    if !incoming.topic.is_empty() {
        m.topic = incoming.topic.clone();
    }
    if !incoming.avatar_url.is_empty() {
        m.avatar_url = incoming.avatar_url.clone();
    }
    if !incoming.owner.is_empty() {
        m.owner = incoming.owner.clone();
    }
    if !incoming.home_node.is_empty() {
        m.home_node = incoming.home_node.clone();
    }
    if incoming.created_at != 0 {
        m.created_at = incoming.created_at;
    }
}

/// 追加到频道日志(限长、去重) + 推送给本地订阅会话。
async fn channel_ingest(
    m: &ChannelMsg,
    log: &std::sync::Mutex<std::collections::VecDeque<ChannelMsg>>,
    subs: &DashSet<Vec<u8>>,
    sessions: &Sessions,
) {
    {
        let mut l = log.lock().unwrap();
        if l.iter().any(|x| x.channel_id == m.channel_id && x.sender == m.sender && x.seq == m.seq) {
            return; // gossip 可能重投，去重
        }
        l.push_back(m.clone());
        while l.len() > CHANNEL_LOG_CAP {
            l.pop_front();
        }
    }
    let gram = channel_gram(m);
    let targets: Vec<Vec<u8>> = subs.iter().map(|s| s.key().clone()).collect();
    for sid in targets {
        try_push(&sid, &gram, sessions).await;
    }
}

/// 把 ChannelMsg 包成一条 CHANNEL_PUBLISH gram（receiver=频道id，客户端据此归入频道流）。
fn channel_gram(m: &ChannelMsg) -> Gram {
    Gram {
        version: PROTOCOL_VERSION,
        kind: GramKind::ChannelPublish as i32,
        gram_id: m.seq,
        ref_gram_id: None,
        sender: m.sender.clone(),
        receiver: m.channel_id.clone(),
        timestamp_ms: m.ts as u64,
        payload: Some(Any { type_url: "text/plain".to_string(), value: m.body.clone().into_bytes() }),
        crc: Vec::new(),
    }
}

/// nmspace 单播/命令/联邦协议处理器：包住既有的每连接处理逻辑，注册到 `Router` 的 `nmspace/0`。
struct ImspaceProto {
    ctx: Ctx,
}

impl std::fmt::Debug for ImspaceProto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImspaceProto")
    }
}

impl ProtocolHandler for ImspaceProto {
    async fn accept(&self, conn: IrohConnection) -> Result<(), AcceptError> {
        let rid_arr = *conn.remote_id().as_bytes();
        // 黑名单闸门：被禁公钥的连接直接拒绝（无白名单——其余一律放行）。
        if self.ctx.blacklist.contains(&rid_arr) {
            tracing::info!(peer = %conn.remote_id().fmt_short(), "rejected banned peer");
            conn.close(0u32.into(), b"banned");
            return Ok(());
        }
        let rid = rid_arr.to_vec();
        self.ctx.sessions.insert(rid.clone(), conn.clone());
        self.ctx
            .sessions_meta
            .insert(rid.clone(), SessionMeta { since: std::time::SystemTime::now() });
        tracing::info!(online = self.ctx.sessions.len(), "session up");

        // 连接关闭事件驱动地清理会话表（仅当表中仍是「这条」连接时移除，避免误删重连后的新会话）。
        {
            let sessions = self.ctx.sessions.clone();
            let watch_conn = conn.clone();
            let watch_rid = rid.clone();
            let closed_sid = watch_conn.stable_id();
            tokio::spawn(async move {
                let _ = watch_conn.closed().await;
                let stale = sessions
                    .get(&watch_rid)
                    .map(|c| c.stable_id() == closed_sid)
                    .unwrap_or(false);
                if stale {
                    sessions.remove(&watch_rid);
                    tracing::info!(online = sessions.len(), "session closed");
                }
            });
        }

        // 上线补投：把该实体的离线消息经 uni 流推送后清空。
        if let Some(store) = &self.ctx.store {
            if let Ok(pending) = store.drain_inbox(&rid) {
                if !pending.is_empty() {
                    let c = conn.clone();
                    tracing::info!(count = pending.len(), "delivering offline inbox");
                    tokio::spawn(async move {
                        for g in pending {
                            if let Ok(mut s) = c.open_uni().await {
                                let _ = write_gram(&mut s, &g).await;
                                let _ = s.finish();
                            }
                        }
                    });
                }
            }
        }

        // 运行该连接的 gram 处理循环，直到连接关闭（handle_conn 内部会清理会话表）。
        handle_conn(conn, self.ctx.clone(), rid).await;
        Ok(())
    }
}

/// gossip 连接的黑名单闸门：被禁公钥的 gossip 连接直接拒，其余转交 iroh-gossip 处理。
/// （注意：只能在**连接/邻居层**拦截；gossip 广播消息不带作者级签名，无法按原始作者逐条过滤。）
struct GossipGate {
    gossip: Gossip,
    blacklist: Arc<DashSet<[u8; 32]>>,
}

impl std::fmt::Debug for GossipGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GossipGate")
    }
}

impl ProtocolHandler for GossipGate {
    async fn accept(&self, conn: IrohConnection) -> Result<(), AcceptError> {
        let rid = *conn.remote_id().as_bytes();
        if self.blacklist.contains(&rid) {
            tracing::info!(peer = %conn.remote_id().fmt_short(), "rejected banned peer (gossip)");
            conn.close(0u32.into(), b"banned");
            return Ok(());
        }
        self.gossip
            .handle_connection(conn)
            .await
            .map_err(AcceptError::from_err)?;
        Ok(())
    }
}

async fn handle_conn(conn: IrohConnection, ctx: Ctx, rid: Vec<u8>) {
    loop {
        let (mut send, mut recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(_) => break, // 连接关闭
        };
        let gram = match read_gram(&mut recv).await {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("read gram failed: {e}");
                break;
            }
        };
        tracing::info!(kind = ?gram.kind(), gram_id = gram.gram_id, "recv gram");
        if let Some(reply) = handle_gram(&gram, &ctx, &rid).await {
            if let Err(e) = write_gram(&mut send, &reply).await {
                tracing::warn!("write reply failed: {e}");
            }
            let _ = send.finish();
        }
    }
    let my_sid = conn.stable_id();
    let stale = ctx.sessions.get(&rid).map(|c| c.stable_id() == my_sid).unwrap_or(false);
    if stale {
        ctx.sessions.remove(&rid);
        tracing::info!(online = ctx.sessions.len(), "session down");
    }
}

async fn handle_gram(gram: &Gram, ctx: &Ctx, caller: &[u8]) -> Option<Gram> {
    // 黑名单（纵深防御）：连接建立后才被拉黑的对端，其后续 gram 一律丢弃。
    if let Ok(c) = <[u8; 32]>::try_from(caller) {
        if ctx.blacklist.contains(&c) {
            return None;
        }
    }
    // 群消息：receiver 是 group_id，由节点扇出（不是直接路由目标）。
    // 扇出可能对每个离线/跨节点成员做目录查询/中继（各带网络超时），若同步等待会
    // 阻塞给发送方的 ack → 前端「发送很迟钝」。故后台扇出、立即回执（存转发语义）。
    if matches!(gram.kind(), GramKind::GroupMessage) {
        let (g, c, who) = (gram.clone(), ctx.clone(), caller.to_vec());
        tokio::spawn(async move { fanout_group(&g, &c, &who).await });
        return Some(receipt_for(gram));
    }
    // Relay 信封（来自对等节点的跨节点转发）：解出内层 gram，按本地投递。
    if matches!(gram.kind(), GramKind::Relay) {
        if let Some(inner) = gram.payload.as_ref().and_then(|p| Gram::decode(p.value.as_slice()).ok()) {
            deliver_or_store(&inner.receiver, &inner, ctx).await;
        }
        return Some(reply_gram(gram, GramKind::Receipt, None));
    }
    let to = &gram.receiver;
    let node_id = &ctx.node_id;
    let addressed_to_node = to.is_empty() || to.as_slice() == node_id.as_slice();
    if !addressed_to_node {
        // 路由命令若目标要求授权(require_grant)，先校验 Grant，不合格则拒绝并回推错误。
        if matches!(gram.kind(), GramKind::Command) {
            if let Ok(Some(target)) = ctx.dir.get(to).await {
                if target.attributes.get("require_grant").map(String::as_str) == Some("true") {
                    if let Err(reason) = authorize_routed(gram, caller, &target) {
                        tracing::warn!(%reason, "routed command denied");
                        push_command_denied(gram, caller, &ctx.sessions, &reason);
                        return Some(reply_gram(gram, GramKind::Receipt, None));
                    }
                }
            }
        }
        // Message 可离线暂存；Command/CommandResult 仅在线路由。
        // 单播私聊改走 gossip（本地投递不了则联邦广播，对端节点投递）——取代 NAT 下常不通的 s2s 中继。
        if matches!(gram.kind(), GramKind::Message) {
            let (g, c) = (gram.clone(), ctx.clone());
            tokio::spawn(async move { deliver_direct(&g, &c).await });
        } else {
            try_push(to, gram, &ctx.sessions).await;
        }
        return Some(reply_gram(gram, GramKind::Receipt, None)); // 给发送方回 ack
    }
    // 面向节点本身的请求。
    match gram.kind() {
        GramKind::Command => handle_command(gram, ctx, caller).await,
        GramKind::Message => Some(receipt_for(gram)),
        GramKind::Login => Some(login_ok(gram)),
        GramKind::Logout => {
            ctx.sessions.remove(caller); // 显式下线：同步移除会话
            tracing::info!(online = ctx.sessions.len(), "session logout");
            Some(reply_gram(gram, GramKind::Reply, None))
        }
        _ => None,
    }
}

/// 群消息扇出：`receiver` 为 group_id；对每个成员(除发送方)——在线则路由、离线则入库。
async fn fanout_group(gram: &Gram, ctx: &Ctx, caller: &[u8]) {
    let Some(group) = ctx.groups.get(&gram.receiver).map(|g| g.clone()) else {
        tracing::warn!("group message to unknown group");
        return;
    };
    // 仅群成员可发。
    if !group.members.iter().any(|m| m.as_slice() == caller) {
        tracing::warn!("non-member group message denied");
        return;
    }
    // 本地投递：本节点在线成员直投；本节点为其 home 的离线成员入库补投。
    deliver_group_here(gram, &group.members, caller, ctx).await;
    // 跨节点：发布到群联邦 gossip，各成员节点收到后各自投递「本地成员」。
    // 取代此前的 s2s 中继（NAT 下常超时不通）——与频道同走 gossip 叠加网。
    let gg = GroupGossip {
        origin: ctx.node_id.to_vec(),
        body: Some(nm_proto::pb::group_gossip::Body::Msg(gram.clone())),
    };
    let _ = ctx.group_pub.send(gg.encode_to_vec());
}

/// 把群消息投递给「本节点负责的成员」：在线本地会话直投；否则若本节点是其 home 则入离线库。
/// 每个成员仅由其所在/归属节点处理——无需依赖联邦目录同步（各节点权威掌握本地实体）。
async fn deliver_group_here(gram: &Gram, members: &[Vec<u8>], skip: &[u8], ctx: &Ctx) {
    for m in members {
        if m.as_slice() == skip {
            continue;
        }
        if ctx.sessions.contains_key(m.as_slice()) {
            try_push(m, gram, &ctx.sessions).await;
        } else if let Ok(Some(e)) = ctx.dir.get(m).await {
            if e.home_node == ctx.node_id.as_slice() {
                if let Some(s) = &ctx.store {
                    let _ = s.push_inbox(m, gram);
                }
            }
        }
    }
}

/// 立即向联邦广播某群当前状态（发现 + 成员表）。群变更后调用，避免等周期公告。
fn announce_group(ctx: &Ctx, gid: &[u8]) {
    if let Some(g) = ctx.groups.get(gid).map(|g| g.clone()) {
        let gg = GroupGossip {
            origin: ctx.node_id.to_vec(),
            body: Some(nm_proto::pb::group_gossip::Body::Announce(g)),
        };
        let _ = ctx.group_pub.send(gg.encode_to_vec());
    }
}

/// 命名记录规范字节（清空 home_sig 后编码，用于签名/验签的确定性输入）。
fn name_canonical(rec: &NameRecord) -> Vec<u8> {
    let mut r = rec.clone();
    r.home_sig = Vec::new();
    r.encode_to_vec()
}
/// 校验命名记录：home_sig 必须由 home_node 私钥对规范字节签名（自证明委派）。
fn verify_name(rec: &NameRecord) -> bool {
    let Ok(home) = <[u8; 32]>::try_from(rec.home_node.as_slice()) else { return false };
    let Ok(sig) = <[u8; 64]>::try_from(rec.home_sig.as_slice()) else { return false };
    nm_crypto::verify_bytes(&home, &name_canonical(rec), &sig).is_ok()
}
/// local-part 合法性：非空、≤63、仅 [a-z0-9._-]。
fn valid_local_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_')
}

/// 私聊单播投递（不依赖 s2s 中继）：
/// 1) 本地在线 → 直投；2) 已知且 home==本节点(离线) → 本地离线库；
/// 3) 已知远端 → 仅联邦 gossip 广播（对端节点投递/入库）；
/// 4) 完全未知 → 本地离线库兜底(可能连来本节点) + 联邦 gossip 广播(可能在远端)。
async fn deliver_direct(gram: &Gram, ctx: &Ctx) {
    let to = &gram.receiver;
    // 1) 本地在线会话直投。
    if ctx.sessions.contains_key(to.as_slice()) {
        try_push(to, gram, &ctx.sessions).await;
        return;
    }
    let known_remote = match ctx.dir.get(to).await {
        Ok(Some(e)) if e.home_node == ctx.node_id.as_slice() => {
            // 2) 收件人 home 在本节点、当前离线 → 入本地离线库，重连补投。
            if let Some(s) = &ctx.store {
                let _ = s.push_inbox(to, gram);
            }
            return;
        }
        Ok(Some(_)) => true, // 3) 已知远端
        _ => false,          // 4) 完全未知
    };
    if !known_remote {
        // 未知收件人：本地兜底入库（可能连来本节点），同时下面再联邦广播（可能在远端）。
        if let Some(s) = &ctx.store {
            let _ = s.push_inbox(to, gram);
        }
    }
    // 远端 / 未知：经联邦 gossip 广播；对端节点收到后投递（取代 s2s 中继）。
    let gg = GroupGossip {
        origin: ctx.node_id.to_vec(),
        body: Some(nm_proto::pb::group_gossip::Body::Direct(gram.clone())),
    };
    let _ = ctx.group_pub.send(gg.encode_to_vec());
}

/// 尝试把 gram 经 uni 流推送给某在线会话；返回是否投递成功。
/// 推送失败（连接已断但会话表尚未清理）则移除陈旧会话，交由上层落离线队列。
async fn try_push(to: &[u8], gram: &Gram, sessions: &Sessions) -> bool {
    let Some(target) = sessions.get(to).map(|r| r.clone()) else {
        return false;
    };
    if target.close_reason().is_some() {
        sessions.remove(to);
        return false;
    }
    let ok = async {
        let mut s = target.open_uni().await.ok()?;
        write_gram(&mut s, gram).await.ok()?;
        let _ = s.finish();
        Some(())
    }
    .await
    .is_some();
    if !ok {
        sessions.remove(to); // 陈旧会话，清理
        tracing::info!("stale session removed on push failure");
    }
    ok
}

/// 投递一份 gram 给目标实体：在线且推送成功→直达；否则落离线队列(若有 store)。
async fn deliver_or_store(to: &[u8], gram: &Gram, ctx: &Ctx) {
    // 1) 本地在线会话直投。
    if try_push(to, gram, &ctx.sessions).await {
        return;
    }
    // 2) 跨节点：目标实体归属其它节点 → Relay 转发给其归属节点。
    if let Ok(Some(target)) = ctx.dir.get(to).await {
        if !target.home_node.is_empty() && target.home_node != ctx.node_id.as_slice() {
            if let Some(addr) = ctx.peers.get(&target.home_node).map(|a| a.clone()) {
                if fed_relay(&ctx.ep, addr, gram).await.is_ok() {
                    tracing::info!("relayed to home node");
                    return;
                }
                tracing::warn!("relay to home node failed");
            }
        }
    }
    // 3) 离线：入库，重连补投。
    if let Some(s) = &ctx.store {
        if let Err(e) = s.push_inbox(to, gram) {
            tracing::warn!("push_inbox failed: {e}");
        } else {
            tracing::info!("target offline; stored to inbox");
        }
    } else {
        tracing::warn!("target offline; no store; dropping");
    }
}

async fn handle_command(gram: &Gram, ctx: &Ctx, caller: &[u8]) -> Option<Gram> {
    let dir = &*ctx.dir;
    let groups = &*ctx.groups;
    let store = &ctx.store;
    let cmd = Command::decode(gram.payload.as_ref()?.value.as_slice()).ok()?;
    let (ok, result, error) = match cmd.method.as_str() {
        "directory.register" | "profile.update" => match cmd
            .params
            .as_ref()
            .and_then(|p| Entity::decode(p.value.as_slice()).ok())
        {
            // 认证：只能注册/更新与连接身份一致的实体（防冒名）。
            Some(entity) if entity.entity_id != caller => {
                (false, None, "entity_id != connection identity".to_string())
            }
            Some(mut entity) => {
                let kind = entity.kind.clone();
                entity.home_node = ctx.node_id.to_vec(); // 归属本节点（联邦寻址）
                // LWW 收敛：拒绝比现有更旧的更新；仅接受时才持久化，避免旧副本回写存储。
                if dir.merge_lww(entity.clone()) {
                    if let Some(s) = store {
                        if let Err(e) = s.put_entity(&entity) {
                            tracing::warn!("persist entity failed: {e}");
                        }
                    }
                    tracing::info!(%kind, total = dir.len(), "entity upserted (register/profile.update)");
                    (true, None, String::new())
                } else {
                    (false, None, "stale update: older than current updated_at".to_string())
                }
            }
            None => (false, None, "invalid entity".to_string()),
        },
        "directory.query" => {
            let q = cmd
                .params
                .as_ref()
                .and_then(|p| DirectoryQuery::decode(p.value.as_slice()).ok())
                .unwrap_or_default();
            let mut list = dir.query(&q).await.unwrap_or_default();
            // P2：给每个实体盖上实时 presence（本地会话/gossip 缓存 + TTL）到 attributes["presence"]。
            // N1：反向盖上命名 name=local@domain（命名缓存里 client_pubkey 命中者）。
            let name_by_pk: std::collections::HashMap<Vec<u8>, String> = ctx
                .names
                .iter()
                .map(|r| (r.client_pubkey.clone(), format!("{}@{}", r.local_part, r.domain)))
                .collect();
            for e in list.iter_mut() {
                let p = presence_status(&ctx.sessions, &ctx.presence, &ctx.status_intent, &e.entity_id);
                e.attributes.insert("presence".to_string(), p);
                if let Some(n) = name_by_pk.get(&e.entity_id) {
                    e.attributes.insert("name".to_string(), n.clone());
                }
            }
            let el = EntityList { entities: list };
            (
                true,
                Some(Any {
                    type_url: "nmspace.v1.EntityList".to_string(),
                    value: el.encode_to_vec(),
                }),
                String::new(),
            )
        }
        // P2：设置本人在线状态（away/busy/dnd/online）；params.value = 状态字符串(UTF-8)。
        "presence.set" => {
            let status = cmd
                .params
                .as_ref()
                .map(|p| String::from_utf8_lossy(&p.value).trim().to_string())
                .unwrap_or_default();
            if status.is_empty() || status == "online" {
                ctx.status_intent.remove(caller);
            } else {
                ctx.status_intent.insert(caller.to_vec(), status.clone());
            }
            if ctx.sessions.contains_key(caller) {
                ctx.presence.insert(
                    caller.to_vec(),
                    PresenceRec {
                        status: if status.is_empty() { "online".into() } else { status },
                        last_seen: now_secs(),
                        home_node: ctx.node_id.to_vec(),
                    },
                );
            }
            (true, None, String::new())
        }
        // P1：内容寻址 blob —— 存头像等小媒体，档案只带 b3:hash，避免内联撑爆目录/gossip。
        "blob.put" => match cmd.params.as_ref().and_then(|p| BlobPut::decode(p.value.as_slice()).ok()) {
            Some(bp) if bp.data.len() > MAX_BLOB => (false, None, "blob too large".to_string()),
            Some(bp) => {
                let hash = nm_crypto::content_hash(&bp.data).to_vec();
                let blob = BlobData { hash: hash.clone(), data: bp.data, mime: bp.mime };
                match store {
                    Some(s) => match s.put_blob(&blob) {
                        Ok(()) => {
                            tracing::info!(bytes = blob.data.len(), "blob stored (content-addressed)");
                            (
                                true,
                                Some(Any {
                                    type_url: "nmspace.v1.BlobRef".to_string(),
                                    value: BlobRef { hash, home_node: ctx.node_id.to_vec() }.encode_to_vec(),
                                }),
                                String::new(),
                            )
                        }
                        Err(e) => (false, None, format!("store blob failed: {e}")),
                    },
                    None => (false, None, "node has no store".to_string()),
                }
            }
            None => (false, None, "invalid blob".to_string()),
        },
        "blob.get" => match cmd.params.as_ref().and_then(|p| BlobRef::decode(p.value.as_slice()).ok()) {
            Some(br) => {
                // 本地命中直接返回；未命中且带 home_node（非本节点）则 s2s 回源拉取并顺带缓存。
                let mut blob = store.as_ref().and_then(|s| s.get_blob(&br.hash).ok().flatten());
                if blob.is_none() && !br.home_node.is_empty() && br.home_node != ctx.node_id.as_slice() {
                    if let Some(addr) = ctx.peers.get(&br.home_node).map(|a| a.clone()) {
                        match s2s_blob_get(&ctx.ep, addr, &br.hash).await {
                            Ok(b) => {
                                if let Some(s) = store {
                                    let _ = s.put_blob(&b);
                                }
                                blob = Some(b);
                            }
                            Err(e) => tracing::warn!("blob relay to home node failed: {e}"),
                        }
                    }
                }
                match blob {
                    Some(b) => (
                        true,
                        Some(Any { type_url: "nmspace.v1.BlobData".to_string(), value: b.encode_to_vec() }),
                        String::new(),
                    ),
                    None => (false, None, "blob not found".to_string()),
                }
            }
            None => (false, None, "invalid ref".to_string()),
        },
        "group.create" => group_create(&cmd, groups, store, caller, ctx.node_id.as_slice()),
        "group.join" => group_mutate(&cmd, groups, store, caller, GroupMut::Join),
        "group.leave" => group_mutate(&cmd, groups, store, caller, GroupMut::Leave),
        "group.add" => group_mutate(&cmd, groups, store, caller, GroupMut::Add),
        "group.kick" => group_mutate(&cmd, groups, store, caller, GroupMut::Kick),
        "group.promote" => group_mutate(&cmd, groups, store, caller, GroupMut::Promote),
        "group.demote" => group_mutate(&cmd, groups, store, caller, GroupMut::Demote),
        "group.rename" => group_mutate(&cmd, groups, store, caller, GroupMut::Rename),
        "group.set_meta" => group_mutate(&cmd, groups, store, caller, GroupMut::SetMeta),
        "group.dissolve" => group_dissolve(&cmd, groups, store, caller),
        // ── 去中心命名（N1：home node 委派 local@domain）──
        "name.claim" => match cmd.params.as_ref().and_then(|p| NameOp::decode(p.value.as_slice()).ok()) {
            Some(op) => {
                let domain = ctx.domains.read().unwrap().first().cloned().unwrap_or_default();
                let local = op.local_part.trim().to_lowercase();
                if domain.is_empty() {
                    (false, None, "本节点未配置域名（无法签发命名）".into())
                } else if !valid_local_part(&local) {
                    (false, None, "非法 local-part（仅 a-z 0-9 . - _，≤63）".into())
                } else {
                    let full = format!("{local}@{domain}");
                    let taken_by_other = ctx.names.get(&full).map(|r| r.client_pubkey != caller).unwrap_or(false);
                    if taken_by_other {
                        (false, None, "该名字已被占用".into())
                    } else {
                        let serial = ctx.names.get(&full).map(|r| r.serial + 1).unwrap_or(1);
                        let mut rec = NameRecord {
                            local_part: local, domain, client_pubkey: caller.to_vec(), serial,
                            issued_at: now_ms() as i64, ttl: 3600, home_node: ctx.node_id.to_vec(),
                            home_sig: Vec::new(),
                        };
                        rec.home_sig = nm_crypto::sign_bytes(ctx.ep.secret_key(), &name_canonical(&rec)).to_vec();
                        if let Some(s) = &ctx.store {
                            let _ = s.put_name(&rec);
                        }
                        ctx.names.insert(full, rec.clone());
                        // 立即经联邦 gossip 广播，各节点填充命名缓存 → 全网可解析。
                        let gg = GroupGossip {
                            origin: ctx.node_id.to_vec(),
                            body: Some(nm_proto::pb::group_gossip::Body::Name(rec.clone())),
                        };
                        let _ = ctx.group_pub.send(gg.encode_to_vec());
                        (true, Some(Any { type_url: "nmspace.v1.NameRecord".into(), value: rec.encode_to_vec() }), String::new())
                    }
                }
            }
            None => (false, None, "invalid name op".into()),
        },
        "name.resolve" => match cmd.params.as_ref().and_then(|p| NameQuery::decode(p.value.as_slice()).ok()) {
            Some(q) => {
                let name = q.name.trim().to_lowercase();
                let records: Vec<NameRecord> = ctx.names.get(&name).map(|r| r.clone()).into_iter().collect();
                (true, Some(Any { type_url: "nmspace.v1.NameList".into(), value: NameList { records }.encode_to_vec() }), String::new())
            }
            None => (false, None, "invalid name query".into()),
        },
        "name.reverse" => match cmd.params.as_ref().and_then(|p| NameQuery::decode(p.value.as_slice()).ok()) {
            Some(q) => {
                let records: Vec<NameRecord> = ctx.names.iter().find(|r| r.client_pubkey == q.pubkey).map(|r| r.clone()).into_iter().collect();
                (true, Some(Any { type_url: "nmspace.v1.NameList".into(), value: NameList { records }.encode_to_vec() }), String::new())
            }
            None => (false, None, "invalid name query".into()),
        },
        "name.list" => {
            let records: Vec<NameRecord> = ctx.names.iter().map(|r| r.clone()).collect();
            (true, Some(Any { type_url: "nmspace.v1.NameList".into(), value: NameList { records }.encode_to_vec() }), String::new())
        }
        // ── 频道 / 主题（P4）──
        "channel.create" => match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
            Some(op) if op.channel_id.len() == 32 => {
                let meta = Channel {
                    channel_id: op.channel_id.clone(), name: op.name, owner: caller.to_vec(),
                    created_at: now_ms() as i64, topic: op.topic,
                    avatar_url: op.avatar_url, home_node: ctx.node_id.to_vec(),
                };
                ensure_channel(ctx, &op.channel_id, Some(meta)).await;
                if let Some(e) = ctx.channels.get(&op.channel_id) {
                    e.subs.insert(caller.to_vec());
                    let _ = e.pub_tx.send(ChannelOut::Meta(e.meta.lock().unwrap().clone())); // 广播元信息
                }
                (true, None, String::new())
            }
            _ => (false, None, "invalid channel op".into()),
        },
        "channel.sub" => match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
            Some(op) if op.channel_id.len() == 32 => {
                let meta = Channel { channel_id: op.channel_id.clone(), name: op.name, topic: op.topic, ..Default::default() };
                if ensure_channel(ctx, &op.channel_id, Some(meta)).await {
                    if let Some(e) = ctx.channels.get(&op.channel_id) { e.subs.insert(caller.to_vec()); }
                    (true, None, String::new())
                } else {
                    (false, None, "channel join failed".into())
                }
            }
            _ => (false, None, "invalid channel op".into()),
        },
        "channel.unsub" => match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
            Some(op) => {
                if let Some(e) = ctx.channels.get(&op.channel_id) { e.subs.remove(caller); }
                (true, None, String::new())
            }
            None => (false, None, "invalid channel op".into()),
        },
        "channel.publish" => match cmd.params.as_ref().and_then(|p| ChannelPub::decode(p.value.as_slice()).ok()) {
            Some(pp) => match ctx.channels.get(&pp.channel_id) {
                Some(e) => {
                    let seq = e.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let m = ChannelMsg {
                        channel_id: pp.channel_id.clone(), sender: caller.to_vec(),
                        seq, ts: now_ms() as i64, body: pp.body,
                    };
                    let _ = e.pub_tx.send(ChannelOut::Msg(m));
                    (true, None, String::new())
                }
                None => (false, None, "not subscribed to channel".into()),
            },
            None => (false, None, "invalid channel pub".into()),
        },
        "channel.backfill" => match cmd.params.as_ref().and_then(|p| ChannelBackfillReq::decode(p.value.as_slice()).ok()) {
            Some(req) => match ctx.channels.get(&req.channel_id) {
                Some(e) => {
                    let msgs: Vec<ChannelMsg> = e.log.lock().unwrap().iter().filter(|x| x.seq > req.since_seq).cloned().collect();
                    (true, Some(Any { type_url: "nmspace.v1.ChannelLog".to_string(), value: ChannelLog { msgs }.encode_to_vec() }), String::new())
                }
                None => (false, None, "unknown channel".into()),
            },
            None => (false, None, "invalid backfill req".into()),
        },
        // owner 设置频道元信息（名称/简介/头像）→ 更新本地并经 gossip 广播给订阅者。
        "channel.set_meta" => match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
            Some(op) if op.channel_id.len() == 32 => match ctx.channels.get(&op.channel_id) {
                Some(e) => {
                    let is_owner = e.meta.lock().unwrap().owner.as_slice() == caller;
                    if !is_owner {
                        (false, None, "仅频道创建者可修改频道信息".into())
                    } else {
                        let snap = {
                            let mut m = e.meta.lock().unwrap();
                            if !op.name.is_empty() {
                                m.name = op.name;
                            }
                            m.topic = op.topic;
                            m.avatar_url = op.avatar_url;
                            if m.home_node.is_empty() {
                                m.home_node = ctx.node_id.to_vec();
                            }
                            m.clone()
                        };
                        let _ = e.pub_tx.send(ChannelOut::Meta(snap));
                        (true, None, String::new())
                    }
                }
                None => (false, None, "unknown channel".into()),
            },
            _ => (false, None, "invalid channel op".into()),
        },
        "channel.list" => {
            let channels: Vec<Channel> = ctx.channels.iter().map(|e| e.meta.lock().unwrap().clone()).collect();
            (true, Some(Any { type_url: "nmspace.v1.ChannelList".to_string(), value: ChannelList { channels }.encode_to_vec() }), String::new())
        }
        "group.list" => {
            let mine: Vec<Group> = groups
                .iter()
                .filter(|g| g.members.iter().any(|m| m.as_slice() == caller))
                .map(|g| g.clone())
                .collect();
            (
                true,
                Some(Any {
                    type_url: "nmspace.v1.GroupList".to_string(),
                    value: GroupList { groups: mine }.encode_to_vec(),
                }),
                String::new(),
            )
        }
        "fed.sync" => {
            // 返回本节点归属的实体（供对等节点并入其目录）。
            let mine: Vec<Entity> = dir
                .entities
                .iter()
                .filter(|e| e.home_node == ctx.node_id.as_slice())
                .map(|e| e.clone())
                .collect();
            (
                true,
                Some(Any {
                    type_url: "nmspace.v1.FedSyncResp".to_string(),
                    value: FedSyncResp { entities: mine }.encode_to_vec(),
                }),
                String::new(),
            )
        }
        other => (false, None, format!("unknown method: {other}")),
    };
    // 群状态变更：立即向联邦 gossip 广播一次群公告，让成员节点尽快学到群（发现 + 成员表），
    // 免等 20s 周期——发现驱动跨节点群消息投递。
    if ok && cmd.method.starts_with("group.") && cmd.method != "group.list" {
        if let Some(op) = cmd.params.as_ref().and_then(|p| GroupOp::decode(p.value.as_slice()).ok()) {
            announce_group(ctx, &op.group_id);
        }
    }
    let cr = CommandResult {
        correlation_id: cmd.correlation_id,
        ok,
        result,
        error,
    };
    Some(command_result_gram(gram, &cr))
}

enum GroupMut { Join, Leave, Add, Kick, Promote, Demote, Rename, SetMeta }

fn persist_group(store: &Option<Arc<RedbStore>>, g: &Group) {
    if let Some(s) = store {
        if let Err(e) = s.put_group(g) {
            tracing::warn!("persist group failed: {e}");
        }
    }
}

fn is_group_admin(g: &Group, who: &[u8]) -> bool {
    g.owner.as_slice() == who || g.admins.iter().any(|a| a.as_slice() == who)
}

/// 创建群：caller 为 owner 兼首个成员。group_id 由参数给定（客户端生成，通常随机）。
fn group_create(
    cmd: &Command,
    groups: &DashMap<Vec<u8>, Group>,
    store: &Option<Arc<RedbStore>>,
    caller: &[u8],
    node_id: &[u8],
) -> (bool, Option<Any>, String) {
    let Some(op) = cmd.params.as_ref().and_then(|p| GroupOp::decode(p.value.as_slice()).ok()) else {
        return (false, None, "invalid group op".into());
    };
    if op.group_id.len() != 32 {
        return (false, None, "group_id must be 32 bytes".into());
    }
    if groups.contains_key(&op.group_id) {
        return (false, None, "group already exists".into());
    }
    let g = Group {
        group_id: op.group_id.clone(),
        name: op.name.clone(),
        owner: caller.to_vec(),
        members: vec![caller.to_vec()],
        updated_at: now_ms() as i64,
        admins: Vec::new(),
        topic: op.topic.clone(),
        avatar_url: op.avatar_url.clone(),
        home_node: node_id.to_vec(),
    };
    persist_group(store, &g);
    groups.insert(g.group_id.clone(), g);
    tracing::info!("group created");
    (true, None, String::new())
}

/// 解散群（仅 owner）。
fn group_dissolve(
    cmd: &Command,
    groups: &DashMap<Vec<u8>, Group>,
    store: &Option<Arc<RedbStore>>,
    caller: &[u8],
) -> (bool, Option<Any>, String) {
    let Some(op) = cmd.params.as_ref().and_then(|p| GroupOp::decode(p.value.as_slice()).ok()) else {
        return (false, None, "invalid group op".into());
    };
    let owner_ok = groups.get(&op.group_id).map(|g| g.owner.as_slice() == caller);
    match owner_ok {
        None => (false, None, "group not found".into()),
        Some(false) => (false, None, "only owner can dissolve".into()),
        Some(true) => {
            groups.remove(&op.group_id);
            if let Some(s) = store {
                let _ = s.del_group(&op.group_id);
            }
            tracing::info!("group dissolved");
            (true, None, String::new())
        }
    }
}

/// 成员/角色变更（按 owner>admin>member 鉴权）。
fn group_mutate(
    cmd: &Command,
    groups: &DashMap<Vec<u8>, Group>,
    store: &Option<Arc<RedbStore>>,
    caller: &[u8],
    op_kind: GroupMut,
) -> (bool, Option<Any>, String) {
    let Some(op) = cmd.params.as_ref().and_then(|p| GroupOp::decode(p.value.as_slice()).ok()) else {
        return (false, None, "invalid group op".into());
    };
    let Some(mut entry) = groups.get_mut(&op.group_id) else {
        return (false, None, "group not found".into());
    };
    let caller_owner = entry.owner.as_slice() == caller;
    let caller_admin = is_group_admin(&entry, caller);
    let target_ok = op.target.len() == 32;
    match op_kind {
        GroupMut::Join => {
            if !entry.members.iter().any(|m| m.as_slice() == caller) {
                entry.members.push(caller.to_vec());
            }
        }
        GroupMut::Leave => {
            if caller_owner {
                return (false, None, "群主不能退出，请先转让或解散群".into());
            }
            entry.members.retain(|m| m.as_slice() != caller);
            entry.admins.retain(|a| a.as_slice() != caller);
        }
        GroupMut::Add => {
            if !caller_admin {
                return (false, None, "only owner/admin can add members".into());
            }
            if !target_ok {
                return (false, None, "add target required".into());
            }
            if !entry.members.iter().any(|m| m.as_slice() == op.target.as_slice()) {
                entry.members.push(op.target.clone());
            }
        }
        GroupMut::Kick => {
            if !caller_admin {
                return (false, None, "only owner/admin can kick".into());
            }
            if !target_ok {
                return (false, None, "kick target required".into());
            }
            if op.target.as_slice() == entry.owner.as_slice() {
                return (false, None, "cannot kick owner".into());
            }
            let target_admin = entry.admins.iter().any(|a| a.as_slice() == op.target.as_slice());
            if target_admin && !caller_owner {
                return (false, None, "only owner can remove an admin".into());
            }
            entry.members.retain(|m| m.as_slice() != op.target.as_slice());
            entry.admins.retain(|a| a.as_slice() != op.target.as_slice());
        }
        GroupMut::Promote => {
            if !caller_owner {
                return (false, None, "only owner can promote".into());
            }
            if !target_ok || !entry.members.iter().any(|m| m.as_slice() == op.target.as_slice()) {
                return (false, None, "target is not a member".into());
            }
            if !entry.admins.iter().any(|a| a.as_slice() == op.target.as_slice()) {
                entry.admins.push(op.target.clone());
            }
        }
        GroupMut::Demote => {
            if !caller_owner {
                return (false, None, "only owner can demote".into());
            }
            entry.admins.retain(|a| a.as_slice() != op.target.as_slice());
        }
        GroupMut::Rename => {
            if !caller_admin {
                return (false, None, "only owner/admin can rename".into());
            }
            entry.name = op.name.clone();
        }
        // 设置群信息（名称/简介/头像）：owner/admin 可改。
        GroupMut::SetMeta => {
            if !caller_admin {
                return (false, None, "only owner/admin can edit group info".into());
            }
            if !op.name.is_empty() {
                entry.name = op.name.clone();
            }
            entry.topic = op.topic.clone();
            entry.avatar_url = op.avatar_url.clone();
        }
    }
    entry.updated_at = now_ms() as i64;
    let g = entry.clone();
    drop(entry);
    persist_group(store, &g);
    (true, None, String::new())
}

/// s2s：向对等节点发一次 fed.sync，取回其归属实体列表。
async fn fed_pull(ep: &NodeEndpoint, addr: nm_transport::Addr) -> Result<Vec<Entity>, NodeError> {
    let cmd = Command {
        method: "fed.sync".to_string(),
        params: None,
        correlation_id: 1,
        timeout_ms: 10_000,
        grant: None,
    };
    let gram = s2s_gram(ep.id_bytes(), GramKind::Command, Some(Any {
        type_url: "nmspace.v1.Command".to_string(),
        value: cmd.encode_to_vec(),
    }));
    let resp = s2s_request(ep, addr, &gram).await?;
    let payload = resp.payload.ok_or_else(|| NodeError::Other("fed.sync no payload".into()))?;
    let cr = CommandResult::decode(payload.value.as_slice())
        .map_err(|e| NodeError::Other(e.to_string()))?;
    let body = cr.result.ok_or_else(|| NodeError::Other("fed.sync no result".into()))?;
    let list = FedSyncResp::decode(body.value.as_slice())
        .map_err(|e| NodeError::Other(e.to_string()))?;
    Ok(list.entities)
}

/// s2s：把一个 gram 包进 Relay 信封转发给目标的归属节点。
async fn fed_relay(ep: &NodeEndpoint, addr: nm_transport::Addr, inner: &Gram) -> Result<(), NodeError> {
    let env = s2s_gram(ep.id_bytes(), GramKind::Relay, Some(Any {
        type_url: "nmspace.v1.Gram".to_string(),
        value: inner.encode_to_vec(),
    }));
    s2s_request(ep, addr, &env).await.map(|_| ())
}

/// s2s 回源拉取一个 blob（本节点未命中时，向实体归属节点取头像等内容寻址数据）。
/// 转发请求的 `home_node` 置空 → 对端不会再次回源，避免链式转发。
async fn s2s_blob_get(
    ep: &NodeEndpoint,
    addr: nm_transport::Addr,
    hash: &[u8],
) -> Result<BlobData, NodeError> {
    let cmd = Command {
        method: "blob.get".to_string(),
        params: Some(Any {
            type_url: "nmspace.v1.BlobRef".to_string(),
            value: BlobRef { hash: hash.to_vec(), home_node: Vec::new() }.encode_to_vec(),
        }),
        correlation_id: 1,
        timeout_ms: 10_000,
        grant: None,
    };
    let gram = s2s_gram(
        ep.id_bytes(),
        GramKind::Command,
        Some(Any { type_url: "nmspace.v1.Command".to_string(), value: cmd.encode_to_vec() }),
    );
    let resp = s2s_request(ep, addr, &gram).await?;
    let payload = resp.payload.ok_or_else(|| NodeError::Other("blob.get no payload".into()))?;
    let cr = CommandResult::decode(payload.value.as_slice())
        .map_err(|e| NodeError::Other(e.to_string()))?;
    if !cr.ok {
        return Err(NodeError::Other(if cr.error.is_empty() { "blob.get failed".into() } else { cr.error }));
    }
    let body = cr.result.ok_or_else(|| NodeError::Other("blob.get no result".into()))?;
    BlobData::decode(body.value.as_slice()).map_err(|e| NodeError::Other(e.to_string()))
}

/// s2s 单次请求：拨号对等节点，开 bi 流，发一个 gram，读回单条响应。
async fn s2s_request(ep: &NodeEndpoint, addr: nm_transport::Addr, gram: &Gram) -> Result<Gram, NodeError> {
    // 全程超时兜底：对端可能连上却始终不回（read_gram 无限等）。s2s 调用绝不能永久挂起——
    // 上层若在锁/DashMap 守卫下调用会拖死整个节点（见 sync_peers_once）。10s 与 fed.sync 的 timeout_ms 对齐。
    let fut = async {
        let conn = ep.connect(addr).await.map_err(|e| NodeError::Other(e.to_string()))?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| NodeError::Other(e.to_string()))?;
        write_gram(&mut send, gram).await.map_err(|e| NodeError::Other(e.to_string()))?;
        let _ = send.finish();
        read_gram(&mut recv).await.map_err(|e| NodeError::Other(e.to_string()))
    };
    match tokio::time::timeout(std::time::Duration::from_secs(10), fut).await {
        Ok(r) => r,
        Err(_) => Err(NodeError::Other("s2s request timed out".into())),
    }
}

fn s2s_gram(sender: [u8; 32], kind: GramKind, payload: Option<Any>) -> Gram {
    Gram {
        version: PROTOCOL_VERSION,
        kind: kind as i32,
        gram_id: 1,
        ref_gram_id: None,
        sender: sender.to_vec(),
        receiver: Vec::new(),
        timestamp_ms: now_ms(),
        payload,
        crc: Vec::new(),
    }
}

fn command_result_gram(req: &Gram, cr: &CommandResult) -> Gram {
    reply_gram(
        req,
        GramKind::CommandResult,
        Some(Any {
            type_url: "nmspace.v1.CommandResult".to_string(),
            value: cr.encode_to_vec(),
        }),
    )
}

fn receipt_for(msg: &Gram) -> Gram {
    reply_gram(msg, GramKind::Receipt, None)
}

fn login_ok(login: &Gram) -> Gram {
    reply_gram(login, GramKind::Reply, None)
}

/// 校验一条路由命令是否被授权：命令须带 Grant，且授权 caller 对 target.kind 执行该 method。
fn authorize_routed(gram: &Gram, caller: &[u8], target: &Entity) -> std::result::Result<(), String> {
    let cmd = gram
        .payload
        .as_ref()
        .and_then(|p| Command::decode(p.value.as_slice()).ok())
        .ok_or_else(|| "malformed command".to_string())?;
    let grant = cmd.grant.as_ref().ok_or_else(|| "missing grant".to_string())?;
    let now = (now_ms() / 1000) as i64;
    // resource 约定为目标实体的 kind；action 为命令 method。
    nm_crypto::authorize(grant, caller, &cmd.method, &target.kind, now)
        .map_err(|e| e.to_string())?;
    // grant 须由目标实体(资源所有者)签发（issuer == target.entity_id）。
    if grant.issuer != target.entity_id {
        return Err("grant issuer != resource owner".to_string());
    }
    Ok(())
}

/// 把「命令被拒」作为 CommandResult 推回调用方在线会话（若在线）。
fn push_command_denied(gram: &Gram, caller: &[u8], sessions: &Sessions, reason: &str) {
    let corr = gram
        .payload
        .as_ref()
        .and_then(|p| Command::decode(p.value.as_slice()).ok())
        .map(|c| c.correlation_id)
        .unwrap_or(0);
    let Some(conn) = sessions.get(caller).map(|r| r.clone()) else {
        return;
    };
    let cr = CommandResult { correlation_id: corr, ok: false, result: None, error: format!("denied: {reason}") };
    let mut reply = reply_gram(gram, GramKind::CommandResult, Some(Any {
        type_url: "nmspace.v1.CommandResult".to_string(),
        value: cr.encode_to_vec(),
    }));
    reply.ref_gram_id = Some(gram.gram_id);
    reply.receiver = caller.to_vec();
    tokio::spawn(async move {
        if let Ok(mut sname) = conn.open_uni().await {
            let _ = write_gram(&mut sname, &reply).await;
            let _ = sname.finish();
        }
    });
}

fn reply_gram(req: &Gram, kind: GramKind, payload: Option<Any>) -> Gram {
    Gram {
        version: req.version,
        kind: kind as i32,
        gram_id: req.gram_id.wrapping_add(1),
        ref_gram_id: Some(req.gram_id),
        sender: req.receiver.clone(),
        receiver: req.sender.clone(),
        timestamp_ms: now_ms(),
        payload,
        crc: Vec::new(),
    }
}

#[cfg(test)]
mod p0_tests {
    use super::*;
    use nm_proto::pb::Entity;

    fn ent(id: u8, ts: i64, name: &str) -> Entity {
        Entity {
            entity_id: vec![id; 32],
            updated_at: ts,
            display_name: name.into(),
            ..Default::default()
        }
    }

    // P0：目录 LWW 收敛——更旧的更新被拒绝，更新/相等的被接受；同一 id 只保留一条。
    #[test]
    fn merge_lww_keeps_newest() {
        let d = MemDirectory::new();
        let key = vec![1u8; 32];
        assert!(d.merge_lww(ent(1, 100, "v1")), "首次插入应接受");
        assert!(!d.merge_lww(ent(1, 50, "old")), "更旧 updated_at 应拒绝");
        assert_eq!(d.entities.get(&key).unwrap().display_name, "v1");
        assert!(d.merge_lww(ent(1, 200, "v2")), "更新 updated_at 应接受");
        assert_eq!(d.entities.get(&key).unwrap().display_name, "v2");
        assert!(d.merge_lww(ent(1, 200, "v2eq")), "相等 updated_at 按 >= 接受");
        assert_eq!(d.entities.get(&key).unwrap().display_name, "v2eq");
        assert_eq!(d.len(), 1, "同一 id 只保留一条");
    }
}

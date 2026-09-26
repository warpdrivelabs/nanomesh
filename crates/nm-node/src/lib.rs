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
    now_ms, Any, BlobData, BlobPut, BlobRef, Command, CommandResult, DirectoryQuery, Entity,
    EntityList, FedSyncResp, Gram, GramKind, Group, GroupList, GroupOp, PROTOCOL_VERSION,
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
    if matches!(gram.kind(), GramKind::GroupMessage) {
        fanout_group(gram, ctx, caller).await;
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
        if matches!(gram.kind(), GramKind::Message) {
            deliver_or_store(to, gram, ctx).await;
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
    for member in &group.members {
        if member.as_slice() == caller {
            continue; // 不回发给自己
        }
        // 逐成员定制一份（receiver=成员），便于其本地按会话入库/展示。
        let mut per = gram.clone();
        per.receiver = member.clone();
        deliver_or_store(member, &per, ctx).await;
    }
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
            let list = dir.query(&q).await.unwrap_or_default();
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
        "group.create" => group_create(&cmd, groups, store, caller),
        "group.join" => group_mutate(&cmd, groups, store, caller, GroupMut::Join),
        "group.leave" => group_mutate(&cmd, groups, store, caller, GroupMut::Leave),
        "group.kick" => group_mutate(&cmd, groups, store, caller, GroupMut::Kick),
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
    let cr = CommandResult {
        correlation_id: cmd.correlation_id,
        ok,
        result,
        error,
    };
    Some(command_result_gram(gram, &cr))
}

enum GroupMut { Join, Leave, Kick }

fn persist_group(store: &Option<Arc<RedbStore>>, g: &Group) {
    if let Some(s) = store {
        if let Err(e) = s.put_group(g) {
            tracing::warn!("persist group failed: {e}");
        }
    }
}

/// 创建群：caller 为 owner 兼首个成员。group_id 由参数给定（客户端生成，通常随机）。
fn group_create(
    cmd: &Command,
    groups: &DashMap<Vec<u8>, Group>,
    store: &Option<Arc<RedbStore>>,
    caller: &[u8],
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
    };
    persist_group(store, &g);
    groups.insert(g.group_id.clone(), g);
    tracing::info!("group created");
    (true, None, String::new())
}

/// 加入/退出/踢人。join：caller 加自己；leave：caller 移除自己；kick：仅 owner 可移除 target。
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
    match op_kind {
        GroupMut::Join => {
            if !entry.members.iter().any(|m| m.as_slice() == caller) {
                entry.members.push(caller.to_vec());
            }
        }
        GroupMut::Leave => {
            entry.members.retain(|m| m.as_slice() != caller);
        }
        GroupMut::Kick => {
            if entry.owner.as_slice() != caller {
                return (false, None, "only owner can kick".into());
            }
            if op.target.is_empty() {
                return (false, None, "kick target required".into());
            }
            entry.members.retain(|m| m.as_slice() != op.target.as_slice());
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

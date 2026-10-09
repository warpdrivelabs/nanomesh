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
    ChannelLog, ChannelMsg, ChannelOp, ChannelPub, Command, CommandResult, DeviceCert, DeviceInfo,
    DeviceList, DeviceRevoke, DirectoryQuery, Entity, EntityList, FedSyncResp, Gram, GramKind, Group, GroupGossip, GroupList, GroupOp, NameList,
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
/// 设备连接 → 账号（`device.hello` 成功后登记）。未登记的连接以连接公钥本身为账号（老客户端）。
type Principals = DashMap<Vec<u8>, Vec<u8>>;
/// 设备公钥 → 登记信息（证书 + 改名 + 最近在线）。
type Devices = DashMap<Vec<u8>, DeviceInfo>;
/// 设备公钥 → 吊销记录。
type Revoked = DashMap<Vec<u8>, DeviceRevoke>;

/// 离线信箱保留期：超期未取走的消息清理掉。
const INBOX_RETENTION_MS: u64 = 30 * 24 * 3600 * 1000;
/// 节点推给账号设备的系统事件（吊销等）的 type_url。
const DEVICE_EVENT_TYPE: &str = "nmspace.v1/device.event";

/// 内容寻址 blob 体量上限（头像等小媒体）；超限拒绝，避免撑爆节点存储/带宽。大媒体应分块（后续）。
const MAX_BLOB: usize = 1024 * 1024; // 1 MiB

/// 会话旁挂元数据（不改动 `Sessions` 值类型；用于管理台展示接入时长）。
#[derive(Clone, Copy)]
struct SessionMeta {
    since: std::time::SystemTime,
}
type SessionsMeta = DashMap<Vec<u8>, SessionMeta>;

/// 一条活动会话的管理快照（后端管理台「连接监控 / 对等节点」用）。
pub struct SessionRow {
    pub id: [u8; 32],
    pub since_unix_ms: u64,
    pub bytes_tx: u64,
    pub bytes_rx: u64,
    pub alpn: String,
    /// 当前选中路径 RTT（毫秒）；无路径时为 `None`。
    pub rtt_ms: Option<f64>,
    /// 拥塞窗口（字节）。
    pub cwnd: Option<u64>,
    /// 当前路径 MTU（UDP 载荷）。
    pub mtu: Option<u16>,
    pub lost_packets: u64,
    pub lost_bytes: u64,
    pub datagrams_tx: u64,
    pub datagrams_rx: u64,
    pub congestion_events: u64,
    pub black_holes: u64,
    /// `"direct"` / `"relay"` / `""`（未知）。
    pub path_kind: String,
    /// 选中路径远端传输地址（IP/中继 URL）。
    pub remote_addr: String,
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
/// F5 存活账本：群 id → (节点 id → 最近在该群主题上收到其广播的 ms)。
/// 在某群主题上收到某节点的消息 ⇒ 该节点此刻订阅并存活于此主题（心跳由成员 home 周期 announce 承载）。
type GroupLive = DashMap<Vec<u8>, DashMap<Vec<u8>, u64>>;
const LIVE_TTL_MS: u64 = 12_000; // 存活 TTL：sweep 每 ~3s 发一次心跳，容 3 次丢失

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

/// F2 pkarr 群发现记录编解码：把「群公钥 → seed 节点集合」签成 pkarr `SignedPacket`，
/// 载荷即 relay payload（可 PUT/GET iroh-dns-server）。此处仅编解码；网络收发见 pkarr 发布/解析任务。
/// 复用 iroh-dns 自带 pkarr（iroh 原生 ed25519），不引入独立 pkarr/mainline 依赖。
mod pkarr_rec {
    use super::{hex_decode_n, hex_encode};
    use iroh_dns::pkarr::SignedPacket;
    use nm_transport::SecretKey;

    pub(super) const SEED_NAME: &str = "_nmspace"; // seed 记录挂载的 TXT 名（相对群公钥 origin）
    pub(super) const REC_TTL: u32 = 300; // 记录 TTL（秒）：seed 集合变动不频繁
    pub(super) const MAX_SEEDS: usize = 8; // seed 上限（控制 DNS 包 < pkarr 上限）

    /// 用群私钥把 seed 节点集合签成 relay payload（PUT 到 relay 的 body）。
    pub(super) fn build(secret: &SecretKey, seeds: &[[u8; 32]]) -> Result<Vec<u8>, String> {
        let values: Vec<String> = seeds.iter().take(MAX_SEEDS).map(|s| hex_encode(s)).collect();
        let pkt = SignedPacket::from_txt_strings(secret, SEED_NAME, values, REC_TTL)
            .map_err(|e| e.to_string())?;
        Ok(pkt.to_relay_payload())
    }

    /// 校验并解析某群公钥下的 relay payload → seed 节点集合（签名由该公钥校验，篡改即失败）。
    pub(super) fn parse(pubkey: &[u8; 32], payload: &[u8]) -> Result<Vec<[u8; 32]>, String> {
        let pk = nm_transport::Id::from_bytes(pubkey).map_err(|e| e.to_string())?;
        let pkt = SignedPacket::from_relay_payload(&pk, payload).map_err(|e| e.to_string())?;
        Ok(pkt
            .txt_records(SEED_NAME)
            .iter()
            .filter_map(|t| hex_decode_n::<32>(t))
            .collect())
    }
}

/// F6/M1 成员发现记录编解码：锚点把「某联邦的 bootstrap 成员节点集合」用**锚点自身私钥**签成 pkarr
/// `SignedPacket`，PUT 到 relay（key=锚点公钥 z32）。新节点配置锚点公钥后 GET + 验签即得入网 bootstrap。
/// 与 `pkarr_rec` 同构，仅 TXT 名不同（`_nmmember`，与群发现 `_nmspace` 记录隔离，可共用同一 relay/锚点公钥）。
mod member_rec {
    use super::{hex_decode_n, hex_encode};
    use iroh_dns::pkarr::SignedPacket;
    use nm_transport::SecretKey;

    pub(super) const NAME: &str = "_nmmember"; // 成员索引挂载的 TXT 名
    pub(super) const REC_TTL: u32 = 300; // 记录 TTL（秒）
    pub(super) const MAX_MEMBERS: usize = 8; // bootstrap 种子上限（控制 DNS 包 < pkarr 上限）；非全量名册

    /// 用锚点私钥把 bootstrap 成员节点集合签成 relay payload。
    pub(super) fn build(secret: &SecretKey, members: &[[u8; 32]]) -> Result<Vec<u8>, String> {
        let values: Vec<String> = members.iter().take(MAX_MEMBERS).map(|s| hex_encode(s)).collect();
        let pkt = SignedPacket::from_txt_strings(secret, NAME, values, REC_TTL)
            .map_err(|e| e.to_string())?;
        Ok(pkt.to_relay_payload())
    }

    /// 校验并解析某锚点公钥下的 relay payload → bootstrap 成员集合（签名由锚点公钥校验，篡改即失败）。
    pub(super) fn parse(anchor_pubkey: &[u8; 32], payload: &[u8]) -> Result<Vec<[u8; 32]>, String> {
        let pk = nm_transport::Id::from_bytes(anchor_pubkey).map_err(|e| e.to_string())?;
        let pkt = SignedPacket::from_relay_payload(&pk, payload).map_err(|e| e.to_string())?;
        Ok(pkt
            .txt_records(NAME)
            .iter()
            .filter_map(|t| hex_decode_n::<32>(t))
            .collect())
    }
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
    fed: Arc<nm_federation::Federation>,                    // F1：每群独立主题收发
    per_topic: Arc<std::sync::atomic::AtomicBool>,          // F1 灰度开关（关=仅火管）
    per_group_live: Arc<GroupLive>,                         // F5：各群主题上各节点最近存活（火管省略判据）
    pkarr_seeds: Arc<DashMap<Vec<u8>, Vec<[u8; 32]>>>,      // F2：pkarr 解析到的各群 seed（group_bootstrap 兜底）
    domains: Arc<std::sync::RwLock<Vec<String>>>, // 本节点自声明域名集合（命名 N1）
    names: Arc<DashMap<String, NameRecord>>,  // 命名缓存 local@domain → NameRecord
    /// 注册中心审批结果：域名 → (是否通过, 时间)。
    domain_notices: Arc<DashMap<String, (bool, u64)>>,
    principals: Arc<Principals>,
    devices: Arc<Devices>,
    revoked: Arc<Revoked>,
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
/// 群消息去重：最近已投递的 `(sender, gram_id)`，有界 FIFO。
/// 火管 + 每群主题双写（F1）时同一条 Msg 会到两次，据此只投一次。
#[derive(Default)]
struct SeenGrams {
    set: std::collections::HashSet<(Vec<u8>, u64)>,
    order: std::collections::VecDeque<(Vec<u8>, u64)>,
}

impl SeenGrams {
    const CAP: usize = 8192;
    /// `true`=首次见（应投递）；`false`=已见（丢弃）。
    fn first_seen(&mut self, sender: &[u8], gram_id: u64) -> bool {
        let key = (sender.to_vec(), gram_id);
        if self.set.contains(&key) {
            return false;
        }
        self.set.insert(key.clone());
        self.order.push_back(key);
        if self.order.len() > Self::CAP {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

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
    // 规模化联邦（F1）：每群独立主题收发；灰度开关 per_topic（默认关=仅火管，零行为变化）。
    fed: Arc<nm_federation::Federation>,
    fed_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<nm_federation::FedMsg>>>,
    per_topic: Arc<std::sync::atomic::AtomicBool>,
    seen: Arc<std::sync::Mutex<SeenGrams>>, // 群消息去重（火管 + 每群主题双写只投一次）
    per_topic_nodes: Arc<DashSet<Vec<u8>>>, // F1 协商：已广告支持每群主题的节点 id（收 announce 时记录；F5 据此停火管）
    per_group_live: Arc<GroupLive>,         // F5：各群主题上各节点的最近存活（据此省略群消息的火管副本）
    pkarr_url: Arc<std::sync::RwLock<Option<String>>>, // F2：pkarr relay 端点（nmd 据 infra 设置；None=不发布/解析）
    pkarr_seeds: Arc<DashMap<Vec<u8>, Vec<[u8; 32]>>>, // F2：pkarr 解析到的各群 seed 节点（join bootstrap 兜底/无锚发现）
    http: reqwest::Client,                  // F2：pkarr relay HTTP 客户端
    // 群联邦 gossip：fanout 把待广播的 GroupGossip 字节丢进 group_pub，由 spawn_group_sync 统一发布。
    group_pub: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    group_pub_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>>,
    // 去中心命名（N1）：本节点自声明的域名集合（可多个）+ 命名缓存（local@domain → NameRecord，含 gossip 学到的）。
    domains: Arc<std::sync::RwLock<Vec<String>>>,
    names: Arc<DashMap<String, NameRecord>>,
    domain_notices: Arc<DashMap<String, (bool, u64)>>,
    // 多设备：连接 → 账号映射、已登记设备、吊销表。
    principals: Arc<Principals>,
    devices: Arc<Devices>,
    revoked: Arc<Revoked>,
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
        // F1：每群主题联邦句柄（复用同一 gossip endpoint）；默认 per_topic=关 → self.fed 空置、零行为变化。
        let (fed, fed_rx) = nm_federation::Federation::new(gossip.clone(), ep.id_bytes());
        // 命名缓存：回填本节点持久化的命名记录（重启恢复）。
        let names: Arc<DashMap<String, NameRecord>> = Arc::new(DashMap::new());
        let mut owned_domains: Vec<String> = Vec::new();
        let devices: Arc<Devices> = Arc::new(DashMap::new());
        let revoked: Arc<Revoked> = Arc::new(DashMap::new());
        if let Some(s) = &store {
            for r in s.all_names().unwrap_or_default() {
                names.insert(format!("{}@{}", r.local_part, r.domain), r);
            }
            owned_domains = s.all_domains().unwrap_or_default();
            for d in s.all_devices().unwrap_or_default() {
                if let Some(c) = &d.cert {
                    devices.insert(c.device.clone(), d.clone());
                }
            }
            for r in s.all_revokes().unwrap_or_default() {
                revoked.insert(r.device.clone(), r);
            }
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
            fed,
            fed_rx: std::sync::Mutex::new(Some(fed_rx)),
            per_topic: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            seen: Arc::new(std::sync::Mutex::new(SeenGrams::default())),
            per_topic_nodes: Arc::new(DashSet::new()),
            per_group_live: Arc::new(GroupLive::new()),
            pkarr_url: Arc::new(std::sync::RwLock::new(None)),
            pkarr_seeds: Arc::new(DashMap::new()),
            http: reqwest::Client::new(),
            group_pub,
            group_pub_rx: std::sync::Mutex::new(Some(group_pub_rx)),
            domains: Arc::new(std::sync::RwLock::new(owned_domains)),
            names,
            domain_notices: Arc::new(DashMap::new()),
            principals: Arc::new(DashMap::new()),
            devices,
            revoked,
        })
    }

    fn ctx(&self) -> Ctx {
        Ctx {
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
            fed: self.fed.clone(),
            per_topic: self.per_topic.clone(),
            per_group_live: self.per_group_live.clone(),
            pkarr_seeds: self.pkarr_seeds.clone(),
            domains: self.domains.clone(),
            names: self.names.clone(),
            domain_notices: self.domain_notices.clone(),
            principals: self.principals.clone(),
            devices: self.devices.clone(),
            revoked: self.revoked.clone(),
        }
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
        // 立即切断在线会话（若有，含该账号下所有设备连接），并主动关闭连接。
        for rid in online_rids(&self.sessions, &self.principals, &pubkey) {
            if let Some((_, conn)) = self.sessions.remove(&rid) {
                self.sessions_meta.remove(&rid);
                self.principals.remove(&rid);
                conn.close(0u32.into(), b"banned");
                tracing::info!(online = self.sessions.len(), "banned peer session cut");
            }
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
    /// 规模化联邦（F0）：构造一个基于本节点 gossip 的联邦句柄（按群主题收发），返回 `(句柄, 事件流)`。
    /// **opt-in**：仅在调用时创建，不改任何现有投递路径（火管照旧）；F1 起才接入 fanout/deliver
    /// （`nmd.toml [federation] per_topic` 灰度切换）。详见 `docs/FEDERATION_SCALING_IMPL.md`。
    pub fn federation(
        &self,
    ) -> (
        Arc<nm_federation::Federation>,
        tokio::sync::mpsc::UnboundedReceiver<nm_federation::FedMsg>,
    ) {
        nm_federation::Federation::new(self.gossip.clone(), self.ep.id_bytes())
    }
    /// F1（规模化联邦）：开/关「每群独立主题」灰度（nmd 据 `nmd.toml [federation] per_topic` 在 serve 前调用）。
    /// 关=仅火管（现状）；开=群 announce/msg 额外走 `nmspace-group:<gid>`（双写，收端去重）。
    pub fn set_per_topic(&self, on: bool) {
        self.per_topic.store(on, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn per_topic_on(&self) -> bool {
        self.per_topic.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// F1 协商就绪查询（F5 预备 / 运维可见）：该群是否「全员经每群主题可达」——每个远端成员的
    /// home 节点都已在 announce 中广告支持 per_topic（本地成员无需联邦）。保守：任一成员 home 未知/未广告 → false。
    /// 当前仅供诊断与 F5（火管退役）判定使用，不改变投递路径（仍双写火管）。
    pub fn group_per_topic_ready(&self, gid: &[u8]) -> bool {
        let Some(g) = self.groups.get(gid).map(|g| g.clone()) else {
            return false;
        };
        group_fully_per_topic(&self.ep.id_bytes(), &self.dir.entities, &self.per_topic_nodes, &g)
    }
    /// F5 诊断/测试：该群的「群消息」火管副本当前是否已可省略（全员远端 home 在该群主题存活）。
    /// 见 [`group_topic_retireable`]。true 时发群消息只走每群主题、不再广播火管。
    pub fn group_firehose_retireable(&self, gid: &[u8]) -> bool {
        let Some(g) = self.groups.get(gid).map(|g| g.clone()) else {
            return false;
        };
        group_topic_retireable(
            &self.ep.id_bytes(),
            &self.dir.entities,
            &self.per_group_live,
            &g,
            now_ms(),
            LIVE_TTL_MS,
        )
    }
    /// F3 诊断/测试：发给 `account` 的私聊单播火管 `Direct` 副本当前是否已可省略
    /// （其 home 节点在收件箱主题存活）。见 [`inbox_retireable`]。
    pub fn dm_firehose_retireable(&self, account: &[u8]) -> bool {
        inbox_retireable(
            account,
            &self.ep.id_bytes(),
            &self.dir.entities,
            &self.per_group_live,
            now_ms(),
            LIVE_TTL_MS,
        )
    }
    /// F2：设置本节点 pkarr relay 端点（nmd 据 infra 透传；`None`=不发布/解析群发现记录）。
    pub fn set_pkarr(&self, url: Option<String>) {
        *self.pkarr_url.write().unwrap() = url;
    }
    fn pkarr_endpoint(&self) -> Option<String> {
        self.pkarr_url.read().unwrap().clone()
    }
    /// F6/D5：relay 端点列表（`[dns] url` 支持逗号分隔多 relay；弱锚多副本，发布写多个、解析轮询）。
    /// F2 群发现仍用 `pkarr_endpoint()`（首个）；成员索引 publish/resolve 用本列表。
    fn pkarr_endpoints(&self) -> Vec<String> {
        self.pkarr_url
            .read()
            .unwrap()
            .as_deref()
            .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default()
    }
    /// F3 诊断/测试：当前已加入的联邦主题标签（群 gid 与收件人 account 混列）。
    pub async fn fed_joined(&self) -> Vec<Vec<u8>> {
        self.fed.joined().await
    }
    /// F2：把某「带密钥群」的 seed 节点集合（本节点=home + 该群主题上存活的成员 home）签名后 PUT 到
    /// pkarr relay，使无锚节点可据群公钥解析入网 seed。无 pkarr / 无群私钥 / 非 home 则跳过。
    /// pub：供 sweep 周期调用，也便于运维/测试直接触发。
    pub async fn pkarr_publish_group(&self, gid: &[u8]) {
        let Some(url) = self.pkarr_endpoint() else { return };
        let Some(store) = &self.store else { return };
        let Ok(Some(secret_bytes)) = store.group_secret(gid) else { return };
        let Ok(sk_arr) = <[u8; 32]>::try_from(secret_bytes.as_slice()) else { return };
        let sk = nm_transport::SecretKey::from_bytes(&sk_arr);
        let mut seeds: Vec<[u8; 32]> = vec![self.ep.id_bytes()]; // 本节点(home) 为首 seed
        if let Some(gl) = self.per_group_live.get(gid) {
            let now = now_ms();
            for e in gl.iter() {
                if now.saturating_sub(*e.value()) < LIVE_TTL_MS {
                    if let Ok(id) = <[u8; 32]>::try_from(e.key().as_slice()) {
                        if !seeds.contains(&id) {
                            seeds.push(id);
                        }
                    }
                }
            }
        }
        let Ok(payload) = pkarr_rec::build(&sk, &seeds) else { return };
        let endpoint = format!("{}/{}", url.trim_end_matches('/'), sk.public().to_z32());
        match self.http.put(&endpoint).body(payload).send().await {
            Ok(r) if r.status().is_success() => {
                tracing::debug!(seeds = seeds.len(), "pkarr group record published")
            }
            Ok(r) => tracing::debug!(status = %r.status(), "pkarr publish non-success"),
            Err(e) => tracing::debug!("pkarr publish failed: {e}"),
        }
    }
    /// F2：据群公钥从 pkarr relay 解析 seed 节点集合（GET + 验签）。无 pkarr / 解析失败则空。
    /// pub：供 sweep 周期调用，也便于运维/测试直接触发。
    pub async fn pkarr_resolve_group(&self, gid: &[u8]) -> Vec<[u8; 32]> {
        let Some(url) = self.pkarr_endpoint() else { return Vec::new() };
        let Ok(gid_arr) = <[u8; 32]>::try_from(gid) else { return Vec::new() };
        let Ok(pk) = nm_transport::Id::from_bytes(&gid_arr) else { return Vec::new() };
        let endpoint = format!("{}/{}", url.trim_end_matches('/'), pk.to_z32());
        let payload = match self.http.get(&endpoint).send().await {
            Ok(r) if r.status().is_success() => match r.bytes().await {
                Ok(b) => b.to_vec(),
                Err(_) => return Vec::new(),
            },
            _ => return Vec::new(),
        };
        pkarr_rec::parse(&gid_arr, &payload).unwrap_or_default()
    }

    /// F6/M1：锚点把「本联邦近期活跃成员」的 bootstrap 集合（本节点 + 已知活跃 peer，≤8）用**本节点私钥**
    /// 签名 PUT 到 relay（key=本节点公钥 z32，TXT 名 `_nmmember`）。仅锚点调用；无 relay 则跳过。
    /// 新节点据带外配置的锚点公钥 GET 此记录即得入网 bootstrap。pub 便于运维/测试直接触发。
    pub async fn publish_member_index(&self) {
        let endpoints = self.pkarr_endpoints();
        if endpoints.is_empty() {
            return;
        }
        let me = self.ep.id_bytes();
        let mut members: Vec<[u8; 32]> = vec![me]; // 锚点自身为首 bootstrap
        let now = now_secs();
        for e in self.peer_info.iter() {
            if members.len() >= member_rec::MAX_MEMBERS {
                break;
            }
            // 只纳入近期活跃（TTL 内）的 peer；manual/seed（last_seen 可能为 0）也纳入。
            let info = e.value();
            let fresh = info.last_seen == 0 || now.saturating_sub(info.last_seen) < 300;
            if !fresh {
                continue;
            }
            if let Ok(id) = <[u8; 32]>::try_from(e.key().as_slice()) {
                if id != me && !members.contains(&id) {
                    members.push(id);
                }
            }
        }
        let Ok(payload) = member_rec::build(self.ep.secret_key(), &members) else { return };
        let z32 = self.ep.id().to_z32();
        for url in &endpoints {
            let endpoint = format!("{}/{}", url.trim_end_matches('/'), z32);
            match self.http.put(&endpoint).body(payload.clone()).send().await {
                Ok(r) if r.status().is_success() => {
                    tracing::debug!(members = members.len(), relay = %url, "member index published")
                }
                Ok(r) => tracing::debug!(status = %r.status(), relay = %url, "member index publish non-success"),
                Err(e) => tracing::debug!("member index publish failed ({url}): {e}"),
            }
        }
    }

    /// F6/M1：据锚点公钥从 relay 解析本联邦 bootstrap 成员集合（GET + 验签），并把解析到的节点喂入
    /// `peers`（满足 gossip bootstrap 契约）。返回学到的节点数。无 relay / 解析失败则 0。多 relay 时轮询，首个成功即用。
    pub async fn resolve_member_index(&self, anchor_pubkey: &[u8; 32]) -> usize {
        let endpoints = self.pkarr_endpoints();
        if endpoints.is_empty() {
            return 0;
        }
        let Ok(pk) = nm_transport::Id::from_bytes(anchor_pubkey) else { return 0 };
        let z32 = pk.to_z32();
        let mut payload: Option<Vec<u8>> = None;
        for url in &endpoints {
            let endpoint = format!("{}/{}", url.trim_end_matches('/'), z32);
            if let Ok(r) = self.http.get(&endpoint).send().await {
                if r.status().is_success() {
                    if let Ok(b) = r.bytes().await {
                        payload = Some(b.to_vec());
                        break; // 首个成功的 relay 即用
                    }
                }
            }
        }
        let Some(payload) = payload else { return 0 };
        let members = match member_rec::parse(anchor_pubkey, &payload) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("member index parse/verify failed: {e}");
                return 0;
            }
        };
        let me = self.ep.id_bytes();
        let mut learned = 0;
        for id in members {
            if id == me {
                continue;
            }
            // 喂入 peers：按公钥解析地址（relay/发现服务在 dial 时解析），满足 gossip bootstrap 契约①。
            if self.add_peer_by_id(id).is_ok() {
                learned += 1;
            }
        }
        if learned > 0 {
            tracing::debug!(learned, "member index resolved → peers seeded");
        }
        learned
    }
    pub fn directory(&self) -> Arc<MemDirectory> {
        self.dir.clone()
    }
    pub fn online_count(&self) -> usize {
        self.sessions.len()
    }

    // ---- 后端管理台访问器 ----

    /// 活动会话快照（连接监控 / 对等连通性）。顺带清理陈旧的会话元数据。
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
                session_row_from_conn(id, since_unix_ms, conn)
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
        let nmspace = ImspaceProto { ctx: self.ctx() };
        if let Some(store) = self.store.clone() {
            tokio::spawn(async move {
                loop {
                    match store.prune_inbox(now_ms().saturating_sub(INBOX_RETENTION_MS)) {
                        Ok(n) if n > 0 => tracing::info!(count = n, "pruned expired inbox"),
                        Ok(_) => {}
                        Err(e) => tracing::warn!("prune inbox failed: {e}"),
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
                }
            });
        }
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

    /// F6/M1 成员发现 relay 索引（叠加在 gossip 成员频道之上，灰度开关 `relay_index`）：
    /// - 锚点（`anchor=true`）：周期把本联邦 bootstrap 成员集合 PUT 到 relay（key=本节点公钥）。
    /// - 所有节点：周期 GET 配置的各锚点公钥记录，把解析到的节点喂入 `peers` 做冷启动 bootstrap。
    /// 需 relay（`[dns] url`）已设；否则本任务空转。周期 60s（成员集合变动不频繁，与 F2 群 pkarr 同档）。
    pub fn spawn_member_index(
        self: Arc<Self>,
        anchor: bool,
        anchor_pubkeys: Vec<[u8; 32]>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let iv = std::time::Duration::from_secs(60);
            loop {
                if !self.pkarr_endpoints().is_empty() {
                    if anchor {
                        self.publish_member_index().await;
                    }
                    for ak in &anchor_pubkeys {
                        self.resolve_member_index(ak).await;
                    }
                }
                tokio::time::sleep(iv).await;
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

    /// 把域名审批结果发给目标节点，并在目标就是自己时立即入账。
    pub fn publish_domain_decision(&self, node_id: [u8; 32], domain: &str, approved: bool) {
        let text = serde_json::json!({ "domain": domain, "approved": approved }).to_string();
        let cmd = Command {
            method: "domain.decision".into(),
            params: Some(Any { type_url: "text/plain".into(), value: text.into_bytes() }),
            correlation_id: 0,
            timeout_ms: 0,
            grant: None,
        };
        let gram = Gram {
            version: PROTOCOL_VERSION,
            kind: GramKind::Command as i32,
            gram_id: now_ms(),
            ref_gram_id: None,
            sender: self.ep.id_bytes().to_vec(),
            receiver: node_id.to_vec(),
            timestamp_ms: now_ms(),
            payload: Some(Any {
                type_url: "nmspace.v1.Command".into(),
                value: cmd.encode_to_vec(),
            }),
            crc: Vec::new(),
        };
        if node_id == self.ep.id_bytes() {
            apply_domain_decision(&gram, &self.domains, &self.store, &self.domain_notices);
        }
        let gg = GroupGossip {
            origin: self.ep.id_bytes().to_vec(),
            body: Some(nm_proto::pb::group_gossip::Body::Direct(gram)),
        };
        let _ = self.group_pub.send(gg.encode_to_vec());
    }

    pub fn domain_notices(&self) -> Vec<(String, bool, u64)> {
        self.domain_notices
            .iter()
            .map(|e| (e.key().clone(), e.value().0, e.value().1))
            .collect()
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
        let origin = gg.origin.clone();
        let ctx = self.ctx();
        match gg.body {
            Some(nm_proto::pb::group_gossip::Body::Announce(mut g)) => {
                // F1 协商：home 节点在 announce 里广告自身 per_topic 能力；据 origin 跟踪可 per_topic 节点。
                if g.supports_per_topic {
                    self.per_topic_nodes.insert(origin);
                } else {
                    self.per_topic_nodes.remove(&origin);
                }
                g.supports_per_topic = false; // 节点能力不写进群状态（保持群状态纯净）
                self.merge_group_lww(g);
            }
            Some(nm_proto::pb::group_gossip::Body::Msg(gram)) => {
                // 去重（F1-2）：火管 + 每群主题双写时同一条 Msg 会到两次，按 (sender, gram_id) 只投一次。
                if !self.seen.lock().unwrap().first_seen(&gram.sender, gram.gram_id) {
                    return;
                }
                let Some(group) = self.groups.get(&gram.receiver).map(|g| g.clone()) else {
                    return; // 尚未学到该群（announce 未到）→ 跳过；周期公告后续会补
                };
                // 每个成员仅由本节点负责的部分投递：在线设备直投；本节点为其 home 则离线设备入库。
                for m in &group.members {
                    let home_here = is_home_here(&ctx, m);
                    deliver_account(m, &gram, &ctx, home_here, None).await;
                }
            }
            Some(nm_proto::pb::group_gossip::Body::Direct(gram)) => {
                let to = &gram.receiver;
                if to.as_slice() == self.ep.id_bytes().as_slice() {
                    apply_domain_decision(&gram, &self.domains, &self.store, &self.domain_notices);
                    return;
                }
                // F3 去重：火管 Direct + 收件箱主题双写时同一条会到两次，按 (sender, gram_id) 只投一次。
                if !self.seen.lock().unwrap().first_seen(&gram.sender, gram.gram_id) {
                    return;
                }
                let home_here = is_home_here(&ctx, to);
                deliver_account(to, &gram, &ctx, home_here, None).await;
            }
            Some(nm_proto::pb::group_gossip::Body::Revoke(r)) => {
                if let Err(e) = apply_revoke(&ctx, r, false) {
                    tracing::debug!("gossip revoke rejected: {e}");
                }
            }
            Some(nm_proto::pb::group_gossip::Body::Name(rec)) => {
                // 命名记录复制：验签后并入命名缓存（各节点据此解析 local@domain）。
                self.merge_name_lww(rec);
            }
            Some(nm_proto::pb::group_gossip::Body::InboxLive(_)) => {
                // F3 退火管：收件人 home 存活心跳。存活已由每群主题泵记入
                // per_group_live[account][origin]；此处无需额外动作（发送方据此省略火管 Direct）。
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
            // F1：per_topic 开启 → 起「每群主题」事件泵（FedMsg → on_group_gossip，与火管同一处理路径）。
            if self.per_topic.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(mut fed_rx) = self.fed_rx.lock().unwrap().take() {
                    let me = self.clone();
                    tokio::spawn(async move {
                        while let Some(fm) = fed_rx.recv().await {
                            // F5 存活账本：在该群主题上收到某节点的广播 ⇒ 该节点此刻订阅并存活于此主题。
                            if let Ok(gg) = GroupGossip::decode(fm.content.as_slice()) {
                                if !gg.origin.is_empty()
                                    && gg.origin.as_slice() != me.ep.id_bytes().as_slice()
                                {
                                    me.per_group_live
                                        .entry(fm.group.clone())
                                        .or_default()
                                        .insert(gg.origin, now_ms());
                                }
                            }
                            me.on_group_gossip(&fm.content).await;
                        }
                    });
                    tracing::info!("per-topic group pump started");
                }
            }
            let mut last_fed = tokio::time::Instant::now() - std::time::Duration::from_secs(10);
            let mut last_pkarr = tokio::time::Instant::now() - std::time::Duration::from_secs(120);
            loop {
                // 发布 fanout 丢来的待广播项（群消息 / 变更即时公告）。
                while let Ok(bytes) = pub_rx.try_recv() {
                    let _ = topic.publish(bytes).await;
                }
                // F1：per_topic 维护（每 ~3s）——join 本节点所托管群的每群主题 + 向自有群主题发 announce，
                // 使成员节点经 nmspace-group:<gid> 收播；非成员节点不订阅 → 收不到（去火管的核心）。
                if self.per_topic.load(std::sync::atomic::Ordering::Relaxed)
                    && last_fed.elapsed() >= std::time::Duration::from_secs(3)
                {
                    let ctx = self.ctx();
                    let me = self.ep.id_bytes();
                    let groups: Vec<Group> = self.groups.iter().map(|e| e.value().clone()).collect();
                    for g in groups {
                        if !g.members.iter().any(|m| is_home_here(&ctx, m)) {
                            continue; // 本节点不托管该群任何成员 → 不订阅其主题
                        }
                        let _ = self.fed.join_group(&g.group_id, group_bootstrap(&g, &ctx)).await;
                        // F5：任何「托管本群成员」的节点都在该群主题周期 announce，兼作存活心跳
                        // （非群 home 也发），使发送方据此判定「全员远端 home 在此主题存活」后省略火管群消息副本。
                        let bytes = encode_announce(&me, true, g.clone());
                        let _ = self.fed.publish(&g.group_id, bytes).await;
                    }
                    // F3：订阅本节点 home 账号的收件箱主题 nmspace-inbox:<account>，私聊单播经此主题投达
                    // （含离线：收件人 home 订阅后收取并入离线库）。join_inbox 幂等。
                    for acct in local_accounts(&ctx) {
                        let _ = self.fed.join_inbox(&acct, inbox_bootstrap(&acct, &ctx)).await;
                        // F3 退火管心跳：在本账号收件箱主题广播存活（origin=本节点=该账号 home）。
                        // 发送方（已 join 该主题以发 Direct）据此记 per_group_live[account][home]，
                        // 全部远端 home 存活时省略火管 Direct 副本。
                        let hb = GroupGossip {
                            origin: me.to_vec(),
                            body: Some(nm_proto::pb::group_gossip::Body::InboxLive(
                                nm_proto::pb::InboxLive { account: acct.clone() },
                            )),
                        };
                        let _ = self.fed.publish_inbox(&acct, hb.encode_to_vec()).await;
                    }
                    // F4：订阅本节点「关心的」域名主题 nmspace-names:<domain>——自持有域 ∪ 已缓存记录的域。
                    // 持有域的记录在 announce 块按域发布到各域主题（下方）；此处只负责 join（幂等）。
                    let mut interest: Vec<String> = self.domains.read().unwrap().clone();
                    for r in self.names.iter() {
                        if !interest.contains(&r.domain) {
                            interest.push(r.domain.clone());
                        }
                    }
                    for d in &interest {
                        let _ = self.fed.join_names(d, names_bootstrap(d, &ctx)).await;
                    }
                    last_fed = tokio::time::Instant::now();
                }
                // F2：pkarr 群发现（每 ~60s）——home 节点发布「群公钥 → seed」记录；非 home 成员节点解析缓存，
                // 供 group_bootstrap 无锚/兜底入网。仅 per_topic 开 + 配置了 pkarr relay 时。
                if self.per_topic.load(std::sync::atomic::Ordering::Relaxed)
                    && self.pkarr_endpoint().is_some()
                    && last_pkarr.elapsed() >= std::time::Duration::from_secs(60)
                {
                    let ctx = self.ctx();
                    let me = self.ep.id_bytes();
                    let groups: Vec<Group> = self.groups.iter().map(|e| e.value().clone()).collect();
                    for g in groups {
                        if !g.members.iter().any(|m| is_home_here(&ctx, m)) {
                            continue;
                        }
                        if g.home_node.as_slice() == me.as_slice() {
                            self.pkarr_publish_group(&g.group_id).await; // home 发布 seed 记录
                        } else {
                            let seeds = self.pkarr_resolve_group(&g.group_id).await; // 非 home 解析兜底
                            if !seeds.is_empty() {
                                self.pkarr_seeds.insert(g.group_id.clone(), seeds);
                            }
                        }
                    }
                    last_pkarr = tokio::time::Instant::now();
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
                        let per_topic = self.per_topic.load(std::sync::atomic::Ordering::Relaxed);
                        let _ = topic.publish(encode_announce(&me, per_topic, g)).await;
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
                            body: Some(nm_proto::pb::group_gossip::Body::Name(r.clone())),
                        };
                        let bytes = gg.encode_to_vec();
                        let _ = topic.publish(bytes.clone()).await; // 火管（兜底 / 未全迁移时）
                        // F4：per_topic 开 → 同一记录也发到其域主题 nmspace-names:<domain>（仅关心该域者收）。
                        // 与火管双写；收端 merge_name_lww（验签 + serial LWW）。退役待 resolve 触发订阅后另议。
                        if self.per_topic.load(std::sync::atomic::Ordering::Relaxed) {
                            let _ = self.fed.publish_names(&r.domain, bytes).await;
                        }
                    }
                    // 重播本节点登记过的设备的吊销记录，令晚加入节点也拒绝这些设备。
                    let my_revokes: Vec<DeviceRevoke> = self
                        .revoked
                        .iter()
                        .filter(|r| {
                            self.devices
                                .get(r.key())
                                .is_some_and(|d| d.cert.as_ref().is_some_and(|c| c.account == r.account))
                        })
                        .map(|r| r.clone())
                        .collect();
                    for r in my_revokes {
                        let gg = GroupGossip {
                            origin: me.to_vec(),
                            body: Some(nm_proto::pb::group_gossip::Body::Revoke(r)),
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
        if account_online(&self.sessions, &self.principals, entity) {
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
        presence_status(&self.sessions, &self.principals, &self.presence, &self.status_intent, entity)
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
        if account_online(&self.sessions, &self.principals, &entity) {
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
                    // 守卫不跨 await：先收集本地在线账号（设备连接折算到账号，去重）。
                    let mut entities: Vec<Vec<u8>> = self
                        .sessions
                        .iter()
                        .map(|e| self.principals.get(e.key()).map(|p| p.clone()).unwrap_or_else(|| e.key().clone()))
                        .collect();
                    entities.sort();
                    entities.dedup();
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

/// 从活动连接提取管理台用的链路质量快照（字节/RTT/丢包/路径类型等）。
fn session_row_from_conn(id: [u8; 32], since_unix_ms: u64, conn: &IrohConnection) -> SessionRow {
    let stats = conn.stats();
    let paths = conn.paths();
    let selected = paths
        .iter()
        .find(|p| p.is_selected())
        .or_else(|| paths.iter().next());
    let (rtt_ms, cwnd, mtu, congestion_events, black_holes, path_kind, remote_addr) =
        if let Some(p) = selected {
            let ps = p.stats();
            let kind = if p.is_relay() {
                "relay"
            } else if p.is_ip() {
                "direct"
            } else {
                ""
            };
            (
                Some(ps.rtt.as_secs_f64() * 1000.0),
                Some(ps.cwnd),
                Some(ps.current_mtu),
                ps.congestion_events,
                ps.black_holes_detected,
                kind.to_string(),
                p.remote_addr().to_string(),
            )
        } else {
            (None, None, None, 0, 0, String::new(), String::new())
        };
    SessionRow {
        id,
        since_unix_ms,
        bytes_tx: stats.udp_tx.bytes,
        bytes_rx: stats.udp_rx.bytes,
        alpn: String::from_utf8_lossy(conn.alpn()).into_owned(),
        rtt_ms,
        cwnd,
        mtu,
        lost_packets: stats.lost_packets,
        lost_bytes: stats.lost_bytes,
        datagrams_tx: stats.udp_tx.datagrams,
        datagrams_rx: stats.udp_rx.datagrams,
        congestion_events,
        black_holes,
        path_kind,
        remote_addr,
    }
}

/// 计算实体对外状态：本地在线会话优先（权威，取状态意图或 online）；否则查 presence 缓存并按 TTL 判离线。
fn presence_status(
    sessions: &Sessions,
    principals: &Principals,
    presence: &PresenceMap,
    intent: &StatusIntent,
    entity: &[u8],
) -> String {
    if account_online(sessions, principals, entity) {
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
        if is_revoked(&self.ctx, &rid_arr, None) {
            tracing::info!(peer = %conn.remote_id().fmt_short(), "rejected revoked device");
            conn.close(0u32.into(), b"device_revoked");
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
            let principals = self.ctx.principals.clone();
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
                    principals.remove(&watch_rid);
                    tracing::info!(online = sessions.len(), "session closed");
                }
            });
        }

        // 上线补投：把该连接（设备）的离线消息经 uni 流推送后清空。
        drain_inbox_to(&self.ctx, &rid, &conn);

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
        let principal = ctx.principals.get(&rid).map(|p| p.clone()).unwrap_or_else(|| rid.clone());
        if let Some(reply) = handle_gram(&gram, &ctx, &principal, &rid).await {
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
        ctx.principals.remove(&rid);
        tracing::info!(online = ctx.sessions.len(), "session down");
    }
}

/// `caller` 是账号身份（设备连接 hello 后为账号公钥，否则等于连接公钥）；`rid` 是连接公钥。
async fn handle_gram(gram: &Gram, ctx: &Ctx, caller: &[u8], rid: &[u8]) -> Option<Gram> {
    // 黑名单（纵深防御）：连接建立后才被拉黑的对端，其后续 gram 一律丢弃。
    for k in [caller, rid] {
        if let Ok(c) = <[u8; 32]>::try_from(k) {
            if ctx.blacklist.contains(&c) {
                return None;
            }
        }
    }
    // 防冒名：聊天消息的 sender 必须是本连接的账号身份。
    if matches!(gram.kind(), GramKind::Message | GramKind::GroupMessage) && gram.sender.as_slice() != caller {
        tracing::warn!("message sender != connection principal; dropped");
        return None;
    }
    // 群消息：receiver 是 group_id，由节点扇出（不是直接路由目标）。
    // 扇出可能对每个离线/跨节点成员做目录查询/中继（各带网络超时），若同步等待会
    // 阻塞给发送方的 ack → 前端「发送很迟钝」。故后台扇出、立即回执（存转发语义）。
    if matches!(gram.kind(), GramKind::GroupMessage) {
        let (g, c, who, r) = (gram.clone(), ctx.clone(), caller.to_vec(), rid.to_vec());
        tokio::spawn(async move { fanout_group(&g, &c, &who, &r).await });
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
                        push_command_denied(gram, rid, &ctx.sessions, &reason);
                        return Some(reply_gram(gram, GramKind::Receipt, None));
                    }
                }
            }
        }
        // Message 可离线暂存；Command/CommandResult 仅在线路由。
        // 单播私聊改走 gossip（本地投递不了则联邦广播，对端节点投递）——取代 NAT 下常不通的 s2s 中继。
        if matches!(gram.kind(), GramKind::Message) {
            let (g, c, who, r) = (gram.clone(), ctx.clone(), caller.to_vec(), rid.to_vec());
            tokio::spawn(async move {
                deliver_direct(&g, &c).await;
                // 已发同步：同账号的其它设备也收到一份（客户端按 sender==自己 归入会话）。
                if g.receiver != who {
                    deliver_account(&who, &g, &c, true, Some(&r)).await;
                }
            });
        } else {
            deliver_account(to, gram, ctx, false, None).await;
        }
        return Some(reply_gram(gram, GramKind::Receipt, None)); // 给发送方回 ack
    }
    // 面向节点本身的请求。
    match gram.kind() {
        GramKind::Command => handle_command(gram, ctx, caller, rid).await,
        GramKind::Message => Some(receipt_for(gram)),
        GramKind::Login => Some(login_ok(gram)),
        GramKind::Logout => {
            ctx.sessions.remove(rid); // 显式下线：同步移除会话
            ctx.principals.remove(rid);
            tracing::info!(online = ctx.sessions.len(), "session logout");
            Some(reply_gram(gram, GramKind::Reply, None))
        }
        _ => None,
    }
}

/// 群消息扇出：`receiver` 为 group_id；对每个成员(除发送方)——在线则路由、离线则入库。
async fn fanout_group(gram: &Gram, ctx: &Ctx, caller: &[u8], rid: &[u8]) {
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
    deliver_group_here(gram, &group.members, rid, ctx).await;
    // 跨节点：发布到群联邦 gossip，各成员节点收到后各自投递「本地成员」。
    // 取代此前的 s2s 中继（NAT 下常超时不通）——与频道同走 gossip 叠加网。
    let gg = GroupGossip {
        origin: ctx.node_id.to_vec(),
        body: Some(nm_proto::pb::group_gossip::Body::Msg(gram.clone())),
    };
    let bytes = gg.encode_to_vec();
    let per_topic = ctx.per_topic.load(std::sync::atomic::Ordering::Relaxed);
    // F5：当「本群所有远端成员(home!=本节点)的 home 都在该群主题上存活」时，省略火管的群消息副本——
    // 该消息仅经每群主题投递(仅成员节点收)，实现真正「仅成员可达」。保守：任一远端 home 存活未知/过期
    // → 仍发火管(自愈，不漏投)。注意仅针对群「消息」；announce 仍走火管(发现/收敛)，直至 F2 DHT 落地。
    let retireable = per_topic
        && group_topic_retireable(&ctx.node_id, &ctx.dir.entities, &ctx.per_group_live, &group, now_ms(), LIVE_TTL_MS);
    // 每群主题双写（仅成员节点收；收端按 gram_id 去重 F1-2a）。
    let mut per_topic_ok = false;
    if per_topic {
        let _ = ctx.fed.join_group(&gram.receiver, group_bootstrap(&group, ctx)).await;
        per_topic_ok = ctx.fed.publish(&gram.receiver, bytes.clone()).await.is_ok();
    }
    // 火管兜底：仅当「可退火管」且每群主题确已发出时才省略；否则(含 per-topic 发布失败)仍发火管，绝不漏投。
    if retireable && per_topic_ok {
        tracing::debug!("firehose suppressed for group message (all remote homes live on topic)");
    } else {
        let _ = ctx.group_pub.send(bytes);
    }
}

/// 某群主题的 bootstrap 对端：群归属节点(锚点) + 本节点已知对等 + F2 pkarr 解析到的 seed（无锚/兜底）。
fn group_bootstrap(group: &Group, ctx: &Ctx) -> Vec<[u8; 32]> {
    let mut v: Vec<[u8; 32]> = Vec::new();
    if let Ok(h) = <[u8; 32]>::try_from(group.home_node.as_slice()) {
        v.push(h);
    }
    for e in ctx.peers.iter() {
        if let Ok(id) = <[u8; 32]>::try_from(e.key().as_slice()) {
            if !v.contains(&id) {
                v.push(id);
            }
        }
    }
    // F2：并入 pkarr 解析到的 seed（home 不可达 / 无锚发现时的入网点）。
    if let Some(s) = ctx.pkarr_seeds.get(&group.group_id) {
        for id in s.value() {
            if !v.contains(id) {
                v.push(*id);
            }
        }
    }
    v
}

/// F3：收件人收件箱主题的 bootstrap——收件人 home 节点(锚点) + 本节点已知对等。
/// 发送方据此接入 `nmspace-inbox:<to>` 叠加网（收件人 home 已订阅在其上）。
fn inbox_bootstrap(account: &[u8], ctx: &Ctx) -> Vec<[u8; 32]> {
    let mut v: Vec<[u8; 32]> = Vec::new();
    if let Some(e) = ctx.dir.entities.get(account) {
        if let Ok(h) = <[u8; 32]>::try_from(e.home_node.as_slice()) {
            v.push(h);
        }
    }
    for e in ctx.peers.iter() {
        if let Ok(id) = <[u8; 32]>::try_from(e.key().as_slice()) {
            if !v.contains(&id) {
                v.push(id);
            }
        }
    }
    v
}

/// F3：本节点为 home 的账号集合（目录 home_node==本节点 ∪ 本地登记设备的账号）。
/// 这些账号的收件箱主题需由本节点订阅，以收取发给它们的私聊单播（在线直投 / 离线入库）。
fn local_accounts(ctx: &Ctx) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    for e in ctx.dir.entities.iter() {
        if e.value().home_node == ctx.node_id.as_slice() && !out.contains(e.key()) {
            out.push(e.key().clone());
        }
    }
    for d in ctx.devices.iter() {
        if let Some(c) = d.cert.as_ref() {
            if !out.contains(&c.account) {
                out.push(c.account.clone());
            }
        }
    }
    out
}

/// F4：某命名域主题的 bootstrap——该域已知记录的 home 节点(锚点) + 本节点已知对等。
fn names_bootstrap(domain: &str, ctx: &Ctx) -> Vec<[u8; 32]> {
    let mut v: Vec<[u8; 32]> = Vec::new();
    for r in ctx.names.iter() {
        if r.domain == domain {
            if let Ok(h) = <[u8; 32]>::try_from(r.home_node.as_slice()) {
                if !v.contains(&h) {
                    v.push(h);
                }
            }
        }
    }
    for e in ctx.peers.iter() {
        if let Ok(id) = <[u8; 32]>::try_from(e.key().as_slice()) {
            if !v.contains(&id) {
                v.push(id);
            }
        }
    }
    v
}

/// F1 协商就绪判定（F5 预备）：该群是否「全员经每群主题可达」——每个**远端**成员的 home 节点都已
/// 广告支持 per_topic。本地成员（home==本节点）本地投递、无需联邦，跳过。
/// 保守：任一成员 home 未知 / home 未广告 → false（继续走火管，不漏投）。
/// F5（火管退役）将据此对「全员可达」的群跳过火管；在此之前只广告 + 跟踪，不改投递路径。
/// 注意：能力广告 ≠ 已订阅该群主题（广告与 join 扫描之间有窗口），F5 落地还须加「主题邻居存活」信号。
fn group_fully_per_topic(
    node_id: &[u8],
    entities: &DashMap<Vec<u8>, Entity>,
    per_topic_nodes: &DashSet<Vec<u8>>,
    group: &Group,
) -> bool {
    for m in &group.members {
        let Some(e) = entities.get(m) else {
            return false; // 成员 home 未知 → 保守
        };
        if e.home_node.is_empty() {
            return false;
        }
        if e.home_node.as_slice() == node_id {
            continue; // 本地成员，本地投递
        }
        if !per_topic_nodes.contains(&e.home_node) {
            return false; // 远端成员 home 未广告 per_topic → 保守
        }
    }
    true
}

/// F5 判据：该群的「群消息」火管副本现在可省略吗？——每个**远端**成员(home!=本节点)的 home 节点
/// 都在该群主题上「存活」(近 `ttl_ms` 内经 nmspace-group:<gid> 收到过其广播/心跳)。本地成员本地直投、
/// 无需联邦，跳过。保守：任一成员 home 未知、或其存活缺失/过期 → false(仍发火管，自愈不漏投)。
/// 仅针对群「消息」；announce 仍走火管(发现/收敛)，直至 F2 DHT 发现落地后方可彻底退火管。
/// 比 [`group_fully_per_topic`]（仅能力广告）更强：存活=确已订阅并在线于此主题，非仅声明支持。
fn group_topic_retireable(
    node_id: &[u8],
    entities: &DashMap<Vec<u8>, Entity>,
    live: &GroupLive,
    group: &Group,
    now_ms: u64,
    ttl_ms: u64,
) -> bool {
    let gl = live.get(&group.group_id);
    for m in &group.members {
        let Some(e) = entities.get(m) else {
            return false; // 成员 home 未知 → 保守
        };
        if e.home_node.is_empty() {
            return false;
        }
        if e.home_node.as_slice() == node_id {
            continue; // 本地成员，本地投递
        }
        let fresh = gl
            .as_ref()
            .and_then(|g| g.get(&e.home_node).map(|ts| now_ms.saturating_sub(*ts) < ttl_ms))
            .unwrap_or(false);
        if !fresh {
            return false; // 远端成员 home 在此主题存活未知/过期 → 保守
        }
    }
    true
}

/// F3 退火管判据：远端收件人 `account` 的 home 节点是否在其收件箱主题 `nmspace-inbox:<account>` 上
/// 「存活」（近 `ttl_ms` 内收到过其心跳 → per_group_live[account][home] 新鲜）。为 true 时发私聊单播
/// 可省略火管 `Direct` 副本（仅经收件箱主题投达）。保守：home 未知/本地/存活过期 → false（仍发火管）。
fn inbox_retireable(
    account: &[u8],
    node_id: &[u8],
    entities: &DashMap<Vec<u8>, Entity>,
    live: &GroupLive,
    now_ms: u64,
    ttl_ms: u64,
) -> bool {
    let Some(e) = entities.get(account) else {
        return false; // 收件人 home 未知 → 保守
    };
    if e.home_node.is_empty() || e.home_node.as_slice() == node_id {
        return false; // 本地收件人不走此路径（已本地直投/入库）
    }
    live
        .get(account)
        .and_then(|m| m.get(&e.home_node).map(|ts| now_ms.saturating_sub(*ts) < ttl_ms))
        .unwrap_or(false)
}

/// 把群消息投递给「本节点负责的成员」：在线设备直投；本节点是其 home 则离线设备入库。
/// 发送方自己的其它设备也会收到（已发同步），只跳过发出这条消息的连接 `skip_rid`。
async fn deliver_group_here(gram: &Gram, members: &[Vec<u8>], skip_rid: &[u8], ctx: &Ctx) {
    for m in members {
        let home_here = is_home_here(ctx, m);
        deliver_account(m, gram, ctx, home_here, Some(skip_rid)).await;
    }
}

/// 本节点是否为该账号的 home（目录登记 home_node 为本节点，或有设备在本节点登记）。
fn is_home_here(ctx: &Ctx, account: &[u8]) -> bool {
    if let Some(e) = ctx.dir.entities.get(account) {
        if e.home_node == ctx.node_id.as_slice() {
            return true;
        }
    }
    ctx.devices.iter().any(|d| d.cert.as_ref().is_some_and(|c| c.account == account))
}

/// 账号当前在本节点的在线连接：账号公钥直连（老客户端）+ hello 过的设备连接。
fn online_rids(sessions: &Sessions, principals: &Principals, account: &[u8]) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = principals
        .iter()
        .filter(|e| e.value().as_slice() == account && sessions.contains_key(e.key()))
        .map(|e| e.key().clone())
        .collect();
    if sessions.contains_key(account) {
        v.push(account.to_vec());
    }
    v
}

fn account_online(sessions: &Sessions, principals: &Principals, account: &[u8]) -> bool {
    sessions.contains_key(account)
        || principals.iter().any(|e| e.value().as_slice() == account && sessions.contains_key(e.key()))
}

/// 账号离线消息的存放对象：有效（未吊销）设备各一份；从未登记设备的老账号存在账号公钥下。
fn inbox_targets(ctx: &Ctx, account: &[u8]) -> Vec<Vec<u8>> {
    let devs: Vec<Vec<u8>> = ctx
        .devices
        .iter()
        .filter(|d| d.cert.as_ref().is_some_and(|c| c.account == account) && !ctx.revoked.contains_key(d.key()))
        .map(|d| d.key().clone())
        .collect();
    if devs.is_empty() {
        vec![account.to_vec()]
    } else {
        devs
    }
}

/// 投递给账号：所有在线连接直推；`store_offline` 时给没收到的有效设备各存一份。
/// `skip` 为不需要回送的连接（发送方自己）。返回是否至少推到一台在线设备。
async fn deliver_account(account: &[u8], gram: &Gram, ctx: &Ctx, store_offline: bool, skip: Option<&[u8]>) -> bool {
    let mut reached: Vec<Vec<u8>> = Vec::new();
    for r in online_rids(&ctx.sessions, &ctx.principals, account) {
        if skip == Some(r.as_slice()) {
            continue;
        }
        if try_push(&r, gram, &ctx.sessions).await {
            reached.push(r);
        }
    }
    if store_offline {
        if let Some(s) = &ctx.store {
            for d in inbox_targets(ctx, account) {
                if skip == Some(d.as_slice()) || reached.contains(&d) {
                    continue;
                }
                if let Err(e) = s.push_inbox(&d, gram) {
                    tracing::warn!("push_inbox failed: {e}");
                }
            }
        }
    }
    !reached.is_empty()
}

/// 取走某连接名下的离线消息并推送给它。
fn drain_inbox_to(ctx: &Ctx, rid: &[u8], conn: &IrohConnection) {
    let Some(store) = &ctx.store else { return };
    let Ok(pending) = store.drain_inbox(rid) else { return };
    if pending.is_empty() {
        return;
    }
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

/// 构造群 Announce 的 gossip 字节：origin=本节点；`supports_per_topic`=本节点能力广告（F1 协商）。
/// 该标志是「节点能力」不是「群状态」——仅在 announce 瞬间按本节点 per_topic 置位，收端据 origin 跟踪。
fn encode_announce(origin: &[u8], per_topic: bool, mut g: Group) -> Vec<u8> {
    g.supports_per_topic = per_topic;
    GroupGossip {
        origin: origin.to_vec(),
        body: Some(nm_proto::pb::group_gossip::Body::Announce(g)),
    }
    .encode_to_vec()
}

/// 立即向联邦广播某群当前状态（发现 + 成员表）。群变更后调用，避免等周期公告。
fn announce_group(ctx: &Ctx, gid: &[u8]) {
    if let Some(g) = ctx.groups.get(gid).map(|g| g.clone()) {
        let per_topic = ctx.per_topic.load(std::sync::atomic::Ordering::Relaxed);
        let _ = ctx.group_pub.send(encode_announce(&ctx.node_id, per_topic, g));
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
fn hash_account_password(pw: &str) -> Result<String, String> {
    use argon2::password_hash::PasswordHasher;
    use argon2::Argon2;
    Argon2::default()
        .hash_password(pw.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn verify_account_password(phc: &str, pw: &str) -> bool {
    use argon2::password_hash::phc::PasswordHash;
    use argon2::password_hash::PasswordVerifier;
    use argon2::Argon2;
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default().verify_password(pw.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

fn account_params(cmd: &Command) -> serde_json::Value {
    let text = cmd
        .params
        .as_ref()
        .map(|p| String::from_utf8_lossy(&p.value).into_owned())
        .unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_default()
}

struct AcctReq {
    domain: String,
    full: String,
    password: String,
    old: String,
}

fn parse_acct(cmd: &Command) -> Result<AcctReq, &'static str> {
    let v = account_params(cmd);
    let local = v.get("local").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    let domain = v.get("domain").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    if !valid_local_part(&local) || domain.is_empty() || domain.len() > 253 {
        return Err("invalid_name");
    }
    Ok(AcctReq {
        full: format!("{local}@{domain}"),
        domain,
        password: v.get("password").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        old: v.get("old").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
}

fn acct_fail(code: &'static str) -> (bool, Option<Any>, String) {
    (false, None, code.into())
}

fn acct_text(text: &str) -> (bool, Option<Any>, String) {
    (true, Some(Any { type_url: "text/plain".into(), value: text.as_bytes().to_vec() }), String::new())
}

fn owns_domain(ctx: &Ctx, domain: &str) -> bool {
    ctx.domains.read().unwrap().iter().any(|d| d == domain)
}

/// 在本节点登记 local@domain。口令只存 Argon2 PHC，明文不落盘。
fn register_name_account(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let v = account_params(cmd);
    let local = v.get("local").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    let domain = v.get("domain").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    let password = v.get("password").and_then(|x| x.as_str()).unwrap_or("");
    if !owns_domain(ctx, &domain) {
        return acct_fail("domain_not_owned");
    }
    if !valid_local_part(&local) {
        return acct_fail("invalid_name");
    }
    if password.chars().count() < 8 {
        return acct_fail("password_short");
    }
    let full = format!("{local}@{domain}");
    if let Some(r) = ctx.names.get(&full) {
        if r.client_pubkey.as_slice() != caller {
            return acct_fail("name_taken");
        }
        return acct_fail("name_taken");
    }
    let phc = match hash_account_password(password) {
        Ok(h) => h,
        Err(e) => return (false, None, format!("口令无法保存: {e}")),
    };
    let Some(store) = ctx.store.as_ref() else {
        return (false, None, "节点未打开数据库，无法保存口令".into());
    };
    if let Err(e) = store.put_name_secret(&full, &phc) {
        return (false, None, format!("口令无法保存: {e}"));
    }
    let mut rec = NameRecord {
        local_part: local,
        domain,
        client_pubkey: caller.to_vec(),
        serial: 1,
        issued_at: now_ms() as i64,
        ttl: 3600,
        home_node: ctx.node_id.to_vec(),
        home_sig: Vec::new(),
    };
    rec.home_sig = nm_crypto::sign_bytes(ctx.ep.secret_key(), &name_canonical(&rec)).to_vec();
    if let Err(e) = store.put_name(&rec) {
        return (false, None, format!("名字无法保存: {e}"));
    }
    ctx.names.insert(full, rec.clone());
    let gg = GroupGossip {
        origin: ctx.node_id.to_vec(),
        body: Some(nm_proto::pb::group_gossip::Body::Name(rec.clone())),
    };
    let _ = ctx.group_pub.send(gg.encode_to_vec());
    (
        true,
        Some(Any {
            type_url: "text/plain".into(),
            value: hex_encode(caller).into_bytes(),
        }),
        String::new(),
    )
}

/// 在家节点比对口令。成功时返回该名字登记的客户端公钥 hex。
fn login_name_account(ctx: &Ctx, _caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let v = account_params(cmd);
    let local = v.get("local").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    let domain = v.get("domain").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    let password = v.get("password").and_then(|x| x.as_str()).unwrap_or("");
    if !valid_local_part(&local) || domain.is_empty() {
        return acct_fail("invalid_name");
    }
    let full = format!("{local}@{domain}");
    let Some(rec) = ctx.names.get(&full) else {
        return acct_fail("no_such_user");
    };
    let Some(store) = ctx.store.as_ref() else {
        return acct_fail("no_store");
    };
    let phc = match store.name_secret(&full) {
        Ok(Some(h)) => h,
        Ok(None) => return acct_fail("no_password"),
        Err(_) => return acct_fail("no_store"),
    };
    if !verify_account_password(&phc, password) {
        return acct_fail("bad_password");
    }
    (
        true,
        Some(Any {
            type_url: "text/plain".into(),
            value: hex_encode(&rec.client_pubkey).into_bytes(),
        }),
        String::new(),
    )
}

/// 注册前询问名字是否已存在。不比对口令，也不返回私钥。
fn lookup_name_account(ctx: &Ctx, cmd: &Command) -> (bool, Option<Any>, String) {
    let req = match parse_acct(cmd) {
        Ok(r) => r,
        Err(code) => return acct_fail(code),
    };
    if !owns_domain(ctx, &req.domain) {
        return acct_fail("domain_not_owned");
    }
    let exists = if ctx.names.contains_key(&req.full) { "1" } else { "0" };
    acct_text(exists)
}

/// 已登录且公钥匹配时，用旧密码换成新的 Argon2 哈希。
fn passwd_name_account(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let req = match parse_acct(cmd) {
        Ok(r) => r,
        Err(code) => return acct_fail(code),
    };
    if req.password.chars().count() < 8 {
        return acct_fail("password_short");
    }
    if req.password == req.old {
        return acct_fail("same_password");
    }
    let owner = match ctx.names.get(&req.full) {
        Some(rec) => rec.client_pubkey.clone(),
        None => return acct_fail("no_such_user"),
    };
    if owner.as_slice() != caller {
        return acct_fail("not_key_owner");
    }
    let Some(store) = ctx.store.as_ref() else {
        return acct_fail("no_store");
    };
    let phc = match store.name_secret(&req.full) {
        Ok(Some(h)) => h,
        Ok(None) => return acct_fail("no_password"),
        Err(_) => return acct_fail("no_store"),
    };
    if !verify_account_password(&phc, &req.old) {
        return acct_fail("bad_password");
    }
    match hash_account_password(&req.password) {
        Ok(h) => match store.put_name_secret(&req.full, &h) {
            Ok(()) => acct_text("ok"),
            Err(_) => acct_fail("no_store"),
        },
        Err(_) => acct_fail("no_store"),
    }
}

/// 本机私钥与登记公钥一致时，不需要旧密码即可设置新密码。
fn reset_name_account(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let req = match parse_acct(cmd) {
        Ok(r) => r,
        Err(code) => return acct_fail(code),
    };
    if req.password.chars().count() < 8 {
        return acct_fail("password_short");
    }
    let owner = match ctx.names.get(&req.full) {
        Some(rec) => rec.client_pubkey.clone(),
        None => return acct_fail("no_such_user"),
    };
    if owner.as_slice() != caller {
        return acct_fail("not_key_owner");
    }
    let Some(store) = ctx.store.as_ref() else {
        return acct_fail("no_store");
    };
    match hash_account_password(&req.password) {
        Ok(h) => match store.put_name_secret(&req.full, &h) {
            Ok(()) => acct_text("ok"),
            Err(_) => acct_fail("no_store"),
        },
        Err(_) => acct_fail("no_store"),
    }
}

// ── 多设备：证书登记 / 列表 / 改名 / 吊销 / 紧急冻结 ──

/// 账号公钥直连（老客户端或持有账号私钥的连接）或 admin 设备连接。
fn is_admin_conn(ctx: &Ctx, caller: &[u8], rid: &[u8]) -> bool {
    rid == caller
        || ctx.devices.get(rid).is_some_and(|d| {
            d.cert.as_ref().is_some_and(|c| c.account == caller && c.role == nm_crypto::DEVICE_ROLE_ADMIN)
        })
}

/// 设备是否已被其账号吊销。`account` 未知时以本节点登记的证书账号为准。
fn is_revoked(ctx: &Ctx, device: &[u8], account: Option<&[u8]>) -> bool {
    let Some(r) = ctx.revoked.get(device) else { return false };
    match account {
        Some(a) => r.account == a,
        None => ctx
            .devices
            .get(device)
            .and_then(|d| d.cert.as_ref().map(|c| c.account == r.account))
            .unwrap_or(false),
    }
}

/// 设备连接出示账号签发的证书，登记后该连接以账号身份收发。
fn device_hello(ctx: &Ctx, rid: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let Some(cert) = cmd.params.as_ref().and_then(|p| DeviceCert::decode(p.value.as_slice()).ok()) else {
        return acct_fail("invalid_cert");
    };
    if nm_crypto::verify_device_cert(&cert).is_err() {
        return acct_fail("bad_cert");
    }
    if cert.device != rid {
        return acct_fail("device_mismatch");
    }
    if is_revoked(ctx, rid, Some(&cert.account)) {
        if let Some(conn) = ctx.sessions.get(rid).map(|c| c.clone()) {
            conn.close(0u32.into(), b"device_revoked");
        }
        return acct_fail("device_revoked");
    }
    if <[u8; 32]>::try_from(cert.account.as_slice()).is_ok_and(|a| ctx.blacklist.contains(&a)) {
        return acct_fail("banned");
    }
    let mut info = ctx.devices.get(rid).map(|d| d.clone()).unwrap_or_default();
    let keep_old = info
        .cert
        .as_ref()
        .is_some_and(|old| old.account == cert.account && old.issued_at > cert.issued_at);
    if !keep_old {
        if info.cert.as_ref().is_some_and(|old| old.account != cert.account) {
            info.label = String::new();
        }
        info.cert = Some(cert.clone());
    }
    if info.label.is_empty() {
        info.label = cert.label.clone();
    }
    info.last_seen = now_ms() as i64;
    info.online = false;
    info.revoked = None;
    if let Some(s) = &ctx.store {
        if let Err(e) = s.put_device(&info) {
            tracing::warn!("persist device failed: {e}");
        }
    }
    let account = info.cert.as_ref().map(|c| c.account.clone()).unwrap_or_default();
    ctx.devices.insert(rid.to_vec(), info.clone());
    ctx.principals.insert(rid.to_vec(), account.clone());
    tracing::info!(device = %hex_encode(&rid[..4]), account = %hex_encode(&account[..4]), "device hello");
    // 补投 hello 之前按设备存下的消息，以及登记设备前按账号存下的旧消息。
    if let Some(conn) = ctx.sessions.get(rid).map(|c| c.clone()) {
        drain_inbox_to(ctx, rid, &conn);
        drain_inbox_to(ctx, &account, &conn);
    }
    info.online = true;
    (true, Some(Any { type_url: "nmspace.v1.DeviceInfo".into(), value: info.encode_to_vec() }), String::new())
}

fn device_list(ctx: &Ctx, caller: &[u8]) -> (bool, Option<Any>, String) {
    let mut devices: Vec<DeviceInfo> = ctx
        .devices
        .iter()
        .filter(|d| d.cert.as_ref().is_some_and(|c| c.account == caller))
        .map(|d| {
            let mut i = d.clone();
            i.online = ctx.sessions.contains_key(d.key())
                && ctx.principals.get(d.key()).is_some_and(|p| p.as_slice() == caller);
            i.revoked = ctx.revoked.get(d.key()).filter(|r| r.account == caller).map(|r| r.clone());
            i
        })
        .collect();
    devices.sort_by_key(|d| d.cert.as_ref().map(|c| c.issued_at).unwrap_or(0));
    (
        true,
        Some(Any { type_url: "nmspace.v1.DeviceList".into(), value: DeviceList { devices }.encode_to_vec() }),
        String::new(),
    )
}

fn device_rename(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let v = account_params(cmd);
    let Some(dev) = v.get("device").and_then(|x| x.as_str()).and_then(hex_decode_n::<32>) else {
        return acct_fail("invalid_device");
    };
    let label = v.get("label").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    if label.is_empty() || label.chars().count() > 64 {
        return acct_fail("invalid_label");
    }
    let Some(mut info) = ctx.devices.get(&dev[..]).map(|d| d.clone()) else {
        return acct_fail("no_such_device");
    };
    if !info.cert.as_ref().is_some_and(|c| c.account == caller) {
        return acct_fail("not_owner");
    }
    info.label = label;
    if let Some(s) = &ctx.store {
        if s.put_device(&info).is_err() {
            return acct_fail("no_store");
        }
    }
    ctx.devices.insert(dev.to_vec(), info);
    acct_text("ok")
}

/// 紧急冻结：凭账号口令由家节点代签吊销（无管理设备在手时用）。
fn device_freeze(ctx: &Ctx, cmd: &Command) -> (bool, Option<Any>, String) {
    let req = match parse_acct(cmd) {
        Ok(r) => r,
        Err(code) => return acct_fail(code),
    };
    let v = account_params(cmd);
    let dev_hex = v.get("device").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let only = if dev_hex.is_empty() {
        None
    } else {
        match hex_decode_n::<32>(&dev_hex) {
            Some(d) => Some(d),
            None => return acct_fail("invalid_device"),
        }
    };
    let account = match ctx.names.get(&req.full) {
        Some(r) if r.home_node == ctx.node_id.as_slice() => r.client_pubkey.clone(),
        Some(_) => return acct_fail("not_home_node"),
        None => return acct_fail("no_such_user"),
    };
    let Some(store) = ctx.store.as_ref() else {
        return acct_fail("no_store");
    };
    match store.name_secret(&req.full) {
        Ok(Some(phc)) if verify_account_password(&phc, &req.password) => {}
        Ok(Some(_)) => return acct_fail("bad_password"),
        Ok(None) => return acct_fail("no_password"),
        Err(_) => return acct_fail("no_store"),
    }
    let Ok(account_arr) = <[u8; 32]>::try_from(account.as_slice()) else {
        return acct_fail("no_such_user");
    };
    // 指定设备只冻结它；不指定则冻结该账号全部有效设备（找不到设备公钥时的兜底）。
    let targets: Vec<[u8; 32]> = ctx
        .devices
        .iter()
        .filter(|d| d.cert.as_ref().is_some_and(|c| c.account == account))
        .filter(|d| !is_revoked(ctx, d.key(), Some(&account)))
        .filter_map(|d| <[u8; 32]>::try_from(d.key().as_slice()).ok())
        .filter(|d| only.is_none_or(|o| o == *d))
        .collect();
    if targets.is_empty() {
        return acct_fail("no_such_device");
    }
    let now = now_ms() as i64;
    for dev in &targets {
        let r = nm_crypto::sign_device_revoke(ctx.ep.secret_key(), account_arr, *dev, now, "freeze", true);
        if let Err(e) = apply_revoke(ctx, r, true) {
            return acct_fail(e);
        }
    }
    acct_text(&targets.len().to_string())
}

/// 节点能否代该账号冻结设备：须是该账号的家节点（目录或命名记录登记）。
fn node_may_freeze(ctx: &Ctx, account: &[u8], node: &[u8]) -> bool {
    node == ctx.node_id.as_slice()
        || ctx.dir.entities.get(account).is_some_and(|e| e.home_node == node)
        || ctx.names.iter().any(|n| n.client_pubkey == account && n.home_node == node)
}

/// 校验并落地一条吊销：断开该设备连接、丢弃其信箱、通知同账号在线设备，可选联邦广播。
/// 返回是否为新吊销（重复的返回 false）。
fn apply_revoke(ctx: &Ctx, r: DeviceRevoke, broadcast: bool) -> Result<bool, &'static str> {
    nm_crypto::verify_device_revoke(&r).map_err(|_| "bad_signature")?;
    if !r.by_node.is_empty() && !node_may_freeze(ctx, &r.account, &r.by_node) {
        return Err("node_not_home");
    }
    let registered_account = ctx.devices.get(&r.device).and_then(|d| d.cert.as_ref().map(|c| c.account.clone()));
    if registered_account.as_ref().is_some_and(|a| *a != r.account) {
        return Err("not_owner");
    }
    if let Some(old) = ctx.revoked.get(&r.device) {
        // 同账号已吊销则幂等；他人抢占的记录仅在本条属于登记账号时覆盖。
        if old.account == r.account || registered_account.is_none() {
            return Ok(false);
        }
    }
    if let Some(s) = &ctx.store {
        s.put_revoke(&r).map_err(|_| "no_store")?;
    }
    ctx.revoked.insert(r.device.clone(), r.clone());
    if let Some((_, conn)) = ctx.sessions.remove(&r.device) {
        ctx.sessions_meta.remove(&r.device);
        conn.close(0u32.into(), b"device_revoked");
    }
    ctx.principals.remove(&r.device);
    if let Some(s) = &ctx.store {
        let _ = s.drain_inbox(&r.device);
    }
    tracing::info!(device = %hex_encode(&r.device[..4]), by_node = !r.by_node.is_empty(), "device revoked");
    notify_account(
        ctx,
        &r.account,
        serde_json::json!({
            "event": "revoked",
            "device": hex_encode(&r.device),
            "reason": r.reason,
            "by_node": !r.by_node.is_empty(),
        }),
    );
    if broadcast {
        let gg = GroupGossip {
            origin: ctx.node_id.to_vec(),
            body: Some(nm_proto::pb::group_gossip::Body::Revoke(r)),
        };
        let _ = ctx.group_pub.send(gg.encode_to_vec());
    }
    Ok(true)
}

/// 向账号所有在线设备推一条设备事件（客户端按 type_url 拦截，不进聊天）。
fn notify_account(ctx: &Ctx, account: &[u8], event: serde_json::Value) {
    let rids = online_rids(&ctx.sessions, &ctx.principals, account);
    if rids.is_empty() {
        return;
    }
    let now = now_ms();
    let gram = Gram {
        version: PROTOCOL_VERSION,
        kind: GramKind::Message as i32,
        gram_id: now,
        ref_gram_id: None,
        sender: ctx.node_id.to_vec(),
        receiver: account.to_vec(),
        timestamp_ms: now,
        payload: Some(Any { type_url: DEVICE_EVENT_TYPE.into(), value: event.to_string().into_bytes() }),
        crc: Vec::new(),
    };
    let sessions = ctx.sessions.clone();
    tokio::spawn(async move {
        for r in rids {
            try_push(&r, &gram, &sessions).await;
        }
    });
}

fn valid_local_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_')
}

/// 收件人是本节点时，把注册中心的通过/驳回写入本地域名或通知列表。
fn apply_domain_decision(
    gram: &Gram,
    domains: &std::sync::RwLock<Vec<String>>,
    store: &Option<Arc<RedbStore>>,
    notices: &DashMap<String, (bool, u64)>,
) {
    if gram.kind() != GramKind::Command {
        return;
    }
    let Some(payload) = gram.payload.as_ref() else { return };
    let Ok(cmd) = Command::decode(payload.value.as_slice()) else { return };
    if cmd.method != "domain.decision" {
        return;
    }
    let text = cmd
        .params
        .as_ref()
        .map(|p| String::from_utf8_lossy(&p.value).into_owned())
        .unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    let Some(domain) = v.get("domain").and_then(|d| d.as_str()) else { return };
    let approved = v.get("approved").and_then(|d| d.as_bool()).unwrap_or(false);
    let d = domain.trim().to_lowercase();
    if d.is_empty() {
        return;
    }
    if approved && !domains.read().unwrap().iter().any(|x| x == &d) {
        domains.write().unwrap().push(d.clone());
        if let Some(s) = store {
            let _ = s.put_domain(&d);
        }
    }
    notices.insert(d, (approved, now_ms()));
}

/// 私聊单播投递（不依赖 s2s 中继）：
/// 1) 本地在线 → 直投；2) 已知且 home==本节点(离线) → 本地离线库；
/// 3) 已知远端 → 仅联邦 gossip 广播（对端节点投递/入库）；
/// 4) 完全未知 → 本地离线库兜底(可能连来本节点) + 联邦 gossip 广播(可能在远端)。
async fn deliver_direct(gram: &Gram, ctx: &Ctx) {
    let to = &gram.receiver;
    let home_here = is_home_here(ctx, to);
    let known_remote = !home_here && ctx.dir.entities.contains_key(to.as_slice());
    // 1) 本地在线设备直投；2) home 在本节点 → 离线设备入库；4) 完全未知 → 本地兜底入库（可能连来本节点）。
    let online = deliver_account(to, gram, ctx, !known_remote, None).await;
    if online || home_here {
        return;
    }
    // 远端 / 未知：经联邦 gossip 广播；对端节点收到后投递（取代 s2s 中继）。
    let gg = GroupGossip {
        origin: ctx.node_id.to_vec(),
        body: Some(nm_proto::pb::group_gossip::Body::Direct(gram.clone())),
    };
    let bytes = gg.encode_to_vec();
    let per_topic = ctx.per_topic.load(std::sync::atomic::Ordering::Relaxed);
    // F3：per_topic 开 → 投到收件人收件箱主题 nmspace-inbox:<to>（仅其 home 订阅 → 私密单播，
    // 取代火管 Direct 全广播 + NAT 下常不通的 s2s 直投）。收端按 (sender,gram_id) 去重。
    // 退火管：收件人 home 已在其收件箱主题存活（收到过其心跳）且收件箱发布成功 → 省略火管 Direct 副本；
    // 否则（存活未知/过期/发布失败）仍发火管，自愈不漏投。
    let retire = per_topic
        && inbox_retireable(to, &ctx.node_id, &ctx.dir.entities, &ctx.per_group_live, now_ms(), LIVE_TTL_MS);
    let mut inbox_ok = false;
    if per_topic {
        let _ = ctx.fed.join_inbox(to, inbox_bootstrap(to, ctx)).await;
        inbox_ok = ctx.fed.publish_inbox(to, bytes.clone()).await.is_ok();
    }
    if retire && inbox_ok {
        tracing::debug!("firehose Direct suppressed (recipient home live on inbox topic)");
    } else {
        let _ = ctx.group_pub.send(bytes); // 火管（兜底 / 未全迁移时）
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
    // 1) 本地在线设备直投（同时给离线的有效设备各存一份）。
    if account_online(&ctx.sessions, &ctx.principals, to) {
        deliver_account(to, gram, ctx, true, None).await;
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
    if ctx.store.is_some() {
        deliver_account(to, gram, ctx, true, None).await;
        tracing::info!("target offline; stored to inbox");
    } else {
        tracing::warn!("target offline; no store; dropping");
    }
}

async fn handle_command(gram: &Gram, ctx: &Ctx, caller: &[u8], rid: &[u8]) -> Option<Gram> {
    let dir = &*ctx.dir;
    let groups = &*ctx.groups;
    let store = &ctx.store;
    let cmd = Command::decode(gram.payload.as_ref()?.value.as_slice()).ok()?;
    let (ok, result, error) = match cmd.method.as_str() {
        "domain.decision" => {
            apply_domain_decision(gram, &ctx.domains, &ctx.store, &ctx.domain_notices);
            (true, None, String::new())
        }
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
                let p = presence_status(&ctx.sessions, &ctx.principals, &ctx.presence, &ctx.status_intent, &e.entity_id);
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
            if account_online(&ctx.sessions, &ctx.principals, caller) {
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
        "name.register" => register_name_account(ctx, caller, &cmd),
        "name.login" => login_name_account(ctx, caller, &cmd),
        "name.lookup" => lookup_name_account(ctx, &cmd),
        "name.passwd" | "name.reset" if !is_admin_conn(ctx, caller, rid) => acct_fail("not_admin_device"),
        "name.passwd" => passwd_name_account(ctx, caller, &cmd),
        "name.reset" => reset_name_account(ctx, caller, &cmd),
        // ── 多设备 ──
        "device.hello" => device_hello(ctx, rid, &cmd),
        "device.list" => device_list(ctx, caller),
        "device.rename" => device_rename(ctx, caller, &cmd),
        "device.revoke" => match cmd.params.as_ref().and_then(|p| DeviceRevoke::decode(p.value.as_slice()).ok()) {
            Some(r) if !r.by_node.is_empty() => acct_fail("by_node_not_allowed"),
            Some(r) => match apply_revoke(ctx, r, true) {
                Ok(_) => acct_text("ok"),
                Err(e) => acct_fail(e),
            },
            None => acct_fail("invalid_revoke"),
        },
        "device.freeze" => device_freeze(ctx, &cmd),
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
                    e.subs.insert(rid.to_vec());
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
                    if let Some(e) = ctx.channels.get(&op.channel_id) { e.subs.insert(rid.to_vec()); }
                    (true, None, String::new())
                } else {
                    (false, None, "channel join failed".into())
                }
            }
            _ => (false, None, "invalid channel op".into()),
        },
        "channel.unsub" => match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
            Some(op) => {
                if let Some(e) = ctx.channels.get(&op.channel_id) { e.subs.remove(rid); }
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
        seq: 0,
        done: true,
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
    // F2「带密钥群」：若携带群私钥，须其公钥 == group_id；存本地供 home 节点 pkarr 发布，绝不回播。
    if !op.secret.is_empty() {
        let Ok(sk_bytes) = <[u8; 32]>::try_from(op.secret.as_slice()) else {
            return (false, None, "group secret must be 32 bytes".into());
        };
        let sk = nm_transport::SecretKey::from_bytes(&sk_bytes);
        if sk.public().as_bytes().as_slice() != op.group_id.as_slice() {
            return (false, None, "group secret does not match group_id (pubkey)".into());
        }
        if let Some(s) = store {
            if let Err(e) = s.put_group_secret(&op.group_id, &sk_bytes) {
                return (false, None, format!("persist group secret failed: {e}"));
            }
        }
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
        supports_per_topic: false, // 群状态不含节点能力；announce 时按本节点 per_topic 置位
    };
    persist_group(store, &g);
    groups.insert(g.group_id.clone(), g);
    tracing::info!(keyed = !op.secret.is_empty(), "group created");
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
    let cr = CommandResult { correlation_id: corr, ok: false, result: None, error: format!("denied: {reason}"), seq: 0, done: true };
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

    #[test]
    fn seen_grams_dedup() {
        let mut s = SeenGrams::default();
        assert!(s.first_seen(&[1u8; 32], 7), "首次见应投递");
        assert!(!s.first_seen(&[1u8; 32], 7), "重复 (sender,gram_id) 应丢弃");
        assert!(s.first_seen(&[1u8; 32], 8), "不同 gram_id 应投递");
        assert!(s.first_seen(&[2u8; 32], 7), "不同 sender、同 gram_id 应投递");
        assert!(!s.first_seen(&[2u8; 32], 7), "再次重复应丢弃");
    }

    #[test]
    fn group_fully_per_topic_predicate() {
        let me = vec![0u8; 32];
        let node_b = vec![1u8; 32];
        let node_c = vec![2u8; 32];
        let alice = vec![10u8; 32]; // home=本节点（本地成员）
        let bob = vec![11u8; 32]; // home=B
        let carol = vec![12u8; 32]; // home=C
        let ents: DashMap<Vec<u8>, Entity> = DashMap::new();
        let mk = |home: &Vec<u8>| Entity { home_node: home.clone(), ..Default::default() };
        ents.insert(alice.clone(), mk(&me));
        ents.insert(bob.clone(), mk(&node_b));
        ents.insert(carol.clone(), mk(&node_c));
        let advertised: DashSet<Vec<u8>> = DashSet::new();
        let g = Group {
            members: vec![alice.clone(), bob.clone(), carol.clone()],
            ..Default::default()
        };
        // 无人广告 → false（保守）
        assert!(!group_fully_per_topic(&me, &ents, &advertised, &g));
        // 仅 B 广告 → 仍 false（C 未广告）
        advertised.insert(node_b.clone());
        assert!(!group_fully_per_topic(&me, &ents, &advertised, &g));
        // B + C 都广告 → true（本地成员 alice 无需联邦）
        advertised.insert(node_c.clone());
        assert!(group_fully_per_topic(&me, &ents, &advertised, &g));
        // 含 home 未知的成员 → false（保守）
        let dave = vec![13u8; 32];
        let g2 = Group {
            members: vec![bob.clone(), dave.clone()],
            ..Default::default()
        };
        assert!(!group_fully_per_topic(&me, &ents, &advertised, &g2));
    }

    #[test]
    fn group_topic_retireable_predicate() {
        let me = vec![0u8; 32];
        let node_b = vec![1u8; 32];
        let node_c = vec![2u8; 32];
        let alice = vec![10u8; 32]; // home=本节点
        let bob = vec![11u8; 32]; // home=B
        let carol = vec![12u8; 32]; // home=C
        let ents: DashMap<Vec<u8>, Entity> = DashMap::new();
        let mk = |h: &Vec<u8>| Entity { home_node: h.clone(), ..Default::default() };
        ents.insert(alice.clone(), mk(&me));
        ents.insert(bob.clone(), mk(&node_b));
        ents.insert(carol.clone(), mk(&node_c));
        let gid = vec![80u8; 32];
        let g = Group {
            group_id: gid.clone(),
            members: vec![alice.clone(), bob.clone(), carol.clone()],
            ..Default::default()
        };
        let now = 1_000_000u64;
        let ttl = 12_000u64;
        let live: GroupLive = GroupLive::new();
        // 无存活 → false
        assert!(!group_topic_retireable(&me, &ents, &live, &g, now, ttl));
        // 仅 B 新鲜 → 仍 false（C 缺失）
        live.entry(gid.clone()).or_default().insert(node_b.clone(), now - 1_000);
        assert!(!group_topic_retireable(&me, &ents, &live, &g, now, ttl));
        // B + C 新鲜 → true（本地成员 alice 跳过）
        live.get(&gid).unwrap().insert(node_c.clone(), now - 2_000);
        assert!(group_topic_retireable(&me, &ents, &live, &g, now, ttl));
        // C 过期（> ttl）→ false（自愈恢复火管）
        live.get(&gid).unwrap().insert(node_c.clone(), now - 20_000);
        assert!(!group_topic_retireable(&me, &ents, &live, &g, now, ttl));
        // 全本地群（无远端成员）→ true（无人需联邦）
        let g_local = Group {
            group_id: vec![81u8; 32],
            members: vec![alice.clone()],
            ..Default::default()
        };
        assert!(group_topic_retireable(&me, &ents, &live, &g_local, now, ttl));
        // 含 home 未知的成员 → false（保守）
        let dave = vec![13u8; 32];
        let g2 = Group {
            group_id: gid.clone(),
            members: vec![bob.clone(), dave.clone()],
            ..Default::default()
        };
        live.get(&gid).unwrap().insert(node_b.clone(), now);
        assert!(!group_topic_retireable(&me, &ents, &live, &g2, now, ttl));
    }

    #[test]
    fn inbox_retireable_predicate() {
        let me = vec![0u8; 32];
        let node_b = vec![1u8; 32];
        let bob = vec![11u8; 32]; // home=B（远端收件人）
        let alice = vec![10u8; 32]; // home=本节点（本地收件人）
        let ents: DashMap<Vec<u8>, Entity> = DashMap::new();
        let mk = |h: &Vec<u8>| Entity { home_node: h.clone(), ..Default::default() };
        ents.insert(bob.clone(), mk(&node_b));
        ents.insert(alice.clone(), mk(&me));
        let now = 1_000_000u64;
        let ttl = 12_000u64;
        let live: GroupLive = GroupLive::new();
        // bob 的 home(B) 无存活 → false（仍发火管）
        assert!(!inbox_retireable(&bob, &me, &ents, &live, now, ttl));
        // B 新鲜存活于 bob 收件箱 → true（可省略火管 Direct）
        live.entry(bob.clone()).or_default().insert(node_b.clone(), now - 1_000);
        assert!(inbox_retireable(&bob, &me, &ents, &live, now, ttl));
        // 过期 → false（自愈恢复火管）
        live.get(&bob).unwrap().insert(node_b.clone(), now - 20_000);
        assert!(!inbox_retireable(&bob, &me, &ents, &live, now, ttl));
        // 本地收件人（home==本节点）→ false（不走联邦路径）
        assert!(!inbox_retireable(&alice, &me, &ents, &live, now, ttl));
        // home 未知的收件人 → false（保守）
        let carol = vec![12u8; 32];
        assert!(!inbox_retireable(&carol, &me, &ents, &live, now, ttl));
    }

    #[test]
    fn pkarr_group_record_roundtrip() {
        // 群私钥 → 群公钥(=group_id)；签 seed 集合 → relay payload → 解析回同一集合。
        let sk = nm_transport::SecretKey::from_bytes(&[42u8; 32]);
        let gid = *sk.public().as_bytes();
        let seeds = [[1u8; 32], [2u8; 32], [3u8; 32]];
        let payload = pkarr_rec::build(&sk, &seeds).expect("build");
        let got = pkarr_rec::parse(&gid, &payload).expect("parse");
        assert_eq!(got, seeds.to_vec(), "seed 集合应原样往返");
        // 错误公钥（非签名者）→ 校验失败。
        let other = *nm_transport::SecretKey::from_bytes(&[7u8; 32]).public().as_bytes();
        assert!(pkarr_rec::parse(&other, &payload).is_err(), "非签名者公钥应校验失败");
        // 篡改载荷 → 校验失败。
        let mut bad = payload.clone();
        *bad.last_mut().unwrap() ^= 0xff;
        assert!(pkarr_rec::parse(&gid, &bad).is_err(), "篡改载荷应校验失败");
    }

    #[test]
    fn member_index_record_roundtrip() {
        // F6/M1：锚点私钥 → 锚点公钥(=relay key)；签 bootstrap 成员集合 → relay payload → 解析回同一集合。
        let sk = nm_transport::SecretKey::from_bytes(&[88u8; 32]);
        let anchor = *sk.public().as_bytes();
        let members = [[10u8; 32], [11u8; 32], [12u8; 32]];
        let payload = member_rec::build(&sk, &members).expect("build");
        let got = member_rec::parse(&anchor, &payload).expect("parse");
        assert_eq!(got, members.to_vec(), "成员集合应原样往返");
        // 非锚点公钥 → 校验失败（防投毒）。
        let other = *nm_transport::SecretKey::from_bytes(&[9u8; 32]).public().as_bytes();
        assert!(member_rec::parse(&other, &payload).is_err(), "非签名者公钥应校验失败");
        // 篡改载荷 → 校验失败。
        let mut bad = payload.clone();
        *bad.last_mut().unwrap() ^= 0xff;
        assert!(member_rec::parse(&anchor, &bad).is_err(), "篡改载荷应校验失败");
        // 与群发现记录 TXT 名隔离：同一锚点公钥下 _nmspace 记录不会被当成员记录解析出来。
        let as_group = pkarr_rec::parse(&anchor, &payload).unwrap_or_default();
        assert!(as_group.is_empty(), "成员记录(_nmmember)不应被群发现(_nmspace)解析出内容");
    }
}

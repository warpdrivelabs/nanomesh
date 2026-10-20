//! `nm-node` — 服务端节点：接受连接、维护在线会话表与实体目录、按 gram 类型分发与路由。
//! 见 `docs/PLAN_B` §8.1 与 `docs/PLAN_C` §6。
//!
//! - 会话：`remote_id(对端公钥) → Connection`，连接建立即登记、断开即移除。
//! - 目录(Directory)：`directory.register` / `directory.query`（内存）。
//! - 路由：收到 `Message` 时若 `receiver` 为本机在线会话，经 uni 流推送给它；并回执给发送方。

// P1 正在把历史聚合大文件拆成小模块；剩余 collapsible_if 会随后续拆分逐块清掉。
#![allow(clippy::collapsible_if)]

use std::sync::Arc;

use dashmap::{DashMap, DashSet};
use nm_proto::{now_ms, DeviceInfo, DeviceRevoke, Group, NameRecord, PROTOCOL_VERSION};
use nm_store::RedbStore;
use nm_transport::{IrohConnection, NodeEndpoint};
use iroh_gossip::Gossip;

mod dedup;
mod admin_api;
mod blobs;
mod channels;
mod delivery;
mod devices;
mod directory;
mod federation_runtime;
mod grams;
mod groups;
mod membership;
mod naming;
mod naming_runtime;
mod node_runtime;
mod pkarr_runtime;
mod presence;
mod protocol;
mod records;
mod retire;
mod roster;
mod s2s;
use channels::Channels;
use dedup::SeenGrams;
pub use directory::MemDirectory;
use presence::{PresenceMap, StatusIntent};
use protocol::{GossipGate, ImspaceProto};
#[cfg(test)]
use records::{member_rec, pkarr_rec};
use retire::GroupLive;
#[cfg(test)]
use retire::{group_fully_per_topic, group_topic_retireable, inbox_retireable};
pub use admin_api::{SessionRow, StoreStat};
pub use membership::{MembershipCfg, PeerInfo};

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

/// 会话旁挂元数据（不改动 `Sessions` 值类型；用于管理台展示接入时长）。
#[derive(Clone, Copy)]
struct SessionMeta {
    since: std::time::SystemTime,
}
type SessionsMeta = DashMap<Vec<u8>, SessionMeta>;

const LIVE_TTL_MS: u64 = 12_000; // 存活 TTL：sweep 每 ~3s 发一次心跳，容 3 次丢失
type PendingForwardItem = (Vec<u8>, Vec<u8>, u64);
type PendingForwardQueue = std::collections::VecDeque<PendingForwardItem>;
const PENDING_FWD_TTL_SECS: u64 = 60; // Defect C：冷启动待发转发项最长保留，超时丢弃
const PENDING_FWD_CAP: usize = 4096;  // Defect C：待发队列上限，防无界堆积

// 跨节点可靠投递（E2E ack + 持久重传）：重传扫描复用 spawn_pending_flush 的 2s 周期。
// 指数退避（index=retries，封顶复用末项）：2s,5s,15s,30s,60s,120s,300s,600s
const OUTBOX_BACKOFF_MS: [u64; 8] = [2_000, 5_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000];
const OUTBOX_MAX_HORIZON_MS: u64 = 24 * 3600 * 1000; // 24h 上限，对标 IM「离线最终送达」；超期放弃+告警
const LEGACY_MAX_RETRIES: u32 = 5;   // 对端 home 不支持 DirectAck 时的有界重传次数（防无限打爆老节点）

// 群 ack 聚合：成员节点把一窗口内的多条 GroupAck 合并为一个 GroupAckBatch（仅走火管），
// 降低高并发群的 ack 数量级与写放大。窗口 ≪ 首退避 2s → 不触发误重传。
const GROUP_ACK_BATCH_MS: u64 = 250;     // 批量合并窗口（累加器 flush 周期）
const GROUP_ACK_BATCH_MAX: usize = 128;  // 单个批量 gram 最多条目数（超则分块，界定 gram 大小）

// P0-1 存储 GC 保留期（周期裁剪无界表，防撑爆磁盘）：
const CHANNEL_INTAKE_RETENTION_MS: u64 = 48 * 3600 * 1000;   // >发布者 outbox 24h horizon：避免裁掉仍会被重传命中的 nonce→seq
const BLOB_RETENTION_MS: u64 = 180 * 24 * 3600 * 1000;       // 内容寻址媒体缓存（误删可按 hash 重新上传/拉取）
const DEAD_LETTER_RETENTION_MS: u64 = 7 * 24 * 3600 * 1000;  // 死信留存期（诊断/人工补投）
const GC_INTERVAL_SECS: u64 = 6 * 3600;                      // 周期 GC 节拍（与原 inbox 裁剪同频）
const INBOX_MAX_PER_ENTITY: usize = 50_000;                  // 单设备离线信箱条数上限（配额）：超则丢新离线副本，防被灌爆

// P0-4 消息级限流（应用层反滥用，补连接级 conn_gate 之不足）：单发送方滑动计数窗口，超出即丢弃入站消息。
const MSG_RATE_WINDOW_SECS: u64 = 10;
const MSG_RATE_MAX_IN_WINDOW: u32 = 300; // 10s 内单发送方至多 300 条（~30 msg/s 突发），远超正常客户端，仅掐洪泛
const MSG_GATE_CAP: usize = 65_536;      // msg_gate 表容量上限（满则整表清空，防无界）
// 发送方公钥 → (当前窗口起点 Unix 秒, 窗口内计数)。
type MsgGate = DashMap<Vec<u8>, (u64, u32)>;

// 连接限流（P0 DoS 防护）：
const DEFAULT_MAX_CONNECTIONS: usize = 10_000; // 全局并发连接上限（超则拒绝新连接，防 FD/内存耗尽）
// 单公钥重连速率门：滑动计数窗口（而非「最小间隔」——那会误杀 app 重启/切后台/断线等 1s 内的正常重连）。
// 同一公钥在 CONN_RATE_WINDOW_SECS 窗口内允许至多 CONN_RATE_MAX_IN_WINDOW 次新建连接；
// 超出才拒（拦真正的持续重连风暴/churn）。正常重连是小突发（几次），风暴是每秒持续多次——此门只掐后者。
const CONN_RATE_WINDOW_SECS: u64 = 10; // 速率窗口长度
const CONN_RATE_MAX_IN_WINDOW: u32 = 10; // 窗口内同一公钥允许的新建连接次数（突发容量）
const CONN_GATE_CAP: usize = 65_536;   // conn_gate 表容量上限（满则整表清空，防无界）
// 公钥 → (当前窗口起点 Unix 秒, 窗口内计数)：单来源重连速率的滑动计数窗口。
type ConnGate = DashMap<[u8; 32], (u64, u32)>;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
    // Defect C：冷启动待发转发队列 (to, GroupGossip字节, 入队秒)——inbox overlay 0 邻居时暂存，邻居成型后补发。
    pending_fwd: Arc<std::sync::Mutex<PendingForwardQueue>>,
    // 优雅关闭：在途投递任务追踪器（投递类 spawn 走它，关闭时 drain）。与 Node.tracker 同一实例。
    tracker: tokio_util::task::TaskTracker,
    // 优雅关闭信号（与 Node.shutdown 同一实例）：长驻任务（如频道收播泵）据此协作退出。
    shutdown: tokio_util::sync::CancellationToken,
    // 连接限流（P0）：全局并发连接上限 + 单公钥新建连接速率门。accept 热路径读。
    max_connections: usize,
    conn_gate: Arc<ConnGate>,
    // 跨节点可靠投递：已广告支持 DirectAck 的节点 id（能力协商；重传据此决定是否有界降级）。
    ack_nodes: Arc<DashSet<Vec<u8>>>,
    // 群消息可靠扇出观测（与 Node.group_metrics 同一实例）。
    group_metrics: Arc<GroupMetrics>,
    // 消息级限流（P0-4）：单发送方滑动计数窗口。入站消息热路径读。
    msg_gate: Arc<MsgGate>,
    // P1-9：送达时延直方图 + 连接拒绝计数（与 Node 同一实例，记录点在 ctx 热路径）。
    delivery_hist: Arc<LatencyHist>,
    conn_rejects: Arc<ConnRejects>,
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
    // Defect C：冷启动待发转发队列 (to, GroupGossip字节, 入队秒)——inbox overlay 0 邻居时暂存，邻居成型后补发。
    pending_fwd: Arc<std::sync::Mutex<PendingForwardQueue>>,
    // 换网观测（P2 看板）：本端点地址变化次数 + 最近一次变化时间（Unix ms）。供 admin /system 展示。
    roam: Arc<RoamStat>,
    // 优雅关闭（P0）：收到 SIGTERM/SIGINT 时 cancel，后台 loop 协作退出、serve 收尾关 router。
    shutdown: tokio_util::sync::CancellationToken,
    // 在途投递任务追踪：关闭时有界等待其清零（drain），避免未发完的单播消息丢失。
    tracker: tokio_util::task::TaskTracker,
    // drain 窗口上限：等在途投递清零的最长时间（nmd.toml 可配，默认 5s）。
    drain_timeout: std::time::Duration,
    // 连接限流（P0 DoS 防护）：全局并发连接上限 + 单公钥新建连接速率门。
    max_connections: usize,
    conn_gate: Arc<ConnGate>,
    // 跨节点可靠投递：已广告支持 DirectAck 的节点 id（能力协商）。
    ack_nodes: Arc<DashSet<Vec<u8>>>,
    // 群 ack 聚合：已广告支持 GroupAckBatch 的发送方 home 节点 id（能力协商，镜像 ack_nodes/per_topic_nodes）。
    ack_batch_nodes: Arc<DashSet<Vec<u8>>>,
    // 群 ack 聚合累加器：成员节点收到跨节点群 Msg 后攒下的待回执条目，由 spawn_group_ack_batcher 周期 flush。
    ack_accum: Arc<std::sync::Mutex<Vec<AckItem>>>,
    // 群消息可靠扇出观测（供 admin 看板）。
    group_metrics: Arc<GroupMetrics>,
    // 消息级限流（P0-4）：单发送方滑动计数窗口。
    msg_gate: Arc<MsgGate>,
    // P1-9：送达时延直方图（send→ack）+ 连接拒绝计数。供 /metrics 导出。
    delivery_hist: Arc<LatencyHist>,
    conn_rejects: Arc<ConnRejects>,
}

/// 换网观测计数（供 admin 看板）：记录本节点端点地址变化次数与最近一次时间。
#[derive(Default)]
pub struct RoamStat {
    /// 本端点地址（IP/relay）变化累计次数（换网/漫游的近似计数）。
    pub changes: std::sync::atomic::AtomicU64,
    /// 最近一次地址变化的 Unix 毫秒（0 = 启动以来未变化）。
    pub last_change_ms: std::sync::atomic::AtomicU64,
}

/// 群消息可靠扇出观测（供 admin 看板判定到达率）：扇出跟踪数 / 收到 ack 数 / 重传次数 / 超期放弃数。
/// 稳态下 acks 应紧跟 fanouts、retransmits 近 0；retransmits 持续攀升或 giveups>0 = 某成员节点长期不可达。
#[derive(Default)]
pub struct GroupMetrics {
    /// 发起并持久跟踪（有远端成员节点）的群消息条数。
    pub fanouts: std::sync::atomic::AtomicU64,
    /// 收到的有效 GroupAck 数（命中本节点待确认条目）。
    pub acks: std::sync::atomic::AtomicU64,
    /// 群消息重传次数（到期未全 ack 时重发到群主题）。
    pub retransmits: std::sync::atomic::AtomicU64,
    /// 达 24h 期限仍未全 ack 而放弃的群消息条数。
    pub giveups: std::sync::atomic::AtomicU64,
    /// 本节点作为成员发出的 GroupAckBatch gram 块数（聚合效果：块数 ≪ 被确认消息数）。
    pub ack_batches_sent: std::sync::atomic::AtomicU64,
    /// P0-3：达期限放弃并落死信的消息条数（DM + 群）。持续 >0 = 有成员/对端 home 长期不可达。
    pub dead_letters: std::sync::atomic::AtomicU64,
    /// P0-4：被消息级限流丢弃的入站消息条数。持续攀升 = 有发送方在洪泛。
    pub msgs_rate_limited: std::sync::atomic::AtomicU64,
}

/// P1-9 送达时延直方图（send→ack，毫秒）：固定 le 桶 + sum + count，供 /metrics 导出 OpenMetrics histogram。
/// DM 记 DirectAck 到达时延；群记末个 GroupAck 清条目的扇出 tail 时延。
pub struct LatencyHist {
    /// 各桶累计计数（≤ 对应 LAT_BUCKETS_MS 上界的样本数，**非**累积——快照时再前缀和）。
    buckets: [std::sync::atomic::AtomicU64; LAT_BUCKETS_MS.len()],
    /// 溢出桶（> 最大上界，即 +Inf 专属增量）。
    overflow: std::sync::atomic::AtomicU64,
    /// 样本总数。
    count: std::sync::atomic::AtomicU64,
    /// 样本毫秒之和（算均值用）。
    sum_ms: std::sync::atomic::AtomicU64,
}

/// 时延桶上界（毫秒），OpenMetrics `le`。覆盖亚秒直连到多秒重传。
pub(crate) const LAT_BUCKETS_MS: [u64; 11] =
    [50, 100, 250, 500, 1_000, 2_000, 5_000, 10_000, 30_000, 60_000, 300_000];

impl Default for LatencyHist {
    fn default() -> Self {
        Self {
            buckets: std::array::from_fn(|_| std::sync::atomic::AtomicU64::new(0)),
            overflow: std::sync::atomic::AtomicU64::new(0),
            count: std::sync::atomic::AtomicU64::new(0),
            sum_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl LatencyHist {
    /// 记一个时延样本（毫秒）。落入第一个 `ms ≤ 上界` 的桶，超最大上界计 overflow。
    pub fn record(&self, ms: u64) {
        use std::sync::atomic::Ordering::Relaxed;
        let mut placed = false;
        for (i, ub) in LAT_BUCKETS_MS.iter().enumerate() {
            if ms <= *ub { self.buckets[i].fetch_add(1, Relaxed); placed = true; break; }
        }
        if !placed { self.overflow.fetch_add(1, Relaxed); }
        self.count.fetch_add(1, Relaxed);
        self.sum_ms.fetch_add(ms, Relaxed);
    }
    /// 快照：返回(累积桶[(le,累计计数)], count, sum_ms)。桶计数已前缀和为 OpenMetrics 累积语义。
    pub fn snapshot(&self) -> (Vec<(u64, u64)>, u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        let mut acc = 0u64;
        let mut out = Vec::with_capacity(LAT_BUCKETS_MS.len());
        for (i, ub) in LAT_BUCKETS_MS.iter().enumerate() {
            acc += self.buckets[i].load(Relaxed);
            out.push((*ub, acc));
        }
        let count = self.count.load(Relaxed);
        (out, count, self.sum_ms.load(Relaxed))
    }
}

/// P1-9 连接拒绝计数（accept 门）：banned/revoked/server_busy/rate_limited 各一个计数，供错误率观测。
#[derive(Default)]
pub struct ConnRejects {
    pub banned: std::sync::atomic::AtomicU64,
    pub revoked: std::sync::atomic::AtomicU64,
    pub server_busy: std::sync::atomic::AtomicU64,
    pub rate_limited: std::sync::atomic::AtomicU64,
}

/// 群 ack 聚合累加器项：成员节点收到一条跨节点群 Msg 后攒下的待回执条目。
/// `sender_home` = 该 Msg 的 `GroupGossip.origin`（= 建 group_outbox 的发送方 home，protocol.rs 构造），
/// 用于按能力分流（该 home 是否支持批量）；必须在收到 Msg 时捕获（flush 时 Msg 已不在）。
#[derive(Clone)]
pub(crate) struct AckItem {
    pub gid: Vec<u8>,
    pub sender: Vec<u8>,
    pub gram_id: u64,
    pub sender_home: Vec<u8>,
}

impl RoamStat {
    /// 记一次地址变化（换网哨兵触发时调用）。
    pub fn record(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.changes.fetch_add(1, Relaxed);
        self.last_change_ms.store(now_ms(), Relaxed);
    }
    /// 读快照：(变化次数, 最近变化 Unix ms)。
    pub fn snapshot(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (self.changes.load(Relaxed), self.last_change_ms.load(Relaxed))
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
        assert!(s.first_seen(&[1u8; 32], 7, b"crcA"), "首次见应投递");
        assert!(!s.first_seen(&[1u8; 32], 7, b"crcA"), "重复 (sender,gram_id,crc) 应丢弃");
        assert!(s.first_seen(&[1u8; 32], 8, b"crcA"), "不同 gram_id 应投递");
        assert!(s.first_seen(&[2u8; 32], 7, b"crcA"), "不同 sender、同 gram_id 应投递");
        assert!(!s.first_seen(&[2u8; 32], 7, b"crcA"), "再次重复应丢弃");
        assert!(s.first_seen(&[1u8; 32], 7, b"crcB"), "同 (sender,gram_id) 但不同 crc 应投递(防 client 重启 gram_id 回绕误判)");
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

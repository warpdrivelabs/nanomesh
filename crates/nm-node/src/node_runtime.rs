use std::sync::Arc;

use dashmap::{DashMap, DashSet};
use iroh::protocol::Router;
use iroh_gossip::Gossip;
use nm_proto::{now_ms, Group, NameRecord};
use nm_store::RedbStore;
use nm_transport::NodeEndpoint;

use crate::channels::Channels;
use crate::dedup::SeenGrams;
use crate::presence::{PresenceMap, StatusIntent};
use crate::retire::GroupLive;
use crate::{
    Ctx, Devices, GossipGate, ImspaceProto, MemDirectory, Node, NodeError, PeerInfo, Revoked, Sessions,
    SessionsMeta, INBOX_RETENTION_MS,
};

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
            pending_fwd: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
        })
    }

    pub(crate) fn ctx(&self) -> Ctx {
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
            pending_fwd: self.pending_fwd.clone(),
        }
    }

    pub fn id(&self) -> nm_transport::Id {
        self.ep.id()
    }

    pub fn addr(&self) -> nm_transport::Addr {
        self.ep.addr()
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
}

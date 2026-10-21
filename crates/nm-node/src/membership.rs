use std::sync::Arc;

use crate::records::{hex_decode_n, hex_encode, member_rec};
use crate::{now_secs, Node};

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
    /// P2-11 B：本节点是否支持 scoped presence（按 home 拉取）。编译期能力位，不入签名（非安全关键：
    /// 谎报最坏后果=对端多留一会儿火管或照常拉取，均安全）。
    #[serde(default)]
    supports_presence_scoped: bool,
    sig: String,
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

impl Node {
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
            supports_presence_scoped: true, // P2-11 B：编译期能力——本节点支持按 home 拉取
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
        // P2-11 B：记录 peer 的 scoped 能力（镜像 per_topic_nodes 的 insert/else-remove 自愈）。
        // 全员支持 → presence_retired() 自动退 overlay；出现不支持的老 peer → 回退维持火管。
        if ann.supports_presence_scoped {
            self.presence_scoped_nodes.insert(node_id.to_vec());
        } else {
            self.presence_scoped_nodes.remove(&node_id.to_vec());
        }
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
    pub(crate) fn persist_peer(&self, node_id: [u8; 32], info: PeerInfo) {
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
            // 换网/休眠唤醒自愈：0 邻居时 nudge iroh 重探测的冷却计时（避免每秒狂触发 re-probe）。
            let mut last_net_nudge: Option<tokio::time::Instant> = None;
            // 换网 epoch：换网后 gossip 订阅可能失效但 neighbor_count 非 0(和种子还连着)→ ==0 自愈不触发。
            // epoch 变化 = 确实换过网 → 无条件硬重 join(换新 topic),彻底重建 mesh、重新广播+收播发现。
            let mut seen_epoch = self.net_epoch();
            loop {
                let ep = self.net_epoch();
                if ep != seen_epoch {
                    seen_epoch = ep;
                    tracing::info!("membership 换网(epoch 变) → 强制重 join gossip 以恢复动态发现");
                    self.notify_network_change().await;
                    if let Ok(t) = self.join_channel(channel, self.peers_list()).await {
                        topic = t;
                        let _ = topic.publish(self.membership_card(&cfg.card)).await; // 立即广播本节点,加速被重新发现
                    }
                }
                if last_announce.elapsed() >= announce_iv {
                    if let Err(e) = topic.publish(self.membership_card(&cfg.card)).await {
                        tracing::debug!("membership publish failed: {e}");
                    }
                    last_announce = tokio::time::Instant::now();
                }
                // 带 1s 超时的收播：既能及时收，又能周期回到顶部广播/清扫。
                tokio::select! {
                    biased;
                    _ = self.shutdown.cancelled() => break,
                    r = tokio::time::timeout(std::time::Duration::from_secs(1), topic.recv()) => match r {
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
                        Err(_) => {
                            // 超时(1s 无消息)：若悄悄掉到 0 邻居(休眠唤醒/换网后连接全死但订阅流未关闭,
                            // 不会走 Ok(None) 重 join)→ 用【最新】peers_list 重连,否则动态发现永久失联,
                            // 只剩手动种子,其他节点再也学不到。镜像 presence 的自愈。
                            if topic.neighbor_count() == 0 {
                                // 同 IP 唤醒不触发换网回调 → iroh 可能还抱着死的 relay 连接。
                                // 带冷却地 nudge 它重探测/重 home relay,让后续 rejoin 真能连上。
                                let now = tokio::time::Instant::now();
                                if last_net_nudge.map_or(true, |t| now.duration_since(t).as_secs() >= 30) {
                                    self.notify_network_change().await;
                                    last_net_nudge = Some(now);
                                }
                                let _ = topic.rejoin(self.peers_list()).await;
                            }
                        }
                    }
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
    ///   需 relay（`[dns] url`）已设；否则本任务空转。周期 60s（成员集合变动不频繁，与 F2 群 pkarr 同档）。
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
                tokio::select! {
                    biased;
                    _ = self.shutdown.cancelled() => break,
                    _ = tokio::time::sleep(iv) => {}
                }
            }
        })
    }
}

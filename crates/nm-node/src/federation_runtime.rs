use std::sync::Arc;

use nm_proto::{now_ms, DeviceRevoke, Group, GroupGossip, NameRecord};
use prost::Message;

use crate::delivery::{
    deliver_account, group_bootstrap, inbox_bootstrap, is_home_here, local_accounts,
    names_bootstrap,
};
use crate::devices::apply_revoke;
use crate::naming_runtime::apply_domain_decision;
use crate::retire::{group_fully_per_topic, group_topic_retireable, inbox_retireable};
use crate::s2s::fed_pull;
use crate::{now_secs, Ctx, Node, LIVE_TTL_MS, PENDING_FWD_CAP, PENDING_FWD_TTL_SECS};

impl Node {
    /// 后台周期性向所有对等节点同步目录，返回任务句柄。
    pub fn spawn_federation_sync(
        self: Arc<Self>,
        interval: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                self.sync_peers_once().await;
                tokio::select! {
                    biased;
                    _ = self.shutdown.cancelled() => break,
                    _ = tokio::time::sleep(interval) => {}
                }
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
    /// F3 诊断/测试：当前已加入的联邦主题标签（群 gid 与收件人 account 混列）。
    pub async fn fed_joined(&self) -> Vec<Vec<u8>> {
        self.fed.joined().await
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
            // 先在独立作用域取出 receiver 并立即释放 MutexGuard，再 match——避免 std Mutex guard
            // 跨越后续逻辑/await（当前安全，此写法防未来在 match 臂内引入 await 时意外跨-await 持锁）。
            let taken = self.group_pub_rx.lock().unwrap().take();
            let mut pub_rx = match taken {
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
                // 先取出 receiver 并释放 guard，再进 if 块——块内有 tokio::spawn，guard 不应跨越它。
                let taken_fed = self.fed_rx.lock().unwrap().take();
                if let Some(mut fed_rx) = taken_fed {
                    let me = self.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                biased;
                                _ = me.shutdown.cancelled() => break,
                                fm = fed_rx.recv() => {
                                    let Some(fm) = fm else { break };
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
                            }
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
                tokio::select! {
                    biased;
                    _ = self.shutdown.cancelled() => break,
                    r = tokio::time::timeout(std::time::Duration::from_millis(300), topic.recv()) => match r {
                        Ok(Some(msg)) => self.on_group_gossip(&msg.content).await,
                        Ok(None) => match self.join_channel(channel, self.peers_list()).await {
                            Ok(t) => topic = t,
                            Err(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
                        },
                        Err(_) => {}
                    }
                }
            }
        })
    }

    /// Defect C: cold-start pending-forward flush. Every ~2s: re-publish queued DMs whose
    /// recipient inbox overlay now HAS neighbors; drop items past TTL. Recipient dedups on
    /// (sender,gram_id), so a re-publish that overlaps the firehose copy is harmless.
    pub fn spawn_pending_flush(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let ctx = self.ctx();
            loop {
                tokio::select! {
                    biased;
                    _ = self.shutdown.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                }
                let items: Vec<(Vec<u8>, Vec<u8>, u64)> = {
                    let mut q = ctx.pending_fwd.lock().unwrap();
                    if q.is_empty() { continue; }
                    q.drain(..).collect()
                };
                let now = now_secs();
                let mut keep: std::collections::VecDeque<(Vec<u8>, Vec<u8>, u64)> = std::collections::VecDeque::new();
                for (to, bytes, ts) in items {
                    if now.saturating_sub(ts) > PENDING_FWD_TTL_SECS {
                        tracing::warn!("pending-forward dropped (TTL expired)");
                        continue;
                    }
                    // inbox overlay formed? re-publish there (private unicast to recipient home).
                    let _ = ctx.fed.join_inbox(&to, inbox_bootstrap(&to, &ctx)).await; // idempotent
                    if ctx.fed.inbox_neighbors(&to).await > 0 {
                        let _ = ctx.fed.publish_inbox(&to, bytes.clone()).await;
                        // belt-and-suspenders: also re-fire firehose (recipient dedups).
                        let _ = ctx.group_pub.send(bytes);
                        tracing::debug!("pending-forward flushed");
                    } else {
                        keep.push_back((to, bytes, ts));
                    }
                }
                if !keep.is_empty() {
                    let mut q = ctx.pending_fwd.lock().unwrap();
                    for it in keep.into_iter().rev() { q.push_front(it); }
                    while q.len() > PENDING_FWD_CAP { q.pop_front(); }
                }
            }
        })
    }
}

/// 构造群 Announce 的 gossip 字节：origin=本节点；`supports_per_topic`=本节点能力广告（F1 协商）。
/// 该标志是「节点能力」不是「群状态」——仅在 announce 瞬间按本节点 per_topic 置位，收端据 origin 跟踪。
pub(crate) fn encode_announce(origin: &[u8], per_topic: bool, mut g: Group) -> Vec<u8> {
    g.supports_per_topic = per_topic;
    GroupGossip {
        origin: origin.to_vec(),
        body: Some(nm_proto::pb::group_gossip::Body::Announce(g)),
    }
    .encode_to_vec()
}

/// 立即向联邦广播某群当前状态（发现 + 成员表）。群变更后调用，避免等周期公告。
pub(crate) fn announce_group(ctx: &Ctx, gid: &[u8]) {
    if let Some(g) = ctx.groups.get(gid).map(|g| g.clone()) {
        let per_topic = ctx.per_topic.load(std::sync::atomic::Ordering::Relaxed);
        let _ = ctx.group_pub.send(encode_announce(&ctx.node_id, per_topic, g));
    }
}


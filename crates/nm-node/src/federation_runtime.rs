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
                    self.per_topic_nodes.insert(origin.clone());
                } else {
                    self.per_topic_nodes.remove(&origin);
                }
                // ack 聚合能力协商（镜像 per_topic）：据 origin 跟踪支持 GroupAckBatch 的发送方 home。
                // else 分支的 remove 必需——发送方 home 回滚到老版本后能自愈（成员改回逐条 GroupAck）。
                if g.supports_ack_batch {
                    self.ack_batch_nodes.insert(origin.clone());
                } else {
                    self.ack_batch_nodes.remove(&origin);
                }
                g.supports_per_topic = false; // 节点能力不写进群状态（保持群状态纯净）
                g.supports_ack_batch = false; // 同上：能力位是 announce 瞬态，不写进持久群状态
                self.merge_group_lww(g);
            }
            Some(nm_proto::pb::group_gossip::Body::Msg(gram)) => {
                let Some(group) = self.groups.get(&gram.receiver).map(|g| g.clone()) else {
                    return; // 尚未学到该群（announce 未到）→ 跳过且不 ack；发送方重传会在学到群后补达
                };
                // 本节点是否有该群本地成员（home==本节点）——发送方的期望 ack 节点集=有成员的节点，故据此决定是否回 GroupAck。
                let have_local = group.members.iter().any(|m| is_home_here(&ctx, m));
                // 去重：内存快筛（含 crc）+ 持久权威（含 crc，防 client 重启 gram_id 回绕误判 & 跨重启/LRU 滚出后的重传）。
                let mem_first = self.seen.lock().unwrap().first_seen(&gram.sender, gram.gram_id, &gram.crc);
                let persist_first = match &ctx.store {
                    Some(s) => s
                        .dedup_seen(&gram.sender, gram.gram_id, &gram.crc)
                        .map(|seen| !seen)
                        .unwrap_or(true),
                    None => mem_first,
                };
                // 仅首见才真正投递；**投递后**才持久 mark（防"标记了却没投出"的空洞 → 发送方重传能补）。
                if mem_first && persist_first {
                    for m in &group.members {
                        let home_here = is_home_here(&ctx, m);
                        deliver_account(m, &gram, &ctx, home_here, None).await;
                    }
                    if let Some(s) = &ctx.store {
                        let _ = s.dedup_mark(&gram.sender, gram.gram_id, &gram.crc, now_ms());
                    }
                }
                // ack 聚合（跨节点可靠扇出）：仅当本节点确有该群本地成员才回（= 发送方期望 ack 的节点）。
                // 不再逐条即时回 GroupAck，而是攒进累加器，由 spawn_group_ack_batcher 按窗口去重 + 按能力分流
                // （支持批量的发送方 home → GroupAckBatch 仅火管一次；否则回退逐条 GroupAck 双写）。
                // 首见/重复都攒（去重在 flush 做）；sender_home=origin 须此刻捕获（flush 时 Msg 已不在）。
                if have_local {
                    self.ack_accum.lock().unwrap().push(crate::AckItem {
                        gid: gram.receiver.clone(),
                        sender: gram.sender.clone(),
                        gram_id: gram.gram_id,
                        sender_home: origin.clone(),
                    });
                }
            }
            Some(nm_proto::pb::group_gossip::Body::Direct(gram)) => {
                let to = &gram.receiver;
                if to.as_slice() == self.ep.id_bytes().as_slice() {
                    apply_domain_decision(&gram, &self.domains, &self.store, &self.domain_notices);
                    return;
                }
                // 一级内存快筛：火管 + inbox 主题双写同一条会到两次。
                let mem_first = self.seen.lock().unwrap().first_seen(&gram.sender, gram.gram_id, &gram.crc);
                let home_here = is_home_here(&ctx, to);
                // 二级持久去重（含 crc，防 client 重启 gram_id 回绕误判）查询——先查不标记。
                let persist_first = match &ctx.store {
                    Some(s) => s
                        .dedup_seen(&gram.sender, gram.gram_id, &gram.crc)
                        .map(|seen| !seen)
                        .unwrap_or(true),
                    None => mem_first,
                };
                // 仅首见才真正投递；**投递后**才持久 mark（防"标记了却没投出"的空洞 → 发送方重传能补）。
                if mem_first && persist_first {
                    deliver_account(to, &gram, &ctx, home_here, None).await;
                    if let Some(s) = &ctx.store {
                        let _ = s.dedup_mark(&gram.sender, gram.gram_id, &gram.crc, now_ms());
                    }
                }
                // 回 E2E ack（跨节点可靠投递）：仅当本节点确是收件人 home 才回（非 home 收火管广播不回，
                // 否则发送方收到非权威 ack 误停重传）。首见/重复都回——重复多因 ack 丢失、发送方在重传。
                if home_here {
                    let ack = GroupGossip {
                        origin: self.ep.id_bytes().to_vec(),
                        body: Some(nm_proto::pb::group_gossip::Body::DirectAck(nm_proto::pb::DirectAck {
                            sender: gram.sender.clone(),
                            gram_id: gram.gram_id,
                            receiver: to.clone(),
                            crc: gram.crc.clone(),
                        })),
                    };
                    let _ = self.fed.join_inbox(&gram.sender, inbox_bootstrap(&gram.sender, &ctx)).await;
                    let _ = self.fed.publish_inbox(&gram.sender, ack.encode_to_vec()).await;
                    let _ = self.group_pub.send(ack.encode_to_vec()); // 火管兜底
                }
            }
            Some(nm_proto::pb::group_gossip::Body::DirectAck(ack)) => {
                // 本节点是原 sender 的 home（订阅了 nmspace-inbox:<my account>）→ 删待确认条目、停重传。
                // DirectAck 不再回 ack、不入 outbox —— 无回环。
                if let Some(s) = &ctx.store {
                    // P1-9：删前读 first_ms，Ok(true) 时记 send→ack 时延。
                    let first_ms = s.outbox_first_ms(&ack.sender, &ack.receiver, ack.gram_id).ok().flatten();
                    match s.outbox_del(&ack.sender, &ack.receiver, ack.gram_id) {
                        Ok(true) => {
                            if let Some(fm) = first_ms {
                                ctx.delivery_hist.record(now_ms().saturating_sub(fm));
                            }
                            tracing::debug!("outbox cleared by DirectAck");
                        }
                        Ok(false) => {}
                        Err(e) => tracing::warn!("outbox_del failed: {e}"),
                    }
                }
            }
            Some(nm_proto::pb::group_gossip::Body::GroupAck(ack)) => {
                // 本节点是原群消息 sender 的 home（持有 group_outbox 条目）→ 把回执节点从待 ack 集移除；
                // 全清则停重传。GroupAck 不再回 ack、不入 outbox —— 无回环。别的 home 的 ack 在本地无匹配条目，忽略。
                if let Some(s) = &ctx.store {
                    // P1-9：清条目(末个成员 home ack)时记扇出 send→last-ack tail 时延——清前读 first_ms。
                    let first_ms = s.group_outbox_first_ms(&ack.gid, &ack.sender, ack.gram_id).ok().flatten();
                    match s.group_outbox_ack_node(&ack.gid, &ack.sender, ack.gram_id, &ack.node) {
                        Ok((true, cleared)) => {
                            ctx.group_metrics.acks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if cleared {
                                if let Some(fm) = first_ms {
                                    ctx.delivery_hist.record(now_ms().saturating_sub(fm));
                                }
                                tracing::debug!("group outbox cleared by GroupAck (all member nodes acked)");
                            }
                        }
                        Ok((false, _)) => {} // 非本节点跟踪的条目
                        Err(e) => tracing::warn!("group_outbox_ack_node failed: {e}"),
                    }
                }
            }
            Some(nm_proto::pb::group_gossip::Body::GroupAckBatch(batch)) => {
                // 聚合销账：本节点可能是其中部分条目的发送方 home（持 group_outbox）。一次读预检 +（有命中才）
                // 一个写事务处理整批 —— 非发送方零写事务。别的 home 的条目在本地无匹配，自动跳过。
                // 保留 legacy GroupAck 臂不动：老发送方 home 仍收逐条 ack（向后兼容）。
                if let Some(s) = &ctx.store {
                    let entries: Vec<(Vec<u8>, Vec<u8>, u64)> = batch
                        .acks
                        .iter()
                        .map(|e| (e.gid.clone(), e.sender.clone(), e.gram_id))
                        .collect();
                    match s.group_outbox_ack_batch(&batch.node, &entries) {
                        Ok((acked, cleared)) => {
                            if acked > 0 {
                                ctx.group_metrics
                                    .acks
                                    .fetch_add(acked as u64, std::sync::atomic::Ordering::Relaxed);
                            }
                            if cleared > 0 {
                                tracing::debug!(cleared, "group outbox entries cleared by GroupAckBatch");
                            }
                        }
                        Err(e) => tracing::warn!("group_outbox_ack_batch failed: {e}"),
                    }
                }
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
            Some(nm_proto::pb::group_gossip::Body::InboxLive(il)) => {
                // F3 退火管：收件人 home 存活心跳。存活已由每群主题泵记入
                // per_group_live[account][origin]；此处无需额外动作（发送方据此省略火管 Direct）。
                // 跨节点可靠投递能力协商：记录 origin(=该收件箱 home) 是否支持 DirectAck，
                // 供 outbox 重传判定（不支持 → 有界降级，防无限打爆老节点）。
                if il.supports_direct_ack {
                    self.ack_nodes.insert(origin.clone());
                } else {
                    self.ack_nodes.remove(&origin);
                }
            }
            Some(nm_proto::pb::group_gossip::Body::Channel(cg)) => {
                // P4 频道火管兜底：全节点无条件收到频道 Meta/定序消息/发布意图/回填，
                // 令跨节点订阅者的 home 无需频道专属 overlay 先成型即可处理（根治「订阅后收不到名称/图标/消息」）。
                crate::channels::handle_channel_firehose(&ctx, &cg).await;
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
                match self.join_channel(channel, self.gossip_bootstrap()).await {
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
            // 换网 epoch：变化即硬重 join 火管主题(neighbor_count 可能非0但订阅已失效),恢复群/频道/presence兜底扇出。
            let mut seen_epoch = self.net_epoch();
            // 0-邻居自愈重注入的退避(2s→30s,收到消息即重置)：防「0 邻居→无退避重灌全部 peers」正反馈冲垮 mesh。
            let mut rejoin_backoff = std::time::Duration::from_secs(2);
            let mut last_rejoin: Option<tokio::time::Instant> = None;
            loop {
                let ep = self.net_epoch();
                if ep != seen_epoch {
                    seen_epoch = ep;
                    tracing::info!("group firehose 换网(epoch 变) → 强制重 join gossip");
                    self.notify_network_change().await;
                    if let Ok(t) = self.join_channel(channel, self.gossip_bootstrap()).await {
                        topic = t;
                    }
                }
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
                                nm_proto::pb::InboxLive { account: acct.clone(), supports_direct_ack: true },
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
                        Ok(Some(msg)) => {
                            rejoin_backoff = std::time::Duration::from_secs(2);
                            last_rejoin = None;
                            self.on_group_gossip(&msg.content).await
                        }
                        Ok(None) => match self.join_channel(channel, self.gossip_bootstrap()).await {
                            Ok(t) => topic = t,
                            Err(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
                        },
                        // 超时:火管主题悄悄掉到 0 邻居 → 退避重注入【收敛】bootstrap 自愈(直连/换网 churn 下必需)。
                        // 退避(2s→30s)斩断「0 邻居→每 300ms 重灌全部 peers」正反馈;邻居恢复即重置。
                        Err(_) => {
                            if topic.neighbor_count() == 0 {
                                let now = tokio::time::Instant::now();
                                if last_rejoin.map_or(true, |t| now.duration_since(t) >= rejoin_backoff) {
                                    let _ = topic.rejoin(self.gossip_bootstrap()).await;
                                    last_rejoin = Some(now);
                                    rejoin_backoff = (rejoin_backoff * 2).min(std::time::Duration::from_secs(30));
                                }
                            } else {
                                rejoin_backoff = std::time::Duration::from_secs(2);
                                last_rejoin = None;
                            }
                        }
                    }
                }
            }
        })
    }

    /// P4 启动恢复：从持久化重建本节点订阅的频道（重 join 主题 + 回填 subs）。
    /// 一次性任务（非循环），在 serve 后调用一次即可。
    pub fn spawn_channel_restore(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let ctx = self.ctx();
            crate::channels::restore_channels(&ctx).await;
        })
    }

    /// P4 频道发现（需 [dns] url）：周期性 home 发布频道 pkarr 记录、订阅者解析 seed 入 `pkarr_seeds`
    /// 供 ensure_channel 冷启动 bootstrap。无 pkarr 端点则本任务空转退出。
    pub fn spawn_channel_pkarr(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = self.shutdown.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
                }
                if self.pkarr_endpoint().is_none() {
                    continue; // 未配置 pkarr → 仅靠 peers + 邀请链接发现
                }
                // 本节点当前持有的频道（内存运行态）。
                let cids: Vec<Vec<u8>> = self.channels.iter().map(|e| e.key().clone()).collect();
                for cid in cids {
                    if crate::channels::node_is_channel_home(&self.store, &cid) {
                        self.pkarr_publish_channel(&cid).await; // home：发布自己为 seed
                    } else {
                        let seeds = self.pkarr_resolve_channel(&cid).await; // 订阅者：解析 home seed
                        if !seeds.is_empty() {
                            self.pkarr_seeds.insert(cid.clone(), seeds);
                        }
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

                // ── 跨节点可靠投递：持久发件箱重传（E2E ack 驱动）──
                if let Some(store) = &ctx.store {
                    let now_m = now_ms();
                    let rows = store.outbox_all().unwrap_or_default();
                    for row in rows {
                        // 达 24h 期限 → 放弃：落死信(诊断/补投) + 通知本地发送方(不再静默丢) + 告警。
                        if now_m.saturating_sub(row.first_ms) > crate::OUTBOX_MAX_HORIZON_MS {
                            tracing::warn!(gram_id = row.gram_id,
                                "cross-node DM giving up after horizon (recipient home unreachable); 落死信+通知发送方");
                            let _ = store.dead_letter_put(&row.sender, &row.receiver, row.gram_id, row.first_ms, &row.gram_bytes);
                            let _ = store.outbox_del(&row.sender, &row.receiver, row.gram_id);
                            ctx.group_metrics.dead_letters.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            crate::delivery::notify_delivery_failed(&ctx, &row.sender, &row.receiver, row.gram_id, "recipient home unreachable 24h").await;
                            continue;
                        }
                        if now_m < row.next_due_ms { continue; } // 未到重试点
                        // 向后兼容：对端 home 未广告 supports_direct_ack → 有界重传后放弃，不无限打爆老节点。
                        let ack_capable = ctx
                            .dir
                            .entities
                            .get(row.receiver.as_slice())
                            .map(|e| ctx.ack_nodes.contains(&e.home_node))
                            .unwrap_or(false);
                        if !ack_capable && row.retries >= crate::LEGACY_MAX_RETRIES {
                            tracing::debug!("legacy peer (no DirectAck); bounded retransmit exhausted");
                            let _ = store.outbox_del(&row.sender, &row.receiver, row.gram_id);
                            continue;
                        }
                        // 重发：join + publish_inbox + 火管兜底（收端 dedup 幂等）。
                        let _ = ctx.fed.join_inbox(&row.receiver, inbox_bootstrap(&row.receiver, &ctx)).await;
                        let _ = ctx.fed.publish_inbox(&row.receiver, row.gram_bytes.clone()).await;
                        let _ = ctx.group_pub.send(row.gram_bytes.clone());
                        // 退避推进。
                        let r1 = row.retries.saturating_add(1);
                        let backoff = crate::OUTBOX_BACKOFF_MS
                            [(r1 as usize).min(crate::OUTBOX_BACKOFF_MS.len() - 1)];
                        let _ = store.outbox_put(&row.sender, &row.receiver, row.gram_id,
                            row.first_ms, r1, now_m + backoff, &row.gram_bytes);
                    }
                    // 顺带清去重表（按龄，对齐重传上限）。
                    let _ = store.dedup_prune(now_m.saturating_sub(crate::OUTBOX_MAX_HORIZON_MS));

                    // ── 跨节点群消息可靠扇出重传（GroupAck 驱动）──
                    let grows = store.group_outbox_all().unwrap_or_default();
                    for row in grows {
                        use std::sync::atomic::Ordering::Relaxed;
                        // 达 24h 期限 → 放弃：落死信 + 告警（记尚未 ack 的成员节点数）。
                        if now_m.saturating_sub(row.first_ms) > crate::OUTBOX_MAX_HORIZON_MS {
                            tracing::warn!(gram_id = row.gram_id, pending = row.pending_nodes.len(),
                                "group message giving up after horizon (member node(s) unreachable); 落死信");
                            let _ = store.dead_letter_put(&row.sender, &row.gid, row.gram_id, row.first_ms, &row.gram_bytes);
                            let _ = store.group_outbox_del(&row.gid, &row.sender, row.gram_id);
                            ctx.group_metrics.giveups.fetch_add(1, Relaxed);
                            ctx.group_metrics.dead_letters.fetch_add(1, Relaxed);
                            continue;
                        }
                        if now_m < row.next_due_ms { continue; } // 未到重试点
                        // 向后兼容：若所有待 ack 节点都未广告 GroupAck 能力 → 有界重传后放弃，不无限打爆老节点。
                        let any_ack_capable = row.pending_nodes.iter().any(|n| ctx.ack_nodes.contains(n));
                        if !any_ack_capable && row.retries >= crate::LEGACY_MAX_RETRIES {
                            tracing::debug!("group member nodes without GroupAck; bounded retransmit exhausted");
                            let _ = store.group_outbox_del(&row.gid, &row.sender, row.gram_id);
                            continue;
                        }
                        // 重发到群主题（若知群）+ 火管兜底（总能到各成员节点；收端 dedup 幂等，已 ack 的节点跳过投递但会再 ack）。
                        if let Some(g) = ctx.groups.get(&row.gid).map(|g| g.clone()) {
                            let _ = ctx.fed.join_group(&row.gid, group_bootstrap(&g, &ctx)).await;
                            let _ = ctx.fed.publish(&row.gid, row.gram_bytes.clone()).await;
                        }
                        let _ = ctx.group_pub.send(row.gram_bytes.clone());
                        ctx.group_metrics.retransmits.fetch_add(1, Relaxed);
                        // 退避推进（pending 节点集不变，等 GroupAck 逐个销账）。
                        let r1 = row.retries.saturating_add(1);
                        let backoff = crate::OUTBOX_BACKOFF_MS
                            [(r1 as usize).min(crate::OUTBOX_BACKOFF_MS.len() - 1)];
                        let _ = store.group_outbox_put(&row.gid, &row.sender, row.gram_id,
                            row.first_ms, r1, now_m + backoff, &row.pending_nodes, &row.gram_bytes);
                    }
                }
            }
        })
    }

    /// 群 ack 聚合 flush 任务（~250ms 窗口）：把累加器里的待回执按 (gid,sender,gram_id) 去重后，
    /// 按发送方 home 能力分流——支持 GroupAckBatch 的 → 合并为批量 gram 仅发火管一次；不支持的（老节点/
    /// 能力未知）→ 回退逐条 GroupAck 双写（每群主题 + 火管），完全复刻旧行为 → 混版零丢失。
    /// 窗口 ≪ 首退避 2s → 不触发误重传。镜像 spawn_pending_flush 的 select 循环。
    pub fn spawn_group_ack_batcher(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let me = self.ep.id_bytes().to_vec();
            loop {
                tokio::select! {
                    biased;
                    _ = self.shutdown.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(crate::GROUP_ACK_BATCH_MS)) => {}
                }
                // drain：取出累加器（guard 不跨 await；空窗直接下一轮）。
                let items = {
                    let mut g = self.ack_accum.lock().unwrap();
                    if g.is_empty() { continue; }
                    std::mem::take(&mut *g)
                };
                // 去重：(gid,sender,gram_id) → sender_home（保留最新）。
                let mut uniq: std::collections::HashMap<(Vec<u8>, Vec<u8>, u64), Vec<u8>> =
                    std::collections::HashMap::with_capacity(items.len());
                for it in items {
                    uniq.insert((it.gid, it.sender, it.gram_id), it.sender_home);
                }
                // 分流：支持批量的发送方 home → 批量桶；否则（老节点/能力未知）→ legacy 桶。
                let mut batch: Vec<nm_proto::pb::GroupAckEntry> = Vec::new();
                let mut legacy: Vec<(Vec<u8>, Vec<u8>, u64)> = Vec::new();
                for ((gid, sender, gram_id), home) in uniq {
                    if self.ack_batch_nodes.contains(&home) {
                        batch.push(nm_proto::pb::GroupAckEntry { gid, sender, gram_id });
                    } else {
                        legacy.push((gid, sender, gram_id));
                    }
                }
                // legacy 桶：逐条重建 GroupAck → 每群主题（若知群）+ 火管双写（复刻旧行为）。
                if !legacy.is_empty() {
                    let ctx = self.ctx();
                    for (gid, sender, gram_id) in legacy {
                        let ack = GroupGossip {
                            origin: me.clone(),
                            body: Some(nm_proto::pb::group_gossip::Body::GroupAck(nm_proto::pb::GroupAck {
                                gid: gid.clone(),
                                sender,
                                gram_id,
                                crc: Vec::new(), // 销账键不含 crc；消费端忽略，留空
                                node: me.clone(),
                            })),
                        };
                        let ab = ack.encode_to_vec();
                        if let Some(g) = self.groups.get(&gid).map(|g| g.clone()) {
                            let _ = self.fed.join_group(&gid, group_bootstrap(&g, &ctx)).await;
                            let _ = self.fed.publish(&gid, ab.clone()).await;
                        }
                        let _ = self.group_pub.send(ab); // 火管兜底（发送方 home 必在火管上）
                    }
                }
                // 批量桶：按 GROUP_ACK_BATCH_MAX 分块 → GroupAckBatch 仅火管一次。
                if !batch.is_empty() {
                    let mut sent = 0u64;
                    for chunk in batch.chunks(crate::GROUP_ACK_BATCH_MAX) {
                        let bb = GroupGossip {
                            origin: me.clone(),
                            body: Some(nm_proto::pb::group_gossip::Body::GroupAckBatch(
                                nm_proto::pb::GroupAckBatch { node: me.clone(), acks: chunk.to_vec() },
                            )),
                        };
                        let _ = self.group_pub.send(bb.encode_to_vec());
                        sent += 1;
                    }
                    self.group_metrics
                        .ack_batches_sent
                        .fetch_add(sent, std::sync::atomic::Ordering::Relaxed);
                }
            }
        })
    }
}

/// 构造群 Announce 的 gossip 字节：origin=本节点；`supports_per_topic`=本节点能力广告（F1 协商）。
/// 该标志是「节点能力」不是「群状态」——仅在 announce 瞬间按本节点 per_topic 置位，收端据 origin 跟踪。
pub(crate) fn encode_announce(origin: &[u8], per_topic: bool, mut g: Group) -> Vec<u8> {
    g.supports_per_topic = per_topic;
    g.supports_ack_batch = true; // 编译期能力：本版本成员/发送方均支持 GroupAckBatch（announce 恒 origin=self）
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


use std::sync::Arc;

use crate::records::hex_encode;
use crate::delivery::{online_rids, try_push};
use crate::{now_ms, Ctx, PROTOCOL_VERSION};

use dashmap::{DashMap, DashSet};
use nm_proto::{
    Any, Channel, ChannelBackfillReq, ChannelGram, ChannelList, ChannelLog, ChannelMsg, ChannelOp,
    ChannelPub, ChannelPubReq, Command, Gram, GramKind,
};
use prost::Message;

/// home 签名覆盖的规范字节：定序后消息的权威内容。订阅者据 channel_id(=公钥) 验签，
/// 防任何主题参与者伪造 `ChannelMsg{seq=X}` 抢占 seq 槽。不含 home_sig/sender_sig 自身。
fn channel_msg_signing_bytes(m: &ChannelMsg) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"nmchan-msg:");
    b.extend_from_slice(&m.channel_id);
    b.extend_from_slice(&m.sender);
    b.extend_from_slice(&m.seq.to_be_bytes());
    b.extend_from_slice(&(m.ts as u64).to_be_bytes());
    b.extend_from_slice(&m.nonce);
    b.extend_from_slice(m.body.as_bytes());
    b
}

/// 发布者签名覆盖的规范字节（PubReq：尚无 seq）。home 据此验发布者身份 + 权限。
fn channel_pubreq_signing_bytes(channel_id: &[u8], sender: &[u8], ts: i64, body: &str, nonce: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"nmchan-req:");
    b.extend_from_slice(channel_id);
    b.extend_from_slice(sender);
    b.extend_from_slice(&(ts as u64).to_be_bytes());
    b.extend_from_slice(nonce);
    b.extend_from_slice(body.as_bytes());
    b
}

/// 本节点是否为该频道 home（持有其签名私钥）→ 可做定序器。
fn is_channel_home(ctx: &Ctx, channel_id: &[u8]) -> bool {
    ctx.store.as_ref().and_then(|s| s.channel_secret(channel_id).ok().flatten()).is_some()
}

/// home 判定（仅需 store，供 Node 层 pkarr 任务调用；语义同 is_channel_home）。
pub(crate) fn node_is_channel_home(store: &Option<Arc<nm_store::RedbStore>>, channel_id: &[u8]) -> bool {
    store.as_ref().and_then(|s| s.channel_secret(channel_id).ok().flatten()).is_some()
}

/// home 定序：验权 → 单事务分配 seq+签名+落日志+记 intake（Phase 0 `channel_sequence`）→ 返回已签 Msg。
/// 幂等：重复 (cid,sender,nonce) 返回已存消息（不重分配）。拒绝 → Err(reason)（调用方回 PubReject）。
/// 崩溃安全：持久（含签名）在本函数内 commit，调用方 commit 后才广播。
fn sequence_pubreq(ctx: &Ctx, req: &ChannelPubReq) -> Result<ChannelMsg, String> {
    let Some(store) = &ctx.store else { return Err("no store".into()) };
    let Some(secret) = store.channel_secret(&req.channel_id).ok().flatten() else {
        return Err("not home".into());
    };
    // 发布策略：owner_only → sender 须为 owner。open → 放行。
    let meta = store.channel(&req.channel_id).ok().flatten();
    if let Some(m) = &meta {
        if m.pub_policy == nm_proto::PubPolicy::OwnerOnly as i32
            && m.owner.as_slice() != req.sender.as_slice()
        {
            return Err("owner_only".into());
        }
    }
    // 验发布者签名（sender_sig 对 PubReq 规范字节）。空签名在 legacy/open 容忍，keyed 频道要求。
    let keyed = meta.as_ref().map(|m| m.keyed).unwrap_or(false);
    if keyed && !req.sender_sig.is_empty() {
        if let (Ok(pk), Ok(sig)) = (
            <[u8; 32]>::try_from(req.sender.as_slice()),
            <[u8; 64]>::try_from(req.sender_sig.as_slice()),
        ) {
            let sb = channel_pubreq_signing_bytes(&req.channel_id, &req.sender, req.ts, &req.body, &req.nonce);
            if nm_crypto::verify_bytes(&pk, &sb, &sig).is_err() {
                return Err("bad_sender_sig".into());
            }
        }
    }
    let sk = {
        let Ok(b) = <[u8; 32]>::try_from(secret.as_slice()) else { return Err("bad secret".into()) };
        nm_transport::SecretKey::from_bytes(&b)
    };
    let (cid, sender, ts, body, nonce) =
        (req.channel_id.clone(), req.sender.clone(), req.ts, req.body.clone(), req.nonce.clone());
    let sender_sig = req.sender_sig.clone();
    let (_seq, msg, _fresh) = store
        .channel_sequence(&req.channel_id, &req.sender, &req.nonce, move |seq| {
            let mut m = ChannelMsg {
                channel_id: cid, sender, seq, ts, body, nonce, home_sig: Vec::new(), sender_sig,
            };
            let sig = nm_crypto::sign_bytes(&sk, &channel_msg_signing_bytes(&m));
            m.home_sig = sig.to_vec();
            m
        })
        .map_err(|e| format!("sequence failed: {e}"))?;
    Ok(msg)
}

/// 校验 home 对定序消息的签名（订阅者强制：仅 keyed 频道有权威签名；legacy 无 keyed 跳过=今日信任）。
fn verify_channel_msg(m: &ChannelMsg, keyed: bool) -> bool {
    if !keyed {
        return true; // legacy 频道：无 home 签名，按 (cid,seq) 信任（降级完整性，与现网一致）
    }
    let (Ok(pk), Ok(sig)) = (
        <[u8; 32]>::try_from(m.channel_id.as_slice()),
        <[u8; 64]>::try_from(m.home_sig.as_slice()),
    ) else {
        return false;
    };
    nm_crypto::verify_bytes(&pk, &channel_msg_signing_bytes(m), &sig).is_ok()
}

/// 频道运行态（P4）：iroh-gossip 主题上的开放 pub/sub。每个已订阅频道一个后台任务：
/// 收播 gossip 消息 → 持久日志 + 转发给本地账号订阅者的在线会话；发布经 mpsc 交给该任务广播。
///
/// Phase 1：订阅改**账号级**（`subs` 存账号公钥，持久镜像于 nm-store CHANNEL_SUBS），
/// 元信息/日志持久化（重启恢复），独立频道泵（ChannelHub，自愈 rejoin，不走 per_topic-gated fed_rx）。
pub(crate) struct ChannelEntry {
    meta: Arc<std::sync::Mutex<Channel>>, // 频道元信息（gossip 可更新：晚订阅者据 owner 重播补全）
    subs: Arc<DashSet<Vec<u8>>>, // 本地订阅**账号**公钥（非连接 rid）；投递时各账号 → online_rids
    seq: Arc<std::sync::atomic::AtomicU64>, // 本地发布序号（Phase 1 暂留每节点；Phase 2 换 home 全局定序）
    pub_tx: tokio::sync::mpsc::UnboundedSender<ChannelOut>, // → 频道任务：待广播的消息 / 元信息
}

/// 频道后台任务的出站项。
enum ChannelOut {
    Sequenced(ChannelMsg), // 已定序消息（home 本地产出）：本地投递 + 广播 Msg
    Req(ChannelPubReq),    // 发布意图（远端 home）：广播 PubReq 到频道主题
    Meta(Channel),         // 元信息广播
}

pub(crate) type Channels = DashMap<Vec<u8>, ChannelEntry>;
const CHANNEL_LOG_CAP: usize = 2000; // 每频道持久日志保留条数（持久化后放宽；超出按 seq 裁剪）
const CHANNEL_BACKFILL_LIMIT: usize = 500; // 单次 backfill 返回上限

/// 频道 gossip 主题 seed：blake3("nmspace:channel:" + hex(channel_id))。
/// Phase 1 保留此 topic 方案（与现网一致，不重排叠加网），频道泵直连 ChannelHub（独立泵，不依赖 per_topic）。
fn channel_topic(channel_id: &[u8]) -> [u8; 32] {
    nm_crypto::content_hash(format!("nmspace:channel:{}", hex_encode(channel_id)).as_bytes())
}

/// 确保频道已 join gossip 并起收播任务（幂等）。首次订阅时调用。
async fn ensure_channel(ctx: &Ctx, channel_id: &[u8], meta: Option<Channel>) -> bool {
    if ctx.channels.contains_key(channel_id) {
        return true;
    }
    let mut bootstrap: Vec<[u8; 32]> = ctx
        .peers
        .iter()
        .filter_map(|e| <[u8; 32]>::try_from(e.key().as_slice()).ok())
        .collect();
    // pkarr 发现兜底：无锚的晚加入者据频道 id 解析到的 seed（由 spawn_channel_pkarr 周期填充）。
    if let Some(seeds) = ctx.pkarr_seeds.get(channel_id) {
        for s in seeds.value() {
            if !bootstrap.contains(s) {
                bootstrap.push(*s);
            }
        }
    }
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
    // seq 从持久计数器水位恢复（Phase 1 每节点；Phase 2 定序器接管）。
    let seq0 = ctx.store.as_ref().and_then(|s| s.channel_seq_peek(channel_id).ok()).unwrap_or(1);
    let seq = Arc::new(std::sync::atomic::AtomicU64::new(seq0));
    let (pub_tx, pub_rx) = tokio::sync::mpsc::unbounded_channel();
    // 元信息优先用持久化的（重启恢复），否则用传入的，否则空壳。
    let persisted = ctx.store.as_ref().and_then(|s| s.channel(channel_id).ok().flatten());
    let meta = Arc::new(std::sync::Mutex::new(
        persisted
            .or(meta)
            .unwrap_or(Channel { channel_id: channel_id.to_vec(), ..Default::default() }),
    ));
    // 先插入 entry 再 spawn：run_channel 首轮自检 ctx.channels.contains_key，若先 spawn 后 insert 有竞态。
    ctx.channels.insert(channel_id.to_vec(), ChannelEntry { meta: meta.clone(), subs: subs.clone(), seq, pub_tx });
    tokio::spawn(run_channel(ctx.clone(), channel_id.to_vec(), topic, subs, pub_rx, meta));
    tracing::info!("channel joined");
    true
}

/// 频道后台任务：发布队列 + gossip 收播；两路都「持久日志 + 转发本地账号订阅者的在线会话」。
/// 收播返回 None（主题死）→ 自愈 rejoin（独立泵补上 fed 泵的自愈能力）。
async fn run_channel(
    ctx: Ctx,
    channel_id: Vec<u8>,
    mut topic: nm_gossip::ChannelTopic,
    subs: Arc<DashSet<Vec<u8>>>,
    mut pub_rx: tokio::sync::mpsc::UnboundedReceiver<ChannelOut>,
    meta: Arc<std::sync::Mutex<Channel>>,
) {
    use nm_proto::pb::channel_gram::Kind;
    let mut last_meta_bcast = std::time::Instant::now();
    let meta_bcast_every = std::time::Duration::from_secs(30);
    let mut last_live = std::time::Instant::now();
    let live_every = std::time::Duration::from_secs(10); // home 水位心跳周期
    let mut last_retx = std::time::Instant::now();
    let retx_every = std::time::Duration::from_secs(2); // publisher outbox 重传扫描周期
    loop {
        if ctx.shutdown.is_cancelled() {
            break;
        }
        // 本节点已无账号订阅该频道（unsub 移除了 entry）→ 停任务。
        if !ctx.channels.contains_key(&channel_id) {
            tracing::debug!("channel task exiting (no local subscribers)");
            break;
        }
        // 周期重播元信息（晚订阅者补全名称/简介/头像）。
        if last_meta_bcast.elapsed() >= meta_bcast_every {
            last_meta_bcast = std::time::Instant::now();
            let snap = meta.lock().unwrap().clone();
            if !snap.name.is_empty() || !snap.topic.is_empty() || !snap.avatar_url.is_empty() {
                let env = ChannelGram { kind: Some(Kind::Meta(snap)) };
                let _ = topic.publish(env.encode_to_vec()).await;
            }
        }
        // home 水位心跳：广告 (max_seq, earliest_seq, supports_sequencing=keyed)，订阅者据此发现落后 → 回填。
        if is_channel_home(&ctx, &channel_id) && last_live.elapsed() >= live_every {
            last_live = std::time::Instant::now();
            if let Some(s) = &ctx.store {
                let (earliest, latest) = s.channel_log_bounds(&channel_id).ok().flatten().unwrap_or((0, 0));
                let keyed = s.channel(&channel_id).ok().flatten().map(|c| c.keyed).unwrap_or(false);
                let live = nm_proto::ChannelLive {
                    channel_id: channel_id.clone(),
                    max_seq: latest,
                    earliest_seq: earliest,
                    supports_sequencing: keyed,
                };
                let env = ChannelGram { kind: Some(Kind::Live(live)) };
                let _ = topic.publish(env.encode_to_vec()).await;
            }
        }
        // publisher outbox 重传：到期未确认的 PubReq 重广播 + 指数退避 + 24h 放弃。
        if last_retx.elapsed() >= retx_every {
            last_retx = std::time::Instant::now();
            retransmit_outbox(&ctx, &channel_id, &mut topic).await;
        }
        // select：发布立即唤醒（不等收播 400ms 超时）→ 低延迟落库/广播；收播正常处理。
        tokio::select! {
            biased;
            _ = ctx.shutdown.cancelled() => break,
            out = pub_rx.recv() => {
                match out {
                    Some(ChannelOut::Sequenced(m)) => {
                        // home 本地产出的已定序消息：本地投递 + 广播 Msg（已落库于 sequence_pubreq）。
                        channel_deliver(&ctx, &m, &subs).await;
                        let env = ChannelGram { kind: Some(Kind::Msg(m)) };
                        let _ = topic.publish(env.encode_to_vec()).await;
                    }
                    Some(ChannelOut::Req(req)) => {
                        // 远端 home：持久入 CHANNEL_OUTBOX（重传直到看到自己的 sequenced Msg / 收 Reject / 24h 放弃），
                        // 再广播发布意图到频道主题，由 home 定序回广播 Msg。
                        if let Some(s) = &ctx.store {
                            let now = now_ms();
                            let _ = s.channel_outbox_put(
                                &req.channel_id, &req.sender, &req.nonce,
                                now, 0, now + crate::OUTBOX_BACKOFF_MS[0], &req.encode_to_vec(),
                            );
                        }
                        let env = ChannelGram { kind: Some(Kind::PubReq(req)) };
                        let _ = topic.publish(env.encode_to_vec()).await;
                    }
                    Some(ChannelOut::Meta(c)) => {
                        let env = ChannelGram { kind: Some(Kind::Meta(c)) };
                        let _ = topic.publish(env.encode_to_vec()).await;
                        last_meta_bcast = std::time::Instant::now();
                    }
                    None => break, // 所有 pub_tx 已 drop（entry 移除）→ 收摊
                }
            }
            r = tokio::time::timeout(std::time::Duration::from_millis(400), topic.recv()) => match r {
                Ok(Some(cm)) => {
                    if let Ok(env) = ChannelGram::decode(cm.content.as_slice()) {
                        match env.kind {
                            Some(Kind::Msg(m)) if m.channel_id == channel_id => {
                                // 收定序消息：验 home 签名（防伪造抢 seq）→ 持久去重 → 投递。
                                channel_ingest(&ctx, &m, &subs).await;
                                // publisher 侧：看到自己发布意图被定序 → 清 outbox 停重传。
                                if let Some(s) = &ctx.store {
                                    let _ = s.channel_outbox_del(&m.channel_id, &m.sender, &m.nonce);
                                }
                            }
                            Some(Kind::PubReq(req)) if req.channel_id == channel_id => {
                                // 仅 home 响应：定序 + 回广播 Msg（+ 本地投递）；拒绝则回 PubReject。
                                if is_channel_home(&ctx, &channel_id) {
                                    match sequence_pubreq(&ctx, &req) {
                                        Ok(m) => {
                                            channel_deliver(&ctx, &m, &subs).await;
                                            let env = ChannelGram { kind: Some(Kind::Msg(m)) };
                                            let _ = topic.publish(env.encode_to_vec()).await;
                                        }
                                        Err(reason) => {
                                            if let Some(sk) = channel_home_sk(&ctx, &channel_id) {
                                                let rej = build_pub_reject(&sk, &req, &reason);
                                                let env = ChannelGram { kind: Some(Kind::Reject(rej)) };
                                                let _ = topic.publish(env.encode_to_vec()).await;
                                            }
                                        }
                                    }
                                }
                            }
                            Some(Kind::Reject(rej)) if rej.channel_id == channel_id => {
                                // publisher 侧：home 签名的拒绝 → 验签后清 outbox 停重传（不再 24h 空等）。
                                if verify_pub_reject(&rej) {
                                    if let Some(s) = &ctx.store {
                                        let _ = s.channel_outbox_del(&rej.channel_id, &rej.sender, &rej.nonce);
                                    }
                                    tracing::info!(reason = %rej.reason, "channel publish rejected by home");
                                }
                            }
                            Some(Kind::BackfillReq(req)) if req.channel_id == channel_id => {
                                // 仅 home 应答：从持久日志取 (since_seq, +limit] 回广播 Log（含 earliest_seq 下限）。
                                if is_channel_home(&ctx, &channel_id) {
                                    if let Some(log) = build_backfill_log(&ctx, &req) {
                                        let env = ChannelGram { kind: Some(Kind::Log(log)) };
                                        let _ = topic.publish(env.encode_to_vec()).await;
                                    }
                                }
                            }
                            Some(Kind::Log(log)) => {
                                // 订阅者侧：回填的历史消息逐条 ingest（验签+去重+投递）。
                                for m in &log.msgs {
                                    if m.channel_id == channel_id {
                                        channel_ingest(&ctx, m, &subs).await;
                                    }
                                }
                            }
                            Some(Kind::Live(live)) if live.channel_id == channel_id => {
                                // home 水位心跳：本地落后（max_seq > 本地最高）→ 触发有界回填补齐。
                                maybe_backfill(&ctx, &channel_id, &live, &mut topic).await;
                            }
                            Some(Kind::Meta(c)) if c.channel_id == channel_id => {
                                merge_channel_meta(&ctx, &meta, &c);
                            }
                            _ => {}
                        }
                    }
                }
                Ok(None) => {
                    // 主题死 → 自愈 rejoin（订阅仍在，不丢频道）。
                    let bootstrap: Vec<[u8; 32]> = ctx
                        .peers
                        .iter()
                        .filter_map(|e| <[u8; 32]>::try_from(e.key().as_slice()).ok())
                        .collect();
                    match nm_gossip::ChannelHub::new(ctx.gossip.clone())
                        .join(channel_topic(&channel_id), bootstrap)
                        .await
                    {
                        Ok(t) => { topic = t; tracing::debug!("channel topic rejoined"); }
                        Err(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
                    }
                }
                Err(_) => {} // 收播超时：正常，回循环顶（检查 shutdown/退订/元信息重播）
            }
        }
    }
}

/// 合并收到的频道元信息到本地（仅用非空字段覆盖）+ 持久化（重启恢复）。
fn merge_channel_meta(ctx: &Ctx, meta: &std::sync::Mutex<Channel>, incoming: &Channel) {
    let snap = {
        let mut m = meta.lock().unwrap();
        if !incoming.name.is_empty() { m.name = incoming.name.clone(); }
        if !incoming.topic.is_empty() { m.topic = incoming.topic.clone(); }
        if !incoming.avatar_url.is_empty() { m.avatar_url = incoming.avatar_url.clone(); }
        if !incoming.owner.is_empty() { m.owner = incoming.owner.clone(); }
        if !incoming.home_node.is_empty() { m.home_node = incoming.home_node.clone(); }
        if incoming.created_at != 0 { m.created_at = incoming.created_at; }
        m.clone()
    };
    if let Some(s) = &ctx.store {
        let _ = s.put_channel(&snap);
    }
}

/// 收到定序消息：验 home 签名 → 持久日志(点查去重) → 投递。验签失败则丢弃（防伪造抢 seq 槽）。
async fn channel_ingest(ctx: &Ctx, m: &ChannelMsg, subs: &DashSet<Vec<u8>>) {
    // keyed 频道强制验 home 签名；legacy 跳过（降级信任）。
    let keyed = ctx.store.as_ref()
        .and_then(|s| s.channel(&m.channel_id).ok().flatten())
        .map(|c| c.keyed)
        .unwrap_or(false);
    if !verify_channel_msg(m, keyed) {
        tracing::warn!(seq = m.seq, "channel msg home_sig verify failed; dropped (possible forgery)");
        return;
    }
    if let Some(s) = &ctx.store {
        match s.channel_log_has(&m.channel_id, m.seq) {
            Ok(true) => return, // 已见，去重（gossip 可能重投）
            Ok(false) => {
                let _ = s.channel_log_put(m);
                prune_channel_log(s, &m.channel_id);
            }
            Err(_) => {} // store 抖动：降级为仍投递，不丢实时消息
        }
    }
    channel_deliver(ctx, m, subs).await;
}

/// 仅投递（不验签/不落库）：home 本地已定序并持久的消息走此路，避免重复落库。
async fn channel_deliver(ctx: &Ctx, m: &ChannelMsg, subs: &DashSet<Vec<u8>>) {
    let gram = channel_gram(m);
    let accounts: Vec<Vec<u8>> = subs.iter().map(|s| s.key().clone()).collect();
    for acct in accounts {
        for rid in online_rids(&ctx.sessions, &ctx.principals, &ctx.devices, &acct) {
            try_push(&rid, &gram, &ctx.sessions).await;
        }
    }
}

/// 日志保留：跨度超 CAP 时按 seq 裁剪旧条目。
fn prune_channel_log(s: &nm_store::RedbStore, channel_id: &[u8]) {
    if let Ok(Some((earliest, latest))) = s.channel_log_bounds(channel_id) {
        if latest.saturating_sub(earliest) >= CHANNEL_LOG_CAP as u64 {
            let floor = latest.saturating_sub(CHANNEL_LOG_CAP as u64) + 1;
            let _ = s.channel_log_prune(channel_id, floor);
        }
    }
}

/// 构造 home 签名的发布拒绝（PubReject）：publisher 据此立即清 outbox + UI 报错，不再 24h 空等。
fn build_pub_reject(sk: &nm_transport::SecretKey, req: &ChannelPubReq, reason: &str) -> nm_proto::ChannelPubReject {
    let mut b = Vec::new();
    b.extend_from_slice(b"nmchan-reject:");
    b.extend_from_slice(&req.channel_id);
    b.extend_from_slice(&req.sender);
    b.extend_from_slice(&req.nonce);
    b.extend_from_slice(reason.as_bytes());
    let sig = nm_crypto::sign_bytes(sk, &b);
    nm_proto::ChannelPubReject {
        channel_id: req.channel_id.clone(),
        sender: req.sender.clone(),
        nonce: req.nonce.clone(),
        reason: reason.to_string(),
        home_sig: sig.to_vec(),
    }
}

/// 规范化 PubReject 签名字节（与 build_pub_reject 一致）。
fn pub_reject_signing_bytes(rej: &nm_proto::ChannelPubReject) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"nmchan-reject:");
    b.extend_from_slice(&rej.channel_id);
    b.extend_from_slice(&rej.sender);
    b.extend_from_slice(&rej.nonce);
    b.extend_from_slice(rej.reason.as_bytes());
    b
}

/// 验 home 对 PubReject 的签名（channel_id=公钥）。publisher 仅认可验证通过的拒绝，防伪造拒绝。
fn verify_pub_reject(rej: &nm_proto::ChannelPubReject) -> bool {
    let (Ok(pk), Ok(sig)) = (
        <[u8; 32]>::try_from(rej.channel_id.as_slice()),
        <[u8; 64]>::try_from(rej.home_sig.as_slice()),
    ) else {
        return false;
    };
    nm_crypto::verify_bytes(&pk, &pub_reject_signing_bytes(rej), &sig).is_ok()
}

/// 取本节点持有的频道签名私钥（home 专用）。
fn channel_home_sk(ctx: &Ctx, channel_id: &[u8]) -> Option<nm_transport::SecretKey> {
    ctx.store.as_ref()
        .and_then(|s| s.channel_secret(channel_id).ok().flatten())
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .map(|b| nm_transport::SecretKey::from_bytes(&b))
}

/// home 构造回填应答：从持久日志取 (since_seq, +limit]，附日志下限 earliest_seq + 分页游标。
fn build_backfill_log(ctx: &Ctx, req: &ChannelBackfillReq) -> Option<ChannelLog> {
    let s = ctx.store.as_ref()?;
    let limit = if req.limit == 0 { CHANNEL_BACKFILL_LIMIT } else { (req.limit as usize).min(CHANNEL_BACKFILL_LIMIT) };
    let msgs = s.channel_log_since(&req.channel_id, req.since_seq, limit).unwrap_or_default();
    let (earliest, _latest) = s.channel_log_bounds(&req.channel_id).ok().flatten().unwrap_or((0, 0));
    let next_seq = msgs.last().map(|m| m.seq).unwrap_or(req.since_seq);
    let more = msgs.len() == limit;
    Some(ChannelLog { msgs, earliest_seq: earliest, next_seq, more })
}

/// 订阅者据 home 水位心跳判断是否落后：本地「从下限起连续 seq」< home.max_seq → 广播 BackfillReq 补齐。
/// 用「连续游标」而非「最高 seq」做 since——晚订阅者可能先收到高 seq 实时消息、底部留洞，
/// 必须从连续处回填才能补洞（否则 since=local_max 永远跳过底部缺口）。
/// floor = home.earliest_seq-1：低于 home 日志下限的历史已裁剪、不可得，防无限回填。
async fn maybe_backfill(ctx: &Ctx, channel_id: &[u8], live: &nm_proto::ChannelLive, topic: &mut nm_gossip::ChannelTopic) {
    let Some(s) = &ctx.store else { return };
    let floor = live.earliest_seq.saturating_sub(1);
    // 从 floor 起本地连续到哪（遇断层即止）→ 即下一个要补的起点。
    let contiguous = s.channel_log_contiguous_upto(channel_id, floor).unwrap_or(floor);
    if contiguous >= live.max_seq {
        return; // 已连续补齐到 home 水位
    }
    let req = ChannelBackfillReq { channel_id: channel_id.to_vec(), since_seq: contiguous, limit: CHANNEL_BACKFILL_LIMIT as u32 };
    let env = ChannelGram { kind: Some(nm_proto::pb::channel_gram::Kind::BackfillReq(req)) };
    let _ = topic.publish(env.encode_to_vec()).await;
    tracing::debug!(contiguous, home_max = live.max_seq, "channel backfill requested (gap below home water-mark)");
}

/// publisher outbox 重传：扫本频道待确认 PubReq，到期重广播 + 指数退避 + 24h 放弃。
async fn retransmit_outbox(ctx: &Ctx, channel_id: &[u8], topic: &mut nm_gossip::ChannelTopic) {
    let Some(s) = &ctx.store else { return };
    let now = now_ms();
    for row in s.channel_outbox_all().unwrap_or_default() {
        if row.channel_id != channel_id {
            continue; // 本任务只管本频道
        }
        // 达 24h 期限 → 放弃（home 长期不可达）。
        if now.saturating_sub(row.first_ms) > crate::OUTBOX_MAX_HORIZON_MS {
            tracing::warn!("channel publish giving up after horizon (home unreachable)");
            let _ = s.channel_outbox_del(&row.channel_id, &row.sender, &row.nonce);
            continue;
        }
        if now < row.next_due_ms {
            continue; // 未到重试点
        }
        // 重广播 PubReq（home 侧 intake 幂等：重复 nonce 不重分配 seq，只重广播已有 Msg）。
        if let Ok(req) = ChannelPubReq::decode(row.pubreq_bytes.as_slice()) {
            let env = ChannelGram { kind: Some(nm_proto::pb::channel_gram::Kind::PubReq(req)) };
            let _ = topic.publish(env.encode_to_vec()).await;
        }
        let r1 = row.retries.saturating_add(1);
        let backoff = crate::OUTBOX_BACKOFF_MS[(r1 as usize).min(crate::OUTBOX_BACKOFF_MS.len() - 1)];
        let _ = s.channel_outbox_put(
            &row.channel_id, &row.sender, &row.nonce,
            row.first_ms, r1, now + backoff, &row.pubreq_bytes,
        );
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

pub(crate) async fn channel_create(ctx: &Ctx, caller: &[u8], _rid: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
        Some(op) if op.channel_id.len() == 32 => {
            // keyed 频道：secret 非空 → 校验 channel_id==其公钥，存 CHANNEL_SECRETS（本节点成为 home 定序器）。
            let keyed = !op.secret.is_empty();
            if keyed {
                let Ok(sk_bytes) = <[u8; 32]>::try_from(op.secret.as_slice()) else {
                    return (false, None, "channel secret must be 32 bytes".into());
                };
                let sk = nm_transport::SecretKey::from_bytes(&sk_bytes);
                if sk.public().as_bytes().as_slice() != op.channel_id.as_slice() {
                    return (false, None, "channel secret does not match channel_id (pubkey)".into());
                }
                if let Some(s) = &ctx.store {
                    if let Err(e) = s.put_channel_secret(&op.channel_id, &sk_bytes) {
                        return (false, None, format!("persist channel secret failed: {e}"));
                    }
                }
            }
            let meta = Channel {
                channel_id: op.channel_id.clone(),
                name: op.name,
                owner: caller.to_vec(),
                created_at: now_ms() as i64,
                topic: op.topic,
                avatar_url: op.avatar_url,
                home_node: ctx.node_id.to_vec(),
                // 广播/公告定位：keyed 频道默认 owner_only；legacy（无 secret）保持 open（旧行为）。
                pub_policy: if keyed { nm_proto::PubPolicy::OwnerOnly as i32 } else { nm_proto::PubPolicy::Open as i32 },
                keyed,
                earliest_seq: 0,
            };
            if let Some(s) = &ctx.store {
                let _ = s.put_channel(&meta);
            }
            ensure_channel(ctx, &op.channel_id, Some(meta)).await;
            if let Some(e) = ctx.channels.get(&op.channel_id) {
                e.subs.insert(caller.to_vec()); // 账号级订阅
                let _ = e.pub_tx.send(ChannelOut::Meta(e.meta.lock().unwrap().clone())); // 广播元信息
            }
            if let Some(s) = &ctx.store {
                let _ = s.channel_sub_put(caller, &op.channel_id, now_ms());
            }
            (true, None, String::new())
        }
        _ => (false, None, "invalid channel op".into()),
    }
}

pub(crate) async fn channel_sub(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
        Some(op) if op.channel_id.len() == 32 => {
            let meta = Channel { channel_id: op.channel_id.clone(), name: op.name, topic: op.topic, ..Default::default() };
            if ensure_channel(ctx, &op.channel_id, Some(meta.clone())).await {
                if let Some(e) = ctx.channels.get(&op.channel_id) {
                    e.subs.insert(caller.to_vec()); // 账号级订阅
                }
                if let Some(s) = &ctx.store {
                    // 订阅记录 + 若尚无持久 meta 则存一份（订阅的频道始终有记录可列出）。
                    let _ = s.channel_sub_put(caller, &op.channel_id, now_ms());
                    if s.channel(&op.channel_id).ok().flatten().is_none() {
                        let _ = s.put_channel(&meta);
                    }
                }
                (true, None, String::new())
            } else {
                (false, None, "channel join failed".into())
            }
        }
        _ => (false, None, "invalid channel op".into()),
    }
}

pub(crate) async fn channel_unsub(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
        Some(op) => {
            let mut now_empty = false;
            if let Some(e) = ctx.channels.get(&op.channel_id) {
                e.subs.remove(caller);
                now_empty = e.subs.is_empty();
            }
            if let Some(s) = &ctx.store {
                let _ = s.channel_sub_del(caller, &op.channel_id);
            }
            // 本节点再无账号订阅该频道 → 移除 entry，收播任务下一轮自检退出，不再空转。
            if now_empty {
                ctx.channels.remove(&op.channel_id);
            }
            (true, None, String::new())
        }
        None => (false, None, "invalid channel op".into()),
    }
}

pub(crate) fn channel_publish(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd.params.as_ref().and_then(|p| ChannelPub::decode(p.value.as_slice()).ok()) {
        Some(pp) => match ctx.channels.get(&pp.channel_id) {
            Some(e) => {
                // 幂等 nonce（client 重传同一条用同 nonce；此处 node-origin 发布随机生成）。
                let nonce = nm_transport::SecretKey::generate().to_bytes()[..16].to_vec();
                let req = ChannelPubReq {
                    channel_id: pp.channel_id.clone(),
                    sender: caller.to_vec(),
                    ts: now_ms() as i64,
                    body: pp.body,
                    nonce: nonce.clone(),
                    sender_sig: Vec::new(), // 完整实现由 client 账号私钥签；node-origin 留空（home 按 open/legacy 容忍）
                };
                if is_channel_home(ctx, &pp.channel_id) {
                    // home==本节点：进程内直接定序（gossip 不自回环，若等自己的 PubReq 回来会死锁）。
                    match sequence_pubreq(ctx, &req) {
                        Ok(msg) => {
                            let seq = msg.seq;
                            let _ = e.pub_tx.send(ChannelOut::Sequenced(msg)); // 本地投递 + 广播 Msg
                            (true, Some(Any { type_url: "nmspace.v1/channel.seq".to_string(), value: seq.to_be_bytes().to_vec() }), String::new())
                        }
                        Err(reason) => (false, None, format!("publish denied: {reason}")),
                    }
                } else {
                    // 远端 home：广播 PubReq 到频道主题，由 home 定序回广播 Msg（Phase 3 加 outbox 重传）。
                    let _ = e.pub_tx.send(ChannelOut::Req(req));
                    (true, Some(Any { type_url: "nmspace.v1/channel.nonce".to_string(), value: nonce }), String::new())
                }
            }
            None => (false, None, "not subscribed to channel".into()),
        },
        None => (false, None, "invalid channel pub".into()),
    }
}

pub(crate) fn channel_backfill(ctx: &Ctx, cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd.params.as_ref().and_then(|p| ChannelBackfillReq::decode(p.value.as_slice()).ok()) {
        Some(req) => {
            let Some(s) = &ctx.store else {
                return (false, None, "no store".into());
            };
            let limit = if req.limit == 0 { CHANNEL_BACKFILL_LIMIT } else { (req.limit as usize).min(CHANNEL_BACKFILL_LIMIT) };
            let msgs = s.channel_log_since(&req.channel_id, req.since_seq, limit).unwrap_or_default();
            // earliest_seq=日志下限（client clamp，低于此的历史已裁剪、不可得，防无限回填）。
            let (earliest, _latest) = s.channel_log_bounds(&req.channel_id).ok().flatten().unwrap_or((0, 0));
            let next_seq = msgs.last().map(|m| m.seq).unwrap_or(req.since_seq);
            let more = msgs.len() == limit;
            (
                true,
                Some(Any {
                    type_url: "nmspace.v1.ChannelLog".to_string(),
                    value: ChannelLog { msgs, earliest_seq: earliest, next_seq, more }.encode_to_vec(),
                }),
                String::new(),
            )
        }
        None => (false, None, "invalid backfill req".into()),
    }
}

pub(crate) fn channel_set_meta(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
        Some(op) if op.channel_id.len() == 32 => match ctx.channels.get(&op.channel_id) {
            Some(e) => {
                let is_owner = e.meta.lock().unwrap().owner.as_slice() == caller;
                if !is_owner {
                    (false, None, "仅频道创建者可修改频道信息".into())
                } else {
                    let snap = {
                        let mut m = e.meta.lock().unwrap();
                        if !op.name.is_empty() { m.name = op.name; }
                        m.topic = op.topic;
                        m.avatar_url = op.avatar_url;
                        if m.home_node.is_empty() { m.home_node = ctx.node_id.to_vec(); }
                        m.clone()
                    };
                    if let Some(s) = &ctx.store {
                        let _ = s.put_channel(&snap); // 持久化元信息（重启恢复）
                    }
                    let _ = e.pub_tx.send(ChannelOut::Meta(snap));
                    (true, None, String::new())
                }
            }
            None => (false, None, "unknown channel".into()),
        },
        _ => (false, None, "invalid channel op".into()),
    }
}

pub(crate) fn channel_list(ctx: &Ctx, caller: &[u8]) -> (bool, Option<Any>, String) {
    // 仅返回 caller 账号订阅的频道（修「节点全局泄漏」：旧版返回本节点内存里所有频道）。
    let channels: Vec<Channel> = match &ctx.store {
        Some(s) => {
            let ids = s.channel_subs_of(caller).unwrap_or_default();
            ids.iter()
                .map(|cid| {
                    // 内存运行态 meta（最新）→ 持久化副本 → 最小 stub（订阅本身始终可见，即便 meta 未知）。
                    ctx.channels
                        .get(cid)
                        .map(|e| e.meta.lock().unwrap().clone())
                        .or_else(|| s.channel(cid).ok().flatten())
                        .unwrap_or_else(|| Channel { channel_id: cid.clone(), ..Default::default() })
                })
                .collect()
        }
        None => ctx.channels.iter().map(|e| e.meta.lock().unwrap().clone()).collect(),
    };
    (
        true,
        Some(Any { type_url: "nmspace.v1.ChannelList".to_string(), value: ChannelList { channels }.encode_to_vec() }),
        String::new(),
    )
}

/// 启动恢复（Phase 1 头号收益）：从持久化重建频道运行态。
/// 对本节点持有本地账号订阅的每个频道：重 join 主题、重起收播泵、回填内存 `subs`（账号集）。
/// 本地账号来自 devices 表（该账号的设备证书由本节点托管 → 本节点是其 home）。
pub(crate) async fn restore_channels(ctx: &Ctx) {
    let Some(store) = &ctx.store else { return };
    let local_accounts: std::collections::HashSet<Vec<u8>> = ctx
        .devices
        .iter()
        .filter_map(|d| d.cert.as_ref().map(|c| c.account.clone()))
        .collect();
    let mut restored = 0usize;
    for acct in &local_accounts {
        let subs = store.channel_subs_of(acct).unwrap_or_default();
        for cid in subs {
            let meta = store.channel(&cid).ok().flatten();
            if ensure_channel(ctx, &cid, meta).await {
                if let Some(e) = ctx.channels.get(&cid) {
                    e.subs.insert(acct.clone());
                    restored += 1;
                }
            }
        }
    }
    if restored > 0 {
        tracing::info!(subs = restored, "channel subscriptions restored from store on boot");
    }
}

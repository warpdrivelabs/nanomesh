use std::sync::Arc;

use crate::records::hex_encode;
use crate::delivery::try_push;
use crate::{now_ms, Ctx, Sessions, PROTOCOL_VERSION};

use dashmap::{DashMap, DashSet};
use nm_proto::{
    Any, Channel, ChannelBackfillReq, ChannelGram, ChannelList, ChannelLog, ChannelMsg, ChannelOp,
    ChannelPub, Command, Gram, GramKind,
};
use prost::Message;

/// 频道运行态（P4）：iroh-gossip 主题上的开放 pub/sub。每个已订阅频道一个后台任务：
/// 收播 gossip 消息 → 追加日志 + 转发给本地订阅会话；发布经 mpsc 交给该任务广播。
pub(crate) struct ChannelEntry {
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

pub(crate) type Channels = DashMap<Vec<u8>, ChannelEntry>;
const CHANNEL_LOG_CAP: usize = 300; // 每频道内存日志上限（回填近期）

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
        if let Ok(Some(cm)) = tokio::time::timeout(std::time::Duration::from_millis(400), topic.recv()).await {
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

pub(crate) async fn channel_create(ctx: &Ctx, caller: &[u8], rid: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
        Some(op) if op.channel_id.len() == 32 => {
            let meta = Channel {
                channel_id: op.channel_id.clone(),
                name: op.name,
                owner: caller.to_vec(),
                created_at: now_ms() as i64,
                topic: op.topic,
                avatar_url: op.avatar_url,
                home_node: ctx.node_id.to_vec(),
            };
            ensure_channel(ctx, &op.channel_id, Some(meta)).await;
            if let Some(e) = ctx.channels.get(&op.channel_id) {
                e.subs.insert(rid.to_vec());
                let _ = e.pub_tx.send(ChannelOut::Meta(e.meta.lock().unwrap().clone())); // 广播元信息
            }
            (true, None, String::new())
        }
        _ => (false, None, "invalid channel op".into()),
    }
}

pub(crate) async fn channel_sub(ctx: &Ctx, rid: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
        Some(op) if op.channel_id.len() == 32 => {
            let meta = Channel { channel_id: op.channel_id.clone(), name: op.name, topic: op.topic, ..Default::default() };
            if ensure_channel(ctx, &op.channel_id, Some(meta)).await {
                if let Some(e) = ctx.channels.get(&op.channel_id) {
                    e.subs.insert(rid.to_vec());
                }
                (true, None, String::new())
            } else {
                (false, None, "channel join failed".into())
            }
        }
        _ => (false, None, "invalid channel op".into()),
    }
}

pub(crate) fn channel_unsub(ctx: &Ctx, rid: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd.params.as_ref().and_then(|p| ChannelOp::decode(p.value.as_slice()).ok()) {
        Some(op) => {
            if let Some(e) = ctx.channels.get(&op.channel_id) {
                e.subs.remove(rid);
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
                let seq = e.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let m = ChannelMsg {
                    channel_id: pp.channel_id.clone(),
                    sender: caller.to_vec(),
                    seq,
                    ts: now_ms() as i64,
                    body: pp.body,
                };
                let _ = e.pub_tx.send(ChannelOut::Msg(m));
                (true, None, String::new())
            }
            None => (false, None, "not subscribed to channel".into()),
        },
        None => (false, None, "invalid channel pub".into()),
    }
}

pub(crate) fn channel_backfill(ctx: &Ctx, cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd
        .params
        .as_ref()
        .and_then(|p| ChannelBackfillReq::decode(p.value.as_slice()).ok())
    {
        Some(req) => match ctx.channels.get(&req.channel_id) {
            Some(e) => {
                let msgs: Vec<ChannelMsg> = e
                    .log
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|x| x.seq > req.since_seq)
                    .cloned()
                    .collect();
                (
                    true,
                    Some(Any {
                        type_url: "nmspace.v1.ChannelLog".to_string(),
                        value: ChannelLog { msgs }.encode_to_vec(),
                    }),
                    String::new(),
                )
            }
            None => (false, None, "unknown channel".into()),
        },
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
    }
}

pub(crate) fn channel_list(ctx: &Ctx) -> (bool, Option<Any>, String) {
    let channels: Vec<Channel> = ctx.channels.iter().map(|e| e.meta.lock().unwrap().clone()).collect();
    (
        true,
        Some(Any { type_url: "nmspace.v1.ChannelList".to_string(), value: ChannelList { channels }.encode_to_vec() }),
        String::new(),
    )
}

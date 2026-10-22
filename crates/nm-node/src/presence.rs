use dashmap::DashMap;
use nm_core::Directory;

use crate::delivery::account_online;
use crate::records::{hex_decode_n, hex_encode};
use crate::{now_secs, Devices, Node, Principals, Sessions};

pub(crate) const PRESENCE_INTERVAL_SECS: u64 = 10;
pub(crate) const PRESENCE_TTL_SECS: u64 = 35;
/// P2-11 按 home 拉取：缓存新鲜窗口（≤此秒数免拨；取略大于客户端 15s 轮询、略小于读 TTL 35s）。
pub(crate) const PRESENCE_PULL_TTL_SECS: u64 = 20;
/// P2-11：单个 home 拉取的有界超时（ms）——单 home 慢/不可达不拖垮整次查询。
pub(crate) const PRESENCE_PULL_HOME_TIMEOUT_MS: u64 = 2500;
/// P2-11 B：peer「近期活跃」窗口（秒）——自动退火只看此窗口内仍在广播的 peer，离线 peer 不阻塞退火。
pub(crate) const PRESENCE_PEER_ACTIVE_SECS: u64 = 180;

/// In-memory presence cache entry. Expired entries read as offline.
#[derive(Clone)]
pub(crate) struct PresenceRec {
    pub(crate) status: String,
    pub(crate) last_seen: u64,
    #[allow(dead_code)] // kept for future presence source/debug routing
    pub(crate) home_node: Vec<u8>,
}

/// P2-11：presence 拉取路径观测（按 home 拉取次数 / 缓存命中 / 失败）。
#[derive(Default)]
pub(crate) struct PresenceStat {
    pub(crate) pulls: std::sync::atomic::AtomicU64,
    pub(crate) cache_hits: std::sync::atomic::AtomicU64,
    pub(crate) errors: std::sync::atomic::AtomicU64,
}

pub(crate) type PresenceMap = DashMap<Vec<u8>, PresenceRec>;
pub(crate) type StatusIntent = DashMap<Vec<u8>, String>;

/// Presence gossip card signed by the home node.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct PresenceAnnounce {
    pub(crate) entity: String,
    pub(crate) status: String,
    pub(crate) ts: u64,
    pub(crate) home_node: String,
    pub(crate) sig: String,
}

/// 计算实体对外状态：本地在线会话优先（权威，取状态意图或 online）；否则查 presence 缓存并按 TTL 判离线。
pub(crate) fn presence_status(
    sessions: &Sessions,
    principals: &Principals,
    devices: &Devices,
    presence: &PresenceMap,
    intent: &StatusIntent,
    entity: &[u8],
) -> String {
    if account_online(sessions, principals, devices, entity) {
        return intent.get(entity).map(|s| s.clone()).unwrap_or_else(|| "online".to_string());
    }
    if let Some(r) = presence.get(entity) {
        if now_secs().saturating_sub(r.last_seen) <= PRESENCE_TTL_SECS {
            return r.status.clone();
        }
    }
    "offline".to_string()
}

impl Node {
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
        if account_online(&self.sessions, &self.principals, &self.devices, entity) {
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
        presence_status(&self.sessions, &self.principals, &self.devices, &self.presence, &self.status_intent, entity)
    }

    /// P2-11：presence 退火管开关（热）。开 → spawn_presence 退出 overlay、presence 全走按 home 拉取。
    pub fn set_presence_scoped(&self, on: bool) {
        self.presence_scoped.store(on, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn presence_scoped_on(&self) -> bool {
        self.presence_scoped.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// P2-11 B：本节点此刻是否应退 presence overlay = 强制开关 || 自动退火。
    /// 自动退火：存在 ≥1 个「近期活跃」peer 且其全部广告支持 scoped（无老消费者依赖火管）。
    /// 近期活跃过滤让离线的手工/种子 peer（last_seen 陈旧）不阻塞退火；出现活跃的老 peer 即回退(自愈)。
    pub fn presence_retired(&self) -> bool {
        let forced = self.presence_scoped.load(std::sync::atomic::Ordering::Relaxed);
        let peers: Vec<(Vec<u8>, u64)> =
            self.peer_info.iter().map(|e| (e.key().clone(), e.value().last_seen)).collect();
        compute_presence_retired(forced, now_secs(), &peers, &self.presence_scoped_nodes, PRESENCE_PEER_ACTIVE_SECS)
    }
    /// 已广告支持 scoped presence 的 peer 数（观测）。
    pub fn presence_scoped_peer_count(&self) -> usize {
        self.presence_scoped_nodes.len()
    }
    /// presence 拉取观测快照：(pulls, cache_hits, errors)。
    pub fn presence_stat_snapshot(&self) -> (u64, u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.presence_stat.pulls.load(Relaxed),
            self.presence_stat.cache_hits.load(Relaxed),
            self.presence_stat.errors.load(Relaxed),
        )
    }

    /// 处理收到的 presence 广播：验签(按 home_node) → 反陈旧 → LWW 入缓存；不覆盖本地在线。
    fn on_presence_msg(&self, content: &[u8]) {
        let ann: PresenceAnnounce = match serde_json::from_slice(content) {
            Ok(a) => a,
            Err(_) => { tracing::debug!("PRESDBG recv: json decode fail"); return; }
        };
        tracing::debug!(entity=%ann.entity.chars().take(12).collect::<String>(), home=%ann.home_node.chars().take(12).collect::<String>(), status=%ann.status, ts=ann.ts, "PRESDBG recv card");
        let (home, entity) = match (hex_decode_n::<32>(&ann.home_node), hex_decode_n::<32>(&ann.entity)) {
            (Some(h), Some(e)) => (h, e),
            _ => { tracing::debug!("PRESDBG drop: hex decode fail"); return; }
        };
        if self.blacklist.contains(&entity) || self.blacklist.contains(&home) {
            tracing::debug!("PRESDBG drop: blacklist");
            return;
        }
        let sig = match hex_decode_n::<64>(&ann.sig) {
            Some(s) => s,
            None => { tracing::debug!("PRESDBG drop: sig hex fail"); return; }
        };
        let signing = Self::presence_signing_bytes(&home, &entity, ann.ts, &ann.status);
        if nm_crypto::verify_bytes(&home, &signing, &sig).is_err() {
            tracing::debug!("PRESDBG drop: SIG VERIFY FAIL");
            return;
        }
        let now = now_secs();
        if ann.ts + 300 < now || ann.ts > now + 300 {
            tracing::debug!(ts=ann.ts, now=now, "PRESDBG drop: stale/future");
            return; // 反陈旧/未来
        }
        if account_online(&self.sessions, &self.principals, &self.devices, &entity) {
            tracing::debug!("PRESDBG drop: account_online local override");
            return; // 本地在线会话权威，不被远端覆盖
        }
        if let Some(cur) = self.presence.get(&entity[..]) {
            if cur.last_seen >= ann.ts {
                tracing::debug!("PRESDBG drop: LWW older");
                return; // LWW：仅接受更新的
            }
        }
        tracing::debug!(entity=%ann.entity.chars().take(12).collect::<String>(), "PRESDBG ACCEPT card");
        self.presence.insert(
            entity.to_vec(),
            PresenceRec { status: ann.status, last_seen: ann.ts, home_node: home.to_vec() },
        );
    }

    /// 后台 presence：presence_scoped=关 时维持联邦 overlay（广播+收播，迁移期双写兜底）；
    /// 开则退出 overlay（不 join/发/收），presence 全走 presence.query 按 home 拉取。开关热生效。
    pub fn spawn_presence(self: std::sync::Arc<Self>, federation: String) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let channel = Self::presence_channel(&federation);
            let interval = std::time::Duration::from_secs(PRESENCE_INTERVAL_SECS);
            let mut topic: Option<nm_gossip::ChannelTopic> = None;
            let mut last_bcast = tokio::time::Instant::now() - interval;
            // 换网/休眠唤醒自愈：0 邻居时 nudge iroh 重探测的冷却计时（避免每秒狂触发）。
            let mut last_net_nudge: Option<tokio::time::Instant> = None;
            // 换网 epoch：变化即硬重 join(不依赖 neighbor_count==0),恢复 presence 跨节点广播/收播。
            let mut seen_epoch = self.net_epoch();
            loop {
                // P2-11 退火（热，含 Phase B 自动）：应退火 → 确保离开 overlay，空转等待（可被关回/关闭唤醒）。
                if self.presence_retired() {
                    if topic.take().is_some() {
                        tracing::info!(%federation, "presence overlay 退火(presence_scoped=on)；改走按 home 拉取");
                    }
                    tokio::select! {
                        biased;
                        _ = self.shutdown.cancelled() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => continue,
                    }
                }
                // 未退火：确保已 join（首次 / 换网后重建 / 被关回）。
                if topic.is_none() {
                    match self.join_channel(channel, self.peers_list()).await {
                        Ok(t) => {
                            topic = Some(t);
                            seen_epoch = self.net_epoch();
                            last_bcast = tokio::time::Instant::now() - interval;
                            tracing::info!(%federation, "presence gossip joined");
                        }
                        Err(_) => {
                            tokio::select! {
                                biased;
                                _ = self.shutdown.cancelled() => break,
                                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                            }
                            continue;
                        }
                    }
                }
                let ep = self.net_epoch();
                if ep != seen_epoch {
                    seen_epoch = ep;
                    tracing::info!("presence 换网(epoch 变) → 强制重 join gossip");
                    self.notify_network_change().await;
                    if let Ok(t) = self.join_channel(channel, self.peers_list()).await {
                        topic = Some(t);
                    }
                    last_bcast = tokio::time::Instant::now() - interval; // 立即重播本地在线态
                }
                if last_bcast.elapsed() >= interval {
                    let home = self.ep.id_bytes();
                    let ts = now_secs();
                    // P1 内存上界：清理早已过期的 presence 条目——此表仅读时判 TTL、insert 从不删，
                    // 否则每个见过的实体（含离线/已消失）永久驻留 → 无界增长。用 TTL 的 10 倍作宽松
                    // 阈值（远超刷新周期，不误删仍活跃者），超期即删。本地在线会话下一轮广播会重新 insert。
                    let stale_before = ts.saturating_sub(PRESENCE_TTL_SECS * 10);
                    self.presence.retain(|_, r| r.last_seen >= stale_before);
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
                        if let Some(t) = topic.as_ref() {
                            let _ = t.publish(serde_json::to_vec(&ann).unwrap_or_default()).await;
                        }
                    }
                    last_bcast = tokio::time::Instant::now();
                }
                // 收播他人状态（1s 超时）：借用在块内结束，match 臂可重置 topic。
                let recv = {
                    let t = match topic.as_mut() {
                        Some(t) => t,
                        None => continue,
                    };
                    tokio::select! {
                        biased;
                        _ = self.shutdown.cancelled() => break,
                        r = tokio::time::timeout(std::time::Duration::from_secs(1), t.recv()) => r,
                    }
                };
                match recv {
                    Ok(Some(msg)) => self.on_presence_msg(&msg.content),
                    Ok(None) => match self.join_channel(channel, self.peers_list()).await {
                        Ok(t) => topic = Some(t),
                        Err(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
                    },
                    // 超时(1s 无消息)：若悄悄掉到 0 邻居(连接 churn 但流未结束,直连/换网易触发)
                    // → 重新注入 bootstrap 自愈。否则 presence 无 outbox/重传冗余会永久失联(对端看不到在线态)。
                    Err(_) => {
                        if topic.as_ref().map(|t| t.neighbor_count() == 0).unwrap_or(false) {
                            // 同 IP 唤醒不触发换网回调 → iroh 可能还抱着死的 relay 连接。
                            // 带冷却地 nudge 它重探测/重 home relay,让 rejoin 真能连上。
                            let now = tokio::time::Instant::now();
                            if last_net_nudge.is_none_or(|t| now.duration_since(t).as_secs() >= 30) {
                                self.notify_network_change().await;
                                last_net_nudge = Some(now);
                            }
                            if let Some(t) = topic.as_mut() {
                                let _ = t.rejoin(self.peers_list()).await;
                            }
                        }
                    }
                }
            }
        })
    }
}

/// P2-11：解析 presence.query 参数 `{ids:[hex], local_only?:bool}`。
pub(crate) fn parse_presence_params(params: Option<&nm_proto::Any>) -> (Vec<String>, bool) {
    let v = params
        .and_then(|p| serde_json::from_slice::<serde_json::Value>(&p.value).ok())
        .unwrap_or(serde_json::Value::Null);
    let ids: Vec<String> = v
        .get("ids")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|e| e.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let local_only = v.get("local_only").and_then(|x| x.as_bool()).unwrap_or(false);
    (ids, local_only)
}

/// P2-11 presence.query 处理（取代旧「只读本地缓存」）。
/// `local_only=true`（节点→home 内部拉取）：纯本地权威读，绝不再向外拉（防递归）。
/// 否则经 `presence_resolve` 按 home 分流拉取。返回形状不变 `{hex:status}`，客户端零改动。
pub(crate) async fn presence_query_handle(
    ctx: &crate::Ctx,
    cmd: &nm_proto::Command,
) -> (bool, Option<nm_proto::Any>, String) {
    let (ids, local_only) = parse_presence_params(cmd.params.as_ref());
    let mut out = serde_json::Map::new();

    // 节点→home 内部调用：纯本地权威读，绝不再向外拉（防递归）。
    if local_only {
        for id_hex in ids.into_iter().take(1024) {
            if let Some(id) = hex_decode_n::<32>(&id_hex) {
                let st = presence_status(&ctx.sessions, &ctx.principals, &ctx.devices, &ctx.presence, &ctx.status_intent, &id);
                out.insert(id_hex, serde_json::Value::String(st));
            }
        }
        return ok_json(out);
    }

    // 客户端查询：按 home 分流拉取（presence_resolve），保留客户端发来的 hex 原样作输出键（客户端据此回查）。
    let decoded: Vec<(String, Vec<u8>)> = ids
        .into_iter()
        .take(1024)
        .filter_map(|h| hex_decode_n::<32>(&h).map(|id| (h, id.to_vec())))
        .collect();
    let resolved = presence_resolve(ctx, decoded.iter().map(|(_, id)| id.clone()).collect()).await;
    for (hx, id) in decoded {
        let st = resolved.get(&id).cloned().unwrap_or_else(|| "offline".to_string());
        out.insert(hx, serde_json::Value::String(st));
    }
    ok_json(out)
}

/// P2-11：把一批账号 id 解析为 `{id→status}`。本地在线账号本地权威；其余按 home 分组**并发**拉取
/// （缓存新鲜免拨、回填缓存；无 addr/失败回落缓存/离线）。供 presence.query 与 directory.query 复用。
/// 并发（JoinSet）使整体时延 ≈ 最慢 home 的有界超时，而非各 home 之和（directory.query 可能跨多 home）。
pub(crate) async fn presence_resolve(
    ctx: &crate::Ctx,
    ids: Vec<Vec<u8>>,
) -> std::collections::HashMap<Vec<u8>, String> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut out: std::collections::HashMap<Vec<u8>, String> = std::collections::HashMap::new();
    let mut by_home: std::collections::HashMap<Vec<u8>, Vec<Vec<u8>>> = std::collections::HashMap::new();
    for id in ids {
        // 本地在线账号 → 本地权威即答（含老节点：对自身本地账号始终权威，灰度安全根）。
        if account_online(&ctx.sessions, &ctx.principals, &ctx.devices, &id) {
            let st = presence_status(&ctx.sessions, &ctx.principals, &ctx.devices, &ctx.presence, &ctx.status_intent, &id);
            out.insert(id, st);
            continue;
        }
        match resolve_home(ctx, &id).await {
            Some(home) if home.as_slice() != ctx.node_id.as_slice() => {
                by_home.entry(home).or_default().push(id);
            }
            // home 未知或就是本节点（账号离线）→ 本地缓存/离线兜底。
            _ => {
                let st = presence_status(&ctx.sessions, &ctx.principals, &ctx.devices, &ctx.presence, &ctx.status_intent, &id);
                out.insert(id, st);
            }
        }
    }

    let now = now_secs();
    type PresencePullResult = (
        Vec<u8>,
        Vec<Vec<u8>>,
        Option<std::collections::HashMap<String, String>>,
    );
    let mut js: tokio::task::JoinSet<PresencePullResult> = tokio::task::JoinSet::new();
    for (home, ids_for_home) in by_home {
        // 全部缓存新鲜（≤PULL_TTL）→ 免拨（火管在时缓存常热 → 几乎不拨）。
        let all_fresh = ids_for_home.iter().all(|id| {
            ctx.presence.get(&id[..]).map(|r| now.saturating_sub(r.last_seen) <= PRESENCE_PULL_TTL_SECS).unwrap_or(false)
        });
        if all_fresh {
            ctx.presence_stat.cache_hits.fetch_add(1, Relaxed);
            for id in ids_for_home {
                let st = presence_status(&ctx.sessions, &ctx.principals, &ctx.devices, &ctx.presence, &ctx.status_intent, &id);
                out.insert(id, st);
            }
            continue;
        }
        // 向 home 并发拉取（各自有界超时）；无 addr/失败 → 回落本地缓存/离线。
        match ctx.peers.get(&home).map(|a| a.clone()) {
            Some(addr) => {
                ctx.presence_stat.pulls.fetch_add(1, Relaxed);
                let ep = ctx.ep.clone();
                let ids_hex: Vec<String> = ids_for_home.iter().map(|id| hex_encode(id)).collect();
                js.spawn(async move {
                    let r = tokio::time::timeout(
                        std::time::Duration::from_millis(PRESENCE_PULL_HOME_TIMEOUT_MS),
                        crate::s2s::s2s_presence_query(&ep, addr, &ids_hex),
                    )
                    .await;
                    (home, ids_for_home, match r { Ok(Ok(m)) => Some(m), _ => None })
                });
            }
            None => {
                for id in ids_for_home {
                    let st = presence_status(&ctx.sessions, &ctx.principals, &ctx.devices, &ctx.presence, &ctx.status_intent, &id);
                    out.insert(id, st);
                }
            }
        }
    }
    while let Some(joined) = js.join_next().await {
        let (home, ids_for_home, map) = match joined {
            Ok(t) => t,
            Err(_) => continue,
        };
        match map {
            Some(m) => {
                for id in ids_for_home {
                    let st = m.get(&hex_encode(&id)).cloned().unwrap_or_else(|| "offline".to_string());
                    // 回填缓存：TTL 内复查免再拨；复用 presence_status/TTL/retain。
                    ctx.presence.insert(id.clone(), PresenceRec { status: st.clone(), last_seen: now, home_node: home.clone() });
                    out.insert(id, st);
                }
            }
            None => {
                ctx.presence_stat.errors.fetch_add(1, Relaxed);
                for id in ids_for_home {
                    let st = presence_status(&ctx.sessions, &ctx.principals, &ctx.devices, &ctx.presence, &ctx.status_intent, &id);
                    out.insert(id, st);
                }
            }
        }
    }
    out
}

fn ok_json(out: serde_json::Map<String, serde_json::Value>) -> (bool, Option<nm_proto::Any>, String) {
    let body = serde_json::Value::Object(out).to_string();
    (true, Some(nm_proto::Any { type_url: "text/plain".into(), value: body.into_bytes() }), String::new())
}

/// 解析账号 home 节点：directory 实体 home_node 为主，presence 缓存 home 为辅。
async fn resolve_home(ctx: &crate::Ctx, id: &[u8]) -> Option<Vec<u8>> {
    if let Ok(Some(e)) = ctx.dir.get(id).await {
        if !e.home_node.is_empty() {
            return Some(e.home_node);
        }
    }
    ctx.presence.get(id).map(|r| r.home_node.clone()).filter(|h| !h.is_empty())
}

/// P2-11 B 纯判定（presence_retired 的内核，便于单测）：强制位 || 自动退火。
/// 自动退火 = 存在 ≥1 个近期活跃 peer（now-last_seen ≤ active_window）且其**全部** ∈ capable；
/// 离线 peer（超窗）不计入——不阻塞退火；无任何活跃 peer → 不退（安全默认，维持火管）。
pub(crate) fn compute_presence_retired(
    forced: bool,
    now: u64,
    peers: &[(Vec<u8>, u64)],
    capable: &dashmap::DashSet<Vec<u8>>,
    active_window: u64,
) -> bool {
    if forced {
        return true;
    }
    let mut any_active = false;
    for (id, last_seen) in peers {
        if now.saturating_sub(*last_seen) > active_window {
            continue;
        }
        any_active = true;
        if !capable.contains(id.as_slice()) {
            return false;
        }
    }
    any_active
}

#[cfg(test)]
mod tests {
    use super::{compute_presence_retired, parse_presence_params};
    use nm_proto::Any;

    fn any(json: &str) -> Any {
        Any { type_url: "text/plain".into(), value: json.as_bytes().to_vec() }
    }

    #[test]
    fn parse_params_local_only_and_ids() {
        // local_only=true（节点→home 内部调用）→ home 只做本地权威读、防递归。
        let (ids, lo) = parse_presence_params(Some(&any(r#"{"ids":["aa","bb"],"local_only":true}"#)));
        assert_eq!(ids, vec!["aa".to_string(), "bb".to_string()]);
        assert!(lo);
        // 缺省（客户端查询）→ local_only=false，走 scatter / 按 home 拉取。
        let (ids2, lo2) = parse_presence_params(Some(&any(r#"{"ids":["cc"]}"#)));
        assert_eq!(ids2, vec!["cc".to_string()]);
        assert!(!lo2);
        // 无 params → 空 ids、非 local_only。
        let (ids3, lo3) = parse_presence_params(None);
        assert!(ids3.is_empty());
        assert!(!lo3);
    }

    #[test]
    fn auto_retire_rules() {
        use dashmap::DashSet;
        let cap: DashSet<Vec<u8>> = DashSet::new();
        cap.insert(vec![1]);
        cap.insert(vec![2]);
        let now = 10_000u64;
        let win = 180u64;
        // 强制退火 → true（无视 peers）。
        assert!(compute_presence_retired(true, now, &[], &cap, win));
        // 无活跃 peer → 不退（安全默认，维持火管）。
        assert!(!compute_presence_retired(false, now, &[], &cap, win));
        // 两个活跃且都支持 → 退。
        assert!(compute_presence_retired(false, now, &[(vec![1], now - 10), (vec![2], now - 20)], &cap, win));
        // 有活跃但不支持的 peer(3) → 不退（潜在老消费者，维持火管）。
        assert!(!compute_presence_retired(false, now, &[(vec![1], now - 10), (vec![3], now - 5)], &cap, win));
        // 不支持的 peer(3) 已离线(超窗) → 不阻塞；其余活跃且支持 → 退。
        assert!(compute_presence_retired(false, now, &[(vec![1], now - 10), (vec![3], now - 9_999)], &cap, win));
    }
}

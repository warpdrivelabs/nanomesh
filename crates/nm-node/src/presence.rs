use dashmap::DashMap;

use crate::delivery::account_online;
use crate::records::{hex_decode_n, hex_encode};
use crate::{now_secs, Devices, Node, Principals, Sessions};

pub(crate) const PRESENCE_INTERVAL_SECS: u64 = 10;
pub(crate) const PRESENCE_TTL_SECS: u64 = 35;

/// In-memory presence cache entry. Expired entries read as offline.
#[derive(Clone)]
pub(crate) struct PresenceRec {
    pub(crate) status: String,
    pub(crate) last_seen: u64,
    #[allow(dead_code)] // kept for future presence source/debug routing
    pub(crate) home_node: Vec<u8>,
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

    /// 后台 presence：周期广播本地在线会话状态；收播他人状态入缓存（TTL 在读时判定）。镜像 spawn_membership。
    pub fn spawn_presence(self: std::sync::Arc<Self>, federation: String) -> tokio::task::JoinHandle<()> {
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
            // 换网/休眠唤醒自愈：0 邻居时 nudge iroh 重探测的冷却计时（避免每秒狂触发）。
            let mut last_net_nudge: Option<tokio::time::Instant> = None;
            loop {
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
                        let _ = topic.publish(serde_json::to_vec(&ann).unwrap_or_default()).await;
                    }
                    last_bcast = tokio::time::Instant::now();
                }
                tokio::select! {
                    biased;
                    _ = self.shutdown.cancelled() => break,
                    r = tokio::time::timeout(std::time::Duration::from_secs(1), topic.recv()) => match r {
                        Ok(Some(msg)) => self.on_presence_msg(&msg.content),
                        Ok(None) => match self.join_channel(channel, self.peers_list()).await {
                            Ok(t) => topic = t,
                            Err(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
                        },
                        // 超时(1s 无消息)：若悄悄掉到 0 邻居(连接 churn 但流未结束,直连/换网易触发)
                        // → 重新注入 bootstrap 自愈。否则 presence 无 outbox/重传冗余会永久失联(对端看不到在线态)。
                        Err(_) => {
                            if topic.neighbor_count() == 0 {
                                // 同 IP 唤醒不触发换网回调 → iroh 可能还抱着死的 relay 连接。
                                // 带冷却地 nudge 它重探测/重 home relay,让 rejoin 真能连上。
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
            }
        })
    }
}

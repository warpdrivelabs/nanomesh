//! `nm-federation` — 规模化联邦（去火管）：管理本节点在「每群独立 gossip 主题」上的订阅生命周期，
//! 复用 [`nm_gossip::ChannelHub`]（与频道同款原语），把发布/收播按群路由。
//!
//! **F0（本阶段）**：真实模块骨架 —— 群主题注册表 + `join_group`/`leave_group`/`publish` + 每群一泵任务，附单测。
//! **尚未接入** `nm-node` 的 fanout/deliver 路径（F1 起经 `nmd.toml [federation] per_topic` 灰度切换，
//! 届时群 announce/msg 改走 `nmspace-group:<gid>`、收端 `content_hash` 去重、与火管双写回滚无损）。
//! 本阶段不引 iroh-docs / DHT / MLS（分列 F2–F5）。
//!
//! 设计见 `docs/FEDERATION_SCALING_IMPL.md`。

use std::collections::HashMap;
use std::sync::Arc;

use iroh_gossip::Gossip;
use nm_gossip::{ChannelHub, ChannelSender};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

/// 群标识（EntityId 字节）。本模块只把它当主题键，不解释其语义。
pub type GroupId = Vec<u8>;

/// 群 → gossip 主题键的确定性映射：`blake3("nmspace-group:" ++ gid)`。
/// 任意节点对同一 `gid` 都算出同一主题，从而汇聚到同一叠加网——且**仅成员 join**（非成员收不到）。
pub fn group_topic_key(gid: &[u8]) -> [u8; 32] {
    const PFX: &[u8] = b"nmspace-group:";
    let mut buf = Vec::with_capacity(PFX.len() + gid.len());
    buf.extend_from_slice(PFX);
    buf.extend_from_slice(gid);
    nm_crypto::content_hash(&buf)
}

/// 从某群主题收到的一条广播，交回宿主（nm-node）按其既有 `on_group_gossip` 解码分发。
#[derive(Debug, Clone)]
pub struct FedMsg {
    /// 来自哪个群主题。
    pub group: GroupId,
    /// 原始载荷字节（上层自解码，如 prost `GroupGossip`）。
    pub content: Vec<u8>,
    /// 直接转发来源节点（PlumTree 邻居；**不一定**是原作者）。
    pub from: [u8; 32],
}

#[derive(Debug, thiserror::Error)]
pub enum FederationError {
    #[error("未加入该群主题")]
    NotJoined,
    #[error("gossip: {0}")]
    Gossip(String),
}

/// 一个已加入群的订阅：发送端 + 后台泵任务（收播 → 事件流）。
struct GroupSub {
    sender: ChannelSender,
    pump: JoinHandle<()>,
}

impl Drop for GroupSub {
    fn drop(&mut self) {
        self.pump.abort(); // 离群/析构即停泵，避免任务泄漏
    }
}

/// 联邦协调器：持有本节点所在群的订阅集（每群一主题 + 一泵任务），统一路由发布/收播。
///
/// 所有已加入群主题收到的广播都汇到 [`Federation::new`] 返回的单一事件流，由宿主消费。
pub struct Federation {
    hub: ChannelHub,
    node_id: [u8; 32],
    events: mpsc::UnboundedSender<FedMsg>,
    subs: Mutex<HashMap<GroupId, GroupSub>>,
}

impl Federation {
    /// 基于一个 [`iroh_gossip::Gossip`]（与节点共用同一 endpoint）构造。
    /// 返回 `(句柄, 事件流)`。
    pub fn new(gossip: Gossip, node_id: [u8; 32]) -> (Arc<Self>, mpsc::UnboundedReceiver<FedMsg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let fed = Arc::new(Self {
            hub: ChannelHub::new(gossip),
            node_id,
            events: tx,
            subs: Mutex::new(HashMap::new()),
        });
        (fed, rx)
    }

    /// 本节点身份（F1 起用作 `GroupGossip.origin` 去自环）。
    pub fn node_id(&self) -> [u8; 32] {
        self.node_id
    }

    /// 加入某群主题并接入其叠加网（**幂等**）。
    ///
    /// `bootstrap` 为已知对端节点公钥（本阶段 = 群 `home_node` ∪ 已知成员 home；F2 起 DHT 兜底）。
    /// N0/selfhost 靠 discovery 自动把公钥解析成地址；LAN/无发现时宿主须先 `add_peer_addr` 播种地址。
    pub async fn join_group(&self, gid: &[u8], bootstrap: Vec<[u8; 32]>) -> Result<(), FederationError> {
        let mut subs = self.subs.lock().await;
        if subs.contains_key(gid) {
            return Ok(()); // 已加入：幂等
        }
        let heal_boot = bootstrap.clone();
        let topic = self
            .hub
            .join(group_topic_key(gid), bootstrap)
            .await
            .map_err(|e| FederationError::Gossip(e.to_string()))?;
        let (sender, mut rx) = topic.into_split();
        let events = self.events.clone();
        let gid_v = gid.to_vec();
        let heal_sender = sender.clone();
        let pump = tokio::spawn(async move {
            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
                    Ok(Some(msg)) => {
                        let fed = FedMsg { group: gid_v.clone(), content: msg.content, from: msg.from };
                        if events.send(fed).is_err() {
                            break; // 事件接收端已丢弃 → 收摊
                        }
                    }
                    Ok(None) => break, // 流结束
                    Err(_) => {
                        // 自愈引导：无邻居且有 bootstrap 时重新注入对端，直到叠加网成型。
                        if !heal_boot.is_empty() && rx.neighbor_count() == 0 {
                            let _ = heal_sender.join_peers(heal_boot.clone()).await;
                        }
                    }
                }
            }
        });
        subs.insert(gid.to_vec(), GroupSub { sender, pump });
        Ok(())
    }

    /// 离开某群主题：停泵并移除（幂等）。
    pub async fn leave_group(&self, gid: &[u8]) {
        self.subs.lock().await.remove(gid); // GroupSub::drop 停泵
    }

    /// 向某群主题广播字节（须先 [`Federation::join_group`]，否则 [`FederationError::NotJoined`]）。
    pub async fn publish(&self, gid: &[u8], payload: Vec<u8>) -> Result<(), FederationError> {
        let sender = {
            let subs = self.subs.lock().await;
            subs.get(gid).map(|s| s.sender.clone())
        };
        match sender {
            Some(s) => s
                .publish(payload)
                .await
                .map_err(|e| FederationError::Gossip(e.to_string())),
            None => Err(FederationError::NotJoined),
        }
    }

    /// 当前已加入的群集合。
    pub async fn joined(&self) -> Vec<GroupId> {
        self.subs.lock().await.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_key_is_deterministic_and_distinct() {
        let a1 = group_topic_key(b"group-A");
        let a2 = group_topic_key(b"group-A");
        let b = group_topic_key(b"group-B");
        assert_eq!(a1, a2, "同 gid 必须算出同一主题");
        assert_ne!(a1, b, "不同 gid 应得到不同主题");
        assert_eq!(a1.len(), 32);
    }
}

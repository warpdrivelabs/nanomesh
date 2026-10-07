//! `nm-federation` — 规模化联邦（去火管）：管理本节点在「每群/每收件人独立 gossip 主题」上的订阅
//! 生命周期，复用 [`nm_gossip::ChannelHub`]（与频道同款原语），把发布/收播按主题路由。
//!
//! 主题族：
//! - **群**：`nmspace-group:<gid>`（F1）——仅成员节点 join。
//! - **收件人收件箱**：`nmspace-inbox:<account_pub>`（F3）——仅收件人 home 节点 join，私聊单播经此投递，
//!   取代 NAT 下常不通的 s2s 直投 / 火管 `Direct` 全广播。
//!
//! 所有已加入主题收到的广播都汇到 [`Federation::new`] 返回的单一事件流，由宿主（nm-node）按其既有
//! `on_group_gossip` 解码分发。设计见 `docs/FEDERATION_SCALING_IMPL.md`。

use std::collections::HashMap;
use std::sync::Arc;

use iroh_gossip::Gossip;
use nm_gossip::{ChannelHub, ChannelSender};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

/// 主题标签（群 gid 或收件人 account 的字节）。本模块只把它当路由标签，不解释其语义。
pub type GroupId = Vec<u8>;

/// 群 → gossip 主题键：`blake3("nmspace-group:" ++ gid)`。
/// 任意节点对同一 `gid` 都算出同一主题，从而汇聚到同一叠加网——且**仅成员 join**（非成员收不到）。
pub fn group_topic_key(gid: &[u8]) -> [u8; 32] {
    topic_key(b"nmspace-group:", gid)
}

/// 收件人收件箱 → gossip 主题键：`blake3("nmspace-inbox:" ++ account_pub)`（F3）。
/// 仅收件人 home 节点订阅 → 私聊单播只投到该节点（非收件人节点收不到），取代火管 `Direct` 全广播。
pub fn inbox_topic_key(account: &[u8]) -> [u8; 32] {
    topic_key(b"nmspace-inbox:", account)
}

fn topic_key(prefix: &[u8], id: &[u8]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(prefix.len() + id.len());
    buf.extend_from_slice(prefix);
    buf.extend_from_slice(id);
    nm_crypto::content_hash(&buf)
}

/// 从某主题收到的一条广播，交回宿主（nm-node）按其既有 `on_group_gossip` 解码分发。
#[derive(Debug, Clone)]
pub struct FedMsg {
    /// 来自哪个主题的标签（群 gid 或收件人 account）。
    pub group: GroupId,
    /// 原始载荷字节（上层自解码，如 prost `GroupGossip`）。
    pub content: Vec<u8>,
    /// 直接转发来源节点（PlumTree 邻居；**不一定**是原作者）。
    pub from: [u8; 32],
}

#[derive(Debug, thiserror::Error)]
pub enum FederationError {
    #[error("未加入该主题")]
    NotJoined,
    #[error("gossip: {0}")]
    Gossip(String),
}

/// 一个已加入主题的订阅：发送端 + 后台泵任务（收播 → 事件流）+ 路由标签。
struct Sub {
    sender: ChannelSender,
    pump: JoinHandle<()>,
    label: GroupId,
}

impl Drop for Sub {
    fn drop(&mut self) {
        self.pump.abort(); // 离群/析构即停泵，避免任务泄漏
    }
}

/// 联邦协调器：持有本节点所在主题的订阅集（每主题一 overlay + 一泵任务），统一路由发布/收播。
/// `subs` 按**主题键**（32 字节 blake3）索引——群与收件箱即便标签字节巧合也因前缀不同而互不干扰。
pub struct Federation {
    hub: ChannelHub,
    node_id: [u8; 32],
    events: mpsc::UnboundedSender<FedMsg>,
    subs: Mutex<HashMap<[u8; 32], Sub>>,
}

impl Federation {
    /// 基于一个 [`iroh_gossip::Gossip`]（与节点共用同一 endpoint）构造。返回 `(句柄, 事件流)`。
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

    /// 本节点身份（用作 `GroupGossip.origin` 去自环）。
    pub fn node_id(&self) -> [u8; 32] {
        self.node_id
    }

    /// 加入某群主题并接入其叠加网（**幂等**）。见 [`Federation::join_topic`]。
    pub async fn join_group(&self, gid: &[u8], bootstrap: Vec<[u8; 32]>) -> Result<(), FederationError> {
        self.join_topic(group_topic_key(gid), gid.to_vec(), bootstrap).await
    }

    /// F3：加入某收件人收件箱主题（**幂等**）。收件人 home 节点订阅后，私聊单播经此主题投达。
    pub async fn join_inbox(&self, account: &[u8], bootstrap: Vec<[u8; 32]>) -> Result<(), FederationError> {
        self.join_topic(inbox_topic_key(account), account.to_vec(), bootstrap).await
    }

    /// 加入某主题并接入其叠加网（**幂等**）。`label` 随每条收播装入 [`FedMsg::group`] 供宿主路由。
    ///
    /// `bootstrap` 为已知对端节点公钥（群= `home_node` ∪ 已知成员 home；收件箱= 收件人 home；F2 起 DHT 兜底）。
    /// N0/selfhost 靠 discovery 自动把公钥解析成地址；LAN/无发现时宿主须先 `add_peer_addr` 播种地址。
    async fn join_topic(
        &self,
        key: [u8; 32],
        label: GroupId,
        bootstrap: Vec<[u8; 32]>,
    ) -> Result<(), FederationError> {
        let mut subs = self.subs.lock().await;
        if subs.contains_key(&key) {
            return Ok(()); // 已加入：幂等
        }
        let heal_boot = bootstrap.clone();
        let topic = self
            .hub
            .join(key, bootstrap)
            .await
            .map_err(|e| FederationError::Gossip(e.to_string()))?;
        let (sender, mut rx) = topic.into_split();
        let events = self.events.clone();
        let label_v = label.clone();
        let heal_sender = sender.clone();
        let pump = tokio::spawn(async move {
            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
                    Ok(Some(msg)) => {
                        let fed = FedMsg { group: label_v.clone(), content: msg.content, from: msg.from };
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
        subs.insert(key, Sub { sender, pump, label });
        Ok(())
    }

    /// 离开某群主题：停泵并移除（幂等）。
    pub async fn leave_group(&self, gid: &[u8]) {
        self.subs.lock().await.remove(&group_topic_key(gid)); // Sub::drop 停泵
    }

    /// F3：离开某收件箱主题（幂等）。
    pub async fn leave_inbox(&self, account: &[u8]) {
        self.subs.lock().await.remove(&inbox_topic_key(account));
    }

    /// 向某群主题广播字节（须先 [`Federation::join_group`]，否则 [`FederationError::NotJoined`]）。
    pub async fn publish(&self, gid: &[u8], payload: Vec<u8>) -> Result<(), FederationError> {
        self.publish_topic(group_topic_key(gid), payload).await
    }

    /// F3：向某收件箱主题广播字节（须先 [`Federation::join_inbox`]）。私聊单播投递用。
    pub async fn publish_inbox(&self, account: &[u8], payload: Vec<u8>) -> Result<(), FederationError> {
        self.publish_topic(inbox_topic_key(account), payload).await
    }

    async fn publish_topic(&self, key: [u8; 32], payload: Vec<u8>) -> Result<(), FederationError> {
        let sender = {
            let subs = self.subs.lock().await;
            subs.get(&key).map(|s| s.sender.clone())
        };
        match sender {
            Some(s) => s
                .publish(payload)
                .await
                .map_err(|e| FederationError::Gossip(e.to_string())),
            None => Err(FederationError::NotJoined),
        }
    }

    /// 当前已加入的主题标签集合（群 gid 与收件人 account 混列）。
    pub async fn joined(&self) -> Vec<GroupId> {
        self.subs.lock().await.values().map(|s| s.label.clone()).collect()
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

    #[test]
    fn inbox_topic_distinct_from_group_even_for_same_id() {
        // 同一 32 字节既当 gid 又当 account：前缀不同 → 两个独立主题（不串台）。
        let id = [7u8; 32];
        assert_ne!(
            group_topic_key(&id),
            inbox_topic_key(&id),
            "群主题与收件箱主题即便 id 相同也必须不同"
        );
        assert_eq!(inbox_topic_key(&id), inbox_topic_key(&id), "同 account 必须算出同一收件箱主题");
    }
}

//! `nm-gossip` — 频道发布/订阅（封装 `iroh-gossip`）。
//! 见 `docs/PLAN_B_iroh_decentralized_im.md` §8.4。
//!
//! topic_id = blake3(channel_id)；订阅者 join(topic) 即接入叠加网，publish 后 O(log N) 扩散
//! （iroh-gossip = HyParView 成员协议 + PlumTree 广播协议）。
//!
//! 本层是**与业务无关的原始字节 pub/sub**：`publish(bytes)` / `recv() -> ChannelMsg{from,content}`。
//! 上层（IM 频道、算力网公告/心跳等）自行决定 `content` 的编码（如 prost `Gram`）。

use bytes::Bytes;
use futures::StreamExt;
use nm_proto::ChannelId;
use iroh::EndpointId;
use iroh_gossip::{
    api::{Event, GossipReceiver, GossipSender},
    Gossip, TopicId,
};

/// 频道 → gossip topic 的确定性映射：`topic_id = blake3(channel_id)`。
/// 任意节点对同一 `channel_id` 都会算出同一 topic，从而汇聚到同一叠加网。
pub fn topic_for_channel(channel: ChannelId) -> TopicId {
    TopicId::from_bytes(nm_crypto::content_hash(channel.as_bytes()))
}

#[derive(Debug, thiserror::Error)]
pub enum GossipError {
    #[error("bad peer id (须为 32 字节公钥): {0}")]
    BadPeer(String),
    #[error("gossip: {0}")]
    Gossip(String),
}

/// 收到的一条频道广播。
#[derive(Debug, Clone)]
pub struct ChannelMsg {
    /// 直接转发来源节点（PlumTree 邻居；**不一定**是原始作者）。
    pub from: [u8; 32],
    /// 载荷原始字节（上层自行解码）。
    pub content: Vec<u8>,
}

/// 频道枢纽：对一个 `iroh_gossip::Gossip` 句柄的薄封装，按 channel 订阅/发布。
#[derive(Clone)]
pub struct ChannelHub {
    gossip: Gossip,
}

impl ChannelHub {
    pub fn new(gossip: Gossip) -> Self {
        Self { gossip }
    }

    /// 订阅一个频道并接入其叠加网。`bootstrap` 为已知对端节点公钥（可空=被动等待他人接入）。
    ///
    /// 注意：底层 endpoint 需能把这些公钥解析成地址——N0/selfhost 靠 discovery 自动完成；
    /// LAN/无发现时须先经 `nm_node::Node::add_peer_addr` 把对端地址播种进 endpoint 地址簿。
    pub async fn join(
        &self,
        channel: [u8; 32],
        bootstrap: Vec<[u8; 32]>,
    ) -> Result<ChannelTopic, GossipError> {
        let topic = TopicId::from_bytes(nm_crypto::content_hash(&channel));
        let mut ids = Vec::with_capacity(bootstrap.len());
        for b in &bootstrap {
            ids.push(EndpointId::from_bytes(b).map_err(|e| GossipError::BadPeer(e.to_string()))?);
        }
        let gt = self
            .gossip
            .subscribe(topic, ids)
            .await
            .map_err(|e| GossipError::Gossip(e.to_string()))?;
        let (tx, rx) = gt.split();
        Ok(ChannelTopic { tx, rx })
    }
}

/// 一个已加入的频道：可向全体订阅者广播、可收取广播流。
pub struct ChannelTopic {
    tx: GossipSender,
    rx: GossipReceiver,
}

impl ChannelTopic {
    /// 向频道广播一段字节（经 PlumTree 扩散给全体订阅者）。
    pub async fn publish(&self, payload: impl Into<Bytes>) -> Result<(), GossipError> {
        self.tx
            .broadcast(payload.into())
            .await
            .map_err(|e| GossipError::Gossip(e.to_string()))
    }

    /// 取下一条频道广播；`None` 表示流结束。忽略邻居 up/down 与 Lagged 等非消息事件。
    pub async fn recv(&mut self) -> Option<ChannelMsg> {
        loop {
            match self.rx.next().await {
                Some(Ok(Event::Received(m))) => {
                    return Some(ChannelMsg {
                        from: *m.delivered_from.as_bytes(),
                        content: m.content.to_vec(),
                    })
                }
                Some(Ok(_)) => continue,  // NeighborUp/NeighborDown/Lagged
                Some(Err(_)) => continue, // 单条错误：跳过继续收
                None => return None,
            }
        }
    }
}

impl ChannelTopic {
    /// 拆分为「可克隆的发送端 + 独占的接收端」，使发布与收播可在不同任务并发进行。
    /// （`publish` 为 `&self`、`recv` 为 `&mut self`，bundled 的 `ChannelTopic` 无法同时做两件事。）
    pub fn into_split(self) -> (ChannelSender, ChannelReceiver) {
        let ChannelTopic { tx, rx } = self;
        (ChannelSender(std::sync::Arc::new(tx)), ChannelReceiver(rx))
    }
}

/// 频道发送端：可克隆、`&self` 广播（内部 `Arc<GossipSender>`）。
#[derive(Clone)]
pub struct ChannelSender(std::sync::Arc<GossipSender>);

impl ChannelSender {
    /// 向频道广播一段字节（经 PlumTree 扩散给全体订阅者）。
    pub async fn publish(&self, payload: impl Into<Bytes>) -> Result<(), GossipError> {
        self.0
            .broadcast(payload.into())
            .await
            .map_err(|e| GossipError::Gossip(e.to_string()))
    }
}

/// 频道接收端：独占 `&mut self` 收播。
pub struct ChannelReceiver(GossipReceiver);

impl ChannelReceiver {
    /// 取下一条频道广播；`None` 表示流结束。忽略邻居 up/down 与 Lagged 等非消息事件。
    pub async fn recv(&mut self) -> Option<ChannelMsg> {
        loop {
            match self.0.next().await {
                Some(Ok(Event::Received(m))) => {
                    return Some(ChannelMsg {
                        from: *m.delivered_from.as_bytes(),
                        content: m.content.to_vec(),
                    })
                }
                Some(Ok(_)) => continue,
                Some(Err(_)) => continue,
                None => return None,
            }
        }
    }
}

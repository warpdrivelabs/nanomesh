//! `im-gossip` — 频道发布/订阅（封装 `iroh-gossip`）。
//! 见 `docs/PLAN_B_iroh_decentralized_im.md` §8.4。
//!
//! topic_id = blake3(channel_id)；订阅者 join(topic) 即接入叠加网，publish 后 O(log N) 扩散
//! （iroh-gossip = HyParView 成员协议 + PlumTree 广播协议）。

use im_proto::ChannelId;
use iroh_gossip::TopicId;

/// 频道 → gossip topic 的确定性映射：`topic_id = blake3(channel_id)`。
/// 任意节点对同一 `channel_id` 都会算出同一 topic，从而汇聚到同一叠加网。
pub fn topic_for_channel(channel: ChannelId) -> TopicId {
    TopicId::from_bytes(im_crypto::content_hash(channel.as_bytes()))
}

#[derive(Debug, thiserror::Error)]
pub enum GossipError {
    #[error("gossip not implemented yet")]
    Unimplemented,
}

/// 频道枢纽（骨架）。后续持有 `iroh_gossip::net::Gossip` 并按 topic 订阅/发布。
pub struct ChannelHub {
    _private: (),
}

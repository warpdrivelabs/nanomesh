//! `nm-federation` — 跨节点状态（封装 `iroh-docs`）+ 路由 / 重试 / 防环。
//! 见 `docs/PLAN_B_iroh_decentralized_im.md` §8.5。
//!
//! 关键：无中心 S2S 网关；节点按公钥互联；Relay 信封带 TTL + 经过节点集合以防环。
//! TODO：用 iroh-docs 同步「用户→归属节点 / 群成员 / 频道成员」表。

#[derive(Debug, thiserror::Error)]
pub enum FederationError {
    #[error("federation not implemented yet")]
    Unimplemented,
}

/// 联邦协调器（骨架）。
pub struct Federation {
    _private: (),
}

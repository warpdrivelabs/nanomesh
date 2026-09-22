//! `im-core` — 领域模型与核心 trait（投递 / 存储 / 在线状态）。
//! 见 `docs/PLAN_B_iroh_decentralized_im.md` §7/§8。

pub use im_proto as proto;
use im_proto::{DirectoryQuery, Entity, Gram, GramId, UserId};

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("not found")]
    NotFound,
    #[error(transparent)]
    Proto(#[from] im_proto::ProtoError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = core::result::Result<T, CoreError>;

/// 投递抽象：在线直达 / 离线入队（§8.2）。
/// edition 2024 原生支持 trait 中的 `async fn`（此处用 RPITIT 形式携带 `Send` 约束）。
pub trait Deliver {
    fn deliver(&self, to: UserId, gram: Gram)
        -> impl core::future::Future<Output = Result<()>> + Send;
    fn ack(&self, gram_id: GramId)
        -> impl core::future::Future<Output = Result<()>> + Send;
}

/// 存储抽象：离线队列 / 历史 / 成员表（后端见 `im-store`）。
pub trait Store {
    fn put_outbox(&self, gram: &Gram)
        -> impl core::future::Future<Output = Result<()>> + Send;
    fn take_inbox(&self, user: UserId)
        -> impl core::future::Future<Output = Result<Vec<Gram>>> + Send;
}

/// 实体目录：注册 / 查询（见 docs/PLAN_C §6）。
/// 本地内存实现见 `im-node::MemDirectory`；E4 将用 iroh-docs 做全网最终一致同步。
pub trait Directory {
    fn upsert(&self, entity: Entity) -> impl core::future::Future<Output = Result<()>> + Send;
    fn get(
        &self,
        entity_id: &[u8],
    ) -> impl core::future::Future<Output = Result<Option<Entity>>> + Send;
    fn query(
        &self,
        query: &DirectoryQuery,
    ) -> impl core::future::Future<Output = Result<Vec<Entity>>> + Send;
}

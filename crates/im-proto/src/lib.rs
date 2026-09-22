//! `im-proto` — 线协议：ID、gram 类型（prost/protobuf 生成）、分帧/编解码。
//!
//! wire 类型由 `proto/im.proto` 经 prost 生成（见 `build.rs`）。
//! ID 采用手写的 32 字节领域类型（protobuf 侧为 `bytes`）。
//! 关键原则：**不要**手写 `unsafe repr(C)` 转换（旧工程曾因此出现未对齐/transmute 的 UB）。

use serde::{Deserialize, Serialize};

/// prost 生成的 protobuf 类型。
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/imspace.v1.rs"));
}

/// 线协议消息与类型（由 protobuf 生成）。
pub use pb::{
    Any, Capability, Command, CommandResult, DirectoryQuery, Entity, EntityList, Grant, Gram,
    FedSyncReq, FedSyncResp, GramKind, Group, GroupList, GroupOp, JobSpec,
};

/// 协议主版本；握手时协商，拒绝不兼容主版本。
pub const PROTOCOL_VERSION: u32 = 1;

/// 当前 Unix 毫秒时间戳。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 32 字节标识（公钥指纹 / topic 等）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Id32(pub [u8; 32]);

pub type UserId = Id32;
pub type DeviceId = Id32;
pub type NodeId = Id32;
pub type GroupId = Id32;
pub type ChannelId = Id32;
pub type TopicId = Id32;

impl Id32 {
    /// 从 protobuf 的 `bytes` 字段转换（长度须为 32）。
    pub fn from_slice(b: &[u8]) -> Result<Self, ProtoError> {
        let arr: [u8; 32] = b.try_into().map_err(|_| ProtoError::Short {
            need: 32,
            got: b.len(),
        })?;
        Ok(Self(arr))
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// 单调递增的 gram 序号（去重 / 排序）。protobuf 侧为 `uint64`。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct GramId(pub u64);

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("bytes length mismatch: need {need}, got {got}")]
    Short { need: usize, got: usize },
    #[error("unsupported protocol version: {0}")]
    Version(u32),
    #[error("decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("encode error: {0}")]
    Encode(#[from] prost::EncodeError),
}

impl core::fmt::Debug for Id32 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for b in &self.0[..4] {
            write!(f, "{:02x}", b)?;
        }
        write!(f, "…")
    }
}

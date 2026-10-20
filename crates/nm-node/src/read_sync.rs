//! P1-6 多设备已读状态同步。
//! `read.mark` 把某会话的已读位单调写入持久层，若有推进则扇到账号**其它在线设备**；
//! `read.list` 返回账号全部已读位，供客户端登录后追平离线期间的变化。
//! 后端只做「账号→会话→已读时刻」的单调 KV + 自有设备扇出；`conversation` 的语义由客户端定义
//! （单聊=对端 EntityId，群=group_id），后端视作 32 字节不透明键。扇出用 `kind=StateSync` 的系统
//! gram，客户端据 `payload.type_url` 识别后推进本地未读。安全：`caller` 为已鉴权账号，只能改自己的
//! 已读态、只扇给自己的设备。

use nm_proto::{now_ms, Any, Command, Gram, GramKind, ReadMarker, ReadMarkerList};
use prost::Message;

use crate::delivery::deliver_account;
use crate::Ctx;

/// 已读标记同步 gram 的 payload type_url（客户端据此识别并推进本地未读计数）。
pub(crate) const READ_MARKER_TYPE: &str = "nmspace.v1/read.marker";

/// 构造一条"已读标记"同步 gram：kind=StateSync，sender=本节点，receiver=账号自身，payload=ReadMarker。
fn read_marker_gram(account: &[u8], node_id: &[u8], marker: &ReadMarker) -> Gram {
    let now = now_ms();
    Gram {
        version: nm_proto::PROTOCOL_VERSION,
        kind: GramKind::StateSync as i32,
        gram_id: now,
        ref_gram_id: None,
        sender: node_id.to_vec(),
        receiver: account.to_vec(),
        timestamp_ms: now,
        payload: Some(Any { type_url: READ_MARKER_TYPE.into(), value: marker.encode_to_vec() }),
        crc: Vec::new(),
    }
}

/// `read.mark`：持久化（单调推进）已读位；确有推进才扇到账号其它**在线**设备（跳过本设备）。
/// 离线设备不入库（避免信箱膨胀），由登录时 `read.list` 追平。`caller`=账号，`rid`=本设备连接。
pub(crate) async fn read_mark(ctx: &Ctx, caller: &[u8], rid: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let Some(marker) = cmd.params.as_ref().and_then(|p| ReadMarker::decode(p.value.as_slice()).ok()) else {
        return (false, None, "bad ReadMarker params".to_string());
    };
    if marker.conversation.len() != 32 {
        return (false, None, "conversation must be 32 bytes".to_string());
    }
    let Some(store) = &ctx.store else {
        return (false, None, "no store".to_string());
    };
    let advanced = match store.read_state_set(caller, &marker.conversation, marker.read_up_to_ms) {
        Ok(a) => a,
        Err(e) => return (false, None, format!("persist failed: {e}")),
    };
    if advanced {
        let g = read_marker_gram(caller, &ctx.node_id, &marker);
        deliver_account(caller, &g, ctx, false, Some(rid)).await; // store_offline=false：仅在线扇出
    }
    (true, None, String::new())
}

/// `read.list`：返回账号全部已读位（客户端登录后调用以追平离线期间别的设备产生的已读变化）。
pub(crate) fn read_list(ctx: &Ctx, caller: &[u8]) -> (bool, Option<Any>, String) {
    let Some(store) = &ctx.store else {
        return (false, None, "no store".to_string());
    };
    let markers = match store.read_state_all(caller) {
        Ok(v) => v
            .into_iter()
            .map(|(conversation, read_up_to_ms)| ReadMarker { conversation, read_up_to_ms })
            .collect(),
        Err(e) => return (false, None, format!("read failed: {e}")),
    };
    let value = ReadMarkerList { markers }.encode_to_vec();
    (true, Some(Any { type_url: "nmspace.v1.ReadMarkerList".to_string(), value }), String::new())
}

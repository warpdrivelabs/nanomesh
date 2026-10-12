use std::collections::HashMap;

use nm_entity::{EntityKind, pack_profile};
use nm_proto::{
    Any, Command, CommandResult, DirectoryQuery, Entity, EntityList, Gram, GramKind, now_ms,
};
use nm_transport::{IrohConnection, read_gram, write_gram};
use prost::Message;

use crate::error::{ClientError, err};
use crate::gram::build_gram;

/// 面向节点的同步 RPC（directory.* 等）：请求与响应走同一 bi 流。
pub(crate) async fn rpc_over(
    conn: &IrohConnection,
    sender: [u8; 32],
    method: &str,
    params: Option<Any>,
) -> Result<CommandResult, ClientError> {
    let cmd = Command {
        method: method.to_string(),
        params,
        correlation_id: 1,
        timeout_ms: 10_000,
        grant: None,
    };
    let gram = build_gram(
        GramKind::Command,
        sender,
        Vec::new(), // 空 receiver = 面向节点
        Some(Any {
            type_url: "nmspace.v1.Command".to_string(),
            value: cmd.encode_to_vec(),
        }),
    );
    let resp = route_send(conn, &gram).await?;
    let payload = resp
        .payload
        .ok_or_else(|| ClientError::Other("command result has no payload".into()))?;
    CommandResult::decode(payload.value.as_slice()).map_err(err)
}

/// 在一条新 bi 流上发送一个 gram 并读取对端的单条响应。
pub(crate) async fn route_send(conn: &IrohConnection, gram: &Gram) -> Result<Gram, ClientError> {
    let (mut send, mut recv) = conn.open_bi().await.map_err(err)?;
    write_gram(&mut send, gram).await.map_err(err)?;
    send.finish().map_err(err)?;
    let reply = read_gram(&mut recv).await.map_err(err)?;
    // 节点对"被丢弃的请求"回明确错误 Reply（type_url=nmspace.v1/error），而非静默关流
    // （否则 client 读空流报底层的 "stream finished early"，既误导又无法诊断）。此处把它
    // 转成可读的业务错误，让上层 toast "被节点拒绝: …" 而非令人费解的传输错。
    if reply
        .payload
        .as_ref()
        .is_some_and(|p| p.type_url == "nmspace.v1/error")
    {
        let msg = reply
            .payload
            .as_ref()
            .map(|p| String::from_utf8_lossy(&p.value).into_owned())
            .unwrap_or_else(|| "被节点拒绝".into());
        return Err(ClientError::Other(msg));
    }
    Ok(reply)
}

pub(crate) async fn register_over<K: EntityKind>(
    conn: &IrohConnection,
    my_id: [u8; 32],
    profile: &K::Profile,
    display_name: &str,
    attributes: HashMap<String, String>,
) -> Result<(), ClientError> {
    let entity = Entity {
        entity_id: my_id.to_vec(),
        kind: K::KIND.to_string(),
        display_name: display_name.to_string(),
        attributes,
        capabilities: K::capabilities().iter().map(|c| *c as i32).collect(),
        profile: Some(pack_profile::<K>(profile)),
        updated_at: now_ms() as i64,
        signature: Vec::new(), // TODO(E2+)：用 endpoint 私钥自签名
        home_node: Vec::new(), // 由归属节点在 register 时盖章
    };
    let params = Any {
        type_url: "nmspace.v1.Entity".to_string(),
        value: entity.encode_to_vec(),
    };
    let res = rpc_over(conn, my_id, "directory.register", Some(params)).await?;
    if res.ok {
        Ok(())
    } else {
        Err(ClientError::Other(res.error))
    }
}

pub(crate) async fn query_over(
    conn: &IrohConnection,
    my_id: [u8; 32],
    query: DirectoryQuery,
) -> Result<Vec<Entity>, ClientError> {
    let params = Any {
        type_url: "nmspace.v1.DirectoryQuery".to_string(),
        value: query.encode_to_vec(),
    };
    let res = rpc_over(conn, my_id, "directory.query", Some(params)).await?;
    if !res.ok {
        return Err(ClientError::Other(res.error));
    }
    let payload = res
        .result
        .ok_or_else(|| ClientError::Other("query result has no payload".into()))?;
    let list = EntityList::decode(payload.value.as_slice()).map_err(err)?;
    Ok(list.entities)
}

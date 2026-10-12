use crate::s2s::s2s_blob_get;
use crate::Ctx;

use nm_proto::{Any, BlobData, BlobPut, BlobRef, Command};
use prost::Message;

/// 内容寻址 blob 体量上限（头像等小媒体）；超限拒绝，避免撑爆节点存储/带宽。大媒体应分块（后续）。
const MAX_BLOB: usize = 1024 * 1024; // 1 MiB

pub(crate) fn blob_put(ctx: &Ctx, cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd.params.as_ref().and_then(|p| BlobPut::decode(p.value.as_slice()).ok()) {
        Some(bp) if bp.data.len() > MAX_BLOB => (false, None, "blob too large".to_string()),
        Some(bp) => {
            let hash = nm_crypto::content_hash(&bp.data).to_vec();
            let blob = BlobData { hash: hash.clone(), data: bp.data, mime: bp.mime };
            match &ctx.store {
                Some(s) => match s.put_blob(&blob) {
                    Ok(()) => {
                        tracing::info!(bytes = blob.data.len(), "blob stored (content-addressed)");
                        (
                            true,
                            Some(Any {
                                type_url: "nmspace.v1.BlobRef".to_string(),
                                value: BlobRef { hash, home_node: ctx.node_id.to_vec() }.encode_to_vec(),
                            }),
                            String::new(),
                        )
                    }
                    Err(e) => (false, None, format!("store blob failed: {e}")),
                },
                None => (false, None, "node has no store".to_string()),
            }
        }
        None => (false, None, "invalid blob".to_string()),
    }
}

pub(crate) async fn blob_get(ctx: &Ctx, cmd: &Command) -> (bool, Option<Any>, String) {
    match cmd.params.as_ref().and_then(|p| BlobRef::decode(p.value.as_slice()).ok()) {
        Some(br) => {
            // 本地命中直接返回；未命中且带 home_node（非本节点）则 s2s 回源拉取并顺带缓存。
            let mut blob = ctx.store.as_ref().and_then(|s| s.get_blob(&br.hash).ok().flatten());
            if blob.is_none() && !br.home_node.is_empty() && br.home_node != ctx.node_id.as_slice() {
                if let Some(addr) = ctx.peers.get(&br.home_node).map(|a| a.clone()) {
                    match s2s_blob_get(&ctx.ep, addr, &br.hash).await {
                        Ok(b) => {
                            if let Some(s) = &ctx.store {
                                let _ = s.put_blob(&b);
                            }
                            blob = Some(b);
                        }
                        Err(e) => tracing::warn!("blob relay to home node failed: {e}"),
                    }
                }
            }
            match blob {
                Some(b) => (
                    true,
                    Some(Any { type_url: "nmspace.v1.BlobData".to_string(), value: b.encode_to_vec() }),
                    String::new(),
                ),
                None => (false, None, "blob not found".to_string()),
            }
        }
        None => (false, None, "invalid ref".to_string()),
    }
}

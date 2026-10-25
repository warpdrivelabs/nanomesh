use std::sync::Arc;

use dashmap::DashMap;
use nm_proto::{now_ms, Any, Command, Gram, GramKind, Group, GroupOp, PROTOCOL_VERSION};
use nm_store::RedbStore;
use prost::Message;

use crate::delivery::{is_home_here, online_rids, try_push};
use crate::records::hex_encode;
use crate::{Ctx, GROUP_UPDATED_TYPE};

pub(crate) enum GroupMut {
    Join,
    Leave,
    Add,
    Kick,
    Promote,
    Demote,
    Rename,
    SetMeta,
}

fn persist_group(store: &Option<Arc<RedbStore>>, group: &Group) {
    if let Some(store) = store
        && let Err(err) = store.put_group(group)
    {
        tracing::warn!("persist group failed: {err}");
    }
}

/// 向本节点托管的在线群成员推送群状态变更（名称/头像/成员等），客户端按 type_url 拦截刷新列表。
pub(crate) fn notify_group_updated(ctx: &Ctx, g: &Group) {
    let event = serde_json::json!({
        "groupId": hex_encode(&g.group_id),
        "name": g.name,
        "topic": g.topic,
        "avatar": g.avatar_url,
        "updatedAt": g.updated_at,
    });
    let value = event.to_string().into_bytes();
    let now = now_ms();
    let sessions = ctx.sessions.clone();
    let mut jobs: Vec<(Vec<u8>, Gram)> = Vec::new();
    for member in &g.members {
        if !is_home_here(ctx, member) {
            continue;
        }
        let rids = online_rids(&ctx.sessions, &ctx.principals, &ctx.devices, member);
        if rids.is_empty() {
            continue;
        }
        let gram = Gram {
            version: PROTOCOL_VERSION,
            kind: GramKind::Message as i32,
            gram_id: now,
            ref_gram_id: None,
            sender: ctx.node_id.to_vec(),
            receiver: member.to_vec(),
            timestamp_ms: now,
            payload: Some(Any {
                type_url: GROUP_UPDATED_TYPE.into(),
                value: value.clone(),
            }),
            crc: Vec::new(),
        };
        for rid in rids {
            jobs.push((rid, gram.clone()));
        }
    }
    if jobs.is_empty() {
        return;
    }
    ctx.tracker.spawn(async move {
        for (rid, gram) in jobs {
            try_push(&rid, &gram, &sessions).await;
        }
    });
}

fn is_group_admin(group: &Group, who: &[u8]) -> bool {
    group.owner.as_slice() == who || group.admins.iter().any(|admin| admin.as_slice() == who)
}

/// Create a group with `caller` as owner and first member.
pub(crate) fn group_create(
    cmd: &Command,
    groups: &DashMap<Vec<u8>, Group>,
    store: &Option<Arc<RedbStore>>,
    caller: &[u8],
    node_id: &[u8],
) -> (bool, Option<Any>, String) {
    let Some(op) = cmd.params.as_ref().and_then(|p| GroupOp::decode(p.value.as_slice()).ok()) else {
        return (false, None, "invalid group op".into());
    };
    if op.group_id.len() != 32 {
        return (false, None, "group_id must be 32 bytes".into());
    }
    if groups.contains_key(&op.group_id) {
        return (false, None, "group already exists".into());
    }
    if !op.secret.is_empty() {
        let Ok(sk_bytes) = <[u8; 32]>::try_from(op.secret.as_slice()) else {
            return (false, None, "group secret must be 32 bytes".into());
        };
        let sk = nm_transport::SecretKey::from_bytes(&sk_bytes);
        if sk.public().as_bytes().as_slice() != op.group_id.as_slice() {
            return (false, None, "group secret does not match group_id (pubkey)".into());
        }
        if let Some(store) = store
            && let Err(err) = store.put_group_secret(&op.group_id, &sk_bytes)
        {
            return (false, None, format!("persist group secret failed: {err}"));
        }
    }
    let group = Group {
        group_id: op.group_id.clone(),
        name: op.name.clone(),
        owner: caller.to_vec(),
        members: vec![caller.to_vec()],
        updated_at: now_ms() as i64,
        admins: Vec::new(),
        topic: op.topic.clone(),
        avatar_url: op.avatar_url.clone(),
        home_node: node_id.to_vec(),
        supports_per_topic: false,
        supports_ack_batch: false, // 能力位仅 announce 瞬态广告，非持久群状态（见 encode_announce）
    };
    persist_group(store, &group);
    groups.insert(group.group_id.clone(), group);
    tracing::info!(keyed = !op.secret.is_empty(), "group created");
    (true, None, String::new())
}

/// Dissolve a group. Only the owner may dissolve it.
pub(crate) fn group_dissolve(
    cmd: &Command,
    groups: &DashMap<Vec<u8>, Group>,
    store: &Option<Arc<RedbStore>>,
    caller: &[u8],
) -> (bool, Option<Any>, String) {
    let Some(op) = cmd.params.as_ref().and_then(|p| GroupOp::decode(p.value.as_slice()).ok()) else {
        return (false, None, "invalid group op".into());
    };
    let owner_ok = groups.get(&op.group_id).map(|group| group.owner.as_slice() == caller);
    match owner_ok {
        None => (false, None, "group not found".into()),
        Some(false) => (false, None, "only owner can dissolve".into()),
        Some(true) => {
            groups.remove(&op.group_id);
            if let Some(store) = store {
                let _ = store.del_group(&op.group_id);
            }
            tracing::info!("group dissolved");
            (true, None, String::new())
        }
    }
}

/// Mutate membership, roles, or metadata after owner/admin authorization.
pub(crate) fn group_mutate(
    cmd: &Command,
    groups: &DashMap<Vec<u8>, Group>,
    store: &Option<Arc<RedbStore>>,
    caller: &[u8],
    op_kind: GroupMut,
) -> (bool, Option<Any>, String) {
    let Some(op) = cmd.params.as_ref().and_then(|p| GroupOp::decode(p.value.as_slice()).ok()) else {
        return (false, None, "invalid group op".into());
    };
    let Some(mut entry) = groups.get_mut(&op.group_id) else {
        return (false, None, "group not found".into());
    };
    let caller_owner = entry.owner.as_slice() == caller;
    let caller_admin = is_group_admin(&entry, caller);
    let target_ok = op.target.len() == 32;
    match op_kind {
        GroupMut::Join => {
            if !entry.members.iter().any(|member| member.as_slice() == caller) {
                entry.members.push(caller.to_vec());
            }
        }
        GroupMut::Leave => {
            if caller_owner {
                return (false, None, "群主不能退出，请先转让或解散群".into());
            }
            entry.members.retain(|member| member.as_slice() != caller);
            entry.admins.retain(|admin| admin.as_slice() != caller);
        }
        GroupMut::Add => {
            if !caller_admin {
                return (false, None, "only owner/admin can add members".into());
            }
            if !target_ok {
                return (false, None, "add target required".into());
            }
            if !entry.members.iter().any(|member| member.as_slice() == op.target.as_slice()) {
                entry.members.push(op.target.clone());
            }
        }
        GroupMut::Kick => {
            if !caller_admin {
                return (false, None, "only owner/admin can kick".into());
            }
            if !target_ok {
                return (false, None, "kick target required".into());
            }
            if op.target.as_slice() == entry.owner.as_slice() {
                return (false, None, "cannot kick owner".into());
            }
            let target_admin = entry.admins.iter().any(|admin| admin.as_slice() == op.target.as_slice());
            if target_admin && !caller_owner {
                return (false, None, "only owner can remove an admin".into());
            }
            entry.members.retain(|member| member.as_slice() != op.target.as_slice());
            entry.admins.retain(|admin| admin.as_slice() != op.target.as_slice());
        }
        GroupMut::Promote => {
            if !caller_owner {
                return (false, None, "only owner can promote".into());
            }
            if !target_ok || !entry.members.iter().any(|member| member.as_slice() == op.target.as_slice()) {
                return (false, None, "target is not a member".into());
            }
            if !entry.admins.iter().any(|admin| admin.as_slice() == op.target.as_slice()) {
                entry.admins.push(op.target.clone());
            }
        }
        GroupMut::Demote => {
            if !caller_owner {
                return (false, None, "only owner can demote".into());
            }
            entry.admins.retain(|admin| admin.as_slice() != op.target.as_slice());
        }
        GroupMut::Rename => {
            if !caller_admin {
                return (false, None, "only owner/admin can rename".into());
            }
            entry.name = op.name.clone();
        }
        GroupMut::SetMeta => {
            if !caller_admin {
                return (false, None, "only owner/admin can edit group info".into());
            }
            if !op.name.is_empty() {
                entry.name = op.name.clone();
            }
            entry.topic = op.topic.clone();
            entry.avatar_url = op.avatar_url.clone();
        }
    }
    entry.updated_at = now_ms() as i64;
    let group = entry.clone();
    drop(entry);
    persist_group(store, &group);
    (true, None, String::new())
}

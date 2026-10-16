use serde_json::{Value, json};
use tauri::State;

use crate::{AppState, hex, parse_id, session_of};

/// Roster — read the caller's contact list from home node.
#[tauri::command]
pub(crate) async fn roster_list(
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let session = session_of(&state).await?;
    session
        .roster_list()
        .await
        .map(|v| v.into_iter().map(|m| serde_json::json!(m)).collect())
        .map_err(|e| e.to_string())
}

/// Roster — add or update one contact.
#[tauri::command]
pub(crate) async fn roster_add(
    state: State<'_, AppState>,
    id: String,
    kind: String,
    name: String,
    handle: String,
    remark: String,
) -> Result<serde_json::Value, String> {
    let parsed = parse_id(&id)?;
    let session = session_of(&state).await?;
    session
        .roster_add(parsed, &kind, &name, &handle, &remark)
        .await
        .map_err(|e| e.to_string())
}

/// Roster — remove one contact.
#[tauri::command]
pub(crate) async fn roster_remove(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let parsed = parse_id(&id)?;
    let session = session_of(&state).await?;
    session
        .roster_remove(parsed)
        .await
        .map_err(|e| e.to_string())
}

/// Roster — update name / handle / remark of one contact.
#[tauri::command]
pub(crate) async fn roster_update(
    state: State<'_, AppState>,
    id: String,
    name: Option<String>,
    handle: Option<String>,
    remark: Option<String>,
) -> Result<serde_json::Value, String> {
    let parsed = parse_id(&id)?;
    let session = session_of(&state).await?;
    session
        .roster_update(
            parsed,
            name.as_deref(),
            handle.as_deref(),
            remark.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())
}

/// 节点据此 gossip 广播，其他端按 TTL 判在线。
#[tauri::command]
pub(crate) async fn presence_set(state: State<'_, AppState>, status: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .presence_set(status.trim())
        .await
        .map_err(|e| e.to_string())
}

// ── 群组（P3）：建群 / 成员与角色管理 / 群消息 ──
#[tauri::command]
pub(crate) async fn group_create(
    state: State<'_, AppState>,
    name: String,
) -> Result<String, String> {
    let session = session_of(&state).await?;
    let gid = nm_transport::SecretKey::generate().to_bytes(); // 随机 32 字节群 id
    session
        .group_create(gid, name.trim())
        .await
        .map_err(|e| e.to_string())?;
    Ok(hex(&gid))
}

#[tauri::command]
pub(crate) async fn group_list(state: State<'_, AppState>) -> Result<Vec<Value>, String> {
    let session = session_of(&state).await?;
    let groups = session.group_list().await.map_err(|e| e.to_string())?;
    Ok(groups
        .iter()
        .map(|g| {
            json!({
                "id": hex(&g.group_id),
                "name": g.name,
                "owner": hex(&g.owner),
                "members": g.members.iter().map(|m| hex(m)).collect::<Vec<_>>(),
                "admins": g.admins.iter().map(|a| hex(a)).collect::<Vec<_>>(),
                "topic": g.topic,
                "avatar": g.avatar_url,
                "homeNode": hex(&g.home_node),
            })
        })
        .collect())
}

#[tauri::command]
pub(crate) async fn group_add(
    state: State<'_, AppState>,
    group_id: String,
    target: String,
) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .group_add(parse_id(&group_id)?, parse_id(&target)?)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn group_kick(
    state: State<'_, AppState>,
    group_id: String,
    target: String,
) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .group_kick(parse_id(&group_id)?, parse_id(&target)?)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn group_promote(
    state: State<'_, AppState>,
    group_id: String,
    target: String,
) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .group_promote(parse_id(&group_id)?, parse_id(&target)?)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn group_demote(
    state: State<'_, AppState>,
    group_id: String,
    target: String,
) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .group_demote(parse_id(&group_id)?, parse_id(&target)?)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn group_rename(
    state: State<'_, AppState>,
    group_id: String,
    name: String,
) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .group_rename(parse_id(&group_id)?, name.trim())
        .await
        .map_err(|e| e.to_string())
}

/// owner/admin 设置群信息（名称/简介/头像）。avatar 传 "b3:<hash>" 或空。
#[tauri::command]
pub(crate) async fn group_set_meta(
    state: State<'_, AppState>,
    group_id: String,
    name: String,
    topic: String,
    avatar: String,
) -> Result<(), String> {
    if avatar.len() > 128 * 1024 {
        return Err("头像过大（请用更小的图）".into());
    }
    let session = session_of(&state).await?;
    session
        .group_set_meta(
            parse_id(&group_id)?,
            name.trim(),
            topic.trim(),
            avatar.trim(),
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn group_dissolve(
    state: State<'_, AppState>,
    group_id: String,
) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .group_dissolve(parse_id(&group_id)?)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn group_leave(
    state: State<'_, AppState>,
    group_id: String,
) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .group_leave(parse_id(&group_id)?)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn send_group(
    state: State<'_, AppState>,
    group_id: String,
    text: String,
) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .send_group(parse_id(&group_id)?, &text)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ── 频道 / 主题（P4）──
#[tauri::command]
pub(crate) async fn channel_create(
    state: State<'_, AppState>,
    name: String,
    topic: String,
    avatar: String,
) -> Result<String, String> {
    if avatar.len() > 128 * 1024 {
        return Err("头像过大（请用更小的图）".into());
    }
    let session = session_of(&state).await?;
    let cid = nm_transport::SecretKey::generate().to_bytes(); // 随机 32 字节频道 id
    session
        .channel_create(cid, name.trim(), topic.trim(), avatar.trim())
        .await
        .map_err(|e| e.to_string())?;
    Ok(hex(&cid))
}

#[tauri::command]
pub(crate) async fn channel_list(state: State<'_, AppState>) -> Result<Vec<Value>, String> {
    let session = session_of(&state).await?;
    let chans = session.channel_list().await.map_err(|e| e.to_string())?;
    Ok(chans
        .iter()
        .map(|c| {
            json!({
                "id": hex(&c.channel_id), "name": c.name, "owner": hex(&c.owner), "topic": c.topic,
                "avatar": c.avatar_url, "cover": c.cover_url, "homeNode": hex(&c.home_node),
                "pubPolicy": c.pub_policy, "keyed": c.keyed,
            })
        })
        .collect())
}

/// owner 设置频道信息（名称/简介/头像/封面）→ gossip 广播给订阅者。
#[tauri::command]
pub(crate) async fn channel_set_meta(
    state: State<'_, AppState>,
    channel_id: String,
    name: String,
    topic: String,
    avatar: String,
    cover: Option<String>,
) -> Result<(), String> {
    if avatar.len() > 128 * 1024 {
        return Err("头像过大（请用更小的图）".into());
    }
    let cover = cover.unwrap_or_default();
    if cover.len() > 256 * 1024 {
        return Err("封面过大（请用更小的图）".into());
    }
    let session = session_of(&state).await?;
    session
        .channel_set_meta(
            parse_id(&channel_id)?,
            name.trim(),
            topic.trim(),
            avatar.trim(),
            cover.trim(),
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn channel_sub(
    state: State<'_, AppState>,
    channel_id: String,
    name: String,
) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .channel_sub(parse_id(&channel_id)?, name.trim())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn channel_unsub(
    state: State<'_, AppState>,
    channel_id: String,
) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .channel_unsub(parse_id(&channel_id)?)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn channel_publish(
    state: State<'_, AppState>,
    channel_id: String,
    body: String,
) -> Result<(), String> {
    let session = session_of(&state).await?;
    session
        .channel_publish(parse_id(&channel_id)?, &body)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn channel_backfill(
    state: State<'_, AppState>,
    channel_id: String,
    since_seq: u64,
) -> Result<Vec<Value>, String> {
    let session = session_of(&state).await?;
    let msgs = session
        .channel_backfill(parse_id(&channel_id)?, since_seq)
        .await
        .map_err(|e| e.to_string())?;
    Ok(msgs
        .iter()
        .map(|m| {
            json!({
                "from": hex(&m.sender), "seq": m.seq, "ts": m.ts, "body": m.body,
            })
        })
        .collect())
}

#[tauri::command]
pub(crate) async fn my_id(state: State<'_, AppState>) -> Result<String, String> {
    let g = state.conn.lock().await;
    Ok(hex(&g.as_ref().ok_or("尚未连接")?.my_id))
}

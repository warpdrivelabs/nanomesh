use serde_json::{Value, json};
use tauri::{AppHandle, State};

use crate::{AppState, hex, parse_id, send_with_retry, session_of};

/// 设置本人在线状态（P2）：online / away / busy / dnd。节点据此 gossip 广播，其他端按 TTL 判在线。
#[tauri::command]
pub(crate) async fn presence_set(
    app: AppHandle,
    state: State<'_, AppState>,
    status: String,
) -> Result<(), String> {
    let status = status.trim().to_string();
    send_with_retry(&app, &state, move |s| {
        let status = status.clone();
        async move { s.presence_set(&status).await }
    })
    .await
}

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

/// A：按 id 批量查在线状态（含跨节点好友）。前端刷新时调，盖到所有列表项。
#[tauri::command]
pub(crate) async fn presence_query(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let session = session_of(&state).await?;
    let parsed: Vec<[u8; 32]> = ids.iter().filter_map(|s| parse_id(s).ok()).collect();
    if parsed.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    session
        .presence_query(&parsed)
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
    app: AppHandle,
    state: State<'_, AppState>,
    group_id: String,
    text: String,
) -> Result<u64, String> {
    let gid = parse_id(&group_id)?;
    send_with_retry(&app, &state, move |s| {
        let text = text.clone();
        // 返回 node ack 的 ref_gram_id = 本条群消息 gram_id，供前端标记单勾✓（消息已达 home node）。
        async move { s.send_group(gid, &text).await.map(|ack| ack.ref_gram_id.unwrap_or(0)) }
    })
    .await
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
    // keyed 频道：生成密钥对，channel_id = 公钥，私钥交 home 节点存 CHANNEL_SECRETS 做定序器签名。
    // （修旧版 bug：旧版把私钥标量直接当 id，而 id 会在邀请/订阅里分享 → 私钥泄漏。）
    let sk = nm_transport::SecretKey::generate();
    let cid = *sk.public().as_bytes();
    let secret = sk.to_bytes();
    session
        .channel_create(cid, name.trim(), topic.trim(), avatar.trim(), &secret)
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
                "avatar": c.avatar_url, "homeNode": hex(&c.home_node),
            })
        })
        .collect())
}

/// owner 设置频道信息（名称/简介/头像）→ gossip 广播给订阅者。
#[tauri::command]
pub(crate) async fn channel_set_meta(
    state: State<'_, AppState>,
    channel_id: String,
    name: String,
    topic: String,
    avatar: String,
) -> Result<(), String> {
    if avatar.len() > 128 * 1024 {
        return Err("头像过大（请用更小的图）".into());
    }
    let session = session_of(&state).await?;
    session
        .channel_set_meta(
            parse_id(&channel_id)?,
            name.trim(),
            topic.trim(),
            avatar.trim(),
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
    app: AppHandle,
    state: State<'_, AppState>,
    channel_id: String,
    body: String,
) -> Result<(), String> {
    let cid = parse_id(&channel_id)?;
    send_with_retry(&app, &state, move |s| {
        let body = body.clone();
        async move { s.channel_publish(cid, &body).await }
    })
    .await
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

use tauri::State;

use crate::{AppState, hex, parse_id, session_of};

/// 在 home node 认领本地名 → 返回完整名 local@domain。
#[tauri::command]
pub(crate) async fn name_claim(
    state: State<'_, AppState>,
    local_part: String,
) -> Result<String, String> {
    let session = session_of(&state).await?;
    let rec = session
        .name_claim(local_part.trim())
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("{}@{}", rec.local_part, rec.domain))
}

/// 解析 name → 目标公钥 hex（无则 None）。
#[tauri::command]
pub(crate) async fn name_resolve(
    state: State<'_, AppState>,
    name: String,
) -> Result<Option<String>, String> {
    let session = session_of(&state).await?;
    let rec = session
        .name_resolve(name.trim())
        .await
        .map_err(|e| e.to_string())?;
    Ok(rec.map(|r| hex(&r.client_pubkey)))
}

/// 反向解析 公钥 hex → 规范名（无则 None）。
#[tauri::command]
pub(crate) async fn name_reverse(
    state: State<'_, AppState>,
    pubkey: String,
) -> Result<Option<String>, String> {
    let session = session_of(&state).await?;
    let rec = session
        .name_reverse(parse_id(&pubkey)?)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rec.map(|r| format!("{}@{}", r.local_part, r.domain)))
}

/// 断开当前连接（清空会话，便于切换节点/模式）。
#[tauri::command]
pub(crate) async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    *state.conn.lock().await = None;
    Ok(())
}

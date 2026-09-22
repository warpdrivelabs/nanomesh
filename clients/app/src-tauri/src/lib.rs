//! Tauri 桥接：把界面 IPC 调用转发到进程内的 `im-client`(iroh)。
//! 原生端(desktop / iOS / android)运行本模块；Web 端改走 im-gateway。
//!
//! - `connect(nodeAddr, displayName)`：绑定随机身份客户端 → online 到节点 → 注册为 person，
//!   并起后台循环把收到的消息以 `core://event` 事件推给前端；返回自己的 id(hex)。
//! - `send_to(target, text)` / `directory_query(kindPrefix)` / `my_id()`。

use std::sync::Arc;

use im_client::{Client, Session};
use im_proto::DirectoryQuery;
use serde_json::{json, Value};
use tauri::{async_runtime, AppHandle, Emitter, State};
use tokio::sync::Mutex;

/// 已建立的连接（客户端 + 会话共享句柄）。
struct Conn {
    _client: Client, // 保活 endpoint
    session: Arc<Session>,
    my_id: [u8; 32],
}

#[derive(Default)]
struct AppState {
    conn: Mutex<Option<Conn>>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn parse_id(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    if s.len() != 64 {
        return Err("需要 64 位十六进制的 EntityId".into());
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| "非法十六进制".to_string())?;
    }
    Ok(out)
}

#[tauri::command]
async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    node_addr: String,
    display_name: String,
) -> Result<String, String> {
    let addr = im_transport::addr_from_string(&node_addr).map_err(|e| e.to_string())?;
    let client = Client::bind_local_random().await.map_err(|e| e.to_string())?;
    let mut session = client.online(addr).await.map_err(|e| e.to_string())?;
    session
        .register_as::<im_entity::kinds::Person>(
            &im_proto::pb::PersonProfile::default(),
            &display_name,
            std::collections::HashMap::new(),
        )
        .await
        .map_err(|e| e.to_string())?;
    let my_id = session.id_bytes();

    // 把消息流交给事件循环 → 推送到前端。
    let mut inbox = session.take_inbox().ok_or("inbox 已被占用")?;
    let apph = app.clone();
    async_runtime::spawn(async move {
        while let Some(gram) = inbox.recv().await {
            let body = gram
                .payload
                .as_ref()
                .map(|p| String::from_utf8_lossy(&p.value).to_string())
                .unwrap_or_default();
            let ev = json!({
                "type": "message",
                "msg": {
                    "id": gram.gram_id.to_string(),
                    "from": hex(&gram.sender),
                    "body": body,
                    "ts": gram.timestamp_ms,
                }
            });
            let _ = apph.emit("core://event", ev);
        }
    });

    *state.conn.lock().await = Some(Conn {
        _client: client,
        session: Arc::new(session),
        my_id,
    });
    Ok(hex(&my_id))
}

async fn session_of(state: &State<'_, AppState>) -> Result<Arc<Session>, String> {
    let g = state.conn.lock().await;
    Ok(g.as_ref().ok_or("尚未连接")?.session.clone())
}

#[tauri::command]
async fn send_to(state: State<'_, AppState>, target: String, text: String) -> Result<(), String> {
    let id = parse_id(&target)?;
    let session = session_of(&state).await?;
    session.send_to(id, &text).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn directory_query(state: State<'_, AppState>, kind_prefix: String) -> Result<Vec<Value>, String> {
    let session = session_of(&state).await?;
    let entities = session
        .directory_query(DirectoryQuery { kind_prefix, ..Default::default() })
        .await
        .map_err(|e| e.to_string())?;
    Ok(entities
        .iter()
        .map(|e| json!({ "id": hex(&e.entity_id), "kind": e.kind, "name": e.display_name }))
        .collect())
}

#[tauri::command]
async fn my_id(state: State<'_, AppState>) -> Result<String, String> {
    let g = state.conn.lock().await;
    Ok(hex(&g.as_ref().ok_or("尚未连接")?.my_id))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![connect, send_to, directory_query, my_id])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

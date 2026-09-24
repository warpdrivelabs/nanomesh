//! Tauri 桥接：把界面 IPC 调用转发到进程内的 `nm-client`(iroh)。
//! 原生端(desktop / iOS / android)运行本模块；Web 端改走 nm-gateway。
//!
//! 连接策略 **同网优先、穿透兜底**：
//! - `nat`（默认，N0）/ `selfhost`（自建 relay+dns）：`node` 给完整地址(JSON)时**先试同网直连**
//!   （Minimal，零基础设施、可离线）；同网失败或只给公钥，则**转穿透 NAT**（发现 + 中继，按公钥拨号）。
//! - `lan`：仅 Minimal 直连（按地址，无 NAT 兜底）。
//!
//! 客户端身份持久化在应用数据目录（`nmspace.identity`，32 字节），启动复用（稳定 EntityId）。

use std::sync::Arc;

use nm_client::{Client, Session};
use nm_proto::DirectoryQuery;
use serde_json::{json, Value};
use tauri::{async_runtime, AppHandle, Emitter, Manager, State};
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
        return Err("需要 64 位十六进制的公钥/EntityId".into());
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| "非法十六进制".to_string())?;
    }
    Ok(out)
}

/// 载入/生成并持久化客户端身份种子（应用数据目录下 `nmspace.identity`，32 字节）。
fn load_or_create_seed(app: &AppHandle) -> Result<[u8; 32], String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("nmspace.identity");
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() == 32 {
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&bytes);
            return Ok(seed);
        }
    }
    let seed = nm_transport::SecretKey::generate().to_bytes();
    std::fs::write(&path, seed).map_err(|e| e.to_string())?;
    Ok(seed)
}

/// 完成会话建立：注册为 person、起消息推送循环、存入状态，返回自己的 id(hex)。
async fn finish_session(
    app: &AppHandle,
    state: &State<'_, AppState>,
    client: Client,
    mut session: Session,
    display_name: &str,
) -> Result<String, String> {
    session
        .register_as::<nm_entity::kinds::Person>(
            &nm_proto::pb::PersonProfile::default(),
            display_name.trim(),
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

/// 连接节点：**同网优先、穿透兜底**。
/// - `lan`：仅 Minimal 直连（按地址，无 NAT 兜底）。
/// - `nat`/`selfhost`：**先试同网直连**（Minimal，零基础设施、可离线）——`node` 为完整地址(JSON)时启用；
///   同网失败或只给了公钥，则**转穿透 NAT**（发现服务 + 中继，按节点公钥拨号）。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    mode: String,               // "nat" | "selfhost" | "lan"
    node: String,               // 节点公钥hex(64) 或 NM_NODE_ADDR(JSON)
    display_name: String,
    relay_urls: Vec<String>,    // selfhost（其余模式传 []）
    pkarr_url: Option<String>,  // selfhost 必填
    dns_origin: Option<String>, // selfhost 可选
) -> Result<String, String> {
    let seed = load_or_create_seed(&app)?;
    let m = mode.trim().to_ascii_lowercase();
    let node = node.trim().to_string();

    // 纯同网模式：Minimal + 按地址直连（无 NAT 兜底）。
    if matches!(m.as_str(), "lan" | "local") {
        let addr = nm_transport::addr_from_string(&node).map_err(|e| e.to_string())?;
        let client = Client::bind_local(seed).await.map_err(|e| e.to_string())?;
        let session = client.online(addr).await.map_err(|e| e.to_string())?;
        return finish_session(&app, &state, client, session, &display_name).await;
    }

    // nat/selfhost：输入可为「节点公钥(hex)」或「完整地址 NM_NODE_ADDR(JSON)」。
    // 地址 → 取其 id，并可先试同网直连；公钥 → 仅穿透（在线时发现服务仍会优先 LAN 路径）。
    let (id, lan_addr) = match nm_transport::addr_from_string(&node) {
        Ok(a) => (*a.id.as_bytes(), Some(a)),
        Err(_) => (parse_id(&node)?, None),
    };

    // 阶段 1：同网直连（Minimal，零基础设施、可离线）。Minimal 无中继，故只走直连候选地址：
    // 同一网络即刻连上，否则短超时失败 → 转阶段 2。仅当拿到地址时尝试。
    if let Some(addr) = &lan_addr {
        if let Ok(c1) = Client::bind_local(seed).await {
            if let Ok(Ok(session)) =
                tokio::time::timeout(std::time::Duration::from_secs(3), c1.online(addr.clone())).await
            {
                return finish_session(&app, &state, c1, session, &display_name).await;
            }
            // 同网失败：释放该 Minimal 端点，转穿透。
        }
    }

    // 阶段 2：穿透 NAT（按模式绑定发现/中继，按公钥拨号）。
    let client = match m.as_str() {
        "selfhost" | "self" | "custom" => {
            let pkarr = pkarr_url
                .filter(|s| !s.trim().is_empty())
                .ok_or("selfhost 模式需要填写 pkarr 端点(如 https://dns.example.com/pkarr)")?;
            let relays: Vec<String> =
                relay_urls.into_iter().filter(|s| !s.trim().is_empty()).collect();
            Client::bind_selfhosted(
                seed,
                relays,
                pkarr,
                dns_origin.filter(|s| !s.trim().is_empty()),
                0,
            )
            .await
            .map_err(|e| e.to_string())?
        }
        _ => Client::bind(seed).await.map_err(|e| e.to_string())?, // nat(N0) 默认
    };
    let session = client.online_by_id(id).await.map_err(|e| e.to_string())?;
    finish_session(&app, &state, client, session, &display_name).await
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

/// 断开当前连接（清空会话，便于切换节点/模式）。
#[tauri::command]
async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    *state.conn.lock().await = None;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            connect,
            send_to,
            directory_query,
            my_id,
            disconnect
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

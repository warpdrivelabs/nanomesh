//! Tauri 桥接：把界面 IPC 调用转发到进程内的 `nm-client`(iroh)。
//! 原生端(desktop / iOS / android)运行本模块；Web 端改走 nm-gateway。
//! 前端资源在 ../ui（静态壳，generate_context! 编译期内嵌；build.rs 声明 rerun-if-changed）。
//! App 图标源 ../../nanomesh-app.png（tauri icon 生成 icons/*，generate_context! 内嵌为窗口图标）。
//! 实体目录、手动添加的实体、群、频道、节点服务和资料存在 app_data_dir/catalog.db。
//! 界面偏好（主题、语言、身份昵称）仍经 ui_kv_* 写到 app_data_dir/ui-state.json。
//! 本地访问认证见 auth.rs / docs/CLIENT_AUTH_SECURITY.md（P1：主口令门 + 身份私钥信封加密；P2：自动锁定 + 加密备份；P3：恢复码 + 失败冷却 + 审计日志）。
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

mod auth;
mod devices;
mod pair;
mod pairing;
#[cfg(desktop)]
mod tray;

/// 已建立的连接（客户端 + 会话共享句柄）。
struct Conn {
    _client: Client, // 保活 endpoint
    session: Arc<Session>,
    my_id: [u8; 32],
}

#[derive(Default)]
struct AppState {
    conn: Mutex<Option<Conn>>,
    /// 解锁后驻留的保险库密钥 VK；None = 已上锁（见 auth.rs / docs/CLIENT_AUTH_SECURITY.md）。
    vault: std::sync::Mutex<Option<[u8; 32]>>,
    pair: pairing::PairState,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 把一个目录 Entity 渲染为前端 JSON —— 含 P0 富属性：bio / 状态文本 / 头像引用 / 链接 / locale，
/// 以及可发现标签 attributes["status"]。非 person 类型解不出 PersonProfile 时相应字段为空。
fn person_entity_json(e: &nm_proto::pb::Entity) -> Value {
    let pp = e
        .profile
        .as_ref()
        .and_then(|a| nm_entity::unpack_profile::<nm_entity::kinds::Person>(a).ok());
    json!({
        "id": hex(&e.entity_id),
        "homeNode": hex(&e.home_node),
        "kind": e.kind,
        "name": e.display_name,
        "status": e.attributes.get("status").cloned().unwrap_or_default(),
        "presence": e.attributes.get("presence").cloned().unwrap_or_default(),
        "handle": e.attributes.get("name").cloned().unwrap_or_default(), // 去中心命名 local@domain（N1）
        "bio": pp.as_ref().map(|p| p.bio.clone()).unwrap_or_default(),
        "statusText": pp.as_ref().map(|p| p.status_text.clone()).unwrap_or_default(),
        "avatar": pp.as_ref().map(|p| p.avatar_url.clone()).unwrap_or_default(),
        "links": pp.as_ref().map(|p| p.links.clone()).unwrap_or_default(),
        "locale": pp.as_ref().map(|p| p.locale.clone()).unwrap_or_default(),
    })
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

/// 应用数据目录（不存在则创建）。
fn data_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// 身份目录（不存在则创建）。
fn identities_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = data_dir(app)?.join("identities");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// 由种子推导公钥（= 用户公钥 / 节点 id）。
fn pubkey_of_seed(seed: &[u8; 32]) -> [u8; 32] {
    *nm_transport::SecretKey::from_bytes(seed).public().as_bytes()
}

/// 取当前保险库密钥 VK（未解锁则报错，作为命令门禁）。
fn vk_of(state: &State<AppState>) -> Result<[u8; 32], String> {
    (*state.vault.lock().unwrap()).ok_or_else(|| "未解锁，请先输入主口令".to_string())
}

// ── 本地访问认证（P1）：主口令门 + 身份私钥信封加密。见 auth.rs / docs/CLIENT_AUTH_SECURITY.md。 ──

#[tauri::command]
fn auth_status(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let dir = data_dir(&app)?;
    Ok(json!({
        "masterSet": auth::vault_exists(&dir),
        "unlocked": state.vault.lock().unwrap().is_some(),
        "hasRecovery": auth::has_recovery(&dir),
    }))
}

/// 首次设置主口令：建库 + 把已有明文身份迁移为加密态。
#[tauri::command]
fn setup_master(app: AppHandle, state: State<'_, AppState>, password: String) -> Result<(), String> {
    let dir = data_dir(&app)?;
    let vk = auth::setup(&dir, &password)?;
    *state.vault.lock().unwrap() = Some(vk);
    auth::audit(&dir, "setup");
    Ok(())
}

/// 解锁（输入主口令）。
#[tauri::command]
fn unlock(app: AppHandle, state: State<'_, AppState>, password: String) -> Result<(), String> {
    let dir = data_dir(&app)?;
    match auth::unlock(&dir, &password) {
        Ok(vk) => {
            *state.vault.lock().unwrap() = Some(vk);
            auth::audit(&dir, "unlock:ok");
            Ok(())
        }
        Err(e) => {
            auth::audit(&dir, "unlock:fail");
            Err(e)
        }
    }
}

/// 上锁：清零内存 VK + 断开当前连接。
#[tauri::command]
async fn lock(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    {
        let mut g = state.vault.lock().unwrap();
        if let Some(mut vk) = g.take() {
            use zeroize::Zeroize;
            vk.zeroize();
        }
    }
    *state.conn.lock().await = None;
    if let Ok(dir) = data_dir(&app) {
        auth::audit(&dir, "lock");
    }
    Ok(())
}

/// 修改主口令（用旧口令验证并重封保险库密钥；身份种子不动）。
#[tauri::command]
fn change_master(app: AppHandle, old: String, new: String) -> Result<(), String> {
    let dir = data_dir(&app)?;
    auth::change(&dir, &old, &new)?;
    auth::audit(&dir, "change_master");
    Ok(())
}

/// 加密导出全部身份（独立备份口令，与主口令解耦）。返回 base64 备份串。
#[tauri::command]
fn export_backup(app: AppHandle, state: State<'_, AppState>, password: String) -> Result<String, String> {
    let vk = vk_of(&state)?;
    let dir = data_dir(&app)?;
    let blob = auth::export(&dir, &vk, &password)?;
    auth::audit(&dir, "backup:export");
    Ok(blob)
}

/// 从备份串导入身份（用备份口令解密，再用当前 VK 重新加密写入）。返回导入条数。
#[tauri::command]
fn import_backup(app: AppHandle, state: State<'_, AppState>, blob: String, password: String) -> Result<usize, String> {
    let vk = vk_of(&state)?;
    let dir = data_dir(&app)?;
    let n = auth::import(&dir, &vk, &blob, &password)?;
    auth::audit(&dir, &format!("backup:import:{n}"));
    Ok(n)
}

/// 生成恢复码（24 词助记词，仅此一次可见）。
#[tauri::command]
fn generate_recovery(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let vk = vk_of(&state)?;
    let dir = data_dir(&app)?;
    let m = auth::generate_recovery(&dir, &vk)?;
    auth::audit(&dir, "recovery:generate");
    Ok(m)
}

/// 用恢复码重置主口令（忘记主口令时）。
#[tauri::command]
fn recover(app: AppHandle, mnemonic: String, new_password: String) -> Result<(), String> {
    let dir = data_dir(&app)?;
    auth::recover(&dir, &mnemonic, &new_password)?;
    auth::audit(&dir, "recovery:reset");
    Ok(())
}

/// 读取最近的安全审计日志（新→旧）。
#[tauri::command]
fn read_audit(app: AppHandle) -> Result<Vec<String>, String> {
    Ok(auth::read_audit(&data_dir(&app)?, 60))
}

// ── UI 状态持久化 KV：前端 localStorage 的耐久镜像，跨 webview 源/重建不丢（存 app_data_dir/ui-state.json）。 ──
#[tauri::command]
fn ui_kv_get_all(app: AppHandle) -> Result<String, String> {
    Ok(std::fs::read_to_string(data_dir(&app)?.join("ui-state.json")).unwrap_or_else(|_| "{}".to_string()))
}

#[tauri::command]
fn ui_kv_set(app: AppHandle, key: String, value: Option<String>) -> Result<(), String> {
    let path = data_dir(&app)?.join("ui-state.json");
    let mut obj: serde_json::Map<String, Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    match value {
        Some(v) => { obj.insert(key, Value::String(v)); }
        None => { obj.remove(&key); }
    }
    std::fs::write(&path, serde_json::to_string(&obj).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(())
}

fn identity_key(user: &str) -> Result<String, String> {
    let user = user.trim().to_ascii_lowercase();
    if user.is_empty() || user.len() > 128 || !user.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("bad_user".into());
    }
    Ok(user)
}

fn conv_key(conv: &str) -> Result<String, String> {
    let conv = conv.trim().to_ascii_lowercase();
    if conv.is_empty() || conv.len() > 128 || !conv.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("bad_conv".into());
    }
    Ok(conv)
}

fn record_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.len() > 128 || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.') {
        return None;
    }
    if s.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(s.to_ascii_lowercase())
    } else {
        Some(s.to_string())
    }
}

fn open_catalog(app: &AppHandle) -> Result<rusqlite::Connection, String> {
    let path = data_dir(app)?.join("catalog.db");
    let conn = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS record (
           user TEXT NOT NULL,
           kind TEXT NOT NULL,
           id TEXT NOT NULL,
           ord INTEGER NOT NULL,
           body TEXT NOT NULL,
           PRIMARY KEY (user, kind, id)
         );
         CREATE TABLE IF NOT EXISTS server (
           id TEXT PRIMARY KEY,
           ord INTEGER NOT NULL,
           body TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS profile (
           id TEXT PRIMARY KEY,
           body TEXT NOT NULL
         );",
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn rows_of(conn: &rusqlite::Connection, user: &str, kind: &str) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare("SELECT body FROM record WHERE user=?1 AND kind=?2 ORDER BY ord")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![user, kind], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        if let Ok(v) = serde_json::from_str::<Value>(&row.map_err(|e| e.to_string())?) {
            out.push(v);
        }
    }
    Ok(out)
}

fn catalog_kind(kind: &str) -> Result<&'static str, String> {
    match kind {
        "directory" => Ok("directory"),
        "added" => Ok("added"),
        "groups" => Ok("groups"),
        "channels" => Ok("channels"),
        _ => Err("bad_kind".into()),
    }
}

/// 读出某个身份的实体目录、手动添加的实体、群和频道。
#[tauri::command]
fn catalog_load(app: AppHandle, user: String) -> Result<String, String> {
    let user = identity_key(&user)?;
    let conn = open_catalog(&app)?;
    serde_json::to_string(&json!({
        "directory": rows_of(&conn, &user, "directory")?,
        "added": rows_of(&conn, &user, "added")?,
        "groups": rows_of(&conn, &user, "groups")?,
        "channels": rows_of(&conn, &user, "channels")?,
    }))
    .map_err(|e| e.to_string())
}

/// 按种类整表替换。kind = directory | added | groups | channels。
#[tauri::command]
fn catalog_save(app: AppHandle, user: String, kind: String, data: String) -> Result<(), String> {
    if data.len() > 8 * 1024 * 1024 {
        return Err("too_large".into());
    }
    let user = identity_key(&user)?;
    let kind = catalog_kind(&kind)?;
    let items: Vec<Value> = serde_json::from_str(&data).map_err(|_| "bad_json".to_string())?;
    let conn = open_catalog(&app)?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM record WHERE user=?1 AND kind=?2", rusqlite::params![user, kind]).map_err(|e| e.to_string())?;
    for (ord, item) in items.iter().take(8000).enumerate() {
        let Some(id) = item.get("id").and_then(|v| v.as_str()).and_then(record_id) else { continue };
        let body = serde_json::to_string(item).map_err(|e| e.to_string())?;
        if body.len() > 256 * 1024 {
            continue;
        }
        tx.execute(
            "INSERT OR REPLACE INTO record(user, kind, id, ord, body) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![user, kind, id, ord as i64, body],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

/// 节点服务（登录页和节点服务面板共用，不按身份拆分）。
#[tauri::command]
fn server_load(app: AppHandle) -> Result<String, String> {
    let conn = open_catalog(&app)?;
    let mut stmt = conn.prepare("SELECT body FROM server ORDER BY ord").map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        if let Ok(v) = serde_json::from_str::<Value>(&row.map_err(|e| e.to_string())?) {
            out.push(v);
        }
    }
    serde_json::to_string(&out).map_err(|e| e.to_string())
}

#[tauri::command]
fn server_save(app: AppHandle, data: String) -> Result<(), String> {
    if data.len() > 1024 * 1024 {
        return Err("too_large".into());
    }
    let items: Vec<Value> = serde_json::from_str(&data).map_err(|_| "bad_json".to_string())?;
    let conn = open_catalog(&app)?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM server", []).map_err(|e| e.to_string())?;
    for (ord, item) in items.iter().take(50).enumerate() {
        let Some(id) = item.get("id").and_then(|v| v.as_str()).and_then(record_id) else { continue };
        let body = serde_json::to_string(item).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO server(id, ord, body) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, ord as i64, body],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

/// 本机资料（按公钥）。返回 {公钥: 资料}。
#[tauri::command]
fn profile_load(app: AppHandle) -> Result<String, String> {
    let conn = open_catalog(&app)?;
    let mut stmt = conn.prepare("SELECT id, body FROM profile").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let mut map = serde_json::Map::new();
    for row in rows {
        let (id, body) = row.map_err(|e| e.to_string())?;
        if let Ok(v) = serde_json::from_str::<Value>(&body) {
            map.insert(id, v);
        }
    }
    serde_json::to_string(&Value::Object(map)).map_err(|e| e.to_string())
}

#[tauri::command]
fn profile_save(app: AppHandle, id: String, data: String) -> Result<(), String> {
    if data.len() > 2 * 1024 * 1024 {
        return Err("too_large".into());
    }
    let id = identity_key(&id)?;
    serde_json::from_str::<Value>(&data).map_err(|_| "bad_json".to_string())?;
    let conn = open_catalog(&app)?;
    conn.execute(
        "INSERT OR REPLACE INTO profile(id, body) VALUES (?1, ?2)",
        rusqlite::params![id, data],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn msg_id(v: &Value) -> String {
    v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn ts_of(v: &Value) -> u64 {
    v.get("ts").and_then(|n| n.as_u64().or_else(|| n.as_i64().map(|x| x.max(0) as u64))).unwrap_or(0)
}

fn chat_db_path(app: &AppHandle, user: &str) -> Result<std::path::PathBuf, String> {
    let user = identity_key(user)?;
    let dir = data_dir(app)?.join("chats");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(format!("{user}.db")))
}

fn open_chat_db(app: &AppHandle, user: &str) -> Result<rusqlite::Connection, String> {
    let path = chat_db_path(app, user)?;
    let conn = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS msg (
           conv TEXT NOT NULL,
           id TEXT NOT NULL,
           ts INTEGER NOT NULL,
           ord INTEGER NOT NULL,
           body TEXT NOT NULL,
           PRIMARY KEY (conv, id)
         );
         CREATE INDEX IF NOT EXISTS msg_conv_ord ON msg(conv, ord);
         CREATE TABLE IF NOT EXISTS unread (
           conv TEXT PRIMARY KEY,
           n INTEGER NOT NULL
         );",
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn bodies_to_json(bodies: Vec<String>) -> Result<String, String> {
    let vals: Vec<Value> = bodies.into_iter().filter_map(|s| serde_json::from_str(&s).ok()).collect();
    serde_json::to_string(&vals).map_err(|e| e.to_string())
}

const MSG_CAP: i64 = 10_000;

fn insert_msgs(conn: &mut rusqlite::Connection, conv: &str, fresh: &[Value]) -> Result<usize, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut stored = 0usize;
    for (i, m) in fresh.iter().enumerate() {
        let ts = ts_of(m);
        let mut id = msg_id(m);
        if id.is_empty() {
            id = format!("m{ts}-{i}");
        }
        let max_ord: i64 = tx
            .query_row("SELECT COALESCE(MAX(ord), -1) FROM msg WHERE conv=?1", [conv], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let ord = max_ord + 1;
        let body = serde_json::to_string(m).map_err(|e| e.to_string())?;
        let n = tx
            .execute(
                "INSERT OR IGNORE INTO msg(conv, id, ts, ord, body) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![conv, id, ts as i64, ord, body],
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            continue;
        }
        stored += 1;
    }
    if stored == 0 {
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(0);
    }
    let cnt: i64 = tx
        .query_row("SELECT COUNT(*) FROM msg WHERE conv=?1", [conv], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if cnt > MSG_CAP {
        tx.execute(
            "DELETE FROM msg WHERE conv=?1 AND ord IN (SELECT ord FROM msg WHERE conv=?1 ORDER BY ord ASC LIMIT ?2)",
            rusqlite::params![conv, cnt - MSG_CAP],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(stored)
}

fn window_bodies(conn: &rusqlite::Connection, conv: &str, end: i64, limit: i64) -> Result<Vec<String>, String> {
    if end <= 0 || limit <= 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT body FROM (
               SELECT body, idx FROM (
                 SELECT body, ROW_NUMBER() OVER (ORDER BY ord) - 1 AS idx
                 FROM msg WHERE conv = ?1
               ) WHERE idx < ?2
             ) ORDER BY idx DESC LIMIT ?3",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![conv, end, limit], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    out.reverse();
    Ok(out)
}

/// 打开某个身份的会话索引。
#[tauri::command]
fn chat_open(app: AppHandle, user: String) -> Result<String, String> {
    let user = identity_key(&user)?;
    let conn = open_chat_db(&app, &user)?;
    let mut convos = serde_json::Map::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT conv, COUNT(*), (SELECT body FROM msg m2 WHERE m2.conv = msg.conv ORDER BY ord DESC LIMIT 1)
                 FROM msg GROUP BY conv",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (id, n, body) = row.map_err(|e| e.to_string())?;
            let last: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            convos.insert(id, json!({"count": n, "last": last}));
        }
    }
    let mut unread = serde_json::Map::new();
    {
        let mut stmt = conn.prepare("SELECT conv, n FROM unread WHERE n > 0").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map_err(|e| e.to_string())?;
        for row in rows {
            let (id, n) = row.map_err(|e| e.to_string())?;
            unread.insert(id, json!(n));
        }
    }
    serde_json::to_string(&json!({"v": 2, "unread": unread, "convos": convos})).map_err(|e| e.to_string())
}

#[tauri::command]
fn chat_tail(app: AppHandle, user: String, conv: String, limit: u32) -> Result<String, String> {
    let conn = open_chat_db(&app, &user)?;
    let conv = conv_key(&conv)?;
    let n = (limit.max(1) as i64).min(200);
    let mut stmt = conn
        .prepare("SELECT body FROM msg WHERE conv=?1 ORDER BY ord DESC LIMIT ?2")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![conv, n], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut bodies = Vec::new();
    for row in rows {
        bodies.push(row.map_err(|e| e.to_string())?);
    }
    bodies.reverse();
    bodies_to_json(bodies)
}

#[tauri::command]
fn chat_before(app: AppHandle, user: String, conv: String, before: u32, limit: u32) -> Result<String, String> {
    let conn = open_chat_db(&app, &user)?;
    let conv = conv_key(&conv)?;
    let n = (limit.max(1) as i64).min(200);
    bodies_to_json(window_bodies(&conn, &conv, before as i64, n)?)
}

#[tauri::command]
fn chat_around(app: AppHandle, user: String, conv: String, id: String, limit: u32) -> Result<String, String> {
    let conn = open_chat_db(&app, &user)?;
    let conv = conv_key(&conv)?;
    let n = (limit.max(1) as i64).min(200);
    let at: Option<i64> = conn
        .query_row(
            "SELECT idx FROM (
               SELECT id, ROW_NUMBER() OVER (ORDER BY ord) - 1 AS idx
               FROM msg WHERE conv=?1
             ) WHERE id=?2",
            rusqlite::params![conv, id],
            |r| r.get(0),
        )
        .ok();
    let Some(at) = at else {
        return serde_json::to_string(&json!({"start": 0, "msgs": []})).map_err(|e| e.to_string());
    };
    let start = (at - n / 2).max(0);
    let end = start + n;
    let mut stmt = conn
        .prepare(
            "SELECT body FROM (
               SELECT body, ROW_NUMBER() OVER (ORDER BY ord) - 1 AS idx
               FROM msg WHERE conv=?1
             ) WHERE idx >= ?2 AND idx < ?3 ORDER BY idx",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![conv, start, end], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut bodies = Vec::new();
    for row in rows {
        bodies.push(row.map_err(|e| e.to_string())?);
    }
    let msgs: Vec<Value> = bodies.into_iter().filter_map(|s| serde_json::from_str(&s).ok()).collect();
    serde_json::to_string(&json!({"start": start, "msgs": msgs})).map_err(|e| e.to_string())
}

#[tauri::command]
fn chat_append(app: AppHandle, user: String, conv: String, msg: String) -> Result<bool, String> {
    let mut conn = open_chat_db(&app, &user)?;
    let conv = conv_key(&conv)?;
    let value: Value = serde_json::from_str(&msg).map_err(|_| "bad_json".to_string())?;
    Ok(insert_msgs(&mut conn, &conv, &[value])? > 0)
}

#[tauri::command]
fn chat_append_many(app: AppHandle, user: String, conv: String, data: String) -> Result<u32, String> {
    if data.len() > 8 * 1024 * 1024 {
        return Err("too_large".into());
    }
    let mut conn = open_chat_db(&app, &user)?;
    let conv = conv_key(&conv)?;
    let values: Vec<Value> = serde_json::from_str(&data).map_err(|_| "bad_json".to_string())?;
    Ok(insert_msgs(&mut conn, &conv, &values)? as u32)
}

#[tauri::command]
fn chat_unread_save(app: AppHandle, user: String, data: String) -> Result<(), String> {
    let conn = open_chat_db(&app, &user)?;
    let unread: Value = serde_json::from_str(&data).map_err(|_| "bad_json".to_string())?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM unread", []).map_err(|e| e.to_string())?;
    if let Some(map) = unread.as_object() {
        for (id, n) in map {
            let Ok(key) = conv_key(id) else { continue };
            let count = n.as_i64().unwrap_or(0);
            if count > 0 {
                tx.execute("INSERT INTO unread(conv, n) VALUES (?1, ?2)", rusqlite::params![key, count]).map_err(|e| e.to_string())?;
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

#[tauri::command]
fn chat_search(app: AppHandle, user: String, conv: String, q: String, limit: u32) -> Result<String, String> {
    let conn = open_chat_db(&app, &user)?;
    let conv = conv_key(&conv)?;
    let n = (limit.max(1) as i64).min(80);
    let needle = format!("%{}%", q.trim().replace('%', ""));
    let mut stmt = conn
        .prepare("SELECT body FROM msg WHERE conv=?1 AND (?2 = '%%' OR body LIKE ?2) ORDER BY ord DESC LIMIT ?3")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![conv, needle, n], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut bodies = Vec::new();
    for row in rows {
        bodies.push(row.map_err(|e| e.to_string())?);
    }
    bodies.reverse();
    bodies_to_json(bodies)
}

/// 列出全部用户身份（公钥 hex）。仅解锁后可用；种子加密存 `identities/<pubkey>.enc`。
#[tauri::command]
fn list_identities(app: AppHandle, state: State<'_, AppState>) -> Result<Vec<String>, String> {
    vk_of(&state)?; // 门禁：未解锁拒绝
    let dir = identities_dir(&app)?;
    let mut out = devices::accounts(&app);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".enc") {
                if stem.len() == 64 {
                    out.push(stem.to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// 新建一个用户身份（生成密钥对，用 VK 加密种子后落盘），返回其公钥 hex。
#[tauri::command]
fn create_identity(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let vk = vk_of(&state)?;
    let mut seed = nm_transport::SecretKey::generate().to_bytes();
    let pk = pubkey_of_seed(&seed);
    let enc = auth::encrypt_seed(&vk, &seed)?;
    {
        use zeroize::Zeroize;
        seed.zeroize();
    }
    let path = identities_dir(&app)?.join(format!("{}.enc", hex(&pk)));
    std::fs::write(&path, enc).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(hex(&pk))
}

/// 载入指定身份的种子（用 VK 解密）。
fn load_identity_seed(app: &AppHandle, vk: &[u8; 32], pubkey: &str) -> Result<[u8; 32], String> {
    let data = std::fs::read(identities_dir(app)?.join(format!("{}.enc", pubkey)))
        .map_err(|_| "用户身份不存在（可能已删除）".to_string())?;
    auth::decrypt_seed(vk, &data)
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
    let node_id = session.node_id();

    // 把消息流交给事件循环 → 推送到前端。
    let mut inbox = session.take_inbox().ok_or("inbox 已被占用")?;
    let session = Arc::new(session);
    let watch = session.clone();
    let apph = app.clone();
    async_runtime::spawn(async move {
        while let Some(gram) = inbox.recv().await {
            if let Some(p) = gram.payload.as_ref().filter(|p| p.type_url.starts_with(pair::PREFIX)) {
                pairing::on_old_side(&apph, &p.type_url, &p.value, &gram.sender).await;
                continue;
            }
            if let Some(p) = gram.payload.as_ref().filter(|p| p.type_url == devices::NODE_EVENT_TYPE) {
                if gram.sender.as_slice() == node_id.as_slice() {
                    devices::node_event(&apph, &p.value);
                }
                continue;
            }
            let (type_url, body) = match gram.payload.as_ref() {
                Some(p) => (p.type_url.clone(), String::from_utf8_lossy(&p.value).to_string()),
                None => (String::new(), String::new()),
            };
            let ev = json!({
                "type": "message",
                "msg": {
                    // gram_id 是发送方每个连接从 1 起的计数，重连会重复；拼上发送方与时间戳才唯一
                    "id": format!("{}-{}-{}", hex(&gram.sender), gram.timestamp_ms, gram.gram_id),
                    "from": hex(&gram.sender),
                    "to": hex(&gram.receiver),                 // 群消息=群id；频道=频道id；私聊=本人id
                    "group": matches!(gram.kind(), nm_proto::GramKind::GroupMessage),
                    "channel": matches!(gram.kind(), nm_proto::GramKind::ChannelPublish),
                    "typeUrl": type_url,
                    "body": body,
                    "ts": gram.timestamp_ms,
                }
            });
            let _ = apph.emit("core://event", ev);
        }
        if watch.close_reason().is_some_and(|r| r.contains("device_revoked")) {
            let _ = apph.emit(devices::EVENT, json!({ "event": "self_revoked" }));
        }
    });

    *state.conn.lock().await = Some(Conn {
        _client: client,
        session,
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
    user: String,               // 选定用户身份公钥 hex（空=用/建默认身份，向后兼容）
    mode: String,               // "nat" | "selfhost" | "lan"
    node: String,               // 节点公钥hex(64) 或 NM_NODE_ADDR(JSON)
    display_name: String,
    relay_urls: Vec<String>,    // selfhost（其余模式传 []）
    pkarr_url: Option<String>,  // selfhost 必填
    dns_origin: Option<String>, // selfhost 可选
) -> Result<String, String> {
    let vk = vk_of(&state)?;
    let user = user.trim();
    if user.is_empty() {
        return Err("请先选择用户身份".into());
    }
    let (client, session) =
        devices::dial_account(&app, &vk, user, &mode, &node, relay_urls, pkarr_url, dns_origin).await?;
    finish_session(&app, &state, client, session, &display_name).await
}

/// 上线：给了设备证书则以设备密钥连接后出示证书（账号身份收发），否则按连接密钥本身上线。
async fn go_online(client: &Client, addr: nm_transport::Addr, cert: Option<&nm_proto::DeviceCert>) -> Result<Session, String> {
    match cert {
        Some(c) => client.online_as(addr, c).await,
        None => client.online(addr).await,
    }
    .map_err(|e| e.to_string())
}

/// 拨号到一个节点（同网优先、穿透兜底），返回 (客户端, 会话)。connect 与 node_users 共用。
async fn dial(
    seed: [u8; 32],
    mode: &str,
    node: &str,
    relay_urls: Vec<String>,
    pkarr_url: Option<String>,
    dns_origin: Option<String>,
    cert: Option<&nm_proto::DeviceCert>,
) -> Result<(Client, Session), String> {
    let m = mode.trim().to_ascii_lowercase();
    let node = node.trim();

    // 纯同网模式：Minimal + 按地址直连（无 NAT 兜底）。
    if matches!(m.as_str(), "lan" | "local") {
        let addr = nm_transport::addr_from_string(node).map_err(|e| e.to_string())?;
        let client = Client::bind_local(seed).await.map_err(|e| e.to_string())?;
        let session = go_online(&client, addr, cert).await?;
        return Ok((client, session));
    }

    // nat/selfhost：输入可为公钥(hex) 或完整地址(JSON)。地址先试同网直连，失败转穿透。
    let (id, lan_addr) = match nm_transport::addr_from_string(node) {
        Ok(a) => (*a.id.as_bytes(), Some(a)),
        Err(_) => (parse_id(node)?, None),
    };
    if let Some(addr) = &lan_addr {
        if let Ok(c1) = Client::bind_local(seed).await {
            match tokio::time::timeout(std::time::Duration::from_secs(3), go_online(&c1, addr.clone(), cert)).await {
                Ok(Ok(session)) => return Ok((c1, session)),
                // 节点已明确拒绝（证书/吊销/版本），不必再走穿透重试。
                Ok(Err(e)) if e.contains("device_") || e.contains("unknown method") || e.contains("cert") => {
                    return Err(e)
                }
                _ => {}
            }
        }
    }
    let client = match m.as_str() {
        "selfhost" | "self" | "custom" => {
            let pkarr = pkarr_url
                .filter(|s| !s.trim().is_empty())
                .ok_or("selfhost 模式需要填写 pkarr 端点(如 https://dns.example.com/pkarr)")?;
            let relays: Vec<String> = relay_urls.into_iter().filter(|s| !s.trim().is_empty()).collect();
            Client::bind_selfhosted(seed, relays, pkarr, dns_origin.filter(|s| !s.trim().is_empty()), 0)
                .await
                .map_err(|e| e.to_string())?
        }
        _ => Client::bind(seed).await.map_err(|e| e.to_string())?, // nat(N0) 默认
    };
    let addr = nm_transport::addr_from_id(id).map_err(|e| e.to_string())?;
    let session = go_online(&client, addr, cert).await?;
    Ok((client, session))
}

/// 查询指定节点服务上的用户/实体（临时匿名拨号 + 目录查询，不影响当前主会话）。
/// 用随机临时身份，避免与主会话相同公钥的端点冲突；目录查询为只读发现，无需成员身份。
#[tauri::command]
async fn node_users(
    mode: String,
    node: String,
    relay_urls: Vec<String>,
    pkarr_url: Option<String>,
    dns_origin: Option<String>,
) -> Result<Vec<Value>, String> {
    let seed = nm_transport::SecretKey::generate().to_bytes();
    let (client, session) = dial(seed, &mode, &node, relay_urls, pkarr_url, dns_origin, None).await?;
    let entities = session
        .directory_query(DirectoryQuery { kind_prefix: String::new(), ..Default::default() })
        .await
        .map_err(|e| e.to_string())?;
    let out: Vec<Value> = entities
        .iter()
        .map(person_entity_json)
        .collect();
    drop(session);
    drop(client);
    Ok(out)
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
        .map(person_entity_json)
        .collect())
}

/// 更新本人资料（P0 富属性）：重新以 person 身份注册一份带完整 PersonProfile + 标签的 Entity。
/// 节点侧按 updated_at 做 LWW 收敛并经联邦扩散；前端需传全量当前值（本地持久，连接后回推）。
#[tauri::command]
async fn update_profile(
    state: State<'_, AppState>,
    display_name: String,
    bio: String,
    status_text: String,
    links: Vec<String>,
    locale: String,
    status: String, // 可发现标签：online / away / busy / dnd …（P2 presence 前的静态占位）
    avatar: String, // 头像：前端已缩放压缩的小图 data:URI（P0 内联随档案分发；P1 迁移为内容寻址 blob）
) -> Result<(), String> {
    // 内联头像体量上限（防止撑爆目录/gossip 负载）：约 128KB data:URI。更大者应走 P1 内容寻址。
    if avatar.len() > 128 * 1024 {
        return Err("头像过大（请用更小的图；上限约 96KB 图片）".into());
    }
    let session = session_of(&state).await?;
    let profile = nm_proto::pb::PersonProfile {
        avatar_url: avatar, // data:image/...;base64,... （P1 起可存 "b3:<hash>"）
        bio,
        status_text,
        links: links.into_iter().filter(|s| !s.trim().is_empty()).collect(),
        locale,
    };
    let mut attrs = std::collections::HashMap::new();
    let status = status.trim();
    if !status.is_empty() {
        attrs.insert("status".to_string(), status.to_string());
    }
    session
        .register_as::<nm_entity::kinds::Person>(&profile, display_name.trim(), attrs)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 上传头像等内容寻址 blob。`data_b64` = 纯 base64（无 data: 前缀）。返回 "b3:<hash-hex>"。
#[tauri::command]
async fn blob_put(state: State<'_, AppState>, data_b64: String, mime: String) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.trim())
        .map_err(|e| format!("非法 base64: {e}"))?;
    let session = session_of(&state).await?;
    let (hash, _home) = session.blob_put(bytes, &mime).await.map_err(|e| e.to_string())?;
    Ok(format!("b3:{}", hex(&hash)))
}

fn blob_hex(reference: &str) -> Result<&str, String> {
    let hexh = reference.strip_prefix("b3:").unwrap_or(reference).trim();
    parse_id(hexh)?;
    Ok(hexh)
}

fn read_cached_uri(app: &AppHandle, reference: &str) -> Option<String> {
    let hexh = blob_hex(reference).ok()?;
    let path = data_dir(app).ok()?.join("blobs").join(format!("{hexh}.uri"));
    let s = std::fs::read_to_string(path).ok()?;
    if s.starts_with("data:") { Some(s) } else { None }
}

fn write_blob_cache(app: &AppHandle, hash_hex: &str, mime: &str, data: &[u8]) {
    use base64::Engine;
    let Ok(dir) = data_dir(app) else { return };
    let dir = dir.join("blobs");
    let _ = std::fs::create_dir_all(&dir);
    let mime = if mime.trim().is_empty() { "application/octet-stream" } else { mime.trim() };
    let uri = format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(data)
    );
    let _ = std::fs::write(dir.join(format!("{hash_hex}.uri")), uri);
}

fn read_cached_bytes(app: &AppHandle, reference: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let uri = read_cached_uri(app, reference)?;
    let b64 = uri.split_once(',')?.1;
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

/// 只读本机缓存。没有的项是空字符串，不会向节点请求。
#[tauri::command]
fn blob_cached(app: AppHandle, references: Vec<String>) -> Result<Vec<String>, String> {
    Ok(references.into_iter().map(|r| read_cached_uri(&app, &r).unwrap_or_default()).collect())
}

/// 取 blob 并返回 data:URI（带 app_data_dir/blobs 本地缓存，避免重复拉取）。
/// `reference` = "b3:<hash-hex>"；`home_node` = 实体归属节点 hex（可空，用于跨节点回源）。
#[tauri::command]
async fn blob_get(
    app: AppHandle,
    state: State<'_, AppState>,
    reference: String,
    home_node: String,
) -> Result<String, String> {
    if let Some(s) = read_cached_uri(&app, &reference) {
        return Ok(s);
    }
    let hexh = blob_hex(&reference)?.to_string();
    let hash = parse_id(&hexh)?;
    let home = if home_node.trim().is_empty() {
        Vec::new()
    } else {
        parse_id(home_node.trim()).map(|h| h.to_vec()).unwrap_or_default()
    };
    let session = session_of(&state).await?;
    let (data, mime) = session.blob_get(hash.to_vec(), home).await.map_err(|e| e.to_string())?;
    let mime = if mime.trim().is_empty() { "application/octet-stream".to_string() } else { mime };
    write_blob_cache(&app, &hexh, &mime, &data);
    read_cached_uri(&app, &reference).ok_or_else(|| "缓存写入失败".to_string())
}

/// 上传一块媒体，返回内容哈希和所在节点。单块须小于节点的 1MB 上限。
#[tauri::command]
async fn blob_put_ref(app: AppHandle, state: State<'_, AppState>, data_b64: String, mime: String) -> Result<Value, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.trim())
        .map_err(|e| format!("非法 base64: {e}"))?;
    let session = session_of(&state).await?;
    let (hash, home) = session.blob_put(bytes.clone(), &mime).await.map_err(|e| e.to_string())?;
    let hash_hex = hex(&hash);
    write_blob_cache(&app, &hash_hex, &mime, &bytes);
    Ok(json!({ "ref": format!("b3:{hash_hex}"), "home": hex(&home) }))
}

const MEDIA_CAP: usize = 50 * 1024 * 1024;

async fn gather_blobs(app: &AppHandle, state: &State<'_, AppState>, references: &[String], home_node: &str) -> Result<Vec<u8>, String> {
    if references.is_empty() {
        return Err("没有可读取的内容".into());
    }
    let home = if home_node.trim().is_empty() {
        Vec::new()
    } else {
        parse_id(home_node.trim()).map(|h| h.to_vec()).unwrap_or_default()
    };
    let session = session_of(state).await?;
    let mut all = Vec::new();
    for reference in references {
        let data = if let Some(cached) = read_cached_bytes(app, reference) {
            cached
        } else {
            let hexh = blob_hex(reference)?.to_string();
            let hash = parse_id(&hexh)?;
            let (data, mime) = session.blob_get(hash.to_vec(), home.clone()).await.map_err(|e| e.to_string())?;
            write_blob_cache(app, &hexh, &mime, &data);
            data
        };
        if all.len().saturating_add(data.len()) > MEDIA_CAP {
            return Err("文件超过 50MB".into());
        }
        all.extend(data);
    }
    Ok(all)
}

fn media_cache_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = data_dir(app)?.join("media-cache");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn downloads_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = std::path::PathBuf::from(if home.is_empty() { "/tmp".into() } else { home }).join("Downloads");
    if dir.is_dir() { dir } else { std::env::temp_dir() }
}

fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() { "文件".into() } else { cleaned.chars().take(120).collect() }
}

fn unique_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = std::path::Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("文件");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    for n in 2..1000 {
        let next = if ext.is_empty() {
            format!("{stem} {n}")
        } else {
            format!("{stem} {n}.{ext}")
        };
        let p = dir.join(next);
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{name}.new"))
}

fn path_under(root: &std::path::Path, target: &std::path::Path) -> bool {
    let Ok(root) = root.canonicalize() else { return false };
    let Ok(target) = target.canonicalize() else { return false };
    target.starts_with(root)
}

/// 把分片拼成一个缓存文件，供播放或另存。已拼过的直接返回路径。
#[tauri::command]
async fn media_file(
    app: AppHandle,
    state: State<'_, AppState>,
    references: Vec<String>,
    home_node: String,
    ext: String,
) -> Result<String, String> {
    let ext = ext.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect::<String>();
    let ext = if ext.is_empty() { "bin".to_string() } else { ext };
    let key = references.join(",");
    let name = format!("{:016x}.{}", simple_key(&key), ext);
    let path = media_cache_dir(&app)?.join(name);
    if !path.is_file() {
        let bytes = gather_blobs(&app, &state, &references, &home_node).await?;
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    }
    Ok(path.to_string_lossy().into_owned())
}

/// 拼好后写入下载目录，返回最终路径。
#[tauri::command]
async fn save_media(
    app: AppHandle,
    state: State<'_, AppState>,
    references: Vec<String>,
    home_node: String,
    filename: String,
) -> Result<String, String> {
    let bytes = gather_blobs(&app, &state, &references, &home_node).await?;
    let path = unique_path(&downloads_dir(), &safe_filename(&filename));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// 用系统默认程序打开缓存或下载目录里的文件。
#[tauri::command]
async fn open_path(app: AppHandle, path: String) -> Result<(), String> {
    let target = std::path::PathBuf::from(&path);
    let cache = media_cache_dir(&app)?;
    let downloads = downloads_dir();
    if !path_under(&cache, &target) && !path_under(&downloads, &target) {
        return Err("只能打开已接收的文件".into());
    }
    let status = {
        #[cfg(target_os = "macos")]
        { std::process::Command::new("open").arg(&target).status() }
        #[cfg(target_os = "linux")]
        { std::process::Command::new("xdg-open").arg(&target).status() }
        #[cfg(target_os = "windows")]
        { std::process::Command::new("cmd").args(["/C", "start", "", &path]).status() }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        { return Err("当前系统不能打开文件".into()); }
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("系统没有打开这个文件".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn simple_key(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 私聊或群发一条富消息。`body` 是 nmspace.v1/chat 的 JSON。
#[tauri::command]
async fn send_rich(state: State<'_, AppState>, target: String, body: String, group: bool) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.send_rich(parse_id(&target)?, &body, group).await.map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn capture_screen(app: AppHandle, hide: bool) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    { capture_screen_mac(app, hide).await }
    #[cfg(not(target_os = "macos"))]
    { let _ = (app, hide); Err("当前系统还不能截图".into()) }
}

#[tauri::command]
async fn list_windows(app: AppHandle) -> Result<Vec<Value>, String> {
    #[cfg(target_os = "macos")]
    { list_windows_mac(app).await }
    #[cfg(not(target_os = "macos"))]
    { let _ = app; Err("当前系统还不能列出窗口".into()) }
}

#[tauri::command]
async fn capture_window(id: u32) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    { capture_window_mac(id).await }
    #[cfg(not(target_os = "macos"))]
    { let _ = id; Err("当前系统还不能截取窗口".into()) }
}

#[cfg(target_os = "macos")]
async fn capture_screen_mac(app: AppHandle, hide: bool) -> Result<String, String> {
    let win = app.get_webview_window("main");
    let restore = if hide {
        if let Some(w) = &win { let _ = w.hide(); }
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        win
    } else {
        None
    };
    let joined = tokio::task::spawn_blocking(|| shot(&["-x", "-m", "-t", "jpg"])).await;
    if let Some(w) = restore {
        let _ = w.show();
        let _ = w.set_focus();
    }
    joined.map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
async fn capture_window_mac(id: u32) -> Result<String, String> {
    let flag = format!("-l{id}");
    tokio::task::spawn_blocking(move || shot(&[flag.as_str(), "-x", "-o", "-t", "jpg"]))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
fn shot(args: &[&str]) -> Result<String, String> {
    use base64::Engine;
    let path = std::env::temp_dir().join(format!(
        "nm-shot-{}-{}.jpg",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
    ));
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args(args)
        .arg(&path)
        .status()
        .map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    if !status.success() || bytes.is_empty() {
        return Err("截图失败。请在系统设置的隐私与安全性中允许本应用录制屏幕".into());
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

#[cfg(target_os = "macos")]
const WINDOW_LIST_SWIFT: &str = r#"import Cocoa
import Foundation
let opts = CGWindowListOption(arrayLiteral: .optionOnScreenOnly, .excludeDesktopElements)
guard let raw = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
    print("[]")
    exit(0)
}
var rows: [[String: Any]] = []
for w in raw {
    let layer = w["kCGWindowLayer"] as? Int ?? 0
    if layer != 0 { continue }
    let wid = w["kCGWindowNumber"] as? Int ?? 0
    if wid == 0 { continue }
    let owner = w["kCGWindowOwnerName"] as? String ?? ""
    let title = w["kCGWindowName"] as? String ?? ""
    if owner == "Window Server" || owner == "Dock" { continue }
    if title == "NANO MESH" || owner == "nmspace" { continue }
    if title.isEmpty && owner.isEmpty { continue }
    rows.append(["id": wid, "title": title, "app": owner])
    if rows.count >= 40 { break }
}
let data = try! JSONSerialization.data(withJSONObject: rows)
FileHandle.standardOutput.write(data)
"#;

#[cfg(target_os = "macos")]
async fn list_windows_mac(app: AppHandle) -> Result<Vec<Value>, String> {
    let dir = data_dir(&app)?;
    tokio::task::spawn_blocking(move || {
        let bin = dir.join("nm-windows");
        if !bin.is_file() {
            let src = dir.join("nm-windows.swift");
            std::fs::write(&src, WINDOW_LIST_SWIFT).map_err(|e| e.to_string())?;
            let st = std::process::Command::new("swiftc")
                .args(["-O", "-o"])
                .arg(&bin)
                .arg(&src)
                .status()
                .map_err(|_| "这台电脑没有 swiftc，不能列出窗口".to_string())?;
            if !st.success() {
                return Err("不能列出窗口".into());
            }
        }
        let out = std::process::Command::new(&bin).output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("不能列出窗口".into());
        }
        let list: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
        Ok(list)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 设置本人在线状态（P2）：online / away / busy / dnd。节点据此 gossip 广播，其他端按 TTL 判在线。
#[tauri::command]
async fn presence_set(state: State<'_, AppState>, status: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.presence_set(status.trim()).await.map_err(|e| e.to_string())
}

// ── 群组（P3）：建群 / 成员与角色管理 / 群消息 ──
#[tauri::command]
async fn group_create(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let session = session_of(&state).await?;
    let gid = nm_transport::SecretKey::generate().to_bytes(); // 随机 32 字节群 id
    session.group_create(gid, name.trim()).await.map_err(|e| e.to_string())?;
    Ok(hex(&gid))
}

#[tauri::command]
async fn group_list(state: State<'_, AppState>) -> Result<Vec<Value>, String> {
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
async fn group_add(state: State<'_, AppState>, group_id: String, target: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.group_add(parse_id(&group_id)?, parse_id(&target)?).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn group_kick(state: State<'_, AppState>, group_id: String, target: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.group_kick(parse_id(&group_id)?, parse_id(&target)?).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn group_promote(state: State<'_, AppState>, group_id: String, target: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.group_promote(parse_id(&group_id)?, parse_id(&target)?).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn group_demote(state: State<'_, AppState>, group_id: String, target: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.group_demote(parse_id(&group_id)?, parse_id(&target)?).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn group_rename(state: State<'_, AppState>, group_id: String, name: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.group_rename(parse_id(&group_id)?, name.trim()).await.map_err(|e| e.to_string())
}

/// owner/admin 设置群信息（名称/简介/头像）。avatar 传 "b3:<hash>" 或空。
#[tauri::command]
async fn group_set_meta(
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
        .group_set_meta(parse_id(&group_id)?, name.trim(), topic.trim(), avatar.trim())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn group_dissolve(state: State<'_, AppState>, group_id: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.group_dissolve(parse_id(&group_id)?).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn group_leave(state: State<'_, AppState>, group_id: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.group_leave(parse_id(&group_id)?).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn send_group(state: State<'_, AppState>, group_id: String, text: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.send_group(parse_id(&group_id)?, &text).await.map(|_| ()).map_err(|e| e.to_string())
}

// ── 频道 / 主题（P4）──
#[tauri::command]
async fn channel_create(state: State<'_, AppState>, name: String, topic: String, avatar: String) -> Result<String, String> {
    if avatar.len() > 128 * 1024 {
        return Err("头像过大（请用更小的图）".into());
    }
    let session = session_of(&state).await?;
    let cid = nm_transport::SecretKey::generate().to_bytes(); // 随机 32 字节频道 id
    session.channel_create(cid, name.trim(), topic.trim(), avatar.trim()).await.map_err(|e| e.to_string())?;
    Ok(hex(&cid))
}

#[tauri::command]
async fn channel_list(state: State<'_, AppState>) -> Result<Vec<Value>, String> {
    let session = session_of(&state).await?;
    let chans = session.channel_list().await.map_err(|e| e.to_string())?;
    Ok(chans.iter().map(|c| json!({
        "id": hex(&c.channel_id), "name": c.name, "owner": hex(&c.owner), "topic": c.topic,
        "avatar": c.avatar_url, "homeNode": hex(&c.home_node),
    })).collect())
}

/// owner 设置频道信息（名称/简介/头像）→ gossip 广播给订阅者。
#[tauri::command]
async fn channel_set_meta(
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
        .channel_set_meta(parse_id(&channel_id)?, name.trim(), topic.trim(), avatar.trim())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn channel_sub(state: State<'_, AppState>, channel_id: String, name: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.channel_sub(parse_id(&channel_id)?, name.trim()).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn channel_unsub(state: State<'_, AppState>, channel_id: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.channel_unsub(parse_id(&channel_id)?).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn channel_publish(state: State<'_, AppState>, channel_id: String, body: String) -> Result<(), String> {
    let session = session_of(&state).await?;
    session.channel_publish(parse_id(&channel_id)?, &body).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn channel_backfill(state: State<'_, AppState>, channel_id: String, since_seq: u64) -> Result<Vec<Value>, String> {
    let session = session_of(&state).await?;
    let msgs = session.channel_backfill(parse_id(&channel_id)?, since_seq).await.map_err(|e| e.to_string())?;
    Ok(msgs.iter().map(|m| json!({
        "from": hex(&m.sender), "seq": m.seq, "ts": m.ts, "body": m.body,
    })).collect())
}

#[tauri::command]
async fn my_id(state: State<'_, AppState>) -> Result<String, String> {
    let g = state.conn.lock().await;
    Ok(hex(&g.as_ref().ok_or("尚未连接")?.my_id))
}

fn write_device_key(dir: &std::path::Path, pw: &str) -> Result<(), String> {
    let key_path = dir.join("device.key");
    std::fs::write(&key_path, pw).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn wipe_device_key_file(dir: &std::path::Path) {
    let key_path = dir.join("device.key");
    if let Ok(meta) = std::fs::metadata(&key_path) {
        let _ = std::fs::write(&key_path, vec![0u8; meta.len() as usize]);
        let _ = std::fs::remove_file(&key_path);
    }
}

/// 设备口令放在系统钥匙串（macOS/iOS Keychain、Windows 凭据管理器、Linux Secret Service）；
/// 没有钥匙串的平台（Android）返回 Unsupported，调用方退回 0600 文件。
mod devkey {
    pub enum Error {
        Unsupported,
        Denied(String),
    }

    #[cfg(any(target_os = "macos", target_os = "ios", windows, all(target_os = "linux", not(target_os = "android"))))]
    fn entry(dir: &std::path::Path) -> Result<keyring::Entry, Error> {
        keyring::Entry::new("io.nmspace.app", &format!("device-vault:{}", dir.display())).map_err(|e| match e {
            keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_) => Error::Unsupported,
            other => Error::Denied(other.to_string()),
        })
    }

    #[cfg(any(target_os = "macos", target_os = "ios", windows, all(target_os = "linux", not(target_os = "android"))))]
    pub fn get(dir: &std::path::Path) -> Result<Option<String>, Error> {
        match entry(dir)?.get_password() {
            Ok(pw) => Ok(Some(pw)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(keyring::Error::PlatformFailure(_)) => Err(Error::Unsupported),
            Err(e) => Err(Error::Denied(e.to_string())),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "ios", windows, all(target_os = "linux", not(target_os = "android"))))]
    pub fn set(dir: &std::path::Path, pw: &str) -> Result<(), Error> {
        entry(dir)?.set_password(pw).map_err(|e| match e {
            keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_) => Error::Unsupported,
            other => Error::Denied(other.to_string()),
        })
    }

    #[cfg(not(any(target_os = "macos", target_os = "ios", windows, all(target_os = "linux", not(target_os = "android")))))]
    pub fn get(_dir: &std::path::Path) -> Result<Option<String>, Error> {
        Err(Error::Unsupported)
    }

    #[cfg(not(any(target_os = "macos", target_os = "ios", windows, all(target_os = "linux", not(target_os = "android")))))]
    pub fn set(_dir: &std::path::Path, _pw: &str) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// 保存设备口令：优先钥匙串，成功后抹掉明文文件；钥匙串不可用时写 0600 文件。
fn store_device_key(dir: &std::path::Path, pw: &str) -> Result<(), String> {
    match devkey::set(dir, pw) {
        Ok(()) => {
            wipe_device_key_file(dir);
            Ok(())
        }
        Err(_) => write_device_key(dir, pw),
    }
}

fn open_device_vault(app: &AppHandle, state: &State<'_, AppState>) -> Result<(), String> {
    if state.vault.lock().unwrap().is_some() {
        return Ok(());
    }
    let dir = data_dir(app)?;
    let key_path = dir.join("device.key");
    if auth::vault_exists(&dir) {
        let mut denied = None;
        match devkey::get(&dir) {
            Ok(Some(pw)) => {
                if let Ok(vk) = auth::unlock(&dir, pw.trim()) {
                    *state.vault.lock().unwrap() = Some(vk);
                    wipe_device_key_file(&dir);
                    return Ok(());
                }
            }
            Ok(None) | Err(devkey::Error::Unsupported) => {}
            Err(devkey::Error::Denied(e)) => denied = Some(e),
        }
        if let Ok(pw) = std::fs::read_to_string(&key_path) {
            if let Ok(vk) = auth::unlock(&dir, pw.trim()) {
                *state.vault.lock().unwrap() = Some(vk);
                let _ = store_device_key(&dir, pw.trim());
                return Ok(());
            }
        }
        // 钥匙串拒绝访问不等于没有口令；此时重建保险库会让本机身份全部失联。
        if let Some(e) = denied {
            return Err(format!("无法读取系统钥匙串，请允许本应用访问后重试：{e}"));
        }
        // 旧主口令库无法在无界面下打开。挪走后改由本机设备密钥建库，不再弹出解锁页。
        let legacy = dir.join(format!("vault.json.legacy-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)));
        std::fs::rename(dir.join("vault.json"), legacy).map_err(|e| e.to_string())?;
    }
    let mut raw = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
    let pw = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw);
    let vk = auth::setup(&dir, &pw)?;
    store_device_key(&dir, &pw)?;
    *state.vault.lock().unwrap() = Some(vk);
    Ok(())
}

fn registry_base(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/');
    if s.is_empty() { "https://robot.link".into() } else { s.to_string() }
}

fn norm_account(local: &str, domain: &str) -> Result<(String, String), String> {
    let local = local.trim().to_lowercase();
    let domain = domain.trim().to_lowercase();
    let ok = !local.is_empty()
        && local.len() <= 63
        && local.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
        && !domain.is_empty()
        && domain.len() <= 253
        && domain.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'));
    if !ok {
        return Err("用户名或域名不合法".into());
    }
    Ok((local, domain))
}

fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.eq_ignore_ascii_case(name) { Some(v.trim()) } else { None }
    })
}

fn decode_chunked(mut rest: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let nl = rest.windows(2).position(|w| w == b"\r\n").ok_or("分块不完整")?;
        let size_line = std::str::from_utf8(&rest[..nl]).map_err(|_| "分块长度无效".to_string())?;
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| "分块长度无效".to_string())?;
        rest = &rest[nl + 2..];
        if size == 0 {
            break;
        }
        if rest.len() < size + 2 {
            return Err("分块被截断".into());
        }
        out.extend_from_slice(&rest[..size]);
        if &rest[size..size + 2] != b"\r\n" {
            return Err("分块格式不对".into());
        }
        rest = &rest[size + 2..];
    }
    Ok(out)
}

fn http_message(raw: &[u8]) -> Result<(u16, String), String> {
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("响应不完整")?;
    let head = std::str::from_utf8(&raw[..sep]).map_err(|_| "响应头不是文本".to_string())?;
    let rest = &raw[sep + 4..];
    let code: u16 = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = if header_value(head, "transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        decode_chunked(rest)?
    } else if let Some(n) = header_value(head, "content-length").and_then(|v| v.parse::<usize>().ok()) {
        let n = n.min(rest.len());
        rest[..n].to_vec()
    } else {
        rest.to_vec()
    };
    Ok((code, String::from_utf8_lossy(&body).into_owned()))
}

/// 用 OpenSSL 发 HTTPS GET。系统 TLS 和 rustls 连 robot.link 会在 ClientHello 后被重置。
fn https_get_json(url: &str) -> Result<Value, String> {
    let rest = url.strip_prefix("https://").ok_or("注册中心地址必须是 https")?;
    let (hostport, pathq) = rest.split_once('/').unwrap_or((rest, ""));
    let path = format!("/{pathq}");
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "注册中心端口无效".to_string())?),
        None => (hostport, 443u16),
    };
    if host.is_empty() {
        return Err("注册中心地址无效".into());
    }
    let tcp = std::net::TcpStream::connect((host, port)).map_err(|e| format!("连接注册中心失败: {e}"))?;
    let _ = tcp.set_read_timeout(Some(std::time::Duration::from_secs(20)));
    let _ = tcp.set_write_timeout(Some(std::time::Duration::from_secs(20)));
    let mut builder = openssl::ssl::SslConnector::builder(openssl::ssl::SslMethod::tls())
        .map_err(|e| format!("TLS 初始化失败: {e}"))?;
    if std::path::Path::new("/etc/ssl/cert.pem").exists() {
        builder.set_ca_file("/etc/ssl/cert.pem").map_err(|e| format!("无法加载系统证书: {e}"))?;
    }
    let connector = builder.build();
    let mut stream = connector.connect(host, tcp).map_err(|e| format!("注册中心 TLS 失败: {e}"))?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: application/json\r\nAccept-Encoding: identity\r\n\r\n"
    );
    std::io::Write::write_all(&mut stream, req.as_bytes()).map_err(|e| format!("请求注册中心失败: {e}"))?;
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut raw).map_err(|e| format!("读取注册中心响应失败: {e}"))?;
    let (code, body) = http_message(&raw)?;
    let v: Value = serde_json::from_str(body.trim()).map_err(|e| format!("注册中心响应无法解析: {e}"))?;
    if !(200..300).contains(&code) {
        let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("注册中心拒绝了请求");
        return Err(err.to_string());
    }
    Ok(v)
}

async fn http_get_json(url: &str) -> Result<Value, String> {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || https_get_json(&url))
        .await
        .map_err(|e| e.to_string())?
}

/// 打开应用时准备本机钥匙库，不再向用户要主口令。
#[tauri::command]
fn ensure_device(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    open_device_vault(&app, &state)
}

/// 注册中心里未停用的域名，供登录页下拉。不含邮箱。
#[tauri::command]
async fn account_domains(registry: String) -> Result<Value, String> {
    let url = format!("{}/api/catalog", registry_base(&registry));
    http_get_json(&url).await
}

fn query_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn registry_phrase(err: &str) -> String {
    if err.contains("域名未登记") {
        "这个域名还没有在注册中心登记，不能用来注册或登录。".into()
    } else if err.contains("域名已停用") {
        "这个域名已停用。".into()
    } else if err == "domain_not_owned" {
        "域名已登记，Home Node 尚未同步，请稍后再试。".into()
    } else {
        err.to_string()
    }
}

async fn resolve_home_node(registry: &str, domain: &str) -> Result<String, String> {
    let url = format!("{}/api/resolve?domain={}", registry_base(registry), query_escape(domain));
    let v = http_get_json(&url).await.map_err(|e| registry_phrase(&e))?;
    v.get("pubkey")
        .and_then(|x| x.as_str())
        .filter(|s| s.len() == 64)
        .map(|s| s.to_string())
        .ok_or_else(|| "注册中心没有返回该域名的节点公钥".into())
}

async fn node_account(node: &str, seed: [u8; 32], method: &str, body: &str) -> Result<String, String> {
    let (_client, session) = dial(seed, "nat", node, Vec::new(), None, None, None).await?;
    let out = session.name_account(method, body).await.map_err(|e| registry_phrase(&e.to_string()))?;
    drop(session);
    Ok(out)
}

/// 在家节点注册。口令只发到家节点做 Argon2 哈希，本机不保存口令。
#[tauri::command]
async fn account_register(
    app: AppHandle,
    state: State<'_, AppState>,
    local: String,
    domain: String,
    nickname: String,
    password: String,
    registry: String,
) -> Result<Value, String> {
    open_device_vault(&app, &state)?;
    if password.chars().count() < 8 {
        return Err("密码至少 8 位".into());
    }
    let (local, domain) = norm_account(&local, &domain)?;
    let node = resolve_home_node(&registry, &domain).await?;
    let probe = nm_transport::SecretKey::generate().to_bytes();
    let body = json!({ "local": local, "domain": domain }).to_string();
    match node_account(&node, probe, "name.lookup", &body).await {
        Ok(exists) if exists.trim() == "1" => return Err("name_taken".into()),
        Ok(_) => {}
        Err(e) if e.contains("unknown method") => {}
        Err(e) => return Err(e),
    }
    let user = create_identity(app.clone(), state.clone())?;
    let nick = if nickname.trim().is_empty() { format!("{local}@{domain}") } else { nickname.trim().to_string() };
    connect(app, state.clone(), user.clone(), "nat".into(), node.clone(), nick, Vec::new(), None, None).await?;
    let session = session_of(&state).await?;
    let body = json!({ "local": local, "domain": domain, "password": password }).to_string();
    session.name_account("name.register", &body).await.map_err(|e| registry_phrase(&e.to_string()))?;
    Ok(json!({ "user": user, "name": format!("{local}@{domain}"), "node": node }))
}

/// 在家节点比对口令，再核对本机私钥是否就是该名字登记的公钥。
#[tauri::command]
async fn account_login(
    app: AppHandle,
    state: State<'_, AppState>,
    local: String,
    domain: String,
    password: String,
    user: String,
    nickname: String,
    registry: String,
) -> Result<Value, String> {
    open_device_vault(&app, &state)?;
    let (local, domain) = norm_account(&local, &domain)?;
    let node = resolve_home_node(&registry, &domain).await?;
    let user = user.trim().to_string();
    let vk = vk_of(&state)?;
    let seed = match user.is_empty() {
        true => None,
        false => load_identity_seed(&app, &vk, &user).ok(),
    }
    .unwrap_or_else(|| nm_transport::SecretKey::generate().to_bytes());
    let (_probe_client, probe) = dial(seed, "nat", &node, Vec::new(), None, None, None).await?;
    let body = json!({ "local": local, "domain": domain, "password": password }).to_string();
    let got = probe.name_account("name.login", &body).await.map_err(|e| registry_phrase(&e.to_string()))?;
    drop(probe);
    let got = got.trim().to_ascii_lowercase();
    // 名字→公钥映射只在前端 localStorage；导入备份或换了 webview 源后会缺失，按家节点返回的公钥找本机私钥。
    let user = if !user.is_empty() && got == user.to_ascii_lowercase() {
        user
    } else if got.len() == 64 && got.chars().all(|c| c.is_ascii_hexdigit()) && devices::has_local(&app, &got) {
        got
    } else {
        return Err("not_key_owner".into());
    };
    let nick = if nickname.trim().is_empty() { format!("{local}@{domain}") } else { nickname.trim().to_string() };
    connect(app, state, user.clone(), "nat".into(), node.clone(), nick, Vec::new(), None, None).await?;
    Ok(json!({ "user": user, "name": format!("{local}@{domain}"), "node": node }))
}

/// 已登录时修改家节点上的登录密码。调用方必须是该名字登记的公钥。
#[tauri::command]
async fn account_passwd(
    state: State<'_, AppState>,
    local: String,
    domain: String,
    old_password: String,
    password: String,
) -> Result<(), String> {
    if password.chars().count() < 8 {
        return Err("password_short".into());
    }
    if password == old_password {
        return Err("same_password".into());
    }
    let (local, domain) = norm_account(&local, &domain)?;
    let session = session_of(&state).await?;
    let body = json!({ "local": local, "domain": domain, "old": old_password, "password": password }).to_string();
    session.name_account("name.passwd", &body).await.map_err(|e| registry_phrase(&e.to_string()))?;
    Ok(())
}

/// 本机持有对应私钥时，不验证旧密码，直接在家节点设置新密码。
#[tauri::command]
async fn account_reset(
    app: AppHandle,
    state: State<'_, AppState>,
    local: String,
    domain: String,
    password: String,
    user: String,
    registry: String,
) -> Result<String, String> {
    open_device_vault(&app, &state)?;
    if password.chars().count() < 8 {
        return Err("password_short".into());
    }
    let (local, domain) = norm_account(&local, &domain)?;
    let user = user.trim().to_string();
    let node = resolve_home_node(&registry, &domain).await?;
    let vk = vk_of(&state)?;
    let body = json!({ "local": local, "domain": domain, "password": password }).to_string();
    if !user.is_empty() {
        // 普通设备没有账号私钥，改密码须在管理设备上做。
        let seed = load_identity_seed(&app, &vk, &user).map_err(|_| "not_admin_device".to_string())?;
        node_account(&node, seed, "name.reset", &body).await?;
        return Ok(user);
    }
    // 没有名字→公钥映射（例如刚导入备份）：逐个用本机身份尝试，家节点只接受登记公钥本人。
    for pk in list_identities(app.clone(), state.clone())? {
        let Ok(seed) = load_identity_seed(&app, &vk, &pk) else { continue };
        match node_account(&node, seed, "name.reset", &body).await {
            Ok(_) => return Ok(pk),
            Err(e) if e.contains("not_key_owner") => {}
            Err(e) => return Err(e),
        }
    }
    Err("not_key_owner".into())
}

/// 域名输入时的提示。优先用目录做前缀过滤；目录不可用时，改用公开解析核对当前域名。
#[tauri::command]
async fn account_suggest(q: String, registry: String) -> Result<Value, String> {
    let needle = q.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(json!({ "ok": true, "items": [], "status": "empty" }));
    }
    let base = registry_base(&registry);
    let catalog = format!("{base}/api/catalog?q={}", query_escape(&needle));
    if let Ok(v) = http_get_json(&catalog).await {
        let items: Vec<Value> = v
            .get("items")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|it| {
                        it.get("domain")
                            .and_then(|d| d.as_str())
                            .map(|d| d.to_lowercase().contains(&needle))
                            .unwrap_or(false)
                    })
                    .take(8)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let exact = items.iter().any(|it| it.get("domain").and_then(|d| d.as_str()) == Some(needle.as_str()));
        let status = if exact {
            "exact"
        } else if !items.is_empty() {
            "list"
        } else if needle.contains('.') {
            "unknown"
        } else {
            "partial"
        };
        if status == "unknown" {
            let resolve = format!("{base}/api/resolve?domain={}", query_escape(&needle));
            if let Err(e) = http_get_json(&resolve).await {
                if e.contains("域名已停用") {
                    return Ok(json!({ "ok": true, "items": [], "status": "disabled" }));
                }
            }
        }
        return Ok(json!({ "ok": true, "items": items, "status": status }));
    }
    if !needle.contains('.') {
        return Ok(json!({ "ok": true, "items": [], "status": "partial" }));
    }
    let resolve = format!("{base}/api/resolve?domain={}", query_escape(&needle));
    match http_get_json(&resolve).await {
        Ok(v) => {
            let domain = v.get("domain").and_then(|d| d.as_str()).unwrap_or(needle.as_str());
            let pubkey = v.get("pubkey").and_then(|d| d.as_str()).unwrap_or("");
            Ok(json!({
                "ok": true,
                "status": "exact",
                "items": [{ "domain": domain, "pubkey": pubkey }],
            }))
        }
        Err(e) if e.contains("域名未登记") => Ok(json!({ "ok": true, "items": [], "status": "unknown" })),
        Err(e) if e.contains("域名已停用") => Ok(json!({ "ok": true, "items": [], "status": "disabled" })),
        Err(e) => Err(e),
    }
}

/// 在 home node 认领本地名 → 返回完整名 local@domain。
#[tauri::command]
async fn name_claim(state: State<'_, AppState>, local_part: String) -> Result<String, String> {
    let session = session_of(&state).await?;
    let rec = session.name_claim(local_part.trim()).await.map_err(|e| e.to_string())?;
    Ok(format!("{}@{}", rec.local_part, rec.domain))
}
/// 解析 name → 目标公钥 hex（无则 None）。
#[tauri::command]
async fn name_resolve(state: State<'_, AppState>, name: String) -> Result<Option<String>, String> {
    let session = session_of(&state).await?;
    let rec = session.name_resolve(name.trim()).await.map_err(|e| e.to_string())?;
    Ok(rec.map(|r| hex(&r.client_pubkey)))
}
/// 反向解析 公钥 hex → 规范名（无则 None）。
#[tauri::command]
async fn name_reverse(state: State<'_, AppState>, pubkey: String) -> Result<Option<String>, String> {
    let session = session_of(&state).await?;
    let rec = session.name_reverse(parse_id(&pubkey)?).await.map_err(|e| e.to_string())?;
    Ok(rec.map(|r| format!("{}@{}", r.local_part, r.domain)))
}

/// 断开当前连接（清空会话，便于切换节点/模式）。
#[tauri::command]
async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    *state.conn.lock().await = None;
    Ok(())
}

fn https_get_raw(url: &str) -> Result<(u16, String), String> {
    let rest = url.strip_prefix("https://").ok_or("地址必须是 https")?;
    let (hostport, pathq) = rest.split_once('/').unwrap_or((rest, ""));
    let path = format!("/{pathq}");
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "端口无效".to_string())?),
        None => (hostport, 443u16),
    };
    let tcp = std::net::TcpStream::connect((host, port)).map_err(|e| e.to_string())?;
    let _ = tcp.set_read_timeout(Some(std::time::Duration::from_secs(12)));
    let _ = tcp.set_write_timeout(Some(std::time::Duration::from_secs(12)));
    let mut builder = openssl::ssl::SslConnector::builder(openssl::ssl::SslMethod::tls()).map_err(|e| e.to_string())?;
    if std::path::Path::new("/etc/ssl/cert.pem").exists() {
        let _ = builder.set_ca_file("/etc/ssl/cert.pem");
    }
    let mut stream = builder.build().connect(host, tcp).map_err(|e| e.to_string())?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: */*\r\nAccept-Encoding: identity\r\n\r\n");
    std::io::Write::write_all(&mut stream, req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut raw).map_err(|e| e.to_string())?;
    let (code, body) = http_message(&raw)?;
    if !(200..300).contains(&code) {
        return Err(format!("HTTP {code}"));
    }
    Ok((code, body))
}

fn rate_num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn ticker_row(label: &str, value: &str, hint: &str, url: &str) -> Value {
    json!({ "label": label, "value": value, "hint": hint, "url": url })
}

fn ticker_fx() -> Option<Value> {
    let url = "https://open.er-api.com/v6/latest/USD";
    let v = https_get_json(url).ok()?;
    let rates = v.get("rates")?;
    let cny = rate_num(rates.get("CNY")?)?;
    let eur = rate_num(rates.get("EUR")?)?;
    let gbp = rate_num(rates.get("GBP")?)?;
    let jpy = rate_num(rates.get("JPY")?)?;
    if eur <= 0.0 || gbp <= 0.0 || jpy <= 0.0 {
        return None;
    }
    let eur_cny = cny / eur;
    let gbp_cny = cny / gbp;
    let jpy_cny = cny / jpy * 100.0;
    let note = v.get("time_last_update_utc").and_then(|t| t.as_str()).unwrap_or("");
    Some(json!({
        "id": "fx",
        "title": "汇率",
        "label": format!("汇率  美元/人民币 {cny:.2}  欧元/人民币 {eur_cny:.2}  英镑/人民币 {gbp_cny:.2}  100日元/人民币 {jpy_cny:.2}"),
        "source": "ExchangeRate-API",
        "url": url,
        "note": note,
        "rows": [
            ticker_row("美元/人民币", &format!("{cny:.4}"), "1 美元", url),
            ticker_row("欧元/人民币", &format!("{eur_cny:.4}"), "1 欧元", url),
            ticker_row("英镑/人民币", &format!("{gbp_cny:.4}"), "1 英镑", url),
            ticker_row("100日元/人民币", &format!("{jpy_cny:.4}"), "100 日元", url),
        ],
    }))
}

fn gate_ticker(pair: &str) -> Option<Value> {
    let v = https_get_json(&format!("https://api.gateio.ws/api/v4/spot/tickers?currency_pair={pair}")).ok()?;
    v.as_array()?.first().cloned()
}

fn gate_num(row: &Value, key: &str) -> Option<f64> {
    row.get(key).and_then(rate_num)
}

fn ticker_crypto() -> Option<Value> {
    let pairs = [("BTC", "BTC_USDT"), ("ETH", "ETH_USDT"), ("SOL", "SOL_USDT")];
    let mut rows = Vec::new();
    let mut bits = Vec::new();
    for (name, pair) in pairs {
        let Some(row) = gate_ticker(pair) else { continue };
        let Some(last) = gate_num(&row, "last") else { continue };
        let page = format!("https://www.gate.io/trade/{pair}");
        let change = row.get("change_percentage").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let mut hint = Vec::new();
        if let Some(hi) = gate_num(&row, "high_24h") { hint.push(format!("24h 高 {hi:.2}")); }
        if let Some(lo) = gate_num(&row, "low_24h") { hint.push(format!("24h 低 {lo:.2}")); }
        let value = if name == "BTC" || name == "ETH" {
            format!("{last:.0} 美元")
        } else {
            format!("{last:.2} 美元")
        };
        let change_txt = if change.is_empty() { String::new() } else { format!("  {change}%") };
        bits.push(format!("{name} {value}{change_txt}"));
        let shown = if change.is_empty() { value } else { format!("{value}  {change}%") };
        rows.push(ticker_row(name, &shown, &hint.join("  "), &page));
    }
    if rows.is_empty() {
        return None;
    }
    let first = rows[0].get("url").and_then(|u| u.as_str()).unwrap_or("https://www.gate.io");
    Some(json!({
        "id": "crypto",
        "title": "数字货币",
        "label": format!("币价  {}", bits.join("  ")),
        "source": "Gate.io",
        "url": first,
        "note": "现货最新价",
        "rows": rows,
    }))
}

fn rss_text(block: &str, tag: &str) -> Option<String> {
    let raw = block.split(&format!("<{tag}>")).nth(1)?.split(&format!("</{tag}>")).next()?.trim();
    let text = raw
        .trim_start_matches("<![CDATA[")
        .trim_end_matches("]]>")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    let text = text.trim();
    if text.is_empty() { None } else { Some(text.to_string()) }
}

fn ticker_news() -> Vec<Value> {
    let Ok((_code, body)) = https_get_raw("https://www.chinanews.com.cn/rss/scroll-news.xml") else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for item in body.split("<item>").skip(1) {
        let Some(title) = rss_text(item, "title") else { continue };
        let link = rss_text(item, "link").unwrap_or_default();
        let when = rss_text(item, "pubDate").unwrap_or_default();
        let n = items.len();
        items.push(json!({
            "id": format!("news-{n}"),
            "title": title,
            "label": format!("要闻  {title}"),
            "source": "中国新闻网",
            "url": link,
            "note": when,
            "rows": [ticker_row(&title, &when, "", &link)],
        }));
        if items.len() == 8 {
            break;
        }
    }
    items
}

fn weather_phrase(code: i64) -> &'static str {
    match code {
        0 => "晴",
        1 | 2 => "少云",
        3 => "阴",
        45 | 48 => "雾",
        51 | 53 | 55 | 56 | 57 | 61 | 63 | 65 | 66 | 67 | 80 | 81 | 82 => "雨",
        71 | 73 | 75 | 77 | 85 | 86 => "雪",
        95 | 96 | 99 => "雷雨",
        _ => "",
    }
}

fn ticker_weather() -> Option<Value> {
    let url = "https://api.open-meteo.com/v1/forecast?latitude=39.90,31.23,35.68,51.51,40.71,1.35&longitude=116.40,121.47,139.69,-0.13,-74.01,103.82&current=temperature_2m,weather_code,wind_speed_10m";
    let v = https_get_json(url).ok()?;
    let names = ["北京", "上海", "东京", "伦敦", "纽约", "新加坡"];
    let spots: Vec<&Value> = if let Some(arr) = v.as_array() {
        arr.iter().collect()
    } else {
        vec![&v]
    };
    let mut rows = Vec::new();
    let mut bits = Vec::new();
    for (name, spot) in names.iter().zip(spots.iter()) {
        let Some(temp) = spot.pointer("/current/temperature_2m").and_then(|t| t.as_f64()) else { continue };
        let phrase = spot.pointer("/current/weather_code").and_then(|c| c.as_i64()).map(weather_phrase).unwrap_or("");
        let wind = spot.pointer("/current/wind_speed_10m").and_then(|w| w.as_f64());
        let mut hint = phrase.to_string();
        if let Some(w) = wind {
            if !hint.is_empty() { hint.push_str("  "); }
            hint.push_str(&format!("风 {w:.0} km/h"));
        }
        bits.push(format!("{name} {temp:.0}°"));
        rows.push(ticker_row(name, &format!("{temp:.0}°"), &hint, ""));
    }
    if rows.is_empty() {
        return None;
    }
    Some(json!({
        "id": "weather",
        "title": "天气",
        "label": format!("天气  {}", bits.join("  ")),
        "source": "Open-Meteo",
        "url": "https://open-meteo.com/",
        "note": "当前气温",
        "rows": rows,
    }))
}

/// 用系统浏览器打开 https 链接。中国新闻网的要闻链接仍是 http，只放行这个域名。
#[tauri::command]
fn open_https(url: String) -> Result<(), String> {
    let url = url.trim();
    let ok = url.starts_with("https://")
        || url.starts_with("http://www.chinanews.com.cn/")
        || url.starts_with("http://www.chinanews.com/");
    if !ok || url.chars().any(|c| c.is_control() || c == ' ') {
        return Err("这个地址不能打开".into());
    }
    let status = {
        #[cfg(target_os = "macos")]
        { std::process::Command::new("open").arg(url).status() }
        #[cfg(target_os = "linux")]
        { std::process::Command::new("xdg-open").arg(url).status() }
        #[cfg(target_os = "windows")]
        { std::process::Command::new("cmd").args(["/C", "start", "", url]).status() }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        { return Err("当前系统不能打开链接".into()); }
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("系统没有打开这个链接".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// 标题栏跑马灯。只返回勾选的来源；某一路失败就略过，不编造数字。
/// 每一条自带来源地址，点开后由界面放到下方标签页。
#[tauri::command]
async fn ticker_feed(fx: bool, crypto: bool, news: bool, weather: bool) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        let mut items = Vec::new();
        if fx {
            if let Some(v) = ticker_fx() { items.push(v); }
        }
        if crypto {
            if let Some(v) = ticker_crypto() { items.push(v); }
        }
        if news {
            items.extend(ticker_news());
        }
        if weather {
            if let Some(v) = ticker_weather() { items.push(v); }
        }
        json!({ "items": items })
    })
    .await
    .map_err(|e| e.to_string())
}

/// 供前端 `platform.js` 探测系统。窗口按钮由标题栏自绘。
#[tauri::command]
fn platform() -> &'static str {
    std::env::consts::OS // "macos" | "windows" | "linux"
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // 单实例必须最先注册：再次启动时唤起已运行的窗口（Windows 点通知也会走这里）。
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| tray::show_main(app)))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![tray::HIDDEN_ARG]),
        ))
        .plugin(tray::shortcut_plugin())
        .on_window_event(tray::on_window_event);
    builder
        .plugin(tauri_plugin_notification::init())
        .manage(AppState::default())
        .setup(|_app| {
            // 系统交通灯改由标题栏右侧的红黄绿按钮承担，各平台都关掉原生装饰。
            if let Some(w) = _app.get_webview_window("main") {
                let _ = w.set_decorations(false);
                let _ = w.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));
            }
            #[cfg(desktop)]
            tray::setup(_app.handle())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect,
            send_to,
            directory_query,
            update_profile,
            blob_put,
            blob_get,
            blob_cached,
            blob_put_ref,
            media_file,
            save_media,
            open_path,
            send_rich,
            capture_screen,
            list_windows,
            capture_window,
            presence_set,
            group_create,
            group_list,
            group_add,
            group_kick,
            group_promote,
            group_demote,
            group_rename,
            group_set_meta,
            group_dissolve,
            group_leave,
            send_group,
            channel_create,
            channel_list,
            channel_sub,
            channel_unsub,
            channel_publish,
            channel_set_meta,
            channel_backfill,
            name_claim,
            name_resolve,
            ensure_device,
            account_domains,
            account_register,
            account_login,
            account_passwd,
            account_reset,
            pairing::pair_request,
            pairing::pair_ticket,
            pairing::pair_cancel,
            pairing::pair_accept_ticket,
            pairing::pair_approve,
            pairing::pair_reject,
            devices::device_self,
            devices::device_list,
            devices::device_rename,
            devices::device_revoke,
            devices::device_freeze,
            account_suggest,
            name_reverse,
            node_users,
            my_id,
            disconnect,
            platform,
            ticker_feed,
            open_https,
            list_identities,
            create_identity,
            auth_status,
            setup_master,
            unlock,
            lock,
            change_master,
            export_backup,
            import_backup,
            generate_recovery,
            recover,
            read_audit,
            ui_kv_get_all,
            ui_kv_set,
            catalog_load,
            catalog_save,
            server_load,
            server_save,
            profile_load,
            profile_save,
            chat_open,
            chat_tail,
            chat_before,
            chat_around,
            chat_append,
            chat_append_many,
            chat_unread_save,
            chat_search,
            #[cfg(desktop)]
            tray::tray_sync,
            #[cfg(desktop)]
            tray::tray_notify,
            #[cfg(desktop)]
            tray::tray_prefs_get,
            #[cfg(desktop)]
            tray::tray_prefs_set,
            #[cfg(desktop)]
            tray::tray_test_notify,
            #[cfg(desktop)]
            tray::app_quit
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _ev| {
            #[cfg(desktop)]
            tray::on_run_event(_app, &_ev);
        });
}

#[cfg(test)]
mod http_body_tests {
    use super::http_message;

    #[test]
    fn chunked_body_joins_pieces() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        let (code, body) = http_message(raw).unwrap();
        assert_eq!(code, 200);
        assert_eq!(body, "{\"a\":1}");
    }

    #[test]
    fn content_length_stops_at_the_declared_end() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\":1}TAIL";
        let (code, body) = http_message(raw).unwrap();
        assert_eq!(code, 200);
        assert_eq!(body, "{\"a\":1}");
    }
}

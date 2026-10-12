use serde_json::{Value, json};
use tauri::AppHandle;

use crate::data_dir;

// ── UI 状态持久化 KV：前端 localStorage 的耐久镜像，跨 webview 源/重建不丢（存 app_data_dir/ui-state.json）。 ──
#[tauri::command]
pub(crate) fn ui_kv_get_all(app: AppHandle) -> Result<String, String> {
    Ok(
        std::fs::read_to_string(data_dir(&app)?.join("ui-state.json"))
            .unwrap_or_else(|_| "{}".to_string()),
    )
}

#[tauri::command]
pub(crate) fn ui_kv_set(app: AppHandle, key: String, value: Option<String>) -> Result<(), String> {
    let path = data_dir(&app)?.join("ui-state.json");
    let mut obj: serde_json::Map<String, Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    match value {
        Some(v) => {
            obj.insert(key, Value::String(v));
        }
        None => {
            obj.remove(&key);
        }
    }
    std::fs::write(
        &path,
        serde_json::to_string(&obj).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
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
    if s.is_empty()
        || s.len() > 128
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':' || c == '.')
    {
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
pub(crate) fn catalog_load(app: AppHandle, user: String) -> Result<String, String> {
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
pub(crate) fn catalog_save(
    app: AppHandle,
    user: String,
    kind: String,
    data: String,
) -> Result<(), String> {
    if data.len() > 8 * 1024 * 1024 {
        return Err("too_large".into());
    }
    let user = identity_key(&user)?;
    let kind = catalog_kind(&kind)?;
    let items: Vec<Value> = serde_json::from_str(&data).map_err(|_| "bad_json".to_string())?;
    let conn = open_catalog(&app)?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM record WHERE user=?1 AND kind=?2",
        rusqlite::params![user, kind],
    )
    .map_err(|e| e.to_string())?;
    for (ord, item) in items.iter().take(8000).enumerate() {
        let Some(id) = item.get("id").and_then(|v| v.as_str()).and_then(record_id) else {
            continue;
        };
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
pub(crate) fn server_load(app: AppHandle) -> Result<String, String> {
    let conn = open_catalog(&app)?;
    let mut stmt = conn
        .prepare("SELECT body FROM server ORDER BY ord")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        if let Ok(v) = serde_json::from_str::<Value>(&row.map_err(|e| e.to_string())?) {
            out.push(v);
        }
    }
    serde_json::to_string(&out).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn server_save(app: AppHandle, data: String) -> Result<(), String> {
    if data.len() > 1024 * 1024 {
        return Err("too_large".into());
    }
    let items: Vec<Value> = serde_json::from_str(&data).map_err(|_| "bad_json".to_string())?;
    let conn = open_catalog(&app)?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM server", [])
        .map_err(|e| e.to_string())?;
    for (ord, item) in items.iter().take(50).enumerate() {
        let Some(id) = item.get("id").and_then(|v| v.as_str()).and_then(record_id) else {
            continue;
        };
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
pub(crate) fn profile_load(app: AppHandle) -> Result<String, String> {
    let conn = open_catalog(&app)?;
    let mut stmt = conn
        .prepare("SELECT id, body FROM profile")
        .map_err(|e| e.to_string())?;
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
pub(crate) fn profile_save(app: AppHandle, id: String, data: String) -> Result<(), String> {
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
    v.get("id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn ts_of(v: &Value) -> u64 {
    v.get("ts")
        .and_then(|n| n.as_u64().or_else(|| n.as_i64().map(|x| x.max(0) as u64)))
        .unwrap_or(0)
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
    let vals: Vec<Value> = bodies
        .into_iter()
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect();
    serde_json::to_string(&vals).map_err(|e| e.to_string())
}

const MSG_CAP: i64 = 10_000;

fn insert_msgs(
    conn: &mut rusqlite::Connection,
    conv: &str,
    fresh: &[Value],
) -> Result<usize, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut stored = 0usize;
    for (i, m) in fresh.iter().enumerate() {
        let ts = ts_of(m);
        let mut id = msg_id(m);
        if id.is_empty() {
            id = format!("m{ts}-{i}");
        }
        let max_ord: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(ord), -1) FROM msg WHERE conv=?1",
                [conv],
                |r| r.get(0),
            )
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
        .query_row("SELECT COUNT(*) FROM msg WHERE conv=?1", [conv], |r| {
            r.get(0)
        })
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

fn window_bodies(
    conn: &rusqlite::Connection,
    conv: &str,
    end: i64,
    limit: i64,
) -> Result<Vec<String>, String> {
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
        .query_map(rusqlite::params![conv, end, limit], |r| {
            r.get::<_, String>(0)
        })
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
pub(crate) fn chat_open(app: AppHandle, user: String) -> Result<String, String> {
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
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
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
        let mut stmt = conn
            .prepare("SELECT conv, n FROM unread WHERE n > 0")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (id, n) = row.map_err(|e| e.to_string())?;
            unread.insert(id, json!(n));
        }
    }
    serde_json::to_string(&json!({"v": 2, "unread": unread, "convos": convos}))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn chat_tail(
    app: AppHandle,
    user: String,
    conv: String,
    limit: u32,
) -> Result<String, String> {
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
pub(crate) fn chat_before(
    app: AppHandle,
    user: String,
    conv: String,
    before: u32,
    limit: u32,
) -> Result<String, String> {
    let conn = open_chat_db(&app, &user)?;
    let conv = conv_key(&conv)?;
    let n = (limit.max(1) as i64).min(200);
    bodies_to_json(window_bodies(&conn, &conv, before as i64, n)?)
}

#[tauri::command]
pub(crate) fn chat_around(
    app: AppHandle,
    user: String,
    conv: String,
    id: String,
    limit: u32,
) -> Result<String, String> {
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
        .query_map(rusqlite::params![conv, start, end], |r| {
            r.get::<_, String>(0)
        })
        .map_err(|e| e.to_string())?;
    let mut bodies = Vec::new();
    for row in rows {
        bodies.push(row.map_err(|e| e.to_string())?);
    }
    let msgs: Vec<Value> = bodies
        .into_iter()
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect();
    serde_json::to_string(&json!({"start": start, "msgs": msgs})).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn chat_append(
    app: AppHandle,
    user: String,
    conv: String,
    msg: String,
) -> Result<bool, String> {
    let mut conn = open_chat_db(&app, &user)?;
    let conv = conv_key(&conv)?;
    let value: Value = serde_json::from_str(&msg).map_err(|_| "bad_json".to_string())?;
    Ok(insert_msgs(&mut conn, &conv, &[value])? > 0)
}

#[tauri::command]
pub(crate) fn chat_append_many(
    app: AppHandle,
    user: String,
    conv: String,
    data: String,
) -> Result<u32, String> {
    if data.len() > 8 * 1024 * 1024 {
        return Err("too_large".into());
    }
    let mut conn = open_chat_db(&app, &user)?;
    let conv = conv_key(&conv)?;
    let values: Vec<Value> = serde_json::from_str(&data).map_err(|_| "bad_json".to_string())?;
    Ok(insert_msgs(&mut conn, &conv, &values)? as u32)
}

#[tauri::command]
pub(crate) fn chat_unread_save(app: AppHandle, user: String, data: String) -> Result<(), String> {
    let conn = open_chat_db(&app, &user)?;
    let unread: Value = serde_json::from_str(&data).map_err(|_| "bad_json".to_string())?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM unread", [])
        .map_err(|e| e.to_string())?;
    if let Some(map) = unread.as_object() {
        for (id, n) in map {
            let Ok(key) = conv_key(id) else { continue };
            let count = n.as_i64().unwrap_or(0);
            if count > 0 {
                tx.execute(
                    "INSERT INTO unread(conv, n) VALUES (?1, ?2)",
                    rusqlite::params![key, count],
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn chat_search(
    app: AppHandle,
    user: String,
    conv: String,
    q: String,
    limit: u32,
) -> Result<String, String> {
    let conn = open_chat_db(&app, &user)?;
    let conv = conv_key(&conv)?;
    let n = (limit.max(1) as i64).min(80);
    let needle = format!("%{}%", q.trim().replace('%', ""));
    let mut stmt = conn
        .prepare("SELECT body FROM msg WHERE conv=?1 AND (?2 = '%%' OR body LIKE ?2) ORDER BY ord DESC LIMIT ?3")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![conv, needle, n], |r| {
            r.get::<_, String>(0)
        })
        .map_err(|e| e.to_string())?;
    let mut bodies = Vec::new();
    for row in rows {
        bodies.push(row.map_err(|e| e.to_string())?);
    }
    bodies.reverse();
    bodies_to_json(bodies)
}

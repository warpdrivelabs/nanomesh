//! Tauri 桥接：把界面 IPC 调用转发到进程内的 `nm-client`(iroh)。
//! 原生端(desktop / iOS / android)运行本模块；Web 端改走 nm-gateway。
//! 前端资源在 ../ui（静态壳，generate_context! 编译期内嵌；build.rs 声明 rerun-if-changed）。
//! App 图标源 ../../nanomesh-app.png（tauri icon 生成 icons/*，generate_context! 内嵌为窗口图标）。
//! 实体目录：点实体看详情(可复制公钥) + 手动添加实体（前端本地，按身份隔离；用户菜单可复制当前用户 id）。
//! UI 状态(节点服务/身份名/偏好)经 ui_kv_* 持久化到 app_data_dir/ui-state.json，跨 webview 源不丢。
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

/// 列出全部用户身份（公钥 hex）。仅解锁后可用；种子加密存 `identities/<pubkey>.enc`。
#[tauri::command]
fn list_identities(app: AppHandle, state: State<'_, AppState>) -> Result<Vec<String>, String> {
    vk_of(&state)?; // 门禁：未解锁拒绝
    let dir = identities_dir(&app)?;
    let mut out = Vec::new();
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
                    "to": hex(&gram.receiver),                 // 群消息=群id；频道=频道id；私聊=本人id
                    "group": matches!(gram.kind(), nm_proto::GramKind::GroupMessage),
                    "channel": matches!(gram.kind(), nm_proto::GramKind::ChannelPublish),
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
    let seed = load_identity_seed(&app, &vk, user)?;
    let (client, session) = dial(seed, &mode, &node, relay_urls, pkarr_url, dns_origin).await?;
    finish_session(&app, &state, client, session, &display_name).await
}

/// 拨号到一个节点（同网优先、穿透兜底），返回 (客户端, 会话)。connect 与 node_users 共用。
async fn dial(
    seed: [u8; 32],
    mode: &str,
    node: &str,
    relay_urls: Vec<String>,
    pkarr_url: Option<String>,
    dns_origin: Option<String>,
) -> Result<(Client, Session), String> {
    let m = mode.trim().to_ascii_lowercase();
    let node = node.trim();

    // 纯同网模式：Minimal + 按地址直连（无 NAT 兜底）。
    if matches!(m.as_str(), "lan" | "local") {
        let addr = nm_transport::addr_from_string(node).map_err(|e| e.to_string())?;
        let client = Client::bind_local(seed).await.map_err(|e| e.to_string())?;
        let session = client.online(addr).await.map_err(|e| e.to_string())?;
        return Ok((client, session));
    }

    // nat/selfhost：输入可为公钥(hex) 或完整地址(JSON)。地址先试同网直连，失败转穿透。
    let (id, lan_addr) = match nm_transport::addr_from_string(node) {
        Ok(a) => (*a.id.as_bytes(), Some(a)),
        Err(_) => (parse_id(node)?, None),
    };
    if let Some(addr) = &lan_addr {
        if let Ok(c1) = Client::bind_local(seed).await {
            if let Ok(Ok(session)) =
                tokio::time::timeout(std::time::Duration::from_secs(3), c1.online(addr.clone())).await
            {
                return Ok((c1, session));
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
    let session = client.online_by_id(id).await.map_err(|e| e.to_string())?;
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
    let (client, session) = dial(seed, &mode, &node, relay_urls, pkarr_url, dns_origin).await?;
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

/// 取 blob 并返回 data:URI（带 app_data_dir/blobs 本地缓存，避免重复拉取）。
/// `reference` = "b3:<hash-hex>"；`home_node` = 实体归属节点 hex（可空，用于跨节点回源）。
#[tauri::command]
async fn blob_get(
    app: AppHandle,
    state: State<'_, AppState>,
    reference: String,
    home_node: String,
) -> Result<String, String> {
    use base64::Engine;
    let hexh = reference.strip_prefix("b3:").unwrap_or(&reference).trim();
    let hash = parse_id(hexh)?; // 64-hex → [u8;32]
    let dir = data_dir(&app)?.join("blobs");
    let _ = std::fs::create_dir_all(&dir);
    let cache = dir.join(format!("{hexh}.uri"));
    if let Ok(s) = std::fs::read_to_string(&cache) {
        return Ok(s); // 缓存命中
    }
    let home = if home_node.trim().is_empty() {
        Vec::new()
    } else {
        parse_id(home_node.trim()).map(|h| h.to_vec()).unwrap_or_default()
    };
    let session = session_of(&state).await?;
    let (data, mime) = session.blob_get(hash.to_vec(), home).await.map_err(|e| e.to_string())?;
    let mime = if mime.trim().is_empty() { "image/jpeg".to_string() } else { mime };
    let uri = format!(
        "data:{};base64,{}",
        mime,
        base64::engine::general_purpose::STANDARD.encode(&data)
    );
    let _ = std::fs::write(&cache, &uri);
    Ok(uri)
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

// ── 去中心命名（N1）──
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

/// 供前端 `platform.js` 探测系统（Win/Linux 自绘窗口三键 + 缩放热区；macOS 用系统交通灯）。
#[tauri::command]
fn platform() -> &'static str {
    std::env::consts::OS // "macos" | "windows" | "linux"
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .setup(|_app| {
            // Windows/Linux：关系统窗口装饰，改用前端自绘标题栏 + 缩放热区（js/platform.js）；
            // macOS 保留系统交通灯（tauri.conf.json 的 titleBarStyle:Overlay）。
            #[cfg(not(target_os = "macos"))]
            if let Some(w) = _app.get_webview_window("main") {
                let _ = w.set_decorations(false);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect,
            send_to,
            directory_query,
            update_profile,
            blob_put,
            blob_get,
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
            name_reverse,
            node_users,
            my_id,
            disconnect,
            platform,
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
            ui_kv_set
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

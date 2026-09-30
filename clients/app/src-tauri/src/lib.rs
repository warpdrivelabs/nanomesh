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

fn open_device_vault(app: &AppHandle, state: &State<'_, AppState>) -> Result<(), String> {
    if state.vault.lock().unwrap().is_some() {
        return Ok(());
    }
    let dir = data_dir(app)?;
    let key_path = dir.join("device.key");
    if auth::vault_exists(&dir) {
        if let Ok(pw) = std::fs::read_to_string(&key_path) {
            if let Ok(vk) = auth::unlock(&dir, pw.trim()) {
                *state.vault.lock().unwrap() = Some(vk);
                return Ok(());
            }
        }
        // 旧主口令库无法在无界面下打开。挪走后改由本机设备密钥建库，不再弹出解锁页。
        let legacy = dir.join(format!("vault.json.legacy-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)));
        std::fs::rename(dir.join("vault.json"), legacy).map_err(|e| e.to_string())?;
    }
    let mut raw = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
    let pw = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw);
    let vk = auth::setup(&dir, &pw)?;
    write_device_key(&dir, &pw)?;
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
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("注册中心响应不完整")?;
    let code: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
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
        "域名已登记，家节点尚未同步，请稍后再试。".into()
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
    let (_client, session) = dial(seed, "nat", node, Vec::new(), None, None).await?;
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
    let seed = if user.is_empty() {
        nm_transport::SecretKey::generate().to_bytes()
    } else {
        let vk = vk_of(&state)?;
        load_identity_seed(&app, &vk, &user)?
    };
    let (_probe_client, probe) = dial(seed, "nat", &node, Vec::new(), None, None).await?;
    let body = json!({ "local": local, "domain": domain, "password": password }).to_string();
    let got = probe.name_account("name.login", &body).await.map_err(|e| registry_phrase(&e.to_string()))?;
    drop(probe);
    let got = got.trim().to_ascii_lowercase();
    if user.is_empty() || got != user.to_ascii_lowercase() {
        return Err("not_key_owner".into());
    }
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
) -> Result<(), String> {
    open_device_vault(&app, &state)?;
    if password.chars().count() < 8 {
        return Err("password_short".into());
    }
    let (local, domain) = norm_account(&local, &domain)?;
    let user = user.trim().to_string();
    if user.is_empty() {
        return Err("not_key_owner".into());
    }
    let node = resolve_home_node(&registry, &domain).await?;
    let vk = vk_of(&state)?;
    let seed = load_identity_seed(&app, &vk, &user)?;
    let body = json!({ "local": local, "domain": domain, "password": password }).to_string();
    node_account(&node, seed, "name.reset", &body).await?;
    Ok(())
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
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("响应不完整")?;
    let code: u16 = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|c| c.parse().ok()).unwrap_or(0);
    if !(200..300).contains(&code) {
        return Err(format!("HTTP {code}"));
    }
    Ok((code, body.to_string()))
}

fn rate_num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn ticker_fx() -> Option<String> {
    let v = https_get_json("https://open.er-api.com/v6/latest/USD").ok()?;
    let rates = v.get("rates")?;
    let cny = rate_num(rates.get("CNY")?)?;
    let eur = rate_num(rates.get("EUR")?)?;
    let gbp = rate_num(rates.get("GBP")?)?;
    let jpy = rate_num(rates.get("JPY")?)?;
    if eur <= 0.0 || gbp <= 0.0 || jpy <= 0.0 {
        return None;
    }
    Some(format!(
        "汇率  美元/人民币 {cny:.2}  欧元/人民币 {:.2}  英镑/人民币 {:.2}  100日元/人民币 {:.2}",
        cny / eur,
        cny / gbp,
        cny / jpy * 100.0
    ))
}

fn gate_last(pair: &str) -> Option<f64> {
    let v = https_get_json(&format!(
        "https://api.gateio.ws/api/v4/spot/tickers?currency_pair={pair}"
    ))
    .ok()?;
    let row = v.as_array()?.first()?;
    row.get("last")?.as_str()?.parse().ok()
}

fn ticker_crypto() -> Option<String> {
    let btc = gate_last("BTC_USDT")?;
    let eth = gate_last("ETH_USDT")?;
    let sol = gate_last("SOL_USDT").unwrap_or(0.0);
    let sol_txt = if sol > 0.0 { format!("  SOL {sol:.2}") } else { String::new() };
    Some(format!("币价  BTC {btc:.0} 美元  ETH {eth:.0} 美元{sol_txt}"))
}

fn ticker_news() -> Option<String> {
    let (_code, body) = https_get_raw("https://www.chinanews.com.cn/rss/scroll-news.xml").ok()?;
    let item = body.split("<item>").nth(1)?;
    let raw = item.split("<title>").nth(1)?.split("</title>").next()?.trim();
    let title = raw
        .trim_start_matches("<![CDATA[")
        .trim_end_matches("]]>")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    let title = title.trim();
    if title.is_empty() { None } else { Some(format!("要闻  {title}")) }
}

fn ticker_weather() -> Option<String> {
    let v = https_get_json("https://api.open-meteo.com/v1/forecast?latitude=39.90,31.23,35.68,51.51,40.71,1.35&longitude=116.40,121.47,139.69,-0.13,-74.01,103.82&current=temperature_2m").ok()?;
    let names = ["北京", "上海", "东京", "伦敦", "纽约", "新加坡"];
    let temps: Vec<f64> = if let Some(arr) = v.as_array() {
        arr.iter().filter_map(|x| x.pointer("/current/temperature_2m").and_then(|t| t.as_f64())).collect()
    } else {
        v.pointer("/current/temperature_2m").and_then(|t| t.as_f64()).into_iter().collect()
    };
    if temps.is_empty() {
        return None;
    }
    let text = names.iter().zip(temps.iter()).map(|(n, t)| format!("{n} {t:.0}°")).collect::<Vec<_>>().join("  ");
    Some(format!("天气  {text}"))
}

/// 标题栏跑马灯。只返回勾选的来源；某一路失败就略过，不编造数字。
#[tauri::command]
async fn ticker_feed(fx: bool, crypto: bool, news: bool, weather: bool) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        let mut parts = Vec::new();
        if fx {
            if let Some(s) = ticker_fx() { parts.push(s); }
        }
        if crypto {
            if let Some(s) = ticker_crypto() { parts.push(s); }
        }
        if news {
            if let Some(s) = ticker_news() { parts.push(s); }
        }
        if weather {
            if let Some(s) = ticker_weather() { parts.push(s); }
        }
        let text = if parts.is_empty() { "行情暂时不可用".to_string() } else { parts.join("     ·     ") };
        json!({ "text": text })
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
    tauri::Builder::default()
        .manage(AppState::default())
        .setup(|_app| {
            // 系统交通灯改由标题栏右侧的红黄绿按钮承担，各平台都关掉原生装饰。
            if let Some(w) = _app.get_webview_window("main") {
                let _ = w.set_decorations(false);
                let _ = w.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));
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
            ensure_device,
            account_domains,
            account_register,
            account_login,
            account_passwd,
            account_reset,
            account_suggest,
            name_reverse,
            node_users,
            my_id,
            disconnect,
            platform,
            ticker_feed,
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

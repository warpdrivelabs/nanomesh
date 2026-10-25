//! 云模型共享管理台：Tauri **只管理**独立进程与身份。
//!
//! - 每个云模型 = 独立 `nm-llm-gatewayd` OS 进程 + 独立 nanomesh 身份（seed）
//! - 本模块：档案 CRUD、写运行时配置、拉起/停用/监控子进程
//! - 消费端代理仍在主应用进程内，复用登录身份（见 `llm_proxy.rs`）
//!
//! 见 `docs/LLM_KEY_SHARE_P2P_DESIGN.md` §0.5。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use nm_compute::METHOD_INFER;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;

use crate::{AppState, auth, data_dir, hex, pubkey_of_seed, vk_of};

/// 已拉起的 gateway 子进程。
pub(crate) struct Running {
    pub(crate) entity_id: [u8; 32],
    pub(crate) pid: u32,
    child: Child,
}

/// local_id → 子进程。
pub(crate) type LlmRuntime = Mutex<HashMap<String, Running>>;

fn models_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = data_dir(app)?.join("llm_models");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn runtime_dir(dir: &Path, local_id: &str) -> PathBuf {
    dir.join(format!("{local_id}.run"))
}

/// 本地可编辑的模型档案（不含明文 Key / seed）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelShare {
    pub local_id: String,
    pub entity_id: String,
    pub display_name: String,
    #[serde(default)]
    pub avatar: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub status_text: String,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub locale: String,
    #[serde(default)]
    pub home_node: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub relay_urls: Vec<String>,
    #[serde(default)]
    pub pkarr_url: Option<String>,
    #[serde(default)]
    pub dns_origin: Option<String>,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    pub model: String,
    #[serde(default = "default_metering")]
    pub metering: String,
    #[serde(default = "default_billing")]
    pub billing: String,
    #[serde(default)]
    pub price: String,
    #[serde(default = "default_true")]
    pub require_grant: bool,
    #[serde(default)]
    pub max_context: u32,
    #[serde(default = "default_concurrency")]
    pub concurrency: u32,
    #[serde(default)]
    pub has_key: bool,
    #[serde(default)]
    pub online: bool,
    /// 运行中进程 PID（仅 list 填充）。
    #[serde(default)]
    pub pid: Option<u32>,
}

fn default_mode() -> String {
    "nat".into()
}
fn default_provider() -> String {
    "openai".into()
}
fn default_base_url() -> String {
    "https://api.openai.com/v1".into()
}
fn default_metering() -> String {
    "token".into()
}
fn default_billing() -> String {
    "friend_quota".into()
}
fn default_true() -> bool {
    true
}
fn default_concurrency() -> u32 {
    4
}

fn path_json(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}
fn path_seed(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.seed.enc"))
}
fn path_key(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.key.enc"))
}

fn load_share(dir: &Path, id: &str) -> Result<ModelShare, String> {
    let raw = std::fs::read_to_string(path_json(dir, id)).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

fn save_share(dir: &Path, m: &ModelShare) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
    let p = path_json(dir, &m.local_id);
    std::fs::write(&p, raw).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn load_seed(dir: &Path, vk: &[u8; 32], id: &str) -> Result<[u8; 32], String> {
    let data = std::fs::read(path_seed(dir, id)).map_err(|_| "模型身份种子缺失".to_string())?;
    auth::decrypt_seed(vk, &data)
}

fn load_key(dir: &Path, vk: &[u8; 32], id: &str) -> Result<String, String> {
    let data = std::fs::read(path_key(dir, id)).map_err(|_| "未配置 API Key".to_string())?;
    let pt = auth::decrypt_blob(vk, &data)?;
    String::from_utf8(pt).map_err(|_| "Key 不是合法 UTF-8".into())
}

fn child_alive(child: &mut Child) -> bool {
    match child.try_wait() {
        Ok(None) => true,
        Ok(Some(_)) | Err(_) => false,
    }
}

/// 解析 nm-llm-gatewayd 可执行文件（与 Qwen/`nm-compute` 同级的独立二进制）。
fn resolve_gatewayd_bin() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("NM_LLM_GATEWAYD") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Ok(pb);
        }
    }
    // 开发：相对仓库 target
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target/debug/nm-llm-gatewayd"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target/release/nm-llm-gatewayd"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../.cargo-shared-target/debug/nm-llm-gatewayd"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../.cargo-shared-target/release/nm-llm-gatewayd"),
    ];
    // 也查 CARGO_TARGET_DIR / 共享 target(~/.cargo/config.toml 的 target-dir 约定,运行时无对应 env) / 相对 cwd
    let mut more = Vec::new();
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        more.push(PathBuf::from(&td).join("debug/nm-llm-gatewayd"));
        more.push(PathBuf::from(&td).join("release/nm-llm-gatewayd"));
    }
    if let Ok(home) = std::env::var("HOME") {
        more.push(PathBuf::from(&home).join(".cargo-shared-target/debug/nm-llm-gatewayd"));
        more.push(PathBuf::from(&home).join(".cargo-shared-target/release/nm-llm-gatewayd"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            more.push(dir.join("nm-llm-gatewayd"));
            more.push(dir.join("bin/nm-llm-gatewayd"));
        }
    }
    for c in candidates.into_iter().chain(more) {
        if c.is_file() {
            return Ok(c);
        }
    }
    // PATH
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let c = dir.join("nm-llm-gatewayd");
            if c.is_file() {
                return Ok(c);
            }
        }
    }
    Err(
        "找不到 nm-llm-gatewayd。请先 cargo build -p nm-llm-gatewayd，或设置环境变量 NM_LLM_GATEWAYD"
            .into(),
    )
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 为子进程写出 runtime 配置（seed 文件 + toml + 日志路径）。
fn write_runtime_files(
    dir: &Path,
    share: &ModelShare,
    seed: &[u8; 32],
    fallback: Option<&crate::state::ConnectArgs>,
) -> Result<(PathBuf, PathBuf, String), String> {
    let run = runtime_dir(dir, &share.local_id);
    std::fs::create_dir_all(&run).map_err(|e| e.to_string())?;

    let seed_path = run.join("seed.hex");
    std::fs::write(&seed_path, hex(seed)).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&seed_path, std::fs::Permissions::from_mode(0o600));
    }

    let key_env = format!(
        "NM_LLM_KEY_{}",
        share.local_id.chars().take(8).collect::<String>().to_uppercase()
    );

    // 连接参数：share 未配时继承 app 自身最近一次 connect 的 relay/pkarr/dns_origin——
    // 否则 selfhost 场景下 gatewayd 缺 pkarr 直接启动失败/落到 N0 公网设施,够不到自建 relay 上的 home。
    let mut relay_list: Vec<String> = share
        .relay_urls
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut pkarr = share
        .pkarr_url
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut dns = share
        .dns_origin
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(fb) = fallback {
        if relay_list.is_empty() {
            relay_list = fb
                .relay_urls
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if pkarr.is_none() {
            pkarr = fb
                .pkarr_url
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
        if dns.is_none() {
            dns = fb
                .dns_origin
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
    }
    let mut relays = String::new();
    if !relay_list.is_empty() {
        relays.push_str("relay = [");
        for (i, r) in relay_list.iter().enumerate() {
            if i > 0 {
                relays.push_str(", ");
            }
            relays.push_str(&format!("\"{}\"", toml_escape(r)));
        }
        relays.push_str("]\n");
    }

    let mut toml = String::new();
    toml.push_str(&format!(
        "home_node = \"{}\"\n",
        toml_escape(share.home_node.trim())
    ));
    toml.push_str(&format!(
        "display_name = \"{}\"\n",
        toml_escape(&share.display_name)
    ));
    toml.push_str(&format!(
        "seed_file = \"{}\"\n",
        toml_escape(&seed_path.to_string_lossy())
    ));
    toml.push_str(&format!("mode = \"{}\"\n", toml_escape(&share.mode)));
    toml.push_str(&relays);
    if let Some(p) = &pkarr {
        toml.push_str(&format!("pkarr = \"{}\"\n", toml_escape(p)));
    }
    if let Some(d) = &dns {
        toml.push_str(&format!("dns_origin = \"{}\"\n", toml_escape(d)));
    }
    toml.push_str(&format!("avatar_url = \"{}\"\n", toml_escape(&share.avatar)));
    toml.push_str(&format!("bio = \"{}\"\n", toml_escape(&share.bio)));
    toml.push_str(&format!(
        "status_text = \"{}\"\n",
        toml_escape(&share.status_text)
    ));
    toml.push_str(&format!("locale = \"{}\"\n", toml_escape(&share.locale)));
    toml.push_str(&format!("metering = \"{}\"\n", toml_escape(&share.metering)));
    toml.push_str(&format!("billing = \"{}\"\n", toml_escape(&share.billing)));
    toml.push_str(&format!("price_label = \"{}\"\n", toml_escape(&share.price)));
    toml.push_str("\n[[backends]]\n");
    toml.push_str("id = \"main\"\n");
    toml.push_str(&format!("provider = \"{}\"\n", toml_escape(&share.provider)));
    toml.push_str(&format!("base_url = \"{}\"\n", toml_escape(&share.base_url)));
    toml.push_str(&format!("key_env = \"{}\"\n", toml_escape(&key_env)));
    toml.push_str(&format!("models = [\"{}\"]\n", toml_escape(&share.model)));
    toml.push_str("\n[policy]\n");
    toml.push_str(&format!(
        "require_grant = {}\n",
        if share.require_grant { "true" } else { "false" }
    ));
    toml.push_str(&format!("max_concurrent = {}\n", share.concurrency.max(1)));
    toml.push_str("daily_token_budget = 2000000\n");

    let cfg_path = run.join("gateway.toml");
    std::fs::write(&cfg_path, toml).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&cfg_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok((cfg_path, run.join("gateway.log"), key_env))
}

fn reaping_offline(map: &mut HashMap<String, Running>) -> Vec<String> {
    let mut dead = Vec::new();
    for (id, r) in map.iter_mut() {
        if !child_alive(&mut r.child) {
            dead.push(id.clone());
        }
    }
    for id in &dead {
        map.remove(id);
    }
    dead
}

/// 列出本机全部云模型档案（含进程 online / pid）。
#[tauri::command]
pub(crate) async fn llm_model_list(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ModelShare>, String> {
    let _ = vk_of(&state)?;
    let dir = models_dir(&app)?;
    let mut running = state.llm_runtime.lock().await;
    let dead = reaping_offline(&mut running);
    for id in dead {
        let _ = app.emit(
            "core://event",
            json!({ "type": "llm_model", "event": "offline", "localId": id }),
        );
    }
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if let Ok(mut m) = load_share(&dir, id) {
                m.has_key = path_key(&dir, id).exists();
                if let Some(r) = running.get(id) {
                    m.online = true;
                    m.pid = Some(r.pid);
                } else {
                    m.online = false;
                    m.pid = None;
                }
                out.push(m);
            }
        }
    }
    out.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    Ok(out)
}

#[tauri::command]
pub(crate) fn llm_model_upsert(
    app: AppHandle,
    state: State<'_, AppState>,
    mut share: ModelShare,
) -> Result<ModelShare, String> {
    let vk = vk_of(&state)?;
    let dir = models_dir(&app)?;
    if share.model.trim().is_empty() {
        return Err("须填写上游模型名（如 gpt-4o）".into());
    }
    if share.display_name.trim().is_empty() {
        share.display_name = share.model.clone();
    }
    if share.local_id.trim().is_empty() {
        let mut rid = [0u8; 16];
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut rid);
        share.local_id = hex(&rid);
        let mut seed = nm_transport::SecretKey::generate().to_bytes();
        let pk = pubkey_of_seed(&seed);
        share.entity_id = hex(&pk);
        let enc = auth::encrypt_seed(&vk, &seed)?;
        {
            use zeroize::Zeroize;
            seed.zeroize();
        }
        let sp = path_seed(&dir, &share.local_id);
        std::fs::write(&sp, enc).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&sp, std::fs::Permissions::from_mode(0o600));
        }
    } else {
        let prev = load_share(&dir, &share.local_id)?;
        share.entity_id = prev.entity_id;
        if !path_seed(&dir, &share.local_id).exists() {
            return Err("模型种子丢失，请删除后重建".into());
        }
    }
    share.has_key = path_key(&dir, &share.local_id).exists();
    share.online = false;
    share.pid = None;
    save_share(&dir, &share)?;
    Ok(share)
}

#[tauri::command]
pub(crate) fn llm_model_set_key(
    app: AppHandle,
    state: State<'_, AppState>,
    local_id: String,
    api_key: String,
) -> Result<(), String> {
    let vk = vk_of(&state)?;
    let dir = models_dir(&app)?;
    let mut m = load_share(&dir, &local_id)?;
    let key = api_key.trim();
    if key.is_empty() {
        let _ = std::fs::remove_file(path_key(&dir, &local_id));
        m.has_key = false;
        save_share(&dir, &m)?;
        return Ok(());
    }
    let enc = auth::encrypt_blob(&vk, key.as_bytes())?;
    let kp = path_key(&dir, &local_id);
    std::fs::write(&kp, enc).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&kp, std::fs::Permissions::from_mode(0o600));
    }
    m.has_key = true;
    save_share(&dir, &m)?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn llm_model_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    local_id: String,
) -> Result<(), String> {
    let _ = vk_of(&state)?;
    let _ = llm_model_offline(app.clone(), state.clone(), local_id.clone()).await;
    let dir = models_dir(&app)?;
    let _ = std::fs::remove_file(path_json(&dir, &local_id));
    let _ = std::fs::remove_file(path_seed(&dir, &local_id));
    let _ = std::fs::remove_file(path_key(&dir, &local_id));
    let _ = std::fs::remove_dir_all(runtime_dir(&dir, &local_id));
    Ok(())
}

/// 上线：拉起独立 `nm-llm-gatewayd` 进程（独立身份）。
#[tauri::command]
pub(crate) async fn llm_model_online(
    app: AppHandle,
    state: State<'_, AppState>,
    local_id: String,
) -> Result<Value, String> {
    let vk = vk_of(&state)?;
    let dir = models_dir(&app)?;
    let share = load_share(&dir, &local_id)?;
    if share.home_node.trim().is_empty() {
        return Err("请先设置 home 节点".into());
    }
    let seed = load_seed(&dir, &vk, &local_id)?;
    let api_key = load_key(&dir, &vk, &local_id)?;
    let entity_id = pubkey_of_seed(&seed);

    {
        let mut g = state.llm_runtime.lock().await;
        reaping_offline(&mut g);
        if g.contains_key(&local_id) {
            return Err("该模型进程已在运行".into());
        }
    }

    let bin = resolve_gatewayd_bin()?;
    // share 未配 relay/pkarr 时继承 app 自身连接参数(last_connect),见 write_runtime_files 注释。
    let fb = state.last_connect.lock().await.clone();
    let (cfg_path, log_path, key_env) = write_runtime_files(&dir, &share, &seed, fb.as_ref())?;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("打开日志失败: {e}"))?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| format!("克隆日志句柄: {e}"))?;

    let mut cmd = Command::new(&bin);
    cmd.arg("serve")
        .arg("--config")
        .arg(&cfg_path)
        .env(&key_env, &api_key)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_err));

    let child = cmd.spawn().map_err(|e| {
        format!(
            "启动 nm-llm-gatewayd 失败: {e}（bin={}）",
            bin.display()
        )
    })?;
    let pid = child.id();

    // 短暂等待：若立刻退出则读日志末尾报错
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let mut child = child;
    if let Ok(Some(status)) = child.try_wait() {
        let tail = std::fs::read_to_string(&log_path).unwrap_or_default();
        let tip: String = tail.chars().rev().take(800).collect::<String>().chars().rev().collect();
        return Err(format!(
            "gatewayd 立即退出 (status={status})。日志尾部：\n{tip}"
        ));
    }

    state.llm_runtime.lock().await.insert(
        local_id.clone(),
        Running {
            entity_id,
            pid,
            child,
        },
    );

    let _ = app.emit(
        "core://event",
        json!({
            "type": "llm_model",
            "event": "online",
            "localId": local_id,
            "entityId": hex(&entity_id),
            "pid": pid,
        }),
    );

    Ok(json!({
        "localId": local_id,
        "entityId": hex(&entity_id),
        "name": share.display_name,
        "pid": pid,
        "logPath": log_path.to_string_lossy(),
        "bin": bin.to_string_lossy(),
    }))
}

/// 运行时监控：进程状态 + 日志尾部（供本机模型详情页）。
#[tauri::command]
pub(crate) async fn llm_model_runtime(
    app: AppHandle,
    state: State<'_, AppState>,
    local_id: String,
) -> Result<Value, String> {
    let _ = vk_of(&state)?;
    let dir = models_dir(&app)?;
    let share = load_share(&dir, &local_id)?;
    let log_path = runtime_dir(&dir, &local_id).join("gateway.log");
    let mut online = false;
    let mut pid: Option<u32> = None;
    {
        let mut g = state.llm_runtime.lock().await;
        reaping_offline(&mut g);
        if let Some(r) = g.get(&local_id) {
            online = true;
            pid = Some(r.pid);
        }
    }
    let log_tail = if log_path.exists() {
        let raw = std::fs::read_to_string(&log_path).unwrap_or_default();
        let chars: String = raw.chars().rev().take(12_000).collect::<String>().chars().rev().collect();
        chars
    } else {
        String::new()
    };
    let log_bytes = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
    Ok(json!({
        "localId": local_id,
        "entityId": share.entity_id,
        "displayName": share.display_name,
        "online": online,
        "pid": pid,
        "logPath": log_path.to_string_lossy(),
        "logBytes": log_bytes,
        "logTail": log_tail,
        "model": share.model,
        "provider": share.provider,
        "requireGrant": share.require_grant,
        "metering": share.metering,
        "billing": share.billing,
        "price": share.price,
        "concurrency": share.concurrency,
        "maxContext": share.max_context,
    }))
}

/// 停用：终止独立 gateway 进程。
#[tauri::command]
pub(crate) async fn llm_model_offline(
    app: AppHandle,
    state: State<'_, AppState>,
    local_id: String,
) -> Result<(), String> {
    let _ = vk_of(&state)?;
    if let Some(mut r) = state.llm_runtime.lock().await.remove(&local_id) {
        let eid = hex(&r.entity_id);
        let _ = r.child.kill();
        let _ = r.child.wait();
        let _ = app.emit(
            "core://event",
            json!({
                "type": "llm_model",
                "event": "offline",
                "localId": local_id,
                "entityId": eid,
            }),
        );
    }
    Ok(())
}

/// 用该模型身份签发 Grant（不必进程在线；用 seed 本地签发）。
#[tauri::command]
pub(crate) async fn llm_model_issue_grant(
    app: AppHandle,
    state: State<'_, AppState>,
    local_id: String,
    audience: String,
    days: i64,
) -> Result<Value, String> {
    let vk = vk_of(&state)?;
    let dir = models_dir(&app)?;
    let share = load_share(&dir, &local_id)?;
    let seed = load_seed(&dir, &vk, &local_id)?;
    let aud = crate::parse_id(&audience)?;
    let expires = if days <= 0 {
        0
    } else {
        (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64)
            + days * 86400
    };
    let client = nm_client::Client::bind_local(seed)
        .await
        .map_err(|e| e.to_string())?;
    let grant = client.issue_grant(aud, METHOD_INFER, "model.llm", expires);
    use base64::Engine;
    use prost::Message;
    let b64 = base64::engine::general_purpose::STANDARD.encode(grant.encode_to_vec());
    Ok(json!({
        "grantB64": b64,
        "issuer": share.entity_id,
        "audience": hex(&aud),
        "action": METHOD_INFER,
        "resource": "model.llm",
        "expires": expires,
        "displayName": share.display_name,
    }))
}

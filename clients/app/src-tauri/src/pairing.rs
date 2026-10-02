//! 设备迁移命令。协议与密码学见 pair.rs。
//! 新设备：`pair_request`（账号 + 密码，家节点把请求投给旧设备）或 `pair_ticket`（只填账号，生成迁移串）。
//! 旧设备：私聊收件循环把 `pair.request` 交给 `on_old_side`；或在安全设置里粘贴迁移串调 `pair_accept_ticket`。
//! 旧设备用户必须输入新设备上显示的核对码才能放行，防止陌生人发来请求后被误点允许。

use std::collections::HashMap;

use zeroize::Zeroize;

use super::*;
use crate::pair;

const EVENT: &str = "pair://event";
const MAX_PENDING: usize = 4;

pub struct NewSide {
    rid: String,
    xsec: x25519_dalek::StaticSecret,
    xpk: [u8; 32],
    eph: [u8; 32],
    owner: Option<[u8; 32]>,
    key: Option<[u8; 32]>,
    name: String,
    _client: Client,
    _session: Arc<Session>,
}

pub struct OldSide {
    peer: [u8; 32],
    key: [u8; 32],
    sas: String,
    at: u64,
}

#[derive(Default)]
pub struct PairState {
    pub new_side: Mutex<Option<NewSide>>,
    pub old_side: std::sync::Mutex<HashMap<String, OldSide>>,
}

fn clean_text(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

fn device_label() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(o) = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output() {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| std::env::consts::OS.to_string())
}

fn device_or_label(device: String) -> String {
    let d = device.trim();
    if d.is_empty() { device_label() } else { d.to_string() }
}

fn emit(app: &AppHandle, v: Value) {
    let _ = app.emit(EVENT, v);
}

// ── 新设备 ──

async fn start_new(
    app: AppHandle,
    state: State<'_, AppState>,
    local: String,
    domain: String,
    password: Option<String>,
    registry: String,
    device: String,
) -> Result<Value, String> {
    open_device_vault(&app, &state)?;
    let (local, domain) = norm_account(&local, &domain)?;
    let node = resolve_home_node(&registry, &domain).await?;
    let eph_seed = nm_transport::SecretKey::generate().to_bytes();
    let (client, mut session) = dial(eph_seed, "nat", &node, Vec::new(), None, None).await?;
    let name = format!("{local}@{domain}");
    let owner = match password {
        Some(pw) => {
            let body = json!({ "local": local, "domain": domain, "password": pw }).to_string();
            let got = session.name_account("name.login", &body).await.map_err(|e| registry_phrase(&e.to_string()))?;
            let got = got.trim().to_ascii_lowercase();
            let pk = pair::unhex32(&got).map_err(|_| "家节点返回的公钥无效".to_string())?;
            if identities_dir(&app)?.join(format!("{got}.enc")).exists() {
                return Ok(json!({ "have": true, "user": got }));
            }
            Some(pk)
        }
        None => None,
    };
    let inbox = session.take_inbox().ok_or("inbox 已被占用")?;
    let session = Arc::new(session);
    let eph = session.id_bytes();
    let (xsec, xpk) = pair::keypair();
    let rid = pair::random_rid();
    let req = pair::Request {
        v: 1,
        rid: rid.clone(),
        xpk: hex(&xpk),
        device: clean_text(&device_or_label(device), 64),
        name: name.clone(),
        ts: pair::now_ms(),
    };
    let mut out = json!({ "rid": rid });
    match owner {
        Some(o) => {
            let body = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
            session.send_typed(o, pair::T_REQUEST, &body).await.map_err(|e| e.to_string())?;
        }
        None => {
            out["ticket"] = json!(pair::encode_ticket(&pair::Ticket { req, eph: hex(&eph) })?);
        }
    }
    *state.pair.new_side.lock().await = Some(NewSide {
        rid: rid.clone(),
        xsec,
        xpk,
        eph,
        owner,
        key: None,
        name,
        _client: client,
        _session: session,
    });
    spawn_new_listener(app.clone(), inbox, rid);
    Ok(out)
}

fn spawn_new_listener(app: AppHandle, mut inbox: tokio::sync::mpsc::UnboundedReceiver<nm_proto::Gram>, rid: String) {
    async_runtime::spawn(async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(pair::TTL_MS);
        loop {
            let gram = match tokio::time::timeout_at(deadline, inbox.recv()).await {
                Ok(Some(g)) => g,
                Ok(None) => break,
                Err(_) => {
                    if clear_new(&app, &rid).await {
                        emit(&app, json!({ "type": "expired", "rid": rid }));
                    }
                    break;
                }
            };
            let Some(p) = gram.payload.as_ref() else { continue };
            let Ok(sender) = <[u8; 32]>::try_from(gram.sender.as_slice()) else { continue };
            match on_new_side(&app, &rid, &p.type_url, &p.value, sender).await {
                Ok(true) => break,
                Ok(false) => {}
                Err(e) => {
                    if clear_new(&app, &rid).await {
                        emit(&app, json!({ "type": "failed", "rid": rid, "error": e }));
                    }
                    break;
                }
            }
        }
    });
}

async fn clear_new(app: &AppHandle, rid: &str) -> bool {
    let state = app.state::<AppState>();
    let mut g = state.pair.new_side.lock().await;
    if g.as_ref().is_some_and(|n| n.rid == rid) {
        *g = None;
        true
    } else {
        false
    }
}

/// 返回 Ok(true) 表示本次迁移已结束（成功、被拒或已被取代）。
async fn on_new_side(app: &AppHandle, rid: &str, ty: &str, body: &[u8], sender: [u8; 32]) -> Result<bool, String> {
    let state = app.state::<AppState>();
    let mut g = state.pair.new_side.lock().await;
    let Some(ns) = g.as_mut() else { return Ok(true) };
    if ns.rid != rid {
        return Ok(true);
    }
    match ty {
        pair::T_OFFER => {
            let Ok(o) = serde_json::from_slice::<pair::Offer>(body) else { return Ok(false) };
            if o.rid != ns.rid || ns.key.is_some() || ns.owner.is_some_and(|w| w != sender) {
                return Ok(false);
            }
            let xpk_old = pair::unhex32(&o.xpk)?;
            let s = pair::derive(&ns.xsec, &xpk_old, &ns.rid, &sender, &ns.eph, &ns.xpk, &xpk_old)?;
            ns.owner = Some(sender);
            ns.key = Some(s.key);
            emit(app, json!({ "type": "sas", "rid": ns.rid, "sas": s.sas }));
            Ok(false)
        }
        pair::T_GRANT => {
            let Ok(gr) = serde_json::from_slice::<pair::Grant>(body) else { return Ok(false) };
            if gr.rid != ns.rid || ns.owner != Some(sender) {
                return Ok(false);
            }
            let key = ns.key.ok_or("尚未完成核对")?;
            let mut plain = pair::open(&key, &gr.ct)?;
            if plain.len() != 32 {
                plain.zeroize();
                return Err("收到的私钥长度异常".into());
            }
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&plain);
            plain.zeroize();
            if pubkey_of_seed(&seed) != sender {
                seed.zeroize();
                return Err("收到的私钥与对方身份不符".into());
            }
            let vk = vk_of(&state)?;
            let enc = auth::encrypt_seed(&vk, &seed);
            seed.zeroize();
            let user = hex(&sender);
            auth::write_private(&identities_dir(app)?.join(format!("{user}.enc")), enc?)?;
            auth::audit(&data_dir(app)?, "pair:received");
            let name = ns.name.clone();
            *g = None;
            emit(app, json!({ "type": "done", "rid": rid, "user": user, "name": name }));
            Ok(true)
        }
        pair::T_DENY => {
            let Ok(d) = serde_json::from_slice::<pair::Deny>(body) else { return Ok(false) };
            if d.rid != ns.rid || ns.owner != Some(sender) {
                return Ok(false);
            }
            *g = None;
            emit(app, json!({ "type": "denied", "rid": rid }));
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// 新设备：输入账号和密码，向在线的旧设备请求迁移。本机已有该账号私钥时返回 `{have:true}`。
#[tauri::command]
pub async fn pair_request(
    app: AppHandle,
    state: State<'_, AppState>,
    local: String,
    domain: String,
    password: String,
    registry: String,
    device: String,
) -> Result<Value, String> {
    start_new(app, state, local, domain, Some(password), registry, device).await
}

/// 新设备：只填账号，生成一次性迁移串，交给旧设备粘贴。
#[tauri::command]
pub async fn pair_ticket(
    app: AppHandle,
    state: State<'_, AppState>,
    local: String,
    domain: String,
    registry: String,
    device: String,
) -> Result<Value, String> {
    start_new(app, state, local, domain, None, registry, device).await
}

#[tauri::command]
pub async fn pair_cancel(state: State<'_, AppState>) -> Result<(), String> {
    *state.pair.new_side.lock().await = None;
    Ok(())
}

// ── 旧设备 ──

/// 私聊收件循环遇到迁移类消息时调用；只处理请求，其余类型属于新设备一侧。
pub async fn on_old_side(app: &AppHandle, ty: &str, body: &[u8], sender: &[u8]) {
    if ty != pair::T_REQUEST {
        return;
    }
    let Ok(peer) = <[u8; 32]>::try_from(sender) else { return };
    let Ok(req) = serde_json::from_slice::<pair::Request>(body) else { return };
    if let Ok(v) = offer_for(app, req, peer).await {
        emit(app, v);
    }
}

async fn offer_for(app: &AppHandle, req: pair::Request, peer: [u8; 32]) -> Result<Value, String> {
    if req.v != 1 || !pair::fresh(req.ts) {
        return Err("迁移请求已过期".into());
    }
    if req.rid.is_empty() || req.rid.len() > 64 {
        return Err("迁移请求无效".into());
    }
    let state = app.state::<AppState>();
    let session = session_of(&state).await?;
    let my_id = session.id_bytes();
    if peer == my_id {
        return Err("不能迁移给本机当前身份".into());
    }
    let xpk_new = pair::unhex32(&req.xpk)?;
    let (xsec, xpk_old) = pair::keypair();
    let s = pair::derive(&xsec, &xpk_new, &req.rid, &my_id, &peer, &xpk_new, &xpk_old)?;
    {
        let mut m = state.pair.old_side.lock().unwrap();
        m.retain(|_, o| pair::fresh(o.at));
        if m.contains_key(&req.rid) {
            return Err("重复的迁移请求".into());
        }
        if m.len() >= MAX_PENDING {
            return Err("待处理的迁移请求过多".into());
        }
        m.insert(req.rid.clone(), OldSide { peer, key: s.key, sas: s.sas, at: pair::now_ms() });
    }
    let offer = pair::Offer { v: 1, rid: req.rid.clone(), xpk: hex(&xpk_old) };
    let body = serde_json::to_vec(&offer).map_err(|e| e.to_string())?;
    session.send_typed(peer, pair::T_OFFER, &body).await.map_err(|e| e.to_string())?;
    Ok(json!({
        "type": "request",
        "rid": req.rid,
        "device": clean_text(&req.device, 64),
        "name": clean_text(&req.name, 128),
        "me": hex(&my_id),
    }))
}

/// 旧设备：粘贴新设备生成的迁移串，返回待确认的请求（与收到推送的请求同形）。
#[tauri::command]
pub async fn pair_accept_ticket(app: AppHandle, ticket: String) -> Result<Value, String> {
    let t = pair::decode_ticket(&ticket)?;
    let peer = pair::unhex32(&t.eph).map_err(|_| "迁移串格式错误".to_string())?;
    offer_for(&app, t.req, peer).await
}

/// 旧设备：核对码一致后放行，把当前身份的私钥加密发给新设备。
#[tauri::command]
pub async fn pair_approve(app: AppHandle, state: State<'_, AppState>, rid: String, sas: String) -> Result<(), String> {
    let typed: String = sas.chars().filter(|c| c.is_ascii_digit()).collect();
    let pending = {
        let mut m = state.pair.old_side.lock().unwrap();
        let ok = match m.get(&rid) {
            None => return Err("迁移请求不存在或已处理".into()),
            Some(o) if !pair::fresh(o.at) => {
                m.remove(&rid);
                return Err("迁移请求已过期".into());
            }
            Some(o) => o.sas.chars().filter(|c| c.is_ascii_digit()).collect::<String>() == typed,
        };
        if !ok {
            return Err("核对码不一致".into());
        }
        m.remove(&rid).ok_or("迁移请求不存在或已处理")?
    };
    let session = session_of(&state).await?;
    let my_id = session.id_bytes();
    let vk = vk_of(&state)?;
    let mut seed = load_identity_seed(&app, &vk, &hex(&my_id))?;
    let ct = pair::seal(&pending.key, &seed);
    seed.zeroize();
    let grant = pair::Grant { v: 1, rid, ct: ct? };
    let body = serde_json::to_vec(&grant).map_err(|e| e.to_string())?;
    session.send_typed(pending.peer, pair::T_GRANT, &body).await.map_err(|e| e.to_string())?;
    auth::audit(&data_dir(&app)?, "pair:grant");
    Ok(())
}

#[tauri::command]
pub async fn pair_reject(app: AppHandle, state: State<'_, AppState>, rid: String) -> Result<(), String> {
    let pending = state.pair.old_side.lock().unwrap().remove(&rid);
    if let Some(p) = pending {
        if let Ok(session) = session_of(&state).await {
            let body = serde_json::to_vec(&pair::Deny { v: 1, rid }).map_err(|e| e.to_string())?;
            let _ = session.send_typed(p.peer, pair::T_DENY, &body).await;
        }
        if let Ok(dir) = data_dir(&app) {
            auth::audit(&dir, "pair:reject");
        }
    }
    Ok(())
}

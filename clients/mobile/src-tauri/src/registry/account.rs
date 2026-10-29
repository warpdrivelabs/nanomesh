use serde_json::{Value, json};
use tauri::{AppHandle, State};

use crate::{
    AppState, connect, create_identity, devices, dial, list_identities, load_identity_seed,
    session_of, vk_of,
};

use super::http::{http_get_json, query_escape, registry_base, registry_phrase, resolve_home_node};
use super::vault::open_device_vault;

// 全网已切自建 relay/pkarr（见部署记录）：home node 只把地址发布到自建 pkarr，不在 N0。
// 故账户登录/注册一律走 selfhost，经自建 pkarr 按公钥发现 home node；否则 N0 发现不到→超时。
// relay/pkarr **可配置**：前端经命令参数传入（存 localStorage，可改）；为空则回退到下方默认
// （照 registry_base 的"空→默认、非空→用传入"模式，换 relay 域名无需重编 app）。
const DEFAULT_SELFHOST_RELAY: &str = "https://relay.robot.link:8443";
const DEFAULT_SELFHOST_PKARR: &str = "https://relay.robot.link:8444/pkarr";
/// 归一化账户流程的自建基础设施参数：传入为空则用默认。返回 (mode, relay_urls, pkarr_url, dns_origin)。
pub(crate) fn selfhost_infra(
    relay_urls: &[String],
    pkarr_url: &Option<String>,
    dns_origin: &Option<String>,
) -> (&'static str, Vec<String>, Option<String>, Option<String>) {
    let relays: Vec<String> = relay_urls
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let relays = if relays.is_empty() {
        vec![DEFAULT_SELFHOST_RELAY.to_string()]
    } else {
        relays
    };
    let pkarr = pkarr_url
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SELFHOST_PKARR.to_string());
    let dns = dns_origin
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    ("selfhost", relays, Some(pkarr), dns)
}

pub(crate) fn norm_account(local: &str, domain: &str) -> Result<(String, String), String> {
    let local = local.trim().to_lowercase();
    let domain = domain.trim().to_lowercase();
    let ok = !local.is_empty()
        && local.len() <= 63
        && local
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
        && !domain.is_empty()
        && domain.len() <= 253
        && domain
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'));
    if !ok {
        return Err("用户名或域名不合法".into());
    }
    Ok((local, domain))
}

async fn node_account(
    node: &str,
    seed: [u8; 32],
    method: &str,
    body: &str,
    relay_urls: &[String],
    pkarr_url: &Option<String>,
    dns_origin: &Option<String>,
) -> Result<String, String> {
    let (m, relays, pkarr, dns) = selfhost_infra(relay_urls, pkarr_url, dns_origin);
    let (_client, session) = dial(seed, m, node, relays, pkarr, dns, None).await?;
    let out = session
        .name_account(method, body)
        .await
        .map_err(|e| registry_phrase(&e.to_string()))?;
    drop(session);
    Ok(out)
}

#[tauri::command]
pub(crate) async fn account_domains(registry: String) -> Result<Value, String> {
    let url = format!("{}/api/catalog", registry_base(&registry));
    http_get_json(&url).await
}

#[tauri::command]
pub(crate) async fn account_register(
    app: AppHandle,
    state: State<'_, AppState>,
    local: String,
    domain: String,
    nickname: String,
    password: String,
    registry: String,
    relay_urls: Vec<String>,
    pkarr_url: Option<String>,
    dns_origin: Option<String>,
) -> Result<Value, String> {
    open_device_vault(&app, &state)?;
    if password.chars().count() < 8 {
        return Err("密码至少 8 位".into());
    }
    let (local, domain) = norm_account(&local, &domain)?;
    let node = resolve_home_node(&registry, &domain).await?;
    let probe = nm_transport::SecretKey::generate().to_bytes();
    let body = json!({ "local": local, "domain": domain }).to_string();
    match node_account(&node, probe, "name.lookup", &body, &relay_urls, &pkarr_url, &dns_origin)
        .await
    {
        Ok(exists) if exists.trim() == "1" => return Err("name_taken".into()),
        Ok(_) => {}
        Err(e) if e.contains("unknown method") => {}
        Err(e) => return Err(e),
    }
    let user = create_identity(app.clone(), state.clone())?;
    let nick = if nickname.trim().is_empty() {
        format!("{local}@{domain}")
    } else {
        nickname.trim().to_string()
    };
    let (m, relays, pkarr, dns) = selfhost_infra(&relay_urls, &pkarr_url, &dns_origin);
    connect(
        app,
        state.clone(),
        user.clone(),
        m.into(),
        node.clone(),
        nick,
        relays,
        pkarr,
        dns,
    )
    .await?;
    let session = session_of(&state).await?;
    let body = json!({ "local": local, "domain": domain, "password": password }).to_string();
    session
        .name_account("name.register", &body)
        .await
        .map_err(|e| registry_phrase(&e.to_string()))?;
    Ok(json!({ "user": user, "name": format!("{local}@{domain}"), "node": node }))
}

#[tauri::command]
pub(crate) async fn account_login(
    app: AppHandle,
    state: State<'_, AppState>,
    local: String,
    domain: String,
    password: String,
    user: String,
    nickname: String,
    registry: String,
    relay_urls: Vec<String>,
    pkarr_url: Option<String>,
    dns_origin: Option<String>,
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
    let (m, relays, pkarr, dns) = selfhost_infra(&relay_urls, &pkarr_url, &dns_origin);
    let (_probe_client, probe) = dial(seed, m, &node, relays.clone(), pkarr.clone(), dns.clone(), None).await?;
    let body = json!({ "local": local, "domain": domain, "password": password }).to_string();
    let got = probe
        .name_account("name.login", &body)
        .await
        .map_err(|e| registry_phrase(&e.to_string()))?;
    drop(probe);
    let got = got.trim().to_ascii_lowercase();
    let user = if !user.is_empty() && got == user.to_ascii_lowercase() {
        user
    } else if got.len() == 64
        && got.chars().all(|c| c.is_ascii_hexdigit())
        && devices::has_local(&app, &got)
    {
        got
    } else {
        return Err("not_key_owner".into());
    };
    let nick = if nickname.trim().is_empty() {
        format!("{local}@{domain}")
    } else {
        nickname.trim().to_string()
    };
    connect(
        app,
        state,
        user.clone(),
        m.into(),
        node.clone(),
        nick,
        relays,
        pkarr,
        dns,
    )
    .await?;
    Ok(json!({ "user": user, "name": format!("{local}@{domain}"), "node": node }))
}

#[tauri::command]
pub(crate) async fn account_passwd(
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
    let body =
        json!({ "local": local, "domain": domain, "old": old_password, "password": password })
            .to_string();
    session
        .name_account("name.passwd", &body)
        .await
        .map_err(|e| registry_phrase(&e.to_string()))?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn account_reset(
    app: AppHandle,
    state: State<'_, AppState>,
    local: String,
    domain: String,
    password: String,
    user: String,
    registry: String,
    relay_urls: Vec<String>,
    pkarr_url: Option<String>,
    dns_origin: Option<String>,
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
        let seed =
            load_identity_seed(&app, &vk, &user).map_err(|_| "not_admin_device".to_string())?;
        node_account(&node, seed, "name.reset", &body, &relay_urls, &pkarr_url, &dns_origin)
            .await?;
        return Ok(user);
    }
    for pk in list_identities(app.clone(), state.clone())? {
        let Ok(seed) = load_identity_seed(&app, &vk, &pk) else {
            continue;
        };
        match node_account(&node, seed, "name.reset", &body, &relay_urls, &pkarr_url, &dns_origin)
            .await
        {
            Ok(_) => return Ok(pk),
            Err(e) if e.contains("not_key_owner") => {}
            Err(e) => return Err(e),
        }
    }
    Err("not_key_owner".into())
}

#[tauri::command]
pub(crate) async fn account_suggest(q: String, registry: String) -> Result<Value, String> {
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
        let exact = items
            .iter()
            .any(|it| it.get("domain").and_then(|d| d.as_str()) == Some(needle.as_str()));
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
            let domain = v
                .get("domain")
                .and_then(|d| d.as_str())
                .unwrap_or(needle.as_str());
            let pubkey = v.get("pubkey").and_then(|d| d.as_str()).unwrap_or("");
            Ok(json!({
                "ok": true,
                "status": "exact",
                "items": [{ "domain": domain, "pubkey": pubkey }],
            }))
        }
        Err(e) if e.contains("域名未登记") => {
            Ok(json!({ "ok": true, "items": [], "status": "unknown" }))
        }
        Err(e) if e.contains("域名已停用") => {
            Ok(json!({ "ok": true, "items": [], "status": "disabled" }))
        }
        Err(e) => Err(e),
    }
}

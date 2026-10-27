//! 消费方本地 OpenAI 兼容代理（**主应用进程内**，复用登录身份；不另起进程/身份）。
//! 可同时为多个远端模型各开一个端口；每端口有管理（启停/Token/Grant/超时）与监控（计数/延迟/日志）。
//! 见 `docs/LLM_KEY_SHARE_P2P_DESIGN.md` §0.5.3。

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::State as AxumState;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use nm_proto::Grant;
use prost::Message;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State, async_runtime};
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::CorsLayer;

use crate::{AppState, hex, parse_id, session_of, vk_of};

const LOG_CAP: usize = 80;

/// 可热更新的运行时配置。
struct ProxyLive {
    local_token: String,
    grant: Option<Grant>,
    timeout: Duration,
    model_hint: String,
    display_name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyLogEntry {
    pub ts_ms: u64,
    pub path: String,
    pub ok: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    #[serde(default)]
    pub error: String,
}

struct ProxyMetrics {
    started_at_ms: u64,
    requests: u64,
    success: u64,
    errors: u64,
    auth_fails: u64,
    in_flight: u32,
    latency_sum_ms: u64,
    last_latency_ms: u64,
    bytes_in: u64,
    bytes_out: u64,
    last_error: String,
    last_error_at_ms: u64,
    last_ok_at_ms: u64,
    recent: VecDeque<ProxyLogEntry>,
}

impl ProxyMetrics {
    fn new() -> Self {
        Self {
            started_at_ms: now_ms(),
            requests: 0,
            success: 0,
            errors: 0,
            auth_fails: 0,
            in_flight: 0,
            latency_sum_ms: 0,
            last_latency_ms: 0,
            bytes_in: 0,
            bytes_out: 0,
            last_error: String::new(),
            last_error_at_ms: 0,
            last_ok_at_ms: 0,
            recent: VecDeque::with_capacity(LOG_CAP),
        }
    }

    fn snapshot(&self, provider_hex: &str, listen: &str, live: &ProxyLive) -> ProxyDetail {
        let avg = if self.success + self.errors > 0 {
            self.latency_sum_ms / (self.success + self.errors)
        } else {
            0
        };
        ProxyDetail {
            provider_id: provider_hex.to_string(),
            display_name: live.display_name.clone(),
            listen: listen.to_string(),
            local_token: live.local_token.clone(),
            base_url: format!("http://{listen}/v1"),
            running: true,
            model: live.model_hint.clone(),
            timeout_secs: live.timeout.as_secs(),
            has_grant: live.grant.is_some(),
            started_at_ms: self.started_at_ms,
            uptime_secs: now_ms().saturating_sub(self.started_at_ms) / 1000,
            requests: self.requests,
            success: self.success,
            errors: self.errors,
            auth_fails: self.auth_fails,
            in_flight: self.in_flight,
            last_latency_ms: self.last_latency_ms,
            avg_latency_ms: avg,
            bytes_in: self.bytes_in,
            bytes_out: self.bytes_out,
            last_error: self.last_error.clone(),
            last_error_at_ms: self.last_error_at_ms,
            last_ok_at_ms: self.last_ok_at_ms,
            recent: self.recent.iter().cloned().collect(),
        }
    }

    fn push_log(&mut self, entry: ProxyLogEntry) {
        if self.recent.len() >= LOG_CAP {
            self.recent.pop_front();
        }
        self.recent.push_back(entry);
    }

    fn clear_log(&mut self) {
        self.recent.clear();
        self.requests = 0;
        self.success = 0;
        self.errors = 0;
        self.auth_fails = 0;
        self.latency_sum_ms = 0;
        self.last_latency_ms = 0;
        self.bytes_in = 0;
        self.bytes_out = 0;
        self.last_error.clear();
        self.last_error_at_ms = 0;
        self.last_ok_at_ms = 0;
        // 保留 started_at / in_flight
    }
}

/// 一条正在跑的代理。
pub(crate) struct ProxyRunning {
    pub provider_id: [u8; 32],
    pub provider_hex: String,
    pub listen: String,
    live: Arc<RwLock<ProxyLive>>,
    metrics: Arc<Mutex<ProxyMetrics>>,
    join: async_runtime::JoinHandle<()>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

pub(crate) type LlmProxies = Mutex<HashMap<String, ProxyRunning>>; // key = provider_hex

impl ProxyRunning {
    /// 聊天直调复用:取该代理当前的 (grant, timeout, model_hint) 快照(llm_chat 远端分支用,
    /// 避免聊天路径重复粘贴 Grant——代理面板配置过即生效)。
    pub(crate) async fn chat_params(&self) -> (Option<Grant>, Duration, String) {
        let l = self.live.read().await;
        (l.grant.clone(), l.timeout, l.model_hint.clone())
    }
}

#[derive(Clone)]
struct ProxyHttpState {
    app: AppHandle,
    provider: [u8; 32],
    provider_hex: String,
    live: Arc<RwLock<ProxyLive>>,
    metrics: Arc<Mutex<ProxyMetrics>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyStartArgs {
    pub provider_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub listen: String,
    #[serde(default = "default_token")]
    pub local_token: String,
    #[serde(default)]
    pub grant_b64: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyUpdateArgs {
    pub provider_id: String,
    #[serde(default)]
    pub local_token: Option<String>,
    #[serde(default)]
    pub grant_b64: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
}

fn default_token() -> String {
    "nm-local-token".into()
}
fn default_timeout() -> u64 {
    120
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyInfo {
    pub provider_id: String,
    pub display_name: String,
    pub listen: String,
    pub local_token: String,
    pub base_url: String,
    pub running: bool,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub timeout_secs: u64,
    #[serde(default)]
    pub has_grant: bool,
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub success: u64,
    #[serde(default)]
    pub errors: u64,
    #[serde(default)]
    pub in_flight: u32,
    #[serde(default)]
    pub last_latency_ms: u64,
    #[serde(default)]
    pub uptime_secs: u64,
    #[serde(default)]
    pub last_error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyDetail {
    pub provider_id: String,
    pub display_name: String,
    pub listen: String,
    pub local_token: String,
    pub base_url: String,
    pub running: bool,
    pub model: String,
    pub timeout_secs: u64,
    pub has_grant: bool,
    pub started_at_ms: u64,
    pub uptime_secs: u64,
    pub requests: u64,
    pub success: u64,
    pub errors: u64,
    pub auth_fails: u64,
    pub in_flight: u32,
    pub last_latency_ms: u64,
    pub avg_latency_ms: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub last_error: String,
    pub last_error_at_ms: u64,
    pub last_ok_at_ms: u64,
    pub recent: Vec<ProxyLogEntry>,
}

fn decode_grant(b64: &str) -> Result<Option<Grant>, String> {
    let s = b64.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.as_bytes()))
        .map_err(|e| format!("grant base64: {e}"))?;
    let g = Grant::decode(bytes.as_slice()).map_err(|e| format!("grant decode: {e}"))?;
    Ok(Some(g))
}

// ── per-provider Grant 落盘(聊天直聊与代理共用;「聊个天还得先开代理粘 Grant」的解法)──
// 存 b64 原文于 <data>/llm_grants/<provider_hex>.grant(0600)。Grant 是对方签发的调用凭证
// (自带签名/有效期,非本方密钥),明文落盘可接受;删除=撤回本机留存。

fn grants_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = crate::data_dir(app)?.join("llm_grants");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// 落盘保存(b64 原文);空串=删除。
pub(crate) fn save_grant_b64(app: &AppHandle, provider_hex: &str, b64: &str) -> Result<(), String> {
    let p = grants_dir(app)?.join(format!("{provider_hex}.grant"));
    let s = b64.trim();
    if s.is_empty() {
        let _ = std::fs::remove_file(&p);
        return Ok(());
    }
    decode_grant(s)?; // 先验格式,坏的不落盘
    std::fs::write(&p, s).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 读盘解析(无文件/坏文件=None)。
pub(crate) fn load_grant(app: &AppHandle, provider_hex: &str) -> Option<Grant> {
    let p = grants_dir(app).ok()?.join(format!("{provider_hex}.grant"));
    let b64 = std::fs::read_to_string(p).ok()?;
    decode_grant(&b64).ok().flatten()
}

/// 保存/删除某 provider 的 Grant(UI 直存,不必开代理)。返回当前是否已存。
#[tauri::command]
pub(crate) async fn llm_grant_save(
    app: AppHandle,
    state: State<'_, AppState>,
    provider_id: String,
    grant_b64: String,
) -> Result<bool, String> {
    let _ = vk_of(&state)?;
    let provider_hex = hex(&parse_id(&provider_id)?);
    save_grant_b64(&app, &provider_hex, &grant_b64)?;
    Ok(!grant_b64.trim().is_empty())
}

async fn pick_listen(preferred: &str) -> Result<(SocketAddr, Option<tokio::net::TcpListener>), String> {
    if !preferred.trim().is_empty() {
        let addr = preferred
            .parse::<SocketAddr>()
            .map_err(|e| format!("listen 地址非法: {e}"))?;
        return Ok((addr, None));
    }
    for port in 18081u16..=18100 {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        if let Ok(l) = tokio::net::TcpListener::bind(addr).await {
            return Ok((addr, Some(l)));
        }
    }
    Err("18081–18100 无可用端口".into())
}

async fn check_token(st: &ProxyHttpState, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let live = st.live.read().await;
    if live.local_token.is_empty() {
        return Ok(());
    }
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))
        .unwrap_or(auth);
    if token == live.local_token {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "invalid local token".into()))
    }
}

async fn handle_models(AxumState(st): AxumState<Arc<ProxyHttpState>>, headers: HeaderMap) -> Response {
    if let Err((code, msg)) = check_token(&st, &headers).await {
        {
            let mut m = st.metrics.lock().await;
            m.auth_fails = m.auth_fails.saturating_add(1);
            m.requests = m.requests.saturating_add(1);
            m.push_log(ProxyLogEntry {
                ts_ms: now_ms(),
                path: "/v1/models".into(),
                ok: false,
                status: code.as_u16(),
                latency_ms: 0,
                bytes_in: 0,
                bytes_out: 0,
                error: msg.clone(),
            });
        }
        return (code, msg).into_response();
    }
    let live = st.live.read().await;
    let id = if live.model_hint.is_empty() {
        "default".to_string()
    } else {
        live.model_hint.clone()
    };
    let body = json!({
        "object": "list",
        "data": [{
            "id": id,
            "object": "model",
            "owned_by": st.provider_hex,
            "nm_provider_id": st.provider_hex,
        }]
    });
    {
        let mut m = st.metrics.lock().await;
        m.requests = m.requests.saturating_add(1);
        m.success = m.success.saturating_add(1);
        m.last_ok_at_ms = now_ms();
        m.push_log(ProxyLogEntry {
            ts_ms: now_ms(),
            path: "/v1/models".into(),
            ok: true,
            status: 200,
            latency_ms: 0,
            bytes_in: 0,
            bytes_out: 0,
            error: String::new(),
        });
    }
    Json(body).into_response()
}

async fn handle_chat(
    AxumState(st): AxumState<Arc<ProxyHttpState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let t0 = Instant::now();
    let bytes_in = body.len() as u64;

    if let Err((code, msg)) = check_token(&st, &headers).await {
        {
            let mut m = st.metrics.lock().await;
            m.auth_fails = m.auth_fails.saturating_add(1);
            m.requests = m.requests.saturating_add(1);
            m.push_log(ProxyLogEntry {
                ts_ms: now_ms(),
                path: "/v1/chat/completions".into(),
                ok: false,
                status: code.as_u16(),
                latency_ms: 0,
                bytes_in,
                bytes_out: 0,
                error: msg.clone(),
            });
        }
        return (
            code,
            Json(json!({"error":{"message":msg,"type":"auth_error"}})),
        )
            .into_response();
    }

    {
        let mut m = st.metrics.lock().await;
        m.in_flight = m.in_flight.saturating_add(1);
        m.requests = m.requests.saturating_add(1);
        m.bytes_in = m.bytes_in.saturating_add(bytes_in);
    }

    let (timeout, grant, model_hint) = {
        let live = st.live.read().await;
        (live.timeout, live.grant.clone(), live.model_hint.clone())
    };

    let mut req_json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            finish_req(
                &st,
                false,
                400,
                t0,
                bytes_in,
                0,
                format!("bad json: {e}"),
            )
            .await;
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":{"message":format!("bad json: {e}"),"type":"invalid_request"}})),
            )
                .into_response();
        }
    };
    if req_json["model"].as_str().unwrap_or("").is_empty() && !model_hint.is_empty() {
        req_json["model"] = json!(model_hint);
    }
    req_json["stream"] = json!(false);
    let forward = serde_json::to_vec(&req_json).unwrap_or_else(|_| body.to_vec());

    let Some(app_st) = st.app.try_state::<AppState>() else {
        finish_req(&st, false, 503, t0, bytes_in, 0, "app state gone".into()).await;
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"message":"app state gone","type":"internal"}})),
        )
            .into_response();
    };
    let sess = match session_of(&app_st).await {
        Ok(s) => s,
        Err(e) => {
            finish_req(
                &st,
                false,
                503,
                t0,
                bytes_in,
                0,
                format!("未连接节点: {e}"),
            )
            .await;
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":{"message":format!("未连接节点: {e}"),"type":"not_connected"}})),
            )
                .into_response();
        }
    };

    match sess
        .infer_with_grant_timeout(st.provider, &forward, grant, timeout)
        .await
    {
        Ok(bytes) => {
            let out = bytes.len() as u64;
            finish_req(&st, true, 200, t0, bytes_in, out, String::new()).await;
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-nm-provider", st.provider_hex.as_str())
                .body(axum::body::Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(e) => {
            let msg = format!("p2p infer failed: {e}");
            finish_req(&st, false, 502, t0, bytes_in, 0, msg.clone()).await;
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": {
                        "message": msg,
                        "type": "p2p_upstream_error"
                    }
                })),
            )
                .into_response()
        }
    }
}

async fn finish_req(
    st: &ProxyHttpState,
    ok: bool,
    status: u16,
    t0: Instant,
    bytes_in: u64,
    bytes_out: u64,
    error: String,
) {
    let latency_ms = t0.elapsed().as_millis() as u64;
    let mut m = st.metrics.lock().await;
    m.in_flight = m.in_flight.saturating_sub(1);
    m.last_latency_ms = latency_ms;
    m.latency_sum_ms = m.latency_sum_ms.saturating_add(latency_ms);
    m.bytes_out = m.bytes_out.saturating_add(bytes_out);
    if ok {
        m.success = m.success.saturating_add(1);
        m.last_ok_at_ms = now_ms();
    } else {
        m.errors = m.errors.saturating_add(1);
        m.last_error = error.clone();
        m.last_error_at_ms = now_ms();
    }
    m.push_log(ProxyLogEntry {
        ts_ms: now_ms(),
        path: "/v1/chat/completions".into(),
        ok,
        status,
        latency_ms,
        bytes_in,
        bytes_out,
        error,
    });
}

async fn handle_health(AxumState(st): AxumState<Arc<ProxyHttpState>>) -> impl IntoResponse {
    let live = st.live.read().await;
    let m = st.metrics.lock().await;
    Json(json!({
        "ok": true,
        "provider": st.provider_hex,
        "model": live.model_hint,
        "requests": m.requests,
        "inFlight": m.in_flight,
    }))
}

async fn info_of(r: &ProxyRunning) -> ProxyInfo {
    let live = r.live.read().await;
    let m = r.metrics.lock().await;
    ProxyInfo {
        provider_id: r.provider_hex.clone(),
        display_name: live.display_name.clone(),
        listen: r.listen.clone(),
        local_token: live.local_token.clone(),
        base_url: format!("http://{}/v1", r.listen),
        running: true,
        model: live.model_hint.clone(),
        timeout_secs: live.timeout.as_secs(),
        has_grant: live.grant.is_some(),
        requests: m.requests,
        success: m.success,
        errors: m.errors,
        in_flight: m.in_flight,
        last_latency_ms: m.last_latency_ms,
        uptime_secs: now_ms().saturating_sub(m.started_at_ms) / 1000,
        last_error: m.last_error.clone(),
    }
}

/// 启动针对某一远端模型的本地代理。
#[tauri::command]
pub(crate) async fn llm_proxy_start(
    app: AppHandle,
    state: State<'_, AppState>,
    args: ProxyStartArgs,
) -> Result<ProxyInfo, String> {
    let _ = vk_of(&state)?;
    let _ = session_of(&state).await?;

    let provider = parse_id(&args.provider_id)?;
    let provider_hex = hex(&provider);
    {
        let g = state.llm_proxies.lock().await;
        if g.contains_key(&provider_hex) {
            return Err("该模型的代理已在运行".into());
        }
    }

    // Grant:粘了 → 验格式并**落盘**(以后聊天/重开代理都免再粘);没粘 → 读盘上留存的。
    let grant = match decode_grant(&args.grant_b64)? {
        Some(g) => {
            let _ = save_grant_b64(&app, &provider_hex, args.grant_b64.trim());
            Some(g)
        }
        None => load_grant(&app, &provider_hex),
    };
    let (addr, held) = pick_listen(&args.listen).await?;
    let listen = addr.to_string();
    let local_token = if args.local_token.trim().is_empty() {
        default_token()
    } else {
        args.local_token.trim().to_string()
    };
    let display_name = if args.display_name.trim().is_empty() {
        provider_hex[..8.min(provider_hex.len())].to_string()
    } else {
        args.display_name.trim().to_string()
    };

    let live = Arc::new(RwLock::new(ProxyLive {
        local_token: local_token.clone(),
        grant,
        timeout: Duration::from_secs(args.timeout_secs.max(30)),
        model_hint: args.model.trim().to_string(),
        display_name: display_name.clone(),
    }));
    let metrics = Arc::new(Mutex::new(ProxyMetrics::new()));

    let http_st = Arc::new(ProxyHttpState {
        app: app.clone(),
        provider,
        provider_hex: provider_hex.clone(),
        live: live.clone(),
        metrics: metrics.clone(),
    });

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let router = Router::new()
        .route("/healthz", get(handle_health))
        .route("/v1/models", get(handle_models))
        .route("/v1/chat/completions", post(handle_chat))
        .route("/models", get(handle_models))
        .route("/chat/completions", post(handle_chat))
        .layer(CorsLayer::permissive())
        .with_state(http_st);

    let listener = match held {
        Some(l) => l,
        None => tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("绑定 {listen} 失败: {e}"))?,
    };

    let apph = app.clone();
    let ph = provider_hex.clone();
    let listen2 = listen.clone();
    let join = async_runtime::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        });
        if let Err(e) = server.await {
            tracing::warn!("llm proxy serve: {e}");
        }
        if let Some(st) = apph.try_state::<AppState>() {
            st.llm_proxies.lock().await.remove(&ph);
        }
        let _ = apph.emit(
            "core://event",
            json!({ "type": "llm_proxy", "event": "stopped", "providerId": ph, "listen": listen2 }),
        );
    });

    state.llm_proxies.lock().await.insert(
        provider_hex.clone(),
        ProxyRunning {
            provider_id: provider,
            provider_hex: provider_hex.clone(),
            listen: listen.clone(),
            live,
            metrics,
            join,
            shutdown: Some(shutdown_tx),
        },
    );

    let base_url = format!("http://{listen}/v1");
    let _ = app.emit(
        "core://event",
        json!({
            "type": "llm_proxy",
            "event": "started",
            "providerId": provider_hex,
            "listen": listen,
            "baseUrl": base_url,
            "localToken": local_token,
            "displayName": display_name,
        }),
    );

    let g = state.llm_proxies.lock().await;
    let r = g.get(&provider_hex).ok_or_else(|| "代理启动后丢失".to_string())?;
    Ok(info_of(r).await)
}

/// 停止某一模型的本地代理。
#[tauri::command]
pub(crate) async fn llm_proxy_stop(
    app: AppHandle,
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<(), String> {
    let _ = vk_of(&state)?;
    let key = parse_id(&provider_id)
        .map(|id| hex(&id))
        .unwrap_or_else(|_| provider_id.trim().to_lowercase());
    if let Some(mut r) = state.llm_proxies.lock().await.remove(&key) {
        if let Some(tx) = r.shutdown.take() {
            let _ = tx.send(());
        }
        r.join.abort();
        let _ = app.emit(
            "core://event",
            json!({ "type": "llm_proxy", "event": "stopped", "providerId": key }),
        );
    }
    Ok(())
}

/// 列出本机正在运行的模型代理（含简要监控）。
#[tauri::command]
pub(crate) async fn llm_proxy_list(state: State<'_, AppState>) -> Result<Vec<ProxyInfo>, String> {
    let _ = vk_of(&state)?;
    let g = state.llm_proxies.lock().await;
    let mut out = Vec::with_capacity(g.len());
    for r in g.values() {
        out.push(info_of(r).await);
    }
    out.sort_by(|a, b| a.listen.cmp(&b.listen));
    Ok(out)
}

/// 查询某 provider 的代理状态（未运行则 running=false）。
#[tauri::command]
pub(crate) async fn llm_proxy_status(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ProxyInfo, String> {
    let _ = vk_of(&state)?;
    let key = parse_id(&provider_id)
        .map(|id| hex(&id))
        .unwrap_or_else(|_| provider_id.trim().to_lowercase());
    let g = state.llm_proxies.lock().await;
    if let Some(r) = g.get(&key) {
        return Ok(info_of(r).await);
    }
    Ok(ProxyInfo {
        provider_id: key,
        display_name: String::new(),
        listen: String::new(),
        local_token: default_token(),
        base_url: String::new(),
        running: false,
        model: String::new(),
        timeout_secs: default_timeout(),
        has_grant: false,
        requests: 0,
        success: 0,
        errors: 0,
        in_flight: 0,
        last_latency_ms: 0,
        uptime_secs: 0,
        last_error: String::new(),
    })
}

/// 完整监控详情（含最近请求日志）。
#[tauri::command]
pub(crate) async fn llm_proxy_detail(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ProxyDetail, String> {
    let _ = vk_of(&state)?;
    let key = parse_id(&provider_id)
        .map(|id| hex(&id))
        .unwrap_or_else(|_| provider_id.trim().to_lowercase());
    let g = state.llm_proxies.lock().await;
    let r = g.get(&key).ok_or_else(|| "代理未运行".to_string())?;
    let live = r.live.read().await;
    let m = r.metrics.lock().await;
    Ok(m.snapshot(&r.provider_hex, &r.listen, &live))
}

/// 热更新 Token / Grant / 超时 / 模型名（不停端口）。
#[tauri::command]
pub(crate) async fn llm_proxy_update(
    app: AppHandle,
    state: State<'_, AppState>,
    args: ProxyUpdateArgs,
) -> Result<ProxyInfo, String> {
    let _ = vk_of(&state)?;
    let key = parse_id(&args.provider_id)
        .map(|id| hex(&id))
        .unwrap_or_else(|_| args.provider_id.trim().to_lowercase());
    let g = state.llm_proxies.lock().await;
    let r = g.get(&key).ok_or_else(|| "代理未运行".to_string())?;
    {
        let mut live = r.live.write().await;
        if let Some(t) = args.local_token {
            let t = t.trim();
            if !t.is_empty() {
                live.local_token = t.to_string();
            }
        }
        if let Some(b64) = args.grant_b64 {
            live.grant = decode_grant(&b64)?;
        }
        if let Some(secs) = args.timeout_secs {
            live.timeout = Duration::from_secs(secs.max(30));
        }
        if let Some(m) = args.model {
            live.model_hint = m.trim().to_string();
        }
        if let Some(n) = args.display_name {
            let n = n.trim();
            if !n.is_empty() {
                live.display_name = n.to_string();
            }
        }
    }
    let info = info_of(r).await;
    let _ = app.emit(
        "core://event",
        json!({
            "type": "llm_proxy",
            "event": "updated",
            "providerId": key,
            "listen": info.listen,
        }),
    );
    Ok(info)
}

/// 清空计数与最近日志（不停代理）。
#[tauri::command]
pub(crate) async fn llm_proxy_clear_stats(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ProxyDetail, String> {
    let _ = vk_of(&state)?;
    let key = parse_id(&provider_id)
        .map(|id| hex(&id))
        .unwrap_or_else(|_| provider_id.trim().to_lowercase());
    let g = state.llm_proxies.lock().await;
    let r = g.get(&key).ok_or_else(|| "代理未运行".to_string())?;
    {
        let mut m = r.metrics.lock().await;
        m.clear_log();
    }
    let live = r.live.read().await;
    let m = r.metrics.lock().await;
    Ok(m.snapshot(&r.provider_hex, &r.listen, &live))
}

/// 连通性探测：经当前登录 Session 发一条极简 infer。
/// 用短超时（≤30s），避免代理默认 120s 让 UI「探测中」空等两分钟。
#[tauri::command]
pub(crate) async fn llm_proxy_probe(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<Value, String> {
    let _ = vk_of(&state)?;
    let key = parse_id(&provider_id)
        .map(|id| hex(&id))
        .unwrap_or_else(|_| provider_id.trim().to_lowercase());
    let (provider, grant, model, has_grant) = {
        let g = state.llm_proxies.lock().await;
        let r = g.get(&key).ok_or_else(|| "代理未运行".to_string())?;
        let live = r.live.read().await;
        (
            r.provider_id,
            live.grant.clone(),
            live.model_hint.clone(),
            live.grant.is_some(),
        )
    };
    let sess = session_of(&state).await?;
    // 空 model → 提供方用其 default_model；勿写死 "default"（多数上游无此模型名）。
    let mut body_val = json!({
        "messages": [{"role":"user","content":"ping"}],
        "max_tokens": 1,
        "stream": false,
    });
    if !model.is_empty() {
        body_val["model"] = json!(model);
    }
    let body = serde_json::to_vec(&body_val).map_err(|e| e.to_string())?;
    let probe_timeout = Duration::from_secs(30);
    let t0 = Instant::now();
    match sess
        .infer_with_grant_timeout(provider, &body, grant, probe_timeout)
        .await
    {
        Ok(bytes) => Ok(json!({
            "ok": true,
            "latencyMs": t0.elapsed().as_millis() as u64,
            "bytes": bytes.len(),
        })),
        Err(e) => {
            let mut msg = e.to_string();
            if msg.contains("timed out") || msg.contains("Timeout") {
                msg = if has_grant {
                    format!(
                        "{msg}（提供方未在时限内响应：请确认 gatewayd 在线、Home 节点一致、上游 Base URL/Key 可用）"
                    )
                } else {
                    format!(
                        "{msg}（若该模型要求 Grant，请先在代理配置中粘贴 Grant 后重试；否则检查 gatewayd 是否在线）"
                    )
                };
            }
            Ok(json!({
                "ok": false,
                "latencyMs": t0.elapsed().as_millis() as u64,
                "error": msg,
            }))
        }
    }
}

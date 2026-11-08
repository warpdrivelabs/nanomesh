//! `nm-relay-admin` — iroh-relay + iroh-dns(pkarr) 监控与管理控制台。
//!
//! 能力：
//! - 抓取本机 Prometheus 指标（relay :9090 / dns :9117）看流量、连接、pkarr 发布
//! - 作为 iroh-relay `access.http` 回调：记录对等节点连接、动态拉黑
//! - Web 管理台：总览 / 对等节点 / 黑名单 / pkarr 探针
//!
//! 部署时在 `/etc/iroh-relay/relay.toml` 配置：
//! ```toml
//! [access.http]
//! url = "http://127.0.0.1:9620/relay-auth"
//! bearer_token = "<与 --relay-auth-token 一致>"
//! ```

mod auth;
mod metrics;
mod pkarr_decode;
mod store;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use serde::Deserialize;
use serde_json::json;

use auth::Auth;
use metrics::MetricsClient;
use store::Store;

#[derive(Parser)]
#[command(name = "nm-relay-admin", about = "iroh-relay / pkarr 监控与管理控制台")]
struct Args {
    /// HTTP 监听地址（本机 access.http 回调 + 可选外网；生产建议仅 127.0.0.1，外网走 HTTPS）。
    #[arg(long, default_value = "127.0.0.1:9620")]
    listen: String,
    /// HTTPS 监听地址（外网管理台）。需同时提供 `--tls-cert` 与 `--tls-key`。
    #[arg(long, default_value = "0.0.0.0:8450")]
    https: String,
    /// TLS 证书（PEM）。
    #[arg(long)]
    tls_cert: Option<String>,
    /// TLS 私钥（PEM）。
    #[arg(long)]
    tls_key: Option<String>,
    /// 管理员凭据文件。
    #[arg(long, default_value = "relay-admin.state.json")]
    state: String,
    /// 拉黑/观测持久化文件。
    #[arg(long, default_value = "relay-admin-data.json")]
    data: String,
    /// iroh-relay Prometheus 地址。
    #[arg(long, default_value = "http://127.0.0.1:9090")]
    relay_metrics: String,
    /// iroh-dns-server Prometheus 地址。
    #[arg(long, default_value = "http://127.0.0.1:9117")]
    dns_metrics: String,
    /// 本机 pkarr HTTPS 根（用于探针 GET /pkarr/{key}）。
    #[arg(long, default_value = "https://127.0.0.1:8444")]
    pkarr_base: String,
    /// iroh-relay access.http 回调共享 Bearer（空=不校验，仅本机建议）。
    #[arg(long, default_value = "")]
    relay_auth_token: String,
    /// iroh-relay 的 systemd unit 名（用于读取启动时间/运行时长）。
    #[arg(long, default_value = "iroh-relay")]
    relay_unit: String,
    /// iroh-dns-server 的 systemd unit 名（用于读取启动时间/运行时长）。
    #[arg(long, default_value = "iroh-dns")]
    dns_unit: String,
}

#[derive(Clone)]
struct AppState {
    auth: Arc<Auth>,
    store: Arc<Store>,
    metrics: Arc<MetricsClient>,
    http: reqwest::Client,
    pkarr_base: String,
    relay_auth_token: String,
    /// 启用了公网 HTTPS 时 cookie 加 Secure。
    cookie_secure: bool,
    /// relay / dns 的 systemd unit 名（读取启动时间/运行时长）。
    relay_unit: String,
    dns_unit: String,
}

const INDEX: &str = include_str!("../web/index.html");
const APPJS: &str = include_str!("../web/app.js");
const CSS: &str = include_str!("../web/styles.css");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // rustls 0.23 需显式安装 crypto provider（与 nm-admind 一致）。
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nm_relay_admin=info".into()),
        )
        .init();
    let args = Args::parse();

    let (auth, initial) = Auth::load_or_init(args.state.clone().into())?;
    if let Some(pw) = initial {
        tracing::warn!(
            "首次运行：管理员账号 \"admin\"，随机初始密码 \"{}\"——仅本次显示，请立即登录修改",
            pw
        );
    }
    let store = Store::load_or_init(args.data.clone().into())?;
    let metrics = MetricsClient::new(&args.relay_metrics, &args.dns_metrics);
    let http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true) // 本机自签/内网探针
        .timeout(Duration::from_secs(8))
        .build()?;

    if args.relay_auth_token.trim().is_empty() {
        tracing::warn!("未设置 --relay-auth-token；/relay-auth 不对 Bearer 校验（请仅本机访问）");
    }

    let https_enabled = args.tls_cert.is_some() && args.tls_key.is_some();
    let state = AppState {
        auth: Arc::new(auth),
        store: Arc::new(store),
        metrics: Arc::new(metrics),
        http,
        pkarr_base: args.pkarr_base.trim_end_matches('/').to_string(),
        relay_auth_token: args.relay_auth_token,
        cookie_secure: https_enabled,
        relay_unit: args.relay_unit,
        dns_unit: args.dns_unit,
    };

    let app = Router::new()
        .route("/", get(|| async { html(INDEX) }))
        .route("/app.js", get(|| async { js(APPJS) }))
        .route("/styles.css", get(|| async { css(CSS) }))
        // iroh-relay access.http 回调（无管理登录）
        .route("/relay-auth", post(relay_auth))
        .route("/healthz", get(|| async { Json(json!({"ok": true})) }))
        // 管理 API
        .route("/api/session", get(session))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/change-password", post(change_password))
        .route("/api/overview", get(overview))
        .route("/api/peers", get(peers))
        .route("/api/blacklist", get(blacklist))
        .route("/api/ban", post(ban))
        .route("/api/unban", post(unban))
        .route("/api/pkarr", get(pkarr_overview))
        .route("/api/pkarr/{key}", get(pkarr_lookup))
        // 可观测性增强：节点地址详情 / DNS 内容 / relay 明细
        .route("/api/nodes", get(nodes_with_addrs))
        .route("/api/dns/records", get(dns_records))
        .route("/api/relay/detail", get(relay_detail))
        .with_state(state);

    match (&args.tls_cert, &args.tls_key) {
        (Some(cert), Some(key)) => {
            let https_addr: SocketAddr = args.https.parse()?;
            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
            let https_app = app.clone();
            tracing::info!(%https_addr, "HTTPS admin listening (public)");
            tokio::spawn(async move {
                if let Err(e) = axum_server::bind_rustls(https_addr, config)
                    .serve(https_app.into_make_service())
                    .await
                {
                    tracing::error!(%e, "HTTPS 管理台退出");
                }
            });
        }
        (None, None) => {}
        _ => anyhow::bail!("HTTPS 需要同时提供 --tls-cert 和 --tls-key"),
    }

    let addr: SocketAddr = args.listen.parse()?;
    tracing::info!(%addr, "HTTP listening (relay-auth / local)");
    println!("nm-relay-admin 已启动");
    println!("  HTTP   {}", args.listen);
    if https_enabled {
        println!("  HTTPS  {}", args.https);
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn html(s: &'static str) -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], s).into_response()
}
fn js(s: &'static str) -> Response {
    ([(header::CONTENT_TYPE, "application/javascript; charset=utf-8")], s).into_response()
}
fn css(s: &'static str) -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], s).into_response()
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| {
            c.split(';')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("ra_session=").map(|s| s.to_string()))
        })
}

fn session_ok(s: &AppState, headers: &HeaderMap) -> bool {
    cookie_token(headers)
        .map(|t| s.auth.valid(&t))
        .unwrap_or(false)
}

fn session_cookie(token: &str, secure: bool) -> String {
    let mut c = format!("ra_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=28800");
    if secure {
        c.push_str("; Secure");
    }
    c
}

async fn session(State(s): State<AppState>, headers: HeaderMap) -> Json<serde_json::Value> {
    Json(json!({
        "authed": session_ok(&s, &headers),
        "must_change": s.auth.must_change(),
    }))
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

async fn login(State(s): State<AppState>, headers: HeaderMap, Json(body): Json<LoginBody>) -> Response {
    let src = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        .unwrap_or_else(|| "direct".to_string());
    match s.auth.login(&body.username, &body.password, &src) {
        Some((token, must_change)) => (
            StatusCode::OK,
            [(header::SET_COOKIE, session_cookie(&token, s.cookie_secure))],
            Json(json!({"ok": true, "must_change": must_change})),
        )
            .into_response(),
        None => (StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "error": "用户名或密码错误"})))
            .into_response(),
    }
}

async fn logout(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(t) = cookie_token(&headers) {
        s.auth.logout(&t);
    }
    (
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            "ra_session=; Path=/; HttpOnly; Max-Age=0".to_string(),
        )],
        Json(json!({"ok": true})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct ChangePw {
    old: String,
    new: String,
}

async fn change_password(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChangePw>,
) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match s.auth.change_password(&body.old, &body.new) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e.to_string()})))
            .into_response(),
    }
}

/// iroh-relay access.http：POST，Header `X-Iroh-Endpoint-Id`，体无要求；回 `true`/`false`。
async fn relay_auth(State(s): State<AppState>, headers: HeaderMap, _req: Request) -> Response {
    if !s.relay_auth_token.is_empty() {
        let ok = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|t| t == s.relay_auth_token);
        if !ok {
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    }
    let id = headers
        .get("x-iroh-endpoint-id")
        .or_else(|| headers.get("X-Iroh-Endpoint-Id"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if id.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing X-Iroh-Endpoint-Id").into_response();
    }
    let allowed = s.store.on_access_check(&id);
    tracing::debug!(%id, allowed, "relay-auth");
    if allowed {
        (StatusCode::OK, "true").into_response()
    } else {
        // 非 200 或非 "true" 都会被 iroh 拒绝；显式 403 + false
        (StatusCode::FORBIDDEN, "false").into_response()
    }
}

async fn overview(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let m = s.metrics.scrape().await;
    let banned = s.store.blacklist().len();
    let peers = s.store.peer_count();
    Json(json!({
        "metrics": m,
        "peer_count": peers,
        "blacklist_count": banned,
        "pkarr_base": s.pkarr_base,
        "uptimes": service_uptimes(&s.relay_unit, &s.dns_unit),
    }))
    .into_response()
}

async fn peers(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let bl = s.store.blacklist();
    let rows: Vec<_> = s
        .store
        .peers_snapshot()
        .into_iter()
        .map(|p| {
            let banned = bl.iter().any(|b| store::Store::normalize_id(b) == store::Store::normalize_id(&p.id));
            json!({
                "id": p.id,
                "first_seen": p.first_seen,
                "last_seen": p.last_seen,
                "connects": p.connects,
                "last_allowed": p.last_allowed,
                "banned": banned,
                "note": p.note,
                // 粗粒度状态：60s 内见过视为 active
                "status": if banned { "banned" }
                    else if now_secs().saturating_sub(p.last_seen) <= 60 { "active" }
                    else if now_secs().saturating_sub(p.last_seen) <= 600 { "idle" }
                    else { "stale" },
            })
        })
        .collect();
    Json(json!({ "count": rows.len(), "peers": rows })).into_response()
}

async fn blacklist(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({ "blacklist": s.store.blacklist() })).into_response()
}

#[derive(Deserialize)]
struct BanBody {
    id: String,
    #[serde(default)]
    note: String,
}

async fn ban(State(s): State<AppState>, headers: HeaderMap, Json(body): Json<BanBody>) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match s.store.ban(&body.id, Some(&body.note)) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e.to_string()}))).into_response(),
    }
}

async fn unban(State(s): State<AppState>, headers: HeaderMap, Json(body): Json<BanBody>) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match s.store.unban(&body.id) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e.to_string()}))).into_response(),
    }
}

async fn pkarr_overview(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let m = s.metrics.scrape().await;
    let dns = &m.dns;
    Json(json!({
        "up": m.dns_up,
        "http_requests": dns.get("dns_server_http_requests_total").copied().unwrap_or(0.0),
        "http_success": dns.get("dns_server_http_requests_success_total").copied().unwrap_or(0.0),
        "http_error": dns.get("dns_server_http_requests_error_total").copied().unwrap_or(0.0),
        "pkarr_publish_update": dns.get("dns_server_pkarr_publish_update_total").copied().unwrap_or(0.0),
        "pkarr_publish_noop": dns.get("dns_server_pkarr_publish_noop_total").copied().unwrap_or(0.0),
        "store_inserted": dns.get("dns_server_store_packets_inserted_total").copied().unwrap_or(0.0),
        "store_updated": dns.get("dns_server_store_packets_updated_total").copied().unwrap_or(0.0),
        "store_removed": dns.get("dns_server_store_packets_removed_total").copied().unwrap_or(0.0),
        "store_expired": dns.get("dns_server_store_packets_expired_total").copied().unwrap_or(0.0),
        "note": "pkarr 存储为 redb（signed-packets-1.db）；列表请用 /api/pkarr/{z32} 探针查询单条",
    }))
    .into_response()
}

async fn pkarr_lookup(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let key = key.trim().trim_start_matches('/').to_string();
    if key.is_empty() || key.len() > 128 {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "invalid key"}))).into_response();
    }
    let url = format!("{}/pkarr/{}", s.pkarr_base, key);
    match s.http.get(&url).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let bytes = res.bytes().await.unwrap_or_default();
            Json(json!({
                "ok": status == 200,
                "status": status,
                "key": key,
                "bytes": bytes.len(),
                "preview_hex": hex_preview(&bytes, 64),
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------
// 可观测性增强：节点地址详情 / pkarr 全量记录 / DNS 内容 / relay 明细
// ---------------------------------------------------------------

/// 查询单个节点的 pkarr 记录并解码出 relay + 直连地址（不依赖 AppState，供并行闭包使用）。
async fn fetch_node_addrs_with(
    http: &reqwest::Client,
    pkarr_base: &str,
    node_id: &str,
) -> pkarr_decode::NodeAddrs {
    let Some(key) = pkarr_decode::to_pkarr_key(node_id) else {
        return pkarr_decode::NodeAddrs { ok: false, error: Some("bad node id".into()), ..Default::default() };
    };
    let Some(pk) = pkarr_decode::pubkey_bytes(node_id) else {
        return pkarr_decode::NodeAddrs { ok: false, error: Some("bad pubkey".into()), ..Default::default() };
    };
    let url = format!("{pkarr_base}/pkarr/{key}");
    match http.get(&url).send().await {
        Ok(res) if res.status().is_success() => match res.bytes().await {
            Ok(b) => pkarr_decode::decode_relay_payload(&pk, &b),
            Err(e) => pkarr_decode::NodeAddrs { ok: false, error: Some(e.to_string()), ..Default::default() },
        },
        Ok(res) => pkarr_decode::NodeAddrs {
            ok: false,
            error: Some(format!("pkarr HTTP {}", res.status().as_u16())),
            ..Default::default()
        },
        Err(e) => pkarr_decode::NodeAddrs { ok: false, error: Some(e.to_string()), ..Default::default() },
    }
}

/// `/api/nodes` — 节点清单，含 relay 连接次数、状态、以及从 pkarr 解出的**地址**。
/// `?resolve=0` 可跳过 pkarr 解析（快速列表）。
async fn nodes_with_addrs(State(s): State<AppState>, headers: HeaderMap, Query(q): Query<NodesQuery>) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let bl = s.store.blacklist();
    let resolve = q.resolve.as_deref().map(|v| v != "0" && v != "false").unwrap_or(true);
    let snap = s.store.peers_snapshot();
    let now = now_secs();
    // 预先取出闭包需要的东西（join_all 的闭包是 FnMut，不能移动 s）。
    let http = s.http.clone();
    let pkarr_base = s.pkarr_base.clone();

    let rows = futures::future::join_all(snap.into_iter().map(|p| {
        let bl = bl.clone();
        let http = http.clone();
        let pkarr_base = pkarr_base.clone();
        async move {
            let banned = bl
                .iter()
                .any(|b| store::Store::normalize_id(b) == store::Store::normalize_id(&p.id));
            let addrs = if resolve && !banned {
                Some(fetch_node_addrs_with(&http, &pkarr_base, &p.id).await)
            } else {
                None
            };
            json!({
                "id": p.id,
                "status": if banned { "banned" }
                    else if now.saturating_sub(p.last_seen) <= 60 { "active" }
                    else if now.saturating_sub(p.last_seen) <= 600 { "idle" }
                    else { "stale" },
                "first_seen": p.first_seen,
                "last_seen": p.last_seen,
                "connects": p.connects,
                "last_allowed": p.last_allowed,
                "banned": banned,
                "relays": addrs.as_ref().map(|a| a.relays.clone()).unwrap_or_default(),
                "ips": addrs.as_ref().map(|a| a.ips.clone()).unwrap_or_default(),
                "pkarr_ok": addrs.as_ref().map(|a| a.ok),
                "pkarr_error": addrs.as_ref().and_then(|a| a.error.clone()),
                "pkarr_bytes": addrs.as_ref().map(|a| a.bytes).unwrap_or(0),
            })
        }
    }))
    .await;

    Json(json!({ "count": rows.len(), "nodes": rows })).into_response()
}

#[derive(Deserialize)]
struct NodesQuery {
    /// 是否解析 pkarr 地址（默认 true）；传 "0"/"false" 跳过。字符串以兼容 ?resolve=1。
    resolve: Option<String>,
}

/// `/api/dns/records` — DNS 侧明细：查询量、命中/未命中、pkarr 写入、存储统计。
async fn dns_records(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let m = s.metrics.scrape().await;
    let d = &m.dns;
    let get = |k: &str| d.get(k).copied().unwrap_or(0.0);

    let total = get("dns_server_dns_requests_total");
    let ok = get("dns_server_dns_lookup_success_total");
    let nf = get("dns_server_dns_lookup_notfound_total");
    let err = get("dns_server_dns_lookup_error_total");
    let hit_rate = if total > 0.0 { ok / total * 100.0 } else { 0.0 };

    Json(json!({
        "up": m.dns_up,
        "dns": {
            "requests_total": total,
            "requests_udp": get("dns_server_dns_requests_udp_total"),
            "requests_https": get("dns_server_dns_requests_https_total"),
            "lookup_success": ok,
            "lookup_notfound": nf,
            "lookup_error": err,
            "hit_rate_pct": (hit_rate * 100.0).round() / 100.0,
        },
        "http": {
            "requests_total": get("dns_server_http_requests_total"),
            "success": get("dns_server_http_requests_success_total"),
            "error": get("dns_server_http_requests_error_total"),
            "duration_ms_total": get("dns_server_http_requests_duration_ms_total"),
        },
        "pkarr": {
            "publish_update": get("dns_server_pkarr_publish_update_total"),
            "publish_noop": get("dns_server_pkarr_publish_noop_total"),
            "store_inserted": get("dns_server_store_packets_inserted_total"),
            "store_updated": get("dns_server_store_packets_updated_total"),
            "store_removed": get("dns_server_store_packets_removed_total"),
            "store_expired": get("dns_server_store_packets_expired_total"),
        },
        "cache_zones": get("dns_server_cache_zones"),
        "cache_zones_dht": get("dns_server_cache_zones_dht"),
        "scraped_at_ms": m.scraped_at_ms,
    }))
    .into_response()
}

/// `/api/relay/detail` — relay 侧明细：连接、字节、丢包、限流、QAD、错误。
async fn relay_detail(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let m = s.metrics.scrape().await;
    let r = &m.relay;
    let get = |k: &str| r.get(k).copied().unwrap_or(0.0);

    Json(json!({
        "up": m.relay_up,
        "conn": {
            "accepts": get("relayserver_accepts_total"),
            "disconnects": get("relayserver_disconnects_total"),
            "http_connections": get("relayserver_http_connections_total"),
            "http_closed": get("relayserver_http_connections_closed_total"),
            "http_errored": get("relayserver_http_connections_errored_total"),
            "unique_client_keys": get("relayserver_unique_client_keys"),
            "clients_inactive_added": get("relayserver_clients_inactive_added_total"),
            "clients_inactive_removed": get("relayserver_clients_inactive_removed_total"),
        },
        "traffic": {
            "bytes_sent": get("relayserver_bytes_sent_total"),
            "bytes_recv": get("relayserver_bytes_recv_total"),
            "send_packets_sent": get("relayserver_send_packets_sent_total"),
            "send_packets_recv": get("relayserver_send_packets_recv_total"),
            "send_packets_dropped": get("relayserver_send_packets_dropped_total"),
            "other_packets_sent": get("relayserver_other_packets_sent_total"),
            "other_packets_recv": get("relayserver_other_packets_recv_total"),
            "other_packets_dropped": get("relayserver_other_packets_dropped_total"),
        },
        "ratelimit": {
            "bytes_rx_limited": get("relayserver_bytes_rx_ratelimited_total"),
            "conns_rx_limited": get("relayserver_conns_rx_ratelimited_total"),
        },
        "qad": {
            "connections": get("relayserver_qad_connections_total"),
            "closed": get("relayserver_qad_connections_closed_total"),
            "errored": get("relayserver_qad_connections_errored_total"),
            "incoming": get("relayserver_qad_incoming_total"),
            "incoming_error": get("relayserver_qad_incoming_error_total"),
        },
        "misc": {
            "got_ping": get("relayserver_got_ping_total"),
            "sent_pong": get("relayserver_sent_pong_total"),
            "unknown_frames": get("relayserver_unknown_frames_total"),
        },
        "rates": m.rates,
        "scraped_at_ms": m.scraped_at_ms,
    }))
    .into_response()
}

fn hex_preview(b: &[u8], n: usize) -> String {
    b.iter().take(n).map(|x| format!("{x:02x}")).collect()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 系统 boot 时刻（unix 秒）——读 /proc/stat 的 `btime`。用于把进程 starttime
/// （相对 boot 的 jiffies）换算成绝对 unix 时刻。读不到返回 None（如非 Linux）。
fn system_btime() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/stat").ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.trim().parse::<u64>().ok();
        }
    }
    None
}

/// 某进程的启动 unix 时刻（秒）：读 /proc/<pid>/stat 第 22 字段 starttime（jiffies，
/// 相对 boot），除以 CLK_TCK(Linux 恒为 100) 加上 btime。读不到返回 None。
/// 注意 comm 字段(第2列)可能含空格并裹在括号里，故从最后一个 ')' 之后再按空格切。
fn process_start_unix(pid: u32, btime: u64) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = &stat[stat.rfind(')')? + 1..];
    // after 以 " S ..." 开头；starttime 是整个 stat 的第 22 字段 = after 里第 20 个。
    let fields: Vec<&str> = after.split_whitespace().collect();
    let starttime: u64 = fields.get(19)?.parse().ok()?; // 0-based: 第20个
    Some(btime + starttime / 100)
}

/// systemd unit 的 MainPID（普通/ root 用户均可读）。失败返回 None。
fn unit_main_pid(unit: &str) -> Option<u32> {
    let out = std::process::Command::new("systemctl")
        .args(["show", unit, "-p", "MainPID", "--value"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let pid: u32 = s.trim().parse().ok()?;
    if pid == 0 { None } else { Some(pid) }
}

/// 三个服务（relay / dns / relay-admin 自身）的启动时刻与运行时长，供总览展示。
/// relay/dns 经 systemd MainPID 定位进程；relay-admin 用自身 pid。全部以
/// /proc 读取启动时刻，纯文件、无需 root、不依赖时钟字符串解析。
fn service_uptimes(relay_unit: &str, dns_unit: &str) -> serde_json::Value {
    let btime = system_btime();
    let now = now_secs();
    let one = |pid: Option<u32>| -> serde_json::Value {
        match (pid, btime) {
            (Some(p), Some(bt)) => match process_start_unix(p, bt) {
                Some(start) => json!({
                    "pid": p,
                    "start_unix": start,
                    "uptime_secs": now.saturating_sub(start),
                    "up": true,
                }),
                None => json!({ "pid": p, "up": true }),
            },
            _ => json!({ "up": false }),
        }
    };
    json!({
        "relay": one(unit_main_pid(relay_unit)),
        "dns": one(unit_main_pid(dns_unit)),
        "admin": one(Some(std::process::id())),
    })
}


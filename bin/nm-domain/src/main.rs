//! `nm-domain` — Nano Mesh 全球域名注册中心。
//!
//! 只保存「域名 → 节点公钥」。`name@domain` 仍由拥有该域名的家节点签发，不进这里。
//! 解析接口公开；登记、停用和列表需要登录。鉴权与 nm-admind 相同：admin + argon2 + 首登改密。
//! 数据放 redb 文件，和 nmd 用的是同一类数据库。

mod auth;
mod store;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use serde::Deserialize;
use serde_json::json;

use auth::Auth;
use store::{normalize_domain, RegisterOutcome, Registry, StoreError};

#[derive(Parser)]
#[command(name = "nm-domain", about = "Nano Mesh 全球域名注册中心")]
struct Args {
    /// HTTP 监听地址。
    #[arg(long, default_value = "0.0.0.0:80")]
    listen: String,
    /// HTTPS 监听地址。需要同时提供 `--tls-cert` 和 `--tls-key`。
    #[arg(long, default_value = "0.0.0.0:443")]
    https: String,
    /// TLS 证书（PEM）。
    #[arg(long)]
    tls_cert: Option<String>,
    /// TLS 私钥（PEM）。
    #[arg(long)]
    tls_key: Option<String>,
    /// redb 数据库文件。
    #[arg(long, default_value = "nm-domain.redb")]
    db: String,
    /// 管理员凭据文件。
    #[arg(long, default_value = "domaind.state.json")]
    state: String,
    /// 本机 nmd 控制 API。审批结果经它发到网格。
    #[arg(long, default_value = "http://127.0.0.1:9611")]
    nmd_api: String,
    /// nmd 控制 API 令牌。为空则只记账，不发网格通知。
    #[arg(long, default_value = "")]
    nmd_token: String,
}

#[derive(Clone)]
struct AppState {
    auth: Arc<Auth>,
    reg: Arc<Registry>,
    http: reqwest::Client,
    nmd_api: String,
    nmd_token: String,
}

const INDEX: &str = include_str!("../web/index.html");
const APPJS: &str = include_str!("../web/app.js");
const CSS: &str = include_str!("../web/styles.css");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nm_domain=info".into()),
        )
        .init();
    let args = Args::parse();
    let (auth, created) = Auth::load_or_init(args.state.clone().into())?;
    if created {
        tracing::warn!(
            "首次运行：管理员账号 \"admin\"，初始密码 \"{}\"——首次登录后请立即修改",
            auth::DEFAULT_PASSWORD
        );
    }
    let reg = Registry::open(&args.db)?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let state = AppState {
        auth: Arc::new(auth),
        reg: Arc::new(reg),
        http,
        nmd_api: args.nmd_api.trim_end_matches('/').to_string(),
        nmd_token: args.nmd_token,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/app.js", get(appjs))
        .route("/styles.css", get(css))
        .route("/api/session", get(session))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/change-password", post(change_password))
        .route("/api/resolve", get(resolve))
        .route("/api/catalog", get(catalog))
        .route("/api/domains", get(list_domains).post(register_domain))
        .route("/api/domains/disable", post(disable_domain))
        .route("/api/domains/enable", post(enable_domain))
        .route("/api/signup", get(signup))
        .route("/api/signup/list", get(signup_list))
        .route("/api/applications", get(list_applications))
        .route("/api/applications/approve", post(approve_application))
        .route("/api/applications/reject", post(reject_application))
        .with_state(state);

    let https_addr: Option<SocketAddr> = match (&args.tls_cert, &args.tls_key) {
        (Some(cert), Some(key)) => {
            let addr = args.https.parse()?;
            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
            let https_app = app.clone().layer(middleware::from_fn(mark_tls));
            tokio::spawn(async move {
                if let Err(e) = axum_server::bind_rustls(addr, config)
                    .serve(https_app.into_make_service())
                    .await
                {
                    tracing::error!(%e, "HTTPS 服务退出");
                }
            });
            Some(addr)
        }
        (None, None) => None,
        _ => anyhow::bail!("HTTPS 需要同时提供 --tls-cert 和 --tls-key"),
    };

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    let access = args.listen.replace("0.0.0.0", "127.0.0.1");
    println!("nm-domain 全球域名注册中心已启动");
    println!("  HTTP      {}", args.listen);
    println!("  管理地址  http://{access}/");
    println!("  解析接口  http://{access}/api/resolve?domain=acme.mesh");
    if let Some(addr) = https_addr {
        let host = addr.ip().to_string().replace("0.0.0.0", "127.0.0.1");
        println!("  HTTPS     {}", args.https);
        println!("  管理地址  https://{host}:{}/", addr.port());
    }
    println!("  数据库    {}", args.db);
    tracing::info!(http = %args.listen, https = https_addr.map(|a| a.to_string()).unwrap_or_default(), db = %args.db, "nm-domain 已启动");
    axum::serve(listener, app).await?;
    Ok(())
}

fn asset(content_type: &'static str, body: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
}
async fn index() -> impl IntoResponse {
    asset("text/html; charset=utf-8", INDEX)
}
async fn appjs() -> impl IntoResponse {
    asset("text/javascript; charset=utf-8", APPJS)
}
async fn css() -> impl IntoResponse {
    asset("text/css; charset=utf-8", CSS)
}

#[derive(Clone, Copy)]
struct ViaTls;

async fn mark_tls(mut req: Request, next: Next) -> Response {
    req.extensions_mut().insert(ViaTls);
    next.run(req).await
}

fn session_cookie(token: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("nmdomain={token}; HttpOnly; Path=/; SameSite=Lax{secure}; Max-Age=28800")
}

fn clear_cookie(secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("nmdomain=; HttpOnly; Path=/; SameSite=Lax{secure}; Max-Age=0")
}

fn cookie_tokens(headers: &HeaderMap) -> Vec<String> {
    let mut out = Vec::new();
    let Some(c) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return out;
    };
    for part in c.split(';') {
        if let Some(v) = part.trim().strip_prefix("nmdomain=") {
            if !v.is_empty() {
                out.push(v.to_string());
            }
        }
    }
    out
}
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let t = v.strip_prefix("Bearer ")?.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

fn session_ok(s: &AppState, headers: &HeaderMap) -> bool {
    if let Some(t) = bearer_token(headers) {
        if s.auth.valid(&t) {
            return true;
        }
    }
    cookie_tokens(headers).iter().any(|t| s.auth.valid(t))
}
fn require_session(s: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    if !session_ok(s, headers) {
        return Err(StatusCode::UNAUTHORIZED.into_response());
    }
    if s.auth.must_change() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": "请先修改初始密码" })),
        )
            .into_response());
    }
    Ok(())
}

async fn session(State(s): State<AppState>, headers: HeaderMap) -> Json<serde_json::Value> {
    Json(json!({
        "authed": session_ok(&s, &headers),
        "mustChange": s.auth.must_change(),
    }))
}

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}
async fn login(State(s): State<AppState>, req: Request) -> Response {
    let secure = req.extensions().get::<ViaTls>().is_some();
    let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    let b: LoginReq = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "请求格式不正确" })),
            )
                .into_response();
        }
    };
    match s.auth.login(&b.username, &b.password) {
        Some((token, must_change)) => {
            (
                [(header::SET_COOKIE, session_cookie(&token, secure))],
                Json(json!({ "ok": true, "mustChange": must_change, "token": token })),
            )
                .into_response()
        }
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "ok": false, "error": "用户名或密码错误" })),
        )
            .into_response(),
    }
}

async fn logout(State(s): State<AppState>, req: Request) -> Response {
    let secure = req.extensions().get::<ViaTls>().is_some();
    let headers = req.headers().clone();
    for t in cookie_tokens(&headers) {
        s.auth.logout(&t);
    }
    (
        [(header::SET_COOKIE, clear_cookie(secure))],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangeReq {
    old_password: String,
    new_password: String,
}
async fn change_password(State(s): State<AppState>, headers: HeaderMap, Json(b): Json<ChangeReq>) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match s.auth.change_password(&b.old_password, &b.new_password) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct ResolveQuery {
    domain: String,
}
/// 公开解析：域名 → 节点公钥。不返回邮箱，不要求登录。
async fn resolve(State(s): State<AppState>, Query(q): Query<ResolveQuery>) -> Response {
    match s.reg.resolve(&q.domain) {
        Ok(Some(rec)) if rec.disabled => (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": "域名已停用" })),
        )
            .into_response(),
        Ok(Some(rec)) => Json(json!({
            "ok": true,
            "domain": rec.domain,
            "pubkey": rec.pubkey,
        }))
        .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": "域名未登记" })),
        )
            .into_response(),
        Err(e) => store_response(e),
    }
}

#[derive(Deserialize)]
struct CatalogQuery {
    q: Option<String>,
}

/// 越小越靠前：完全相同、前缀、子串、字符按顺序出现。
fn domain_match_rank(domain: &str, q: &str) -> Option<u8> {
    if q.is_empty() {
        return Some(4);
    }
    if domain == q {
        return Some(0);
    }
    if domain.starts_with(q) {
        return Some(1);
    }
    if domain.contains(q) {
        return Some(2);
    }
    let mut rest = domain.chars();
    if q.chars().all(|c| rest.any(|d| d == c)) {
        Some(3)
    } else {
        None
    }
}

/// 公开目录：已登记且未停用的域名及其节点公钥。不含登记邮箱。
/// `q` 做模糊过滤，供登录框在输入「.」后下拉点选。
async fn catalog(State(s): State<AppState>, Query(q): Query<CatalogQuery>) -> Response {
    match s.reg.list() {
        Ok(all) => {
            let needle = q.q.unwrap_or_default().trim().to_lowercase();
            let mut ranked: Vec<_> = all
                .into_iter()
                .filter(|r| !r.disabled)
                .filter_map(|r| domain_match_rank(&r.domain, &needle).map(|rank| (rank, r)))
                .collect();
            ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.domain.cmp(&b.1.domain)));
            let items: Vec<_> = ranked
                .into_iter()
                .take(12)
                .map(|(_, r)| json!({ "domain": r.domain, "pubkey": r.pubkey }))
                .collect();
            Json(json!({ "ok": true, "items": items })).into_response()
        }
        Err(e) => store_response(e),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
    page: Option<u32>,
}
async fn list_domains(State(s): State<AppState>, headers: HeaderMap, Query(q): Query<ListQuery>) -> Response {
    if let Err(resp) = require_session(&s, &headers) {
        return resp;
    }
    let all = match s.reg.list() {
        Ok(v) => v,
        Err(e) => return store_response(e),
    };
    let needle = q.q.unwrap_or_default().trim().to_ascii_lowercase();
    let filtered: Vec<_> = all
        .into_iter()
        .filter(|r| {
            needle.is_empty()
                || r.domain.contains(&needle)
                || r.pubkey.contains(&needle)
                || r.email.contains(&needle)
        })
        .collect();
    let page_size = 20u32;
    let pages = ((filtered.len() as u32) + page_size - 1) / page_size;
    let pages = pages.max(1);
    let page = q.page.unwrap_or(1).clamp(1, pages);
    let start = ((page - 1) * page_size) as usize;
    let items: Vec<_> = filtered
        .iter()
        .skip(start)
        .take(page_size as usize)
        .cloned()
        .collect();
    Json(json!({
        "ok": true,
        "total": filtered.len(),
        "page": page,
        "pages": pages,
        "pageSize": page_size,
        "items": items,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct DomainBody {
    domain: String,
    pubkey: String,
    email: String,
}
async fn register_domain(State(s): State<AppState>, headers: HeaderMap, Json(b): Json<DomainBody>) -> Response {
    if let Err(resp) = require_session(&s, &headers) {
        return resp;
    }
    match s.reg.register(&b.domain, &b.pubkey, &b.email) {
        Ok(RegisterOutcome::Created) => {
            let domain = normalize_domain(&b.domain).unwrap_or(b.domain);
            Json(json!({ "ok": true, "created": true, "domain": domain })).into_response()
        }
        Ok(RegisterOutcome::Unchanged) => {
            let domain = normalize_domain(&b.domain).unwrap_or(b.domain);
            Json(json!({ "ok": true, "created": false, "domain": domain })).into_response()
        }
        Err(e) => store_response(e),
    }
}

#[derive(Deserialize)]
struct DisableBody {
    domain: String,
}
async fn disable_domain(State(s): State<AppState>, headers: HeaderMap, Json(b): Json<DisableBody>) -> Response {
    set_domain_disabled(s, headers, &b.domain, true)
}
async fn enable_domain(State(s): State<AppState>, headers: HeaderMap, Json(b): Json<DisableBody>) -> Response {
    set_domain_disabled(s, headers, &b.domain, false)
}
fn set_domain_disabled(s: AppState, headers: HeaderMap, domain: &str, disabled: bool) -> Response {
    if let Err(resp) = require_session(&s, &headers) {
        return resp;
    }
    match s.reg.set_disabled(domain, disabled) {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": "域名未登记" })),
        )
            .into_response(),
        Err(e) => store_response(e),
    }
}

#[derive(Deserialize)]
struct SignupQuery {
    nodeid: String,
    domain: String,
    email: String,
}
/// 公开申请。不登录。写入待审列表。
async fn signup(State(s): State<AppState>, Query(q): Query<SignupQuery>) -> Response {
    match s.reg.apply(&q.domain, &q.nodeid, &q.email) {
        Ok(app) => Json(json!({
            "ok": true,
            "domain": app.domain,
            "nodeId": app.node_id,
            "status": app.status,
        }))
        .into_response(),
        Err(e) => store_response(e),
    }
}

#[derive(Deserialize)]
struct SignupListQuery {
    nodeid: String,
}
/// 公开查询某节点的申请。供 nm-admind 展示待审列表。
async fn signup_list(State(s): State<AppState>, Query(q): Query<SignupListQuery>) -> Response {
    match s.reg.list_for_node(&q.nodeid) {
        Ok(items) => Json(json!({ "ok": true, "items": items })).into_response(),
        Err(e) => store_response(e),
    }
}

async fn list_applications(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_session(&s, &headers) {
        return resp;
    }
    match s.reg.list_applications() {
        Ok(items) => Json(json!({ "ok": true, "items": items })).into_response(),
        Err(e) => store_response(e),
    }
}

#[derive(Deserialize)]
struct DecideBody {
    domain: String,
}
async fn approve_application(State(s): State<AppState>, headers: HeaderMap, Json(b): Json<DecideBody>) -> Response {
    decide(&s, &headers, &b.domain, true).await
}
async fn reject_application(State(s): State<AppState>, headers: HeaderMap, Json(b): Json<DecideBody>) -> Response {
    decide(&s, &headers, &b.domain, false).await
}
async fn decide(s: &AppState, headers: &HeaderMap, domain: &str, approved: bool) -> Response {
    if let Err(resp) = require_session(s, headers) {
        return resp;
    }
    let app = match s.reg.decide(domain, approved) {
        Ok(app) => app,
        Err(e) => return store_response(e),
    };
    notify_node(s, &app.node_id, &app.domain, approved).await;
    Json(json!({ "ok": true, "domain": app.domain, "status": app.status })).into_response()
}

async fn notify_node(s: &AppState, node_id: &str, domain: &str, approved: bool) {
    if s.nmd_token.is_empty() {
        tracing::warn!("未配置 --nmd-token，审批结果未发到网格");
        return;
    }
    let url = format!("{}/names/domain-decision", s.nmd_api);
    let res = s
        .http
        .post(url)
        .header("x-admin-token", &s.nmd_token)
        .json(&json!({ "node_id": node_id, "domain": domain, "approved": approved }))
        .send()
        .await;
    match res {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!(domain, node_id, approved, "已请求 nmd 通知对端");
        }
        Ok(resp) => tracing::warn!(status = %resp.status(), "nmd 未接受域名通知"),
        Err(e) => tracing::warn!(%e, "通知 nmd 失败"),
    }
}

fn store_response(e: StoreError) -> Response {
    let status = match &e {
        StoreError::Taken(_) => StatusCode::CONFLICT,
        StoreError::Invalid(_) => StatusCode::BAD_REQUEST,
        StoreError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "ok": false, "error": e.to_string() }))).into_response()
}

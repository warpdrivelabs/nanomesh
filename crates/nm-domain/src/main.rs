//! `nm-domain` — Nano Mesh 全球域名注册中心。
//!
//! 只保存「域名 → 节点公钥」。`name@domain` 仍由拥有该域名的家节点签发，不进这里。
//! 解析接口公开；登记、停用和列表需要登录。鉴权与 nm-admind 相同：admin + argon2 + 首登改密。
//! 数据放 redb 文件，和 nmd 用的是同一类数据库。

mod auth;
mod store;

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
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
    /// 监听地址。
    #[arg(long, default_value = "0.0.0.0:9620")]
    listen: String,
    /// redb 数据库文件。
    #[arg(long, default_value = "nm-domain.redb")]
    db: String,
    /// 管理员凭据文件。
    #[arg(long, default_value = "domaind.state.json")]
    state: String,
}

#[derive(Clone)]
struct AppState {
    auth: Arc<Auth>,
    reg: Arc<Registry>,
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
    let state = AppState {
        auth: Arc::new(auth),
        reg: Arc::new(reg),
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
        .route("/api/domains", get(list_domains).post(register_domain))
        .route("/api/domains/disable", post(disable_domain))
        .route("/api/domains/enable", post(enable_domain))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    let access = args.listen.replace("0.0.0.0", "127.0.0.1");
    println!("nm-domain 全球域名注册中心已启动");
    println!("  监听      {}", args.listen);
    println!("  管理地址  http://{access}/");
    println!("  解析接口  http://{access}/api/resolve?domain=acme.mesh");
    println!("  数据库    {}", args.db);
    tracing::info!(listen = %args.listen, db = %args.db, "nm-domain 已启动");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}
async fn appjs() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], APPJS)
}
async fn css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], CSS)
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let c = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in c.split(';') {
        if let Some(v) = part.trim().strip_prefix("nmdomain=") {
            return Some(v.to_string());
        }
    }
    None
}
fn session_ok(s: &AppState, headers: &HeaderMap) -> bool {
    cookie_token(headers).map(|t| s.auth.valid(&t)).unwrap_or(false)
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
async fn login(State(s): State<AppState>, Json(b): Json<LoginReq>) -> Response {
    match s.auth.login(&b.username, &b.password) {
        Some((token, must_change)) => {
            let cookie = format!("nmdomain={token}; HttpOnly; Path=/; SameSite=Strict; Max-Age=28800");
            (
                [(header::SET_COOKIE, cookie)],
                Json(json!({ "ok": true, "mustChange": must_change })),
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

async fn logout(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(t) = cookie_token(&headers) {
        s.auth.logout(&t);
    }
    (
        [(header::SET_COOKIE, "nmdomain=; Path=/; Max-Age=0")],
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

fn store_response(e: StoreError) -> Response {
    let status = match &e {
        StoreError::Taken(_) => StatusCode::CONFLICT,
        StoreError::Invalid(_) => StatusCode::BAD_REQUEST,
        StoreError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "ok": false, "error": e.to_string() }))).into_response()
}

//! `nm-admind` — 独立后端管理控制台。axum 托管 Web Components 前端 + 登录鉴权，
//! 反向代理到 nmd 的控制/指标 API（`--nmd-api`，带共享 token）。固定管理端口。

mod auth;

use std::sync::Arc;
use std::time::Duration;

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

#[derive(Parser)]
#[command(name = "nm-admind", about = "nmd 后端管理控制台")]
struct Args {
    /// 管理台监听地址（固定管理端口）。
    #[arg(long, default_value = "0.0.0.0:9610")]
    listen: String,
    /// nmd 控制 API 地址。
    #[arg(long, default_value = "http://127.0.0.1:9611")]
    nmd_api: String,
    /// nmd 控制 API 的共享 token（须与 nmd `[admin] api_token` 一致）。
    #[arg(long, default_value = "")]
    nmd_token: String,
    /// 管理员凭据文件路径。
    #[arg(long, default_value = "admind.state.json")]
    state: String,
}

#[derive(Clone)]
struct AppState {
    auth: Arc<Auth>,
    http: reqwest::Client,
    nmd_api: String,
    nmd_token: String,
}

const INDEX: &str = include_str!("../web/index.html");
const APPJS: &str = include_str!("../web/app.js");
const CSS: &str = include_str!("../web/styles.css");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 安装 rustls 加密提供者：reqwest 与 iroh 栈特性合一后启用了 rustls 却无默认 provider，
    // 需显式安装，否则 reqwest::Client::new() 在 Linux 上 panic。幂等，已装则忽略。
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nm_admind=info".into()),
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
    if args.nmd_token.trim().is_empty() {
        tracing::warn!("未设置 --nmd-token；若 nmd 控制 API 需要 token，取数会 401");
    }

    // 反代 HTTP 客户端设超时：nmd 若卡死/无响应，代理应快速失败并返回可见错误
    // （proxy_get 的 Err 分支 → 502 "nmd 不可达"），而不是无限等待、让前端菜单内容一直空白。
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let state = AppState {
        auth: Arc::new(auth),
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
        .route("/api/overview", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/overview")))
        .route("/api/connections", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/connections")))
        .route("/api/users", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/users")))
        .route("/api/storage", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/storage")))
        .route("/api/traffic", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/traffic")))
        .route("/api/system", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/system")))
        .route("/api/identity", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/identity")))
        .route("/api/peers", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/peers")))
        .route("/api/qr", get(qr))
        .route("/api/ban", post(|s: State<AppState>, h: HeaderMap, b: Json<serde_json::Value>| proxy_post(s, h, "/ban", b)))
        .route("/api/unban", post(|s: State<AppState>, h: HeaderMap, b: Json<serde_json::Value>| proxy_post(s, h, "/unban", b)))
        .route("/api/kick", post(|s: State<AppState>, h: HeaderMap, b: Json<serde_json::Value>| proxy_post(s, h, "/kick", b)))
        .route("/api/add-peer", post(|s: State<AppState>, h: HeaderMap, b: Json<serde_json::Value>| proxy_post(s, h, "/add-peer", b)))
        .route("/api/remove-peer", post(|s: State<AppState>, h: HeaderMap, b: Json<serde_json::Value>| proxy_post(s, h, "/remove-peer", b)))
        .route("/api/names/domains", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/names/domains")))
        .route("/api/names/list", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/names/list")))
        .route("/api/names/domain-add", post(|s: State<AppState>, h: HeaderMap, b: Json<serde_json::Value>| proxy_post(s, h, "/names/domain-add", b)))
        .route("/api/names/set", post(|s: State<AppState>, h: HeaderMap, b: Json<serde_json::Value>| proxy_post(s, h, "/names/set", b)))
        .route("/api/names/del", post(|s: State<AppState>, h: HeaderMap, b: Json<serde_json::Value>| proxy_post(s, h, "/names/del", b)))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    // 后端管理服务已就绪：显示管理 HTTP 连接地址（0.0.0.0 → 127.0.0.1 便于本机点击）。
    let access = args.listen.replace("0.0.0.0", "127.0.0.1");
    println!("nm-admind 后端管理台已启动");
    println!("  监听      {}", args.listen);
    println!("  管理地址  http://{access}/");
    println!("  反代 nmd  {}", args.nmd_api);
    tracing::info!(listen = %args.listen, "nm-admind 管理台已启动");
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
        if let Some(v) = part.trim().strip_prefix("admind=") {
            return Some(v.to_string());
        }
    }
    None
}
fn session_ok(s: &AppState, headers: &HeaderMap) -> bool {
    cookie_token(headers)
        .map(|t| s.auth.valid(&t))
        .unwrap_or(false)
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
            let cookie =
                format!("admind={token}; HttpOnly; Path=/; SameSite=Strict; Max-Age=28800");
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
        [(header::SET_COOKIE, "admind=; Path=/; Max-Age=0")],
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
async fn change_password(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(b): Json<ChangeReq>,
) -> Response {
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

/// 反代 GET 到 nmd 控制 API（需已登录且已改密）。
async fn proxy_get(s: State<AppState>, headers: HeaderMap, path: &str) -> Response {
    let s = s.0;
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if s.auth.must_change() {
        return (StatusCode::FORBIDDEN, "首次登录请先修改密码").into_response();
    }
    match s
        .http
        .get(format!("{}{}", s.nmd_api, path))
        .header("x-admin-token", &s.nmd_token)
        .send()
        .await
    {
        Ok(resp) => {
            let code = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = resp.text().await.unwrap_or_default();
            (code, [(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("nmd 不可达: {e}") })),
        )
            .into_response(),
    }
}

/// 反代 POST 到 nmd 控制 API（转发 JSON body）。
async fn proxy_post(
    s: State<AppState>,
    headers: HeaderMap,
    path: &str,
    body: Json<serde_json::Value>,
) -> Response {
    let s = s.0;
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if s.auth.must_change() {
        return (StatusCode::FORBIDDEN, "首次登录请先修改密码").into_response();
    }
    match s
        .http
        .post(format!("{}{}", s.nmd_api, path))
        .header("x-admin-token", &s.nmd_token)
        .json(&body.0)
        .send()
        .await
    {
        Ok(resp) => {
            let code = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = resp.text().await.unwrap_or_default();
            (code, [(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("nmd 不可达: {e}") })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct QrQuery {
    kind: String,
}

/// 生成节点标识二维码(SVG)。kind=node|addr；载荷带类型前缀以区分类型，并按类型着色。
async fn qr(State(s): State<AppState>, headers: HeaderMap, Query(q): Query<QrQuery>) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if s.auth.must_change() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let ident: serde_json::Value = match s
        .http
        .get(format!("{}/identity", s.nmd_api))
        .header("x-admin-token", &s.nmd_token)
        .send()
        .await
    {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("nmd 不可达: {e}")).into_response(),
    };
    // 类型前缀 → 扫码可区分类型；并按类型着色（node=靛蓝, addr=天蓝）。
    let (payload, dark) = if q.kind == "addr" {
        (format!("nmspace:addr:{}", ident["addr"].as_str().unwrap_or("")), "#0ea5e9")
    } else {
        (format!("nmspace:node:{}", ident["node_id"].as_str().unwrap_or("")), "#5b5bf0")
    };
    match qrcode::QrCode::new(payload.as_bytes()) {
        Ok(code) => {
            let svg = code
                .render::<qrcode::render::svg::Color>()
                .min_dimensions(200, 200)
                .quiet_zone(true)
                .dark_color(qrcode::render::svg::Color(dark))
                .light_color(qrcode::render::svg::Color("#ffffff"))
                .build();
            ([(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")], svg).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("qr: {e}")).into_response(),
    }
}

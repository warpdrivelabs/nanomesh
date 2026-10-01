//! `nm-admind` — 独立后端管理控制台。axum 托管 Web Components 前端 + 登录鉴权，
//! 反向代理到 nmd 的控制/指标 API（`--nmd-api`，带共享 token）。固定管理端口。

mod auth;

use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

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

#[derive(Parser)]
#[command(name = "nm-admind", about = "nmd 后端管理控制台")]
struct Args {
    /// 管理台 HTTP 监听地址。
    #[arg(long, default_value = "0.0.0.0:9610")]
    listen: String,
    /// 管理台 HTTPS 监听地址。需要同时提供 `--tls-cert` 和 `--tls-key`。
    #[arg(long, default_value = "0.0.0.0:8443")]
    https: String,
    /// TLS 证书（PEM）。
    #[arg(long)]
    tls_cert: Option<String>,
    /// TLS 私钥（PEM）。
    #[arg(long)]
    tls_key: Option<String>,
    /// nmd 控制 API 地址。
    #[arg(long, default_value = "http://127.0.0.1:9611")]
    nmd_api: String,
    /// nmd 控制 API 的共享 token（须与 nmd `[admin] api_token` 一致）。
    #[arg(long, default_value = "")]
    nmd_token: String,
    /// 管理员凭据文件路径。
    #[arg(long, default_value = "admind.state.json")]
    state: String,
    /// 全球域名注册中心。申请注册发到这里。
    #[arg(long, default_value = "https://robot.link")]
    registry: String,
}

#[derive(Clone)]
struct AppState {
    auth: Arc<Auth>,
    http: reqwest::Client,
    nmd_api: String,
    nmd_token: String,
    registry: String,
}

const INDEX: &str = include_str!("../web/index.html");
const APPJS: &str = include_str!("../web/app.js");
const CSS: &str = include_str!("../web/styles.css");
const LOGO_PNG: &[u8] = include_bytes!("../web/nanomesh-logo.png");

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
        .http1_only()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let state = AppState {
        auth: Arc::new(auth),
        http,
        nmd_api: args.nmd_api.trim_end_matches('/').to_string(),
        nmd_token: args.nmd_token,
        registry: args.registry.trim_end_matches('/').to_string(),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/app.js", get(appjs))
        .route("/styles.css", get(css))
        .route("/nanomesh-logo.png", get(logo_png))
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
        .route("/api/names/signup", post(names_signup))
        .route("/api/names/pending", get(names_pending))
        .route("/api/names/domain-notices", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/names/domain-notices")))
        .route("/api/names/domains", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/names/domains")))
        .route("/api/names/list", get(|s: State<AppState>, h: HeaderMap| proxy_get(s, h, "/names/list")))
        .route("/api/names/domain-add", post(|s: State<AppState>, h: HeaderMap, b: Json<serde_json::Value>| proxy_post(s, h, "/names/domain-add", b)))
        .route("/api/names/set", post(|s: State<AppState>, h: HeaderMap, b: Json<serde_json::Value>| proxy_post(s, h, "/names/set", b)))
        .route("/api/names/del", post(|s: State<AppState>, h: HeaderMap, b: Json<serde_json::Value>| proxy_post(s, h, "/names/del", b)))
        .route("/api/nmd/restart", post(nmd_restart))
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
                    tracing::error!(%e, "HTTPS 管理台退出");
                }
            });
            Some(addr)
        }
        (None, None) => None,
        _ => anyhow::bail!("HTTPS 需要同时提供 --tls-cert 和 --tls-key"),
    };

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    let access = args.listen.replace("0.0.0.0", "127.0.0.1");
    println!("nm-admind 后端管理台已启动");
    println!("  HTTP      {}", args.listen);
    println!("  管理地址  http://{access}/");
    if let Some(addr) = https_addr {
        let host = addr.ip().to_string().replace("0.0.0.0", "127.0.0.1");
        println!("  HTTPS     {}", args.https);
        println!("  管理地址  https://{host}:{}/", addr.port());
    }
    println!("  反代 nmd  {}", args.nmd_api);
    tracing::info!(http = %args.listen, https = https_addr.map(|a| a.to_string()).unwrap_or_default(), "nm-admind 管理台已启动");
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
async fn logo_png() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-store")], LOGO_PNG)
}

/// 标记这次请求来自 HTTPS 监听，登录时给 Cookie 加上 Secure。
#[derive(Clone, Copy)]
struct ViaTls;

async fn mark_tls(mut req: Request, next: Next) -> Response {
    req.extensions_mut().insert(ViaTls);
    next.run(req).await
}

fn err_chain(err: &impl std::error::Error) -> String {
    let mut out = err.to_string();
    let mut cur = std::error::Error::source(err);
    while let Some(src) = cur {
        out.push_str(": ");
        out.push_str(&src.to_string());
        cur = src.source();
    }
    out
}

/// 访问注册中心。macOS 上 reqwest 的系统 TLS 会被 robot.link 断开，这里用 OpenSSL。
fn registry_get(url: &str) -> Result<(u16, String), String> {
    let rest = url.strip_prefix("https://").ok_or_else(|| format!("注册中心地址不是 https: {url}"))?;
    let (hostport, pathq) = rest.split_once('/').unwrap_or((rest, ""));
    let path = format!("/{pathq}");
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "注册中心端口无效".to_string())?),
        None => (hostport, 443u16),
    };
    let tcp = std::net::TcpStream::connect((host, port)).map_err(|e| format!("连接注册中心失败: {e}"))?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(15)));
    let mut builder = openssl::ssl::SslConnector::builder(openssl::ssl::SslMethod::tls())
        .map_err(|e| format!("TLS 初始化失败: {e}"))?;
    if std::path::Path::new("/etc/ssl/cert.pem").exists() {
        builder.set_ca_file("/etc/ssl/cert.pem").map_err(|e| format!("无法加载系统证书: {e}"))?;
    }
    let mut stream = builder.build().connect(host, tcp).map_err(|e| format!("注册中心 TLS 失败: {e}"))?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
    );
    std::io::Write::write_all(&mut stream, req.as_bytes()).map_err(|e| format!("请求注册中心失败: {e}"))?;
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut raw).map_err(|e| format!("读取注册中心响应失败: {e}"))?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("注册中心响应不完整")?;
    let code = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|c| c.parse().ok()).unwrap_or(0);
    Ok((code, body.to_string()))
}

async fn registry_get_async(url: String) -> Result<(u16, String), String> {
    tokio::task::spawn_blocking(move || registry_get(&url)).await.map_err(|e| e.to_string())?
}

fn urlencoding_min(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

fn session_cookie(token: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("admind={token}; HttpOnly; Path=/; SameSite=Lax{secure}; Max-Age=28800")
}

fn clear_cookie(secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("admind=; HttpOnly; Path=/; SameSite=Lax{secure}; Max-Age=0")
}

fn cookie_tokens(headers: &HeaderMap) -> Vec<String> {
    let mut out = Vec::new();
    let Some(c) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return out;
    };
    for part in c.split(';') {
        if let Some(v) = part.trim().strip_prefix("admind=") {
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
    if let Some(t) = bearer_token(&headers) {
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

#[derive(Deserialize)]
struct SignupBody {
    domain: String,
    email: String,
}
/// 向全球注册中心提交域名申请，随后前端等待本机收到网格通知。
async fn names_signup(State(s): State<AppState>, headers: HeaderMap, Json(b): Json<SignupBody>) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if s.auth.must_change() {
        return (StatusCode::FORBIDDEN, "首次登录请先修改密码").into_response();
    }
    let ident = match s
        .http
        .get(format!("{}/identity", s.nmd_api))
        .header("x-admin-token", &s.nmd_token)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "ok": false, "error": format!("nmd 不可达: {}", err_chain(&e)) })),
            )
                .into_response();
        }
    };
    let ident: serde_json::Value = ident.json().await.unwrap_or_default();
    let node_id = ident.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.len() != 64 {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": "读不到本机 node id" })),
        )
            .into_response();
    }
    let url = format!(
        "{}/api/signup?nodeid={}&domain={}&email={}",
        s.registry,
        node_id,
        urlencoding_min(&b.domain),
        urlencoding_min(&b.email)
    );
    match registry_get_async(url).await {
        Ok((code, body)) => {
            let code = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY);
            (code, [(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": format!("注册中心不可达: {e}") })),
        )
            .into_response(),
    }
}

async fn names_pending(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let node_id = match local_node_id(&s).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let url = format!("{}/api/signup/list?nodeid={}", s.registry, node_id);
    match registry_get_async(url).await {
        Ok((code, body)) => {
            let code = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY);
            (code, [(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": format!("注册中心不可达: {e}") })),
        )
            .into_response(),
    }
}

async fn local_node_id(s: &AppState) -> Result<String, Response> {
    let ident = match s
        .http
        .get(format!("{}/identity", s.nmd_api))
        .header("x-admin-token", &s.nmd_token)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({ "ok": false, "error": format!("nmd 不可达: {}", err_chain(&e)) })),
            )
                .into_response());
        }
    };
    let ident: serde_json::Value = ident.json().await.unwrap_or_default();
    let node_id = ident.get("node_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if node_id.len() != 64 {
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": "读不到本机 node id" })),
        )
            .into_response());
    }
    Ok(node_id)
}

/// 重启本机 nmd。优先走 systemd（系统服务或用户服务）；没有服务时按监听端口找到进程再拉起。
async fn nmd_restart(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if !session_ok(&s, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if s.auth.must_change() {
        return (StatusCode::FORBIDDEN, "首次登录请先修改密码").into_response();
    }
    let how = match restart_nmd_service(&s.nmd_api) {
        Ok(msg) => msg,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "ok": false, "error": e })),
            )
                .into_response();
        }
    };
    let back = wait_nmd_up(&s).await;
    Json(json!({ "ok": true, "how": how, "back": back })).into_response()
}

fn systemctl(user: bool, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = Command::new("systemctl");
    if user {
        cmd.arg("--user");
    }
    cmd.args(args).output().ok()
}

fn unit_live(user: bool) -> bool {
    let Some(out) = systemctl(user, &["is-active", "nmd"]) else {
        return false;
    };
    let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
    matches!(state.as_str(), "active" | "activating" | "reloading")
}

fn restart_nmd_service(nmd_api: &str) -> Result<String, String> {
    if unit_live(false) {
        let out = systemctl(false, &["restart", "nmd"]).ok_or("无法执行 systemctl")?;
        if out.status.success() {
            return Ok("已通过 systemctl 重启 nmd".into());
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { "systemctl 重启 nmd 失败".into() } else { err });
    }
    if unit_live(true) {
        let out = systemctl(true, &["restart", "nmd"]).ok_or("无法执行 systemctl --user")?;
        if out.status.success() {
            return Ok("已通过 systemctl --user 重启 nmd".into());
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { "systemctl --user 重启 nmd 失败".into() } else { err });
    }
    restart_nmd_process(nmd_api)
}

fn api_port(nmd_api: &str) -> Option<u16> {
    let rest = nmd_api.split("://").nth(1).unwrap_or(nmd_api);
    let hostport = rest.split('/').next().unwrap_or(rest);
    hostport.rsplit_once(':')?.1.parse().ok()
}

fn listener_pid(port: u16) -> Option<u32> {
    let out = Command::new("lsof")
        .args(["-nP", "-t", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next()?.trim().parse().ok()
}

fn process_args(pid: u32) -> Option<Vec<String>> {
    let out = Command::new("ps").args(["-ww", "-o", "args=", "-p", &pid.to_string()]).output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if line.is_empty() {
        return None;
    }
    Some(line.split_whitespace().map(str::to_string).collect())
}

fn process_cwd(pid: u32) -> Option<std::path::PathBuf> {
    if let Ok(p) = std::fs::read_link(format!("/proc/{pid}/cwd")) {
        return Some(p);
    }
    let out = Command::new("lsof").args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]).output().ok()?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(path) = line.strip_prefix('n') {
            if !path.is_empty() {
                return Some(std::path::PathBuf::from(path));
            }
        }
    }
    None
}

fn restart_nmd_process(nmd_api: &str) -> Result<String, String> {
    let port = api_port(nmd_api).ok_or("无法从 nmd 地址解析端口")?;
    let pid = listener_pid(port).ok_or(format!("没有进程在监听 {port}"))?;
    let args = process_args(pid).ok_or("读不到 nmd 的启动命令")?;
    let exe = args.first().map(String::as_str).unwrap_or("");
    if !exe.ends_with("/nmd") && exe != "nmd" && !exe.ends_with("\\nmd") {
        return Err("监听端口上的进程不是 nmd".into());
    }
    let cwd = process_cwd(pid).ok_or("读不到 nmd 的工作目录")?;
    let script = format!("while kill -0 {pid} 2>/dev/null; do sleep 0.2; done; cd \"$NMD_CWD\" && exec \"$@\"");
    Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .args(&args)
        .env("NMD_CWD", &cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("无法安排重启: {e}"))?;
    let killed = Command::new("kill").arg(pid.to_string()).status().map(|s| s.success()).unwrap_or(false);
    if !killed {
        return Err("已安排拉起，但没能结束当前 nmd".into());
    }
    Ok("已重启 nmd 进程".into())
}

async fn wait_nmd_up(s: &AppState) -> bool {
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        if s.http
            .get(format!("{}/identity", s.nmd_api))
            .header("x-admin-token", &s.nmd_token)
            .send()
            .await
            .is_ok()
        {
            return true;
        }
    }
    false
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
        Ok(resp) => proxy_upstream(resp).await,
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("nmd 不可达: {e}") })),
        )
            .into_response(),
    }
}

/// nmd 自己的 401 不能原样返回，否则前端会当成管理台会话失效并跳回登录页。
async fn proxy_upstream(resp: reqwest::Response) -> Response {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "nmd 拒绝了管理请求，请检查管理令牌" })),
        )
            .into_response();
    }
    let code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    (code, [(header::CONTENT_TYPE, "application/json")], body).into_response()
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
        Ok(resp) => proxy_upstream(resp).await,
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

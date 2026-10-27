//! 智能体工作台(cmx-agent 库内嵌)宿主:零复制、cmx-agent 仓库零改动、**全参数化适配**。
//!
//! 配置 `<nanomesh-data>/cmx-workbench.json`(serde 全默认,缺失=默认;改动经 cmx_restart 生效):
//! 每个适配点都可配、都可切回原版行为——`login:"portal"` 恢复原门户登录、`dataDir:"~/.cmx-agent"`
//! 共享原 TrueMate 数据、`model.mode:"cmx"` 走原模型配置链、`shim` 控制登录垫片注入。
//!
//! - **构造**:`DesktopAppBuilder::new(workdir, dataDir, model)`;login=off 不调 `.auth()`(免登录,
//!   其 CLI serve 同形态);login=portal 调 `.auth(AuthConfig{base_url})`(原版登录完整回归)。
//! - **net_rt**:cmx-agent 的 reqwest 在 Tauri 运行时上会挂;照其 shell 配方独立 multi_thread
//!   运行时 + spawn_blocking 桥。
//! - **内置 axum**:实现 cmx-agent-web 公开协议(POST /api、SSE /api/stream、SSE /api/subscribe、
//!   GET /api/attachment、POST /api/att/upload)+ 同源静态服务其 ui/(路径引用原仓库,不拷贝);
//!   index.html 按 shim 配置追加 ~20 行登录垫片、按 hideRail 配置追加边栏隐藏样式
//!   (均为 nanomesh 适配层,serve 时内存追加,cmx-agent 零改动)。
//! - **安全**:绑 127.0.0.1 + loopback Host 校验 + token 闸(默认每次启动随机;?t= 首访种 cookie)。

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use cmx_agent_app::{dispatch_json, AgentApp, AuthConfig, DesktopAppBuilder};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, State as TState};

use crate::state::AppState;

// ── 配置 ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct CmxModelCfg {
    /// "cmx"=原模型配置链(env/providers.json/model.json) | "openai"=下列显式值 | "nanomesh"(P1 预留)
    pub mode: String,
    pub base_url: String,
    /// 支持 "${ENV:VAR}" 引用环境变量,避免明文落盘。
    pub api_key: String,
    pub name: String,
}
impl Default for CmxModelCfg {
    fn default() -> Self {
        Self { mode: "cmx".into(), base_url: String::new(), api_key: String::new(), name: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct CmxCfg {
    /// 总开关:false=隐藏标签、不启动。
    pub enabled: bool,
    /// true=app 启动即拉起;false=点标签懒启动。
    pub autostart: bool,
    /// "off"=免登录(不调 .auth) | "portal"=原版门户登录(调 .auth)。
    pub login: String,
    /// login=portal 时的门户地址。
    pub portal_base_url: String,
    /// 登录垫片:"auto"=随 login(off 注入/portal 不注入) | "on" | "off"。
    pub shim: String,
    /// 嵌入态隐藏 cmx-agent 自带左侧纵向边栏(iframe 内注 CSS);false=恢复原版带边栏界面
    /// (含其 IM 面板与设置中心入口)。
    pub hide_rail: bool,
    /// 空=自动(dev 源码路径);可指任意 cmx-agent ui/ 目录。
    pub ui_dir: String,
    /// 空=<nanomesh-data>/cmx-agent(隔离);"~/.cmx-agent"=共享原 TrueMate 数据。
    pub data_dir: String,
    /// agent 工作目录;空=$HOME。
    pub workdir: String,
    /// 0=随机端口。
    pub port: u16,
    /// "auto"=每次启动随机 | 固定串 | "off"=关 token 闸(仅剩 loopback,不建议)。
    pub token: String,
    pub model: CmxModelCfg,
    /// cmx-agent-im 遥控(默认关;开启要求 login=portal)。P1 接线。
    pub im: bool,
}
impl Default for CmxCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            autostart: false,
            login: "off".into(),
            portal_base_url: "https://cmx.pansoft.com".into(),
            shim: "auto".into(),
            hide_rail: true,
            ui_dir: String::new(),
            data_dir: String::new(),
            workdir: String::new(),
            port: 0,
            token: "auto".into(),
            model: CmxModelCfg::default(),
            im: false,
        }
    }
}

fn cfg_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::data_dir(app)?.join("cmx-workbench.json"))
}

pub(crate) fn load_cfg(app: &AppHandle) -> CmxCfg {
    cfg_path(app)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_cfg(app: &AppHandle, cfg: &CmxCfg) -> Result<(), String> {
    let p = cfg_path(app)?;
    let s = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&p, s).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn expand_home(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(rest);
        }
    }
    PathBuf::from(s)
}

/// "${ENV:VAR}" → 环境变量值;其余原样。
fn expand_env(s: &str) -> String {
    s.strip_prefix("${ENV:")
        .and_then(|r| r.strip_suffix('}'))
        .and_then(|v| std::env::var(v).ok())
        .unwrap_or_else(|| s.to_string())
}

// ── 宿主状态 ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub(crate) struct CmxHost {
    inner: tokio::sync::Mutex<Option<CmxRunning>>,
}

struct CmxRunning {
    app: Arc<AgentApp>,
    port: u16,
    token: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

/// 专用运行时(io+time):cmx-agent 的网络调用不能跑在 Tauri 自带运行时(reqwest 会挂)。
fn net_rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build cmx net runtime")
    })
}

async fn dispatch_on_net(app: Arc<AgentApp>, payload: String) -> String {
    tauri::async_runtime::spawn_blocking(move || net_rt().block_on(dispatch_json(&app, &payload)))
        .await
        .unwrap_or_else(|e| json!({"ok":false,"error":format!("task failed: {e}")}).to_string())
}

/// ui 目录解析:配置显式 > build.rs 从 git/path 依赖定位 > 常见本地 checkout。
/// 打包 resources 版 P1 接线。
fn resolve_ui_dir(cfg: &CmxCfg) -> Option<PathBuf> {
    if !cfg.ui_dir.trim().is_empty() {
        let p = expand_home(cfg.ui_dir.trim());
        return p.join("index.html").is_file().then_some(p);
    }
    if let Some(p) = option_env!("CMX_AGENT_UI_DIR") {
        let p = PathBuf::from(p);
        if p.join("index.html").is_file() {
            return Some(p);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for rel in [
        "../../../cmx-agent/crates/cmx-agent-web/ui",
        "../../../../../../cmx-workspace/backend/cmx-agent/crates/cmx-agent-web/ui",
    ] {
        let p = manifest.join(rel);
        if p.join("index.html").is_file() {
            return Some(p);
        }
    }
    None
}

/// 登录垫片(~20 行,nanomesh 适配层):拦 current_user/login/logout 返回合成本地用户 +
/// 把 #/login 弹回主视图。仅 shim 生效时注入;cmx-agent 文件零改动(serve 时内存追加)。
/// 信封形状必须对齐 dispatch_json:`{ok,data:{user}}`(login.js 判 `r.data.user`),
/// user 字段对齐 LoggedInUser(settings.js 读 nickname/username/user_id,main.js 读 must_change_password)。
const BOOT_SHIM: &str = r##"<script>
(function(){
  // Tauri 可能把 __TAURI__ 注进所有 frame:iframe 里它会让 bridge.js 走 invoke("agent")
  // (nanomesh 无此命令,远程 origin IPC 也被拒)→ 异常 → 登录页。嵌入态强制 HTTP 分支。
  try{ delete window.__TAURI__; }catch(_){}
  try{ window.__TAURI__=undefined; }catch(_){}
  var USER={user_id:"local",username:"local",nickname:"NanoMesh",roles:[],must_change_password:false};
  var SELF={ok:true,data:{user:USER}};
  var of=window.fetch.bind(window);
  window.fetch=function(u,o){
    try{
      if(typeof u==="string"&&u.indexOf("/api")===0&&o&&typeof o.body==="string"){
        var c=JSON.parse(o.body);
        if(c&&(c.cmd==="current_user"||c.cmd==="login"||c.cmd==="logout"))
          return Promise.resolve(new Response(JSON.stringify(SELF),{status:200,headers:{"Content-Type":"application/json"}}));
      }
    }catch(_){}
    return of(u,o);
  };
  // #/login 弹回主视图(免登录态该视图是死路;也兜住登出按钮)。主视图 hash 是 "#/"(login.js 同款)。
  var fix=function(){
    if(location.hash==="#/login"){
      location.hash="#/";
      if(typeof showMainView==="function") showMainView();
    }
  };
  window.addEventListener("hashchange",fix);
  document.addEventListener("DOMContentLoaded",fix);
})();
</script>"##;

/// 边栏隐藏样式(hideRail=true):cmx-agent 的 .activitybar 是 flex 定宽项(52px),
/// display:none 后 aside+主区自动占满。恢复原版=配置 hideRail=false(重启工作台生效)。
const HIDE_RAIL_CSS: &str = "<style>.activitybar{display:none!important}</style>";

#[derive(Clone)]
struct HttpState {
    app: Arc<AgentApp>,
    /// 空串=token 闸关闭(cfg.token="off")。
    token: String,
    ui: PathBuf,
    inject_shim: bool,
    hide_rail: bool,
}

/// token 闸:iframe 场景(tauri 壳内嵌 127.0.0.1)是跨站上下文,SameSite=Lax 的 cookie
/// 不随请求发送——cookie 只对「独立浏览器标签直开」有效。主通道是 Referer:页面 URL 为
/// `/?t=TOKEN`,同源子资源/fetch/EventSource/img 默认携带完整 Referer(含查询);跨源导航
/// 按 strict-origin-when-cross-origin 只送 origin,token 不外泄。外部攻击页 Referer 无 token。
fn token_ok(st: &HttpState, headers: &HeaderMap, query: Option<&str>) -> bool {
    if st.token.is_empty() {
        return true; // 配置显式关闭
    }
    let tok = format!("t={}", st.token);
    let q_has = |q: &str| q.split('&').any(|kv| kv == tok);
    if query.map(q_has).unwrap_or(false) {
        return true;
    }
    if headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.split_once('?'))
        .map(|(_, q)| q_has(q))
        .unwrap_or(false)
    {
        return true;
    }
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|c| c.contains(&format!("cmxt={}", st.token)))
        .unwrap_or(false)
}

fn loopback_ok(headers: &HeaderMap) -> bool {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| h.starts_with("127.0.0.1") || h.starts_with("localhost"))
        .unwrap_or(false)
}

fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

async fn serve_static(st: &HttpState, rel: &str, set_cookie: bool) -> axum::response::Response {
    let rel = rel.trim_start_matches('/');
    if rel.contains("..") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let path = if rel.is_empty() { st.ui.join("index.html") } else { st.ui.join(rel) };
    match std::fs::read(&path) {
        Ok(mut bytes) => {
            let mime = mime_of(&path.to_string_lossy());
            if path.file_name().map(|n| n == "index.html").unwrap_or(false) {
                if st.hide_rail {
                    bytes.extend_from_slice(HIDE_RAIL_CSS.as_bytes());
                }
                if st.inject_shim {
                    bytes.extend_from_slice(BOOT_SHIM.as_bytes());
                }
            }
            let mut b = axum::response::Response::builder()
                .header(header::CONTENT_TYPE, mime)
                .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
            if set_cookie && !st.token.is_empty() {
                b = b.header(header::SET_COOKIE, format!("cmxt={}; Path=/; SameSite=Lax", st.token));
            }
            b.body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn h_index(State(st): State<HttpState>, headers: HeaderMap, uri: axum::http::Uri) -> axum::response::Response {
    if !loopback_ok(&headers) || !token_ok(&st, &headers, uri.query()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    serve_static(&st, "", true).await
}

async fn h_asset(State(st): State<HttpState>, headers: HeaderMap, uri: axum::http::Uri) -> axum::response::Response {
    if !loopback_ok(&headers) || !token_ok(&st, &headers, uri.query()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    serve_static(&st, uri.path(), false).await
}

async fn h_api(State(st): State<HttpState>, headers: HeaderMap, body: String) -> axum::response::Response {
    if !loopback_ok(&headers) || !token_ok(&st, &headers, None) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let resp = dispatch_on_net(st.app.clone(), body).await;
    ([(header::CONTENT_TYPE, "application/json")], resp).into_response()
}

/// 流式回合(协议对齐 cmx-agent-web /api/stream:逐事件 SSE,尾帧 stream_done/stream_error)。
async fn h_stream(State(st): State<HttpState>, headers: HeaderMap, body: String) -> axum::response::Response {
    if !loopback_ok(&headers) || !token_ok(&st, &headers, None) {
        return StatusCode::FORBIDDEN.into_response();
    }
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio_stream::wrappers::UnboundedReceiverStream;
    use tokio_stream::StreamExt;

    #[derive(Deserialize)]
    struct Req {
        session_id: String,
        text: String,
        #[serde(default)]
        images: Vec<cmx_agent_app::IncomingImage>,
        #[serde(default)]
        files: Vec<cmx_agent_app::IncomingFile>,
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    let req: Result<Req, _> = serde_json::from_str(&body);
    let app = st.app.clone();
    // 模型调用走网络 → 必须在 net_rt 上跑(与 dispatch_on_net 同理)。
    tauri::async_runtime::spawn_blocking(move || {
        net_rt().block_on(async move {
            let Ok(req) = req else {
                let _ = tx.send(json!({"kind":"stream_error","message":"invalid request json"}));
                return;
            };
            let sink = Arc::new(cmx_agent_app::ChannelSink::new(tx.clone()));
            match app
                .send_streaming_with_attachments(&req.session_id, &req.text, req.images, req.files, sink)
                .await
            {
                Ok(_) => {
                    let _ = tx.send(json!({"kind":"stream_done"}));
                }
                Err(e) => {
                    let _ = tx.send(json!({"kind":"stream_error","message": e.to_string()}));
                }
            }
        })
    });
    let stream = UnboundedReceiverStream::new(rx)
        .map(|v| Ok::<Event, std::convert::Infallible>(Event::default().data(v.to_string())));
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

/// 事件总线订阅(协议对齐 /api/subscribe):broadcast → SSE 帧转发。
async fn h_subscribe(State(st): State<HttpState>, headers: HeaderMap) -> axum::response::Response {
    if !loopback_ok(&headers) || !token_ok(&st, &headers, None) {
        return StatusCode::FORBIDDEN.into_response();
    }
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio::sync::broadcast::error::RecvError;
    use tokio_stream::wrappers::UnboundedReceiverStream;
    use tokio_stream::StreamExt;

    let mut rx = st.app.event_bus().subscribe();
    let (tx, rx_out) = tokio::sync::mpsc::unbounded_channel::<Event>();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => {
                    let data = serde_json::to_string(&env).unwrap_or_default();
                    if tx.send(Event::default().data(data)).is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
    let stream = UnboundedReceiverStream::new(rx_out).map(Ok::<Event, std::convert::Infallible>);
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

/// 附件直出(协议对齐 /api/attachment;归属/穿越校验在 AgentApp::read_attachment)。
async fn h_attachment(
    State(st): State<HttpState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    if !loopback_ok(&headers) || !token_ok(&st, &headers, None) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let (Some(sid), Some(rel)) = (q.get("session_id"), q.get("path")) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match st.app.read_attachment(sid, rel) {
        Ok((bytes, mime, inline)) => {
            let mut b = axum::response::Response::builder()
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, "private, max-age=86400")
                .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
            if !inline {
                let name = rel.rsplit('/').next().unwrap_or("attachment");
                b = b.header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""));
            }
            b.body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// 附件暂存上传(协议对齐 /api/att/upload:原始字节 → 暂存区,返回数据根相对路径)。
async fn h_upload(
    State(st): State<HttpState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    if !loopback_ok(&headers) || !token_ok(&st, &headers, None) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(name) = q.get("name").map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let ext = cmx_agent_app::attachments::ext_for_name(name);
    match cmx_agent_app::attachments::stage_incoming_bytes(&st.app, &body, ext) {
        Ok((path, bytes)) => axum::Json(json!({ "ok": true, "data": { "path": path, "bytes": bytes } })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, json!({"ok":false,"error":e.to_string()}).to_string()).into_response(),
    }
}

// ── 生命周期命令 ─────────────────────────────────────────────────────────────

async fn start_inner(app: &AppHandle, state: &TState<'_, AppState>) -> Result<serde_json::Value, String> {
    {
        let g = state.cmx.inner.lock().await;
        if let Some(r) = g.as_ref() {
            return Ok(json!({ "running": true, "port": r.port,
                "url": format!("http://127.0.0.1:{}/?t={}", r.port, r.token) }));
        }
    }
    let cfg = load_cfg(app);
    if !cfg.enabled {
        return Err("工作台已在配置中停用(enabled=false)".into());
    }
    let Some(ui) = resolve_ui_dir(&cfg) else {
        return Err(
            "找不到 cmx-agent UI 资产(配置 uiDir，或确保已拉取 gitee.com/warpdrivelabs/cmx-agent 依赖)"
                .into(),
        );
    };
    // 数据目录:配置显式(可 ~/.cmx-agent 共享原 TrueMate) > nanomesh 隔离目录。
    let data_dir = if cfg.data_dir.trim().is_empty() {
        crate::data_dir(app)?.join("cmx-agent")
    } else {
        expand_home(cfg.data_dir.trim())
    };
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    // belt-and-suspenders:部分 cmx helper 读全局 data_dir(env)。SAFETY: 启动期单次设置。
    unsafe { std::env::set_var("CMX_AGENT_DATA_DIR", &data_dir) };
    let workdir = if cfg.workdir.trim().is_empty() {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
    } else {
        expand_home(cfg.workdir.trim())
    };
    // 模型:mode=openai → 显式 env 注入后仍走 select_model(其 env 链优先级最高,等效显式指定);
    //        mode=cmx → 原配置链;mode=nanomesh → P1(当前回落 cmx 链并警告)。
    match cfg.model.mode.as_str() {
        "openai" => {
            // SAFETY: 启动期单次设置。
            unsafe {
                std::env::set_var("CMX_AGENT_MODEL_BASE_URL", expand_env(&cfg.model.base_url));
                std::env::set_var("CMX_AGENT_MODEL_API_KEY", expand_env(&cfg.model.api_key));
                if !cfg.model.name.trim().is_empty() {
                    std::env::set_var("CMX_AGENT_MODEL", cfg.model.name.trim());
                }
            }
        }
        "nanomesh" => tracing::warn!("model.mode=nanomesh 为 P1 预留,暂回落 cmx 配置链"),
        _ => {}
    }
    let model = cmx_agent_app::select_model(Some(&data_dir.join("config")));

    // 构造(可能做 IO/网络探测)→ net_rt 上执行。login=portal 时挂 .auth = 原版登录门回归。
    let login_portal = cfg.login.eq_ignore_ascii_case("portal");
    let portal_base = cfg.portal_base_url.clone();
    let agent = tauri::async_runtime::spawn_blocking({
        let data_dir = data_dir.clone();
        move || -> Result<Arc<AgentApp>, String> {
            net_rt().block_on(async move {
                let mut b = DesktopAppBuilder::new(workdir, data_dir, model).interactive_approval();
                if login_portal {
                    b = b.auth(AuthConfig { base_url: portal_base });
                }
                let a = b.build().map_err(|e| e.to_string())?.into_shared();
                a.install_assembler();
                a.sweep_orphan_attachments();
                Ok(a)
            })
        }
    })
    .await
    .map_err(|e| e.to_string())??;

    // 垫片:auto=仅免登录态注入;on/off 强制。
    let inject_shim = match cfg.shim.as_str() {
        "on" => true,
        "off" => false,
        _ => !login_portal,
    };
    // token:auto=随机;off=关闸;其余=固定串。
    let token = match cfg.token.as_str() {
        "off" => String::new(),
        "auto" | "" => {
            use rand::RngCore;
            let mut b = [0u8; 16];
            rand::rngs::OsRng.fill_bytes(&mut b);
            crate::hex(&b)
        }
        fixed => fixed.to_string(),
    };

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", cfg.port))
        .await
        .map_err(|e| format!("绑定 127.0.0.1:{} 失败: {e}", cfg.port))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let http = HttpState { app: agent.clone(), token: token.clone(), ui, inject_shim, hide_rail: cfg.hide_rail };
    let router = axum::Router::new()
        .route("/", get(h_index))
        .route("/index.html", get(h_index))
        .route("/css/{*p}", get(h_asset))
        .route("/js/{*p}", get(h_asset))
        .route("/vendor/{*p}", get(h_asset))
        .route("/cmx.png", get(h_asset))
        .route("/api", post(h_api))
        .route("/api/stream", post(h_stream))
        .route("/api/subscribe", get(h_subscribe))
        .route("/api/attachment", get(h_attachment))
        .route("/api/att/upload", post(h_upload))
        .with_state(http)
        .layer(axum::extract::DefaultBodyLimit::max(96 * 1024 * 1024));
    let (sd_tx, sd_rx) = tokio::sync::oneshot::channel::<()>();
    tauri::async_runtime::spawn(async move {
        let serve = axum::serve(listener, router).with_graceful_shutdown(async {
            let _ = sd_rx.await;
        });
        if let Err(e) = serve.await {
            tracing::warn!("cmx workbench http exited: {e}");
        }
    });

    *state.cmx.inner.lock().await =
        Some(CmxRunning { app: agent, port, token: token.clone(), shutdown: Some(sd_tx) });
    tracing::info!(port, login = %cfg.login, shim = inject_shim, "cmx-agent workbench started");
    Ok(json!({ "running": true, "port": port,
        "url": format!("http://127.0.0.1:{port}/?t={token}") }))
}

/// 启动工作台(幂等)。返回 {running, port, url}。
#[tauri::command]
pub(crate) async fn cmx_start(app: AppHandle, state: TState<'_, AppState>) -> Result<serde_json::Value, String> {
    start_inner(&app, &state).await
}

/// 状态查询。
#[tauri::command]
pub(crate) async fn cmx_status(app: AppHandle, state: TState<'_, AppState>) -> Result<serde_json::Value, String> {
    let cfg = load_cfg(&app);
    let g = state.cmx.inner.lock().await;
    Ok(match g.as_ref() {
        Some(r) => json!({ "running": true, "enabled": cfg.enabled, "port": r.port,
            "url": format!("http://127.0.0.1:{}/?t={}", r.port, r.token) }),
        None => json!({ "running": false, "enabled": cfg.enabled }),
    })
}

/// 重启工作台:停 HTTP + 丢实例 → 按最新配置重建(「智能体设置」保存后调)。
#[tauri::command]
pub(crate) async fn cmx_restart(app: AppHandle, state: TState<'_, AppState>) -> Result<serde_json::Value, String> {
    if let Some(mut r) = state.cmx.inner.lock().await.take() {
        if let Some(tx) = r.shutdown.take() {
            let _ = tx.send(());
        }
        // AgentApp 随 Arc 释放;JSONL 会话已落盘,无需显式 flush。
    }
    start_inner(&app, &state).await
}

/// 读配置(设置表单)。
#[tauri::command]
pub(crate) async fn cmx_cfg_get(app: AppHandle) -> Result<CmxCfg, String> {
    Ok(load_cfg(&app))
}

/// 写配置(设置表单;生效需 cmx_restart)。
#[tauri::command]
pub(crate) async fn cmx_cfg_set(app: AppHandle, cfg: CmxCfg) -> Result<(), String> {
    save_cfg(&app, &cfg)
}

//! 智能体工作台(cmx-agent 库内嵌)宿主:零复制、cmx-agent 仓库零改动、**全参数化适配**。
//!
//! 配置 `<nanomesh-data>/cmx-workbench.json`(serde 全默认,缺失=默认;改动经 cmx_restart 生效):
//! 每个适配点都可配、都可切回原版行为——`login:"portal"` 恢复原门户登录、`dataDir` 默认
//! `~/.cmx-agent`(与独立版 TrueMate 共享同一份数据,填其它路径才隔离)、`model.mode:"cmx"`
//! 走原模型配置链、`shim` 控制登录垫片注入。
//!
//! - **构造**:`DesktopAppBuilder::new(workdir, dataDir, model)`;login=off 不调 `.auth()`(免登录,
//!   其 CLI serve 同形态);login=portal 调 `.auth(AuthConfig{base_url})`(原版登录完整回归)。
//! - **net_rt**:cmx-agent 的 reqwest 在 Tauri 运行时上会挂;照其 shell 配方独立 multi_thread
//!   运行时 + spawn_blocking 桥。
//! - **内置 axum**:实现 cmx-agent-web 公开协议(POST /api、SSE /api/stream、SSE /api/subscribe、
//!   GET /api/attachment、POST /api/att/upload)+ 同源静态服务其 ui/(路径引用原仓库,不拷贝);
//!   index.html 按 shim 配置追加 ~20 行登录垫片、按 hideRail 配置追加边栏隐藏样式
//!   (均为 nanomesh 适配层,serve 时内存追加,cmx-agent 零改动)。
//! - **IM 遥控**:cmx-agent-im 通道桥(飞书/QQ/微信)接线;组件「设置 → IM 遥控」的三条契约命令
//!   `im_config` / `im_wechat_login` / `im_qq_login` 由本宿主实现(HTTP 与 invoke 双面同语义),
//!   配置落数据根 `config/im.json`,保存/扫码即热重载,工作台启动时自动起已启用通道。
//! - **安全**:绑 127.0.0.1 + loopback Host 校验 + token 闸(默认每次启动随机;?t= 首访种 cookie)。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use cmx_agent_app::{dispatch_json, AgentApp, AuthConfig, DesktopAppBuilder};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Manager, State as TState};

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
    /// 空=~/.cmx-agent(默认与独立版 TrueMate / Web 壳共享同一份数据:模型配置/会话/workspaces);
    /// 填其它路径=隔离(多实例/测试)。
    pub data_dir: String,
    /// agent 工作目录;空=$HOME。
    pub workdir: String,
    /// 0=随机端口。
    pub port: u16,
    /// "auto"=每次启动随机 | 固定串 | "off"=关 token 闸(仅剩 loopback,不建议)。
    pub token: String,
    pub model: CmxModelCfg,
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

/// `~` / `~/x` 展开。必须走 tauri 的 home_dir，**不能只看环境变量 HOME**：Windows 上
/// 从资源管理器/IDE 启动的 GUI 进程没有 HOME（只有 Git Bash 会设），此时 `~/.cmx-agent`
/// 会被当成相对路径，落到进程 cwd 下的字面 `~` 目录 —— 数据根与 TrueMate 错开，
/// 表现为模型列表空、会话/workspaces 全空（且不会报错）。
fn expand_home(app: &AppHandle, s: &str) -> PathBuf {
    if s == "~" {
        if let Ok(h) = app.path().home_dir() {
            return h;
        }
    } else if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        if let Ok(h) = app.path().home_dir() {
            return h.join(rest);
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
    /// 组件 invoke 桥的用户垫片:true=「agent」命令拦 current_user/login/logout 返回合成本地用户
    /// (对齐 BOOT_SHIM 语义;login=portal 时 false,透传内核认证)。
    shim_user: bool,
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

// ── IM 遥控（cmx-agent-im 通道桥）────────────────────────────────────────────
// 组件「设置 → IM 遥控」面板经命令总线调用三条契约命令（Tauri 壳同名命令的命令总线版）：
// im_config / im_wechat_login / im_qq_login。配置存数据根 `config/im.json`（脱敏回显、
// 保存即热重载、扫码凭证落盘即上线），工作台启动时按数据根自动起已启用通道。
// 全部在 net_rt 上执行——飞书 Stream 长连接 / 微信长轮询是常驻网络任务，不能跑 Tauri 运行时。
// ⚠ 数据目录设成 `~/.cmx-agent`（与独立版 TrueMate 共享）时，勿两边同时开 IM：通道会双在线
//    竞争消费同一条消息。

/// IM 桥句柄（热重载停旧用：发 stop 信号 + provider 断长连接）。
type ImBridgeSlot = (tokio::sync::watch::Sender<bool>, Arc<dyn cmx_agent_im::ImProvider>);
static IM_BRIDGES: OnceLock<Mutex<Vec<ImBridgeSlot>>> = OnceLock::new();
/// 微信扫码登录会话槽（start 取码入槽 → 前端 ~2s 一次 poll；confirmed 落盘后出槽）。
static WECHAT_QR: Mutex<Option<cmx_agent_im::WechatQrSession>> = Mutex::new(None);
/// QQ 机器人扫码绑定会话槽（官方 q.qq.com/lite 绑定任务）。
static QQ_BIND: Mutex<Option<cmx_agent_im::QqBindSession>> = Mutex::new(None);

fn im_err(code: &str, msg: &str) -> String {
    json!({ "ok": false, "error": { "code": code, "message": msg } }).to_string()
}

fn im_slots() -> &'static Mutex<Vec<ImBridgeSlot>> {
    IM_BRIDGES.get_or_init(|| Mutex::new(Vec::new()))
}

/// 停掉全部 IM 桥（热重载 / 工作台重启；幂等）。
fn stop_im_bridges() {
    if let Ok(mut slots) = im_slots().lock() {
        let had = !slots.is_empty(); // drain 前记录（drain 后再判空恒 false）
        for (stop_tx, provider) in slots.drain(..) {
            let _ = stop_tx.send(true);
            provider.stop();
        }
        if had {
            tracing::info!("[cmx][im] 旧桥已停（热重载/重启）");
        }
    }
}

/// 启动 IM 遥控桥（配置了才起）。**必须在 net_rt 上下文内调用**——内部 `tokio::spawn`
/// 的常驻任务要落在该运行时上。
fn start_im_bridges(app: &Arc<AgentApp>) {
    stop_im_bridges();
    let data_dir = cmx_agent_app::shared_data_dir();
    let resolved = match cmx_agent_im::resolve(Some(&data_dir)) {
        Ok(r) => r,
        Err(e) => {
            // 常见原因：env/im.json 均未配置 = 未启用 IM，静默不打扰。
            tracing::info!("[cmx][im] 未启用 IM 遥控（{e}）");
            return;
        }
    };
    let allow = resolved.allow.clone();
    let (personal, full_access) = (resolved.personal, resolved.full_access);
    let labels: Vec<&str> = resolved.channels.iter().map(|c| c.kind.label()).collect();
    tracing::info!(
        "[cmx][im] 启动 IM 遥控（provider={}，配置来源={}）",
        labels.join("+"),
        resolved.source
    );
    for ch in resolved.channels {
        let kind_label = ch.kind.label();
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let provider = Arc::clone(&ch.provider);
        let app = Arc::clone(app);
        let allow = allow.clone();
        tokio::spawn(async move {
            // 飞书 Stream：起常驻后台 task；微信长轮询：trait 默认 no-op。
            if let Err(e) = provider.start().await {
                tracing::warn!("[cmx][im] provider[{kind_label}].start 失败：{e}");
                return;
            }
            let bridge = cmx_agent_im::ImBridge::new(app, provider, kind_label, allow)
                .with_personal(personal)
                .with_full_access(full_access);
            bridge.run(stop_rx).await;
        });
        im_slots()
            .lock()
            .expect("im slots")
            .push((stop_tx, ch.provider));
    }
}

/// 三条 IM 契约命令统一入口（从命令总线体取 action / payload / reveal）。
async fn im_command(app: Arc<AgentApp>, body: String) -> String {
    let req: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    let cmd = req.get("cmd").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let action = req.get("action").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let payload = req.get("payload").and_then(|v| v.as_str()).map(str::to_owned);
    let reveal = req.get("reveal").and_then(|v| v.as_bool());
    let out: Result<String, String> = match cmd.as_str() {
        "im_config" => im_config_command(&app, &action, payload, reveal).await,
        "im_wechat_login" => im_wechat_login_command(&app, &action).await,
        "im_qq_login" => im_qq_login_command(&app, &action).await,
        _ => Ok(im_err("bad_request", &format!("未知命令：{cmd}"))),
    };
    out.unwrap_or_else(|e| im_err("command_failed", &e))
}

fn im_portal_base() -> String {
    cmx_agent_app::AuthConfig::default().base_url
}

async fn im_config_command(
    app: &Arc<AgentApp>,
    action: &str,
    payload: Option<String>,
    reveal: Option<bool>,
) -> Result<String, String> {
    let data_dir = cmx_agent_app::shared_data_dir();
    match action {
        "get" => {
            let mut data = cmx_agent_im::load_im_config(&data_dir)
                .map(|c| c.view(reveal.unwrap_or(false)))
                .unwrap_or_else(|| {
                    json!({
                        "configured": false, "enabled": true, "kind": "feishu",
                        "personal": true, "full_access": true,
                        "app_id": "", "app_secret_masked": "", "base": "",
                        "qq_app_id": "", "qq_secret_masked": "", "qq_base": "",
                        "wechat_bot_id": "", "wechat_token_masked": "", "wechat_base": "",
                    })
                });
            data["portal_base"] = json!(im_portal_base()); // 只读回显（运行期不可改）
            data["env_active"] = json!(cmx_agent_im::env_active());
            Ok(json!({ "ok": true, "data": data }).to_string())
        }
        "set" => {
            let v: serde_json::Value = payload
                .and_then(|p| serde_json::from_str(&p).ok())
                .ok_or_else(|| "invalid payload".to_string())?;
            let get_str =
                |k: &str| v.get(k).and_then(|x| x.as_str()).map(|s| s.trim().to_string());
            // 读旧配置（keep 语义沿用已存 secret；首次保存则从面板取 set 值）。
            let mut cfg = cmx_agent_im::load_im_config(&data_dir).unwrap_or_default();
            cfg.enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true);
            cfg.personal = true; // 身份模式固定个人：消息以接入身份跑回合
            cfg.full_access = v.get("full_access").and_then(|x| x.as_bool()).unwrap_or(true);
            // 多通道：面板多选 → active 列表；kind 同步为首个启用项（兼容旧读档语义）。
            if let Some(active) = v.get("active").and_then(|x| x.as_array()) {
                cfg.active = active
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect();
                cfg.kind = cfg.active.first().cloned().unwrap_or_default();
            } else if let Some(kind) = get_str("kind").filter(|s| !s.is_empty()) {
                cfg.kind = kind;
                cfg.active = vec![cfg.kind.clone()];
            }
            if let Some(app_id) = get_str("app_id") {
                cfg.feishu.app_id = app_id;
            }
            if let Some(base) = get_str("base") {
                cfg.feishu.base = base;
            }
            if get_str("app_secret_action").as_deref() == Some("set") {
                cfg.feishu.app_secret = get_str("app_secret_value").unwrap_or_default();
            }
            if let Some(qq_app_id) = get_str("qq_app_id") {
                cfg.qq.app_id = qq_app_id;
            }
            if get_str("qq_secret_action").as_deref() == Some("set") {
                cfg.qq.app_secret = get_str("qq_secret_value").unwrap_or_default();
            }
            // 白名单 / 微信凭证 / 各通道 base 不进 GUI：set 不触碰（im.json 手工维护，已有值保留）。
            // 启用态下逐通道校验凭证齐备（任一勾选通道缺凭证即拒）。
            if cfg.enabled {
                let missing = cfg.active().into_iter().find_map(|k| match k.as_str() {
                    "feishu" if cfg.feishu.app_id.is_empty() => Some("飞书 App ID"),
                    "feishu" if cfg.feishu.app_secret.is_empty() => Some("飞书 App Secret"),
                    "qq" if cfg.qq.app_id.is_empty() => Some("QQ AppID"),
                    "qq" if cfg.qq.app_secret.is_empty() => Some("QQ AppSecret"),
                    "wechat" if cfg.wechat.bot_token.is_empty() => {
                        Some("微信 bot_token（先扫码登录）")
                    }
                    _ => None,
                });
                if let Some(field) = missing {
                    return Ok(im_err("bad_request", &format!("{field} 不能为空")));
                }
            }
            cmx_agent_im::save_im_config(&data_dir, &cfg)?;
            // 热重载：保存即生效（env 激活时不重载——env 优先且不可热换）。
            let note = if cmx_agent_im::env_active() {
                "已保存。⚠ 检测到环境变量 CMX_AGENT_IM_*（开发模式）优先生效，im.json 暂不生效。".to_string()
            } else {
                start_im_bridges(app);
                "已保存，IM 遥控已按新配置重载（立即生效）".to_string()
            };
            Ok(json!({ "ok": true, "data": { "note": note } }).to_string())
        }
        other => Ok(im_err("bad_request", &format!("未知 action：{other}"))),
    }
}

async fn im_wechat_login_command(app: &Arc<AgentApp>, action: &str) -> Result<String, String> {
    let data_dir = cmx_agent_app::shared_data_dir();
    let app = Arc::clone(app);
    match action {
        "start" => {
            let base = cmx_agent_im::load_im_config(&data_dir)
                .map(|c| c.wechat.base)
                .filter(|b| !b.trim().is_empty());
            let provider = cmx_agent_im::WechatProvider::new("", base);
            let sess = provider.qr_begin().await.map_err(|e| {
                tracing::warn!("[cmx][im] 微信扫码 start 失败：{e}");
                im_err("login_failed", &e)
            })?;
            let qr = sess.qr_content().to_string();
            if let Ok(mut slot) = WECHAT_QR.lock() {
                *slot = Some(sess);
            }
            Ok(json!({ "ok": true, "data": { "status": "qr", "qr": qr } }).to_string())
        }
        "poll" => {
            // 锁不跨 await：先 take 出会话，按结果放回（继续轮询）或作废。
            let sess = WECHAT_QR.lock().ok().and_then(|mut s| s.take());
            let Some(mut sess) = sess else {
                return Ok(json!({ "ok": true, "data": { "status": "idle" } }).to_string());
            };
            match sess.poll_once().await {
                Ok(cmx_agent_im::QrEvent::Confirmed(login)) => {
                    // 凭证落盘 im.json + wechat 追加进 active（与 CLI im-login 同款）。
                    let mut cfg = cmx_agent_im::load_im_config(&data_dir).unwrap_or_default();
                    cfg.wechat = cmx_agent_im::WechatCreds {
                        bot_token: login.bot_token,
                        bot_id: login.bot_id.clone(),
                        user_id: login.user_id,
                        base: login.base,
                    };
                    if !cfg.active.iter().any(|k| k == "wechat") {
                        cfg.active.push("wechat".into());
                    }
                    if cfg.kind.trim().is_empty() {
                        cfg.kind = "wechat".into();
                    }
                    cmx_agent_im::save_im_config(&data_dir, &cfg)
                        .map_err(|e| im_err("login_failed", &e))?;
                    // 启用态且 env 未接管 → 立即热重载（扫码即上线，无需再点保存）。
                    let note = if !cfg.enabled {
                        "已保存；遥控总开关当前关闭，开启后生效".to_string()
                    } else if cmx_agent_im::env_active() {
                        "已保存。⚠ 检测到环境变量 CMX_AGENT_IM_* 优先生效，im.json 暂不生效。".to_string()
                    } else {
                        start_im_bridges(&app);
                        "已保存，微信通道已上线".to_string()
                    };
                    Ok(json!({
                        "ok": true,
                        "data": { "status": "confirmed", "bot_id": login.bot_id, "note": note }
                    })
                    .to_string())
                }
                Ok(ev) => {
                    // Refreshed 后会话里是新码：先取内容再放回槽。
                    let qr = if let cmx_agent_im::QrEvent::Refreshed = ev {
                        Some(sess.qr_content().to_string())
                    } else {
                        None
                    };
                    if let Ok(mut slot) = WECHAT_QR.lock() {
                        *slot = Some(sess);
                    }
                    let status = match ev {
                        cmx_agent_im::QrEvent::Scaned => "scaned",
                        cmx_agent_im::QrEvent::Refreshed => "refreshed",
                        _ => "waiting",
                    };
                    let mut data = json!({ "status": status });
                    if let Some(qr) = qr {
                        data["qr"] = json!(qr);
                    }
                    Ok(json!({ "ok": true, "data": data }).to_string())
                }
                // 终态错误（超时/多次过期/未知状态）：会话已作废，前端停止轮询。
                Err(e) => {
                    tracing::warn!("[cmx][im] 微信扫码轮询失败：{e}");
                    Ok(im_err("login_failed", &e))
                }
            }
        }
        other => Ok(im_err("bad_request", &format!("未知 action：{other}"))),
    }
}

async fn im_qq_login_command(app: &Arc<AgentApp>, action: &str) -> Result<String, String> {
    let data_dir = cmx_agent_app::shared_data_dir();
    let app = Arc::clone(app);
    match action {
        "start" => {
            let sess = cmx_agent_im::create_bind_task(None).await.map_err(|e| {
                tracing::warn!("[cmx][im] QQ 扫码 start 失败：{e}");
                im_err("login_failed", &e)
            })?;
            let qr = cmx_agent_im::connect_url(&sess.task_id, None);
            if let Ok(mut slot) = QQ_BIND.lock() {
                *slot = Some(sess);
            }
            Ok(json!({ "ok": true, "data": { "status": "qr", "qr": qr } }).to_string())
        }
        "poll" => {
            let sess = QQ_BIND.lock().ok().and_then(|mut s| s.take());
            let Some(sess) = sess else {
                return Ok(json!({ "ok": true, "data": { "status": "idle" } }).to_string());
            };
            match cmx_agent_im::poll_bind_result(&sess, None).await {
                Ok(cmx_agent_im::QqBindEvent::Pending) => {
                    if let Ok(mut slot) = QQ_BIND.lock() {
                        *slot = Some(sess);
                    }
                    Ok(json!({ "ok": true, "data": { "status": "waiting" } }).to_string())
                }
                Ok(cmx_agent_im::QqBindEvent::Expired) => {
                    // 任务过期：会话作废（终态），前端停轮询，可重按「扫码登录」。
                    Ok(im_err("login_failed", "二维码已过期，请重新扫码"))
                }
                Ok(cmx_agent_im::QqBindEvent::Completed { app_id, secret }) => {
                    // 凭证落盘 im.json + qq 追加进 active（官方 WS 接入协议不变）。
                    let mut cfg = cmx_agent_im::load_im_config(&data_dir).unwrap_or_default();
                    cfg.qq.app_id = app_id.clone();
                    cfg.qq.app_secret = secret;
                    if !cfg.active.iter().any(|k| k == "qq") {
                        cfg.active.push("qq".into());
                    }
                    if cfg.kind.trim().is_empty() {
                        cfg.kind = "qq".into();
                    }
                    cmx_agent_im::save_im_config(&data_dir, &cfg)
                        .map_err(|e| im_err("login_failed", &e))?;
                    let note = if !cfg.enabled {
                        "已保存；遥控总开关当前关闭，开启后生效".to_string()
                    } else if cmx_agent_im::env_active() {
                        "已保存。⚠ 检测到环境变量 CMX_AGENT_IM_* 优先生效，im.json 暂不生效。".to_string()
                    } else {
                        start_im_bridges(&app);
                        "已保存，QQ 通道已上线".to_string()
                    };
                    Ok(json!({
                        "ok": true,
                        "data": { "status": "confirmed", "app_id": app_id, "note": note }
                    })
                    .to_string())
                }
                Err(e) => {
                    // 轮询单次失败（网络抖动）：会话放回 + 返回 waiting 让前端继续轮。
                    if let Ok(mut slot) = QQ_BIND.lock() {
                        *slot = Some(sess);
                    }
                    tracing::warn!("[cmx][im] QQ 扫码轮询失败（下轮重试）：{e}");
                    Ok(json!({ "ok": true, "data": { "status": "waiting" } }).to_string())
                }
            }
        }
        other => Ok(im_err("bad_request", &format!("未知 action：{other}"))),
    }
}

/// IM 命令走 net_rt（扫码轮询 / 热重载起桥都是网络面）。
async fn dispatch_im_on_net(app: Arc<AgentApp>, body: String) -> String {
    tauri::async_runtime::spawn_blocking(move || net_rt().block_on(im_command(app, body)))
        .await
        .unwrap_or_else(|e| im_err("command_failed", &format!("task failed: {e}")))
}

fn command_name(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("cmd").and_then(|c| c.as_str()).map(str::to_owned))
        .unwrap_or_default()
}

fn is_im_cmd(cmd: &str) -> bool {
    matches!(cmd, "im_config" | "im_wechat_login" | "im_qq_login")
}

/// ui 目录解析:配置显式 > Tauri 资源目录 > build.rs 从 git/path 依赖定位 > 常见本地 checkout。
/// 组件化嵌入后,合格 ui 目录须含 `embed/truemate-app.js`——旧本地 checkout(无 embed/)
/// 会导致组件脚本 404「组件脚本加载失败」(cmxlinux 实测踩中),故逐候选校验,不合格跳过。
fn resolve_ui_dir(app: &AppHandle, cfg: &CmxCfg) -> Option<PathBuf> {
    let ok = |p: &Path| p.join("index.html").is_file() && p.join("embed/truemate-app.js").is_file();
    if !cfg.ui_dir.trim().is_empty() {
        let p = expand_home(app, cfg.ui_dir.trim());
        // 显式配置:只要 index 在就尊重(用户自担);缺 embed 打警告便于排障。
        if p.join("index.html").is_file() {
            if !p.join("embed/truemate-app.js").is_file() {
                tracing::warn!(ui = %p.display(), "uiDir 缺 embed/truemate-app.js,组件化嵌入将 404(请更新该 checkout)");
            }
            return Some(p);
        }
        return None;
    }
    if let Some(p) = option_env!("CMX_AGENT_UI_DIR") {
        let p = PathBuf::from(p);
        if ok(&p) {
            return Some(p);
        }
        if p.join("index.html").is_file() {
            tracing::warn!(ui = %p.display(), "build.rs 定位的 ui 缺 embed/,跳过并继续找(本地 checkout 过旧?)");
        }
    }
    // 打包 resources 版：发布脚本把选中的 checkout 同步到 src-tauri/cmx-agent-ui。
    // Linux deb/rpm 的资源目录是 /usr/lib/<productName>(不是 exe 同级),必须走 Tauri
    // 资源目录才能命中;macOS 同路径即 Contents/Resources,Windows 为 exe 同级。
    if let Ok(dir) = app.path().resource_dir() {
        let p = dir.join("cmx-agent-ui");
        if ok(&p) {
            return Some(p);
        }
    }
    // 兜底:exe 同级(Windows / 无 bundle 直接运行)。
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for p in [dir.join("cmx-agent-ui"), dir.join("../Resources/cmx-agent-ui")] {
                if ok(&p) {
                    return Some(p);
                }
            }
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for rel in [
        "../../../cmx-agent/crates/cmx-agent-web/ui",
        "../../../../../../cmx-workspace/backend/cmx-agent/crates/cmx-agent-web/ui",
    ] {
        let p = manifest.join(rel);
        if ok(&p) {
            return Some(p);
        }
    }
    // 兜底:cargo git checkouts 里找**含 embed 的**最新 rev(运行期等价 build.rs 逻辑;
    // build.rs 命中的本地 checkout 过旧时由此接住)。
    find_ui_in_cargo_checkouts()
}

/// 运行期扫 ~/.cargo/git/checkouts/cmx-agent-*/<rev>/crates/cmx-agent-web/ui,
/// 只认含 embed/truemate-app.js 的,取 mtime 最新。
fn find_ui_in_cargo_checkouts() -> Option<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))?;
    let rd = std::fs::read_dir(home.join("git").join("checkouts")).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for ent in rd.flatten() {
        if !ent.file_name().to_string_lossy().contains("cmx-agent") {
            continue;
        }
        let Ok(revs) = std::fs::read_dir(ent.path()) else { continue };
        for rev in revs.flatten() {
            let ui = rev.path().join("crates/cmx-agent-web/ui");
            if !(ui.join("index.html").is_file() && ui.join("embed/truemate-app.js").is_file()) {
                continue;
            }
            let mtime = rev
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                best = Some((mtime, ui));
            }
        }
    }
    best.map(|(_, p)| p)
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
    // 组件化嵌入:组件持 token 属性时所有 HTTP 请求带 Authorization: Bearer <token>
    // (bridge.js embHeaders;附件上传/SSE 兜底经此过闸,静态装配面不带凭证故已豁免 token)。
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t == st.token)
        .unwrap_or(false)
    {
        return true;
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
        .map(hostport_is_loopback)
        .unwrap_or(false)
}

/// 敏感面（/api*）守卫 = 回环 Host + Origin 白名单。
/// Origin 只放行回环与 Tauri webview 源（组件在 tauri://localhost / http://tauri.localhost
/// 文档里跨源请求本机 axum）；任意网页（https://evil.com）一律拒绝——防 DNS rebinding /
/// 跨站驱动本机内核（token 闸仍是主防线，这是纵深第二层）。
fn guard_ok(headers: &HeaderMap) -> bool {
    loopback_ok(headers) && origin_ok(headers)
}

fn origin_ok(headers: &HeaderMap) -> bool {
    headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(origin_allowed)
        .unwrap_or(true) // 无 Origin（curl / 同源导航）由 Host+token 守
}

fn origin_allowed(raw: &str) -> bool {
    let s = raw.trim();
    // Tauri v2 webview 自定义协议源：Windows/Linux 走 http(s)://tauri.localhost，
    // macOS 走 tauri://localhost。组件装配与附件请求的 Origin 即这些。
    if s.eq_ignore_ascii_case("tauri://localhost")
        || s.eq_ignore_ascii_case("http://tauri.localhost")
        || s.eq_ignore_ascii_case("https://tauri.localhost")
    {
        return true;
    }
    hostport_is_loopback(s)
}

/// 判定 `host[:port]` / `scheme://host[:port]` 是否精确回环（对齐 cmx-agent demo：
/// 仅 localhost / IPv4 127/8 / ::1；解析失败一律拒绝，禁前缀匹配防 `127.0.0.1.evil.com`）。
fn hostport_is_loopback(raw: &str) -> bool {
    let s = raw.trim();
    let s = match s.find("://") {
        Some(i) => &s[i + 3..],
        None => s,
    };
    let s = s.split(['/', '?', '#']).next().unwrap_or("");
    let host = if let Some(rest) = s.strip_prefix('[') {
        match rest.find(']') {
            Some(j) => &rest[..j],
            None => return false,
        }
    } else {
        match s.rfind(':') {
            Some(i) if !s[i + 1..].is_empty() && s[i + 1..].bytes().all(|b| b.is_ascii_digit()) => &s[..i],
            _ => s,
        }
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "ttf" => "font/ttf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
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

// 静态资产(index/css/js/vendor/embed):只做回环校验,**不进 token 闸**。
// 组件化嵌入的装配 fetch(拉 index.html 解析 css/js 清单)不带 ?t=、跨源无 cookie,
// Origin 形态又因 webview 实现而异(macOS WKWebView 非 tauri://localhost)——按凭证放行不可靠。
// 静态文件只是 cmx-agent 的 UI 代码,无敏感数据;敏感面是 /api*(命令总线/附件),token 闸照守。
// (组件在 Tauri 桥环境下 API 走 invoke,不打 HTTP /api;浏览器直开形态带 ?t= 过 /api 闸,不受影响。)
async fn h_index(State(st): State<HttpState>, headers: HeaderMap, _uri: axum::http::Uri) -> axum::response::Response {
    if !loopback_ok(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    serve_static(&st, "", true).await
}

async fn h_asset(State(st): State<HttpState>, headers: HeaderMap, uri: axum::http::Uri) -> axum::response::Response {
    if !loopback_ok(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    serve_static(&st, uri.path(), false).await
}

async fn h_api(State(st): State<HttpState>, headers: HeaderMap, body: String) -> axum::response::Response {
    if !guard_ok(&headers) || !token_ok(&st, &headers, None) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // IM 契约命令：HTTP 面与 invoke 面同语义（浏览器直开形态走这里）。
    let resp = if is_im_cmd(&command_name(&body)) {
        dispatch_im_on_net(st.app.clone(), body).await
    } else {
        dispatch_on_net(st.app.clone(), body).await
    };
    ([(header::CONTENT_TYPE, "application/json")], resp).into_response()
}

/// 流式回合(协议对齐 cmx-agent-web /api/stream:逐事件 SSE,尾帧 stream_done/stream_error)。
async fn h_stream(State(st): State<HttpState>, headers: HeaderMap, body: String) -> axum::response::Response {
    if !guard_ok(&headers) || !token_ok(&st, &headers, None) {
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
    if !guard_ok(&headers) || !token_ok(&st, &headers, None) {
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
    if !guard_ok(&headers) || !token_ok(&st, &headers, None) {
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
    if !guard_ok(&headers) || !token_ok(&st, &headers, None) {
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
    // 走到这里 = 在真正重建内核：先停旧 IM 桥（cmx_restart 已把旧实例 take 走，
    // 但桥持有旧 AgentApp 的 Arc，不停会与新实例并行消费消息）。
    stop_im_bridges();
    let cfg = load_cfg(app);
    if !cfg.enabled {
        return Err("工作台已在配置中停用(enabled=false)".into());
    }
    let Some(ui) = resolve_ui_dir(app, &cfg) else {
        return Err(
            "找不到 cmx-agent UI 资产(配置 uiDir，或确保已拉取 gitee.com/warpdrivelabs/cmx-agent 依赖)"
                .into(),
        );
    };
    // 数据目录:默认与独立版 TrueMate / Web 壳同根(~/.cmx-agent) —— 模型配置(providers.json)、
    // 会话历史、workspaces、IM 配置全部共享;显式填其它路径才隔离(多实例/测试)。
    // 不用 cmx_agent_app::shared_data_dir():它读 CMX_AGENT_DATA_DIR,而本函数下面会设该 env,
    // 工作台重启时会把上一次解析出的路径误当默认值。
    let data_dir = if cfg.data_dir.trim().is_empty() {
        app.path().home_dir().map_err(|e| e.to_string())?.join(".cmx-agent")
    } else {
        expand_home(app, cfg.data_dir.trim())
    };
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    tracing::info!(dir = %data_dir.display(), "cmx-agent 数据根");
    // 共享根里没有 providers.json = 指向了空目录（典型：隔离目录 / `~` 没展开成家目录），
    // 模型列表与会话都会是空的。显式告警，省得只看到"没数据"无从排查。
    if !data_dir.join("config/providers.json").is_file() {
        tracing::warn!(
            dir = %data_dir.display(),
            "cmx-agent 数据根下没有 config/providers.json：模型列表将为空；若期望复用 TrueMate 数据，把「数据目录」留空"
        );
    }
    // belt-and-suspenders:部分 cmx helper 读全局 data_dir(env)。SAFETY: 启动期单次设置。
    unsafe { std::env::set_var("CMX_AGENT_DATA_DIR", &data_dir) };
    // 旧扁平布局 → 新分层布局迁移（与双壳同款启动序）。默认根就是 ~/.cmx-agent,迁移语义与
    // 独立版一致(旧 ProjectDirs 数据搬进共享根);marker 存在或 data/ 已存在即跳过,幂等。
    let migrated = cmx_agent_app::migrate_if_needed(&data_dir);
    if migrated {
        tracing::info!(dir = %data_dir.display(), "cmx-agent 数据根已迁移到分层布局");
    }
    let workdir = if cfg.workdir.trim().is_empty() {
        // 同 expand_home：Windows GUI 进程没有 HOME，退回 tauri 的家目录解析（与 TrueMate 一致）。
        app.path().home_dir().map_err(|e| e.to_string())?
    } else {
        expand_home(app, cfg.workdir.trim())
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
                let mut b = DesktopAppBuilder::new(workdir, data_dir, model)
                    .interactive_approval()
                    // per-user 模型配置：与 TrueMate Web 壳 / 嵌入 demo 同款——
                    // <root>/data/users/<username>/model.json（首登从全局 config/providers.json 播种）。
                    // 免登录(login=off)无 current_user，内核自动回落全局 config 目录，行为不变。
                    .user_config_base(cmx_agent_app::data_dir().join("users"));
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
        .route("/embed/{*p}", get(h_asset)) // 嵌入组件入口 truemate-app.js(组件化嵌入)
        .route("/cmx.png", get(h_asset))
        .route("/api", post(h_api))
        .route("/api/stream", post(h_stream))
        .route("/api/subscribe", get(h_subscribe))
        .route("/api/attachment", get(h_attachment))
        .route("/api/att/upload", post(h_upload))
        .with_state(http)
        // 组件化嵌入:组件在宿主 webview(tauri://localhost)里按基址 fetch 本服务,属跨源——
        // 放开 CORS(回环守卫 + token/宿主源闸仍在,permissive 只解浏览器侧预检)。
        .layer(tower_http::cors::CorsLayer::permissive())
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
        Some(CmxRunning { app: agent.clone(), port, token: token.clone(), shim_user: inject_shim, shutdown: Some(sd_tx) });
    // 组件化嵌入:会话事件总线 → Tauri session_event(组件 main.js __TAURI__ 分支实时渲染)。
    spawn_session_event_forwarder(app.clone(), agent.clone());
    // IM 遥控：数据根配置了 im.json / CMX_AGENT_IM_* 即自动起通道桥（组件面板保存后热重载）。
    // 必须在 net_rt 上起：常驻长连接/长轮询不能跑 Tauri 运行时。
    net_rt().spawn(async move { start_im_bridges(&agent) });
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

// ── 组件化嵌入(truemate-app Web Component)的 Tauri invoke 桥 ────────────────
// 组件在宿主页面(nanomesh 自己的 webview)里运行,bridge.js 检测到 window.__TAURI__
// 即走 invoke 直连内核(对接指南 §二 Tauri 桥环境):invoke("agent") 命令总线 +
// invoke("send_stream") 流式回合(事件经 channel emit)。HTTP 面保留(SSE 订阅 /
// 附件上传下载仍走 axum,组件 server 属性指向它);iframe 整页承载则不再需要。

/// 取运行中的内核句柄(未启动则报错;前端先 cmx_start 再装配组件)。
async fn running_app(state: &TState<'_, AppState>) -> Result<(Arc<AgentApp>, bool), String> {
    let g = state.cmx.inner.lock().await;
    let r = g.as_ref().ok_or("工作台未启动")?;
    Ok((r.app.clone(), r.shim_user))
}

/// 命令总线(组件 bridge.js 的 invoke("agent") 分支)。
/// shim_user 时拦 current_user/login/logout 合成本地用户——语义对齐 HTTP 面的 BOOT_SHIM。
#[tauri::command]
pub(crate) async fn agent(state: TState<'_, AppState>, payload: String) -> Result<String, String> {
    let (app, shim_user) = running_app(&state).await?;
    let cmd = command_name(&payload);
    // auth_token/auth_user:组件认证契约命令(我们给组件传了 token 属性,它会先发 auth_token)。
    // 免登录态直接认通过并返回合成用户,随后 current_user 探测同样命中,组件进入已认证态。
    if shim_user
        && matches!(cmd.as_str(), "current_user" | "login" | "logout" | "auth_token" | "auth_user")
    {
        return Ok(json!({
            "ok": true,
            "data": { "user": {
                "user_id": "local", "username": "local", "nickname": "NanoMesh",
                "roles": [], "must_change_password": false
            } }
        })
        .to_string());
    }
    // IM 契约命令在宿主面实现（组件「设置 → IM 遥控」面板），其余命令透传真实内核。
    if is_im_cmd(&cmd) {
        return Ok(dispatch_im_on_net(app, payload).await);
    }
    Ok(dispatch_on_net(app, payload).await)
}

/// 流式回合(组件 bridge.js 的 invoke("send_stream") 分支):事件经 Tauri 事件 `channel`
/// 逐帧 emit,末尾 stream_done/stream_error。实现对齐 cmx-agent-shell 同名命令。
#[tauri::command]
pub(crate) async fn send_stream(
    app: AppHandle,
    state: TState<'_, AppState>,
    session_id: String,
    text: String,
    channel: String,
    images: Option<Vec<cmx_agent_app::IncomingImage>>,
    files: Option<Vec<cmx_agent_app::IncomingFile>>,
) -> Result<(), String> {
    let (agent, _) = running_app(&state).await?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    // 转发:rx → Tauri emit(Tauri 运行时上)。
    let apph = app.clone();
    let chan = channel.clone();
    let fwd = tauri::async_runtime::spawn(async move {
        use tauri::Emitter;
        while let Some(v) = rx.recv().await {
            let _ = apph.emit(&chan, v);
        }
    });
    // 跑回合:spawn_blocking + net_rt(模型网络调用必须离开 Tauri 运行时,见 net_rt 注释)。
    let run = tauri::async_runtime::spawn_blocking(move || {
        net_rt().block_on(async move {
            let sink = Arc::new(cmx_agent_app::ChannelSink::new(tx.clone()));
            let r = agent
                .send_streaming_with_attachments(
                    &session_id,
                    &text,
                    images.unwrap_or_default(),
                    files.unwrap_or_default(),
                    sink,
                )
                .await;
            match &r {
                Ok(_) => {
                    let _ = tx.send(json!({"kind":"stream_done"}));
                }
                Err(e) => {
                    let _ = tx.send(json!({"kind":"stream_error","message": e.to_string()}));
                }
            }
            r.map(|_| ()).map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("task failed: {e}"))?;
    let _ = fwd.await;
    run
}

/// 会话事件总线 → Tauri `session_event` 事件(组件 main.js 的 __TAURI__ 分支监听;
/// IM 桥等非本地回合的事件也实时到达)。工作台启动后由 start_inner 拉起,随内核 Arc 存活。
pub(crate) fn spawn_session_event_forwarder(app: AppHandle, agent: Arc<AgentApp>) {
    tauri::async_runtime::spawn(async move {
        use tauri::Emitter;
        use tokio::sync::broadcast::error::RecvError;
        let mut rx = agent.event_bus().subscribe();
        loop {
            match rx.recv().await {
                Ok(env) => {
                    let _ = app.emit("session_event", &env);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit("session_resync", json!({}));
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// 原生多选附件(组件 session.js 的 invoke("pick_attachment_files") 分支):
/// rfd 弹窗 → {path,name,bytes} 列表,前端只传路径引用(零拷贝,不经 base64)。
#[tauri::command]
pub(crate) async fn pick_attachment_files(
) -> Result<Option<Vec<cmx_agent_app::workspace::PickedFileMeta>>, String> {
    tauri::async_runtime::spawn_blocking(cmx_agent_app::workspace::native_pick_attachments)
        .await
        .map_err(|e| format!("附件选择任务失败：{e}"))?
}

/// 原生选工作区文件夹(组件 workspace.js 的 invoke 分支)。
#[tauri::command]
pub(crate) async fn pick_local_directory() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(cmx_agent_app::workspace::native_pick_local_directory)
        .await
        .map_err(|e| format!("文件夹选择任务失败：{e}"))?
}

/// 原生选插件包(组件 market.js 的 invoke 分支)。
#[tauri::command]
pub(crate) async fn pick_plugin_package() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(cmx_agent_app::workspace::native_pick_plugin_package)
        .await
        .map_err(|e| format!("插件包选择任务失败：{e}"))?
}

/// 原生选专家包(组件 market.js 的 invoke 分支)。
#[tauri::command]
pub(crate) async fn pick_expert_package() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(cmx_agent_app::workspace::native_pick_expert_package)
        .await
        .map_err(|e| format!("专家包选择任务失败：{e}"))?
}

/// 外链用系统浏览器打开(组件「获取 API Key ↗」等;webview 里 window.open 被拦)。只放行 http(s)。
#[tauri::command]
pub(crate) fn open_external(url: String) -> Result<(), String> {
    let lower = url.to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(format!("不支持打开的链接：{url}"));
    }
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open").arg(&url).status();
    #[cfg(target_os = "linux")]
    let status = std::process::Command::new("xdg-open").arg(&url).status();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let status: std::io::Result<std::process::ExitStatus> =
        Err(std::io::Error::other("unsupported platform"));
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("打开失败(status {s})")),
        Err(e) => Err(format!("打开失败：{e}")),
    }
}

//! 系统托盘：图标状态（正常/未读/离线/勿扰）、新消息闪烁、右键菜单、关闭到托盘、
//! Dock/任务栏未读角标、系统通知、全局快捷键、开机自启、单实例唤起。
//!
//! 前端经 `tray_sync` 推送界面状态（登录、在线状态、未读），托盘菜单的操作经
//! `tray://action` 事件回传前端执行。偏好存 app_data_dir/tray.json。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, UserAttentionType, WebviewWindow, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;

const TRAY_ID: &str = "main";
pub const ACTION_EVENT: &str = "tray://action";
pub const HIDDEN_ARG: &str = "--hidden";

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Prefs {
    pub close_to_tray: bool,
    pub close_hint_shown: bool,
    pub notify: bool,
    pub preview: bool,
    pub sound: bool,
    pub flash: bool,
    pub badge: bool,
    /// 免打扰截止（毫秒时间戳）；-1 = 一直；0 = 未开启。
    pub mute_until: i64,
    pub hotkey_on: bool,
    pub hotkey: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            close_hint_shown: false,
            notify: true,
            preview: true,
            sound: true,
            flash: true,
            badge: true,
            mute_until: 0,
            hotkey_on: true,
            hotkey: "CommandOrControl+Alt+Z".into(),
        }
    }
}

#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Conv {
    id: String,
    name: String,
    count: u32,
}

#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Model {
    logged_in: bool,
    user: String,
    status: String,
    connected: bool,
    unread: u32,
    convos: Vec<Conv>,
    /// 本地时区相对 UTC 的分钟数（东八区 = 480）。
    tz_offset_min: i64,
}

#[derive(Default)]
pub struct Tray {
    model: Mutex<Model>,
    prefs: Mutex<Prefs>,
    flash_gen: AtomicU64,
    flashing: AtomicBool,
    quitting: AtomicBool,
    hotkey: Mutex<Option<Shortcut>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Normal,
    Unread,
    Offline,
    Dnd,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn png(bytes: &'static [u8]) -> Image<'static> {
    Image::from_bytes(bytes).expect("内嵌托盘图标解码失败")
}

fn icon_for(look: Look) -> Image<'static> {
    #[cfg(target_os = "macos")]
    let bytes: &'static [u8] = match look {
        Look::Normal => include_bytes!("../icons/tray/mac-normal.png"),
        Look::Unread => include_bytes!("../icons/tray/mac-unread.png"),
        Look::Offline => include_bytes!("../icons/tray/mac-offline.png"),
        Look::Dnd => include_bytes!("../icons/tray/mac-dnd.png"),
    };
    #[cfg(not(target_os = "macos"))]
    let bytes: &'static [u8] = match look {
        Look::Normal => include_bytes!("../icons/tray/normal.png"),
        Look::Unread => include_bytes!("../icons/tray/unread.png"),
        Look::Offline => include_bytes!("../icons/tray/offline.png"),
        Look::Dnd => include_bytes!("../icons/tray/dnd.png"),
    };
    png(bytes)
}

#[cfg(target_os = "windows")]
fn count_badge(n: u32) -> Image<'static> {
    png(match n {
        1 => include_bytes!("../icons/tray/badge-1.png"),
        2 => include_bytes!("../icons/tray/badge-2.png"),
        3 => include_bytes!("../icons/tray/badge-3.png"),
        4 => include_bytes!("../icons/tray/badge-4.png"),
        5 => include_bytes!("../icons/tray/badge-5.png"),
        6 => include_bytes!("../icons/tray/badge-6.png"),
        7 => include_bytes!("../icons/tray/badge-7.png"),
        8 => include_bytes!("../icons/tray/badge-8.png"),
        9 => include_bytes!("../icons/tray/badge-9.png"),
        _ => include_bytes!("../icons/tray/badge-more.png"),
    })
}

fn status_label(s: &str) -> &'static str {
    match s {
        "away" => "离开",
        "busy" => "忙碌",
        "dnd" => "勿扰",
        _ => "在线",
    }
}

fn status_dot(s: &str) -> &'static str {
    match s {
        "away" => "🟡",
        "busy" => "🔴",
        "dnd" => "⛔",
        _ => "🟢",
    }
}

fn prefs_path<R: Runtime>(app: &AppHandle<R>) -> Option<std::path::PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("tray.json"))
}

fn load_prefs<R: Runtime>(app: &AppHandle<R>) -> Prefs {
    prefs_path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_prefs<R: Runtime>(app: &AppHandle<R>, p: &Prefs) {
    if let Some(path) = prefs_path(app) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(b) = serde_json::to_vec_pretty(p) {
            let _ = std::fs::write(path, b);
        }
    }
}

impl Tray {
    fn prefs(&self) -> Prefs {
        self.prefs.lock().unwrap().clone()
    }
    fn model(&self) -> Model {
        self.model.lock().unwrap().clone()
    }
    fn muted(&self) -> bool {
        let m = self.prefs.lock().unwrap().mute_until;
        m == -1 || m > now_ms()
    }
    /// 勿扰状态或免打扰时段：不弹通知、不闪烁、不响铃，未读照常累计。
    fn quiet(&self) -> bool {
        self.muted() || self.model.lock().unwrap().status == "dnd"
    }
    fn look(&self) -> Look {
        let m = self.model.lock().unwrap();
        if !m.logged_in || !m.connected {
            Look::Offline
        } else if m.unread > 0 {
            Look::Unread
        } else if m.status == "dnd" || self.muted() {
            Look::Dnd
        } else {
            Look::Normal
        }
    }
}

fn main_window<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    app.get_webview_window("main")
}

pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = main_window(app) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = app.show();
    }
}

fn toggle_main<R: Runtime>(app: &AppHandle<R>) {
    let Some(w) = main_window(app) else { return };
    let visible = w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false);
    if visible && w.is_focused().unwrap_or(false) {
        let _ = w.hide();
    } else {
        show_main(app);
    }
}

fn emit_action<R: Runtime>(app: &AppHandle<R>, action: &str, arg: Value) {
    let _ = app.emit(ACTION_EVENT, json!({ "action": action, "arg": arg }));
}

fn tray_handle<R: Runtime>(app: &AppHandle<R>) -> Option<TrayIcon<R>> {
    app.tray_by_id(TRAY_ID)
}

/// 按当前状态刷新图标、标题（macOS 菜单栏数字）、悬停提示与角标。
fn refresh_look(app: &AppHandle<Wry>) {
    let st = app.state::<Tray>();
    let model = st.model();
    let prefs = st.prefs();
    if let Some(tray) = tray_handle(app) {
        if !st.flashing.load(Ordering::SeqCst) {
            let _ = tray.set_icon(Some(icon_for(st.look())));
            #[cfg(target_os = "macos")]
            let _ = tray.set_icon_as_template(true);
        }
        #[cfg(target_os = "macos")]
        {
            let title = (model.unread > 0).then(|| if model.unread > 99 { "99+".to_string() } else { model.unread.to_string() });
            let _ = tray.set_title(title);
        }
        let mut tip = String::from("NANO MESH");
        if model.logged_in {
            tip.push_str(&format!("\n{} · {}", model.user, if model.connected { status_label(&model.status) } else { "未连接" }));
            if st.muted() {
                tip.push_str(" · 免打扰");
            }
            if model.unread > 0 {
                tip.push_str(&format!("\n{} 条未读消息", model.unread));
            }
        } else {
            tip.push_str("\n未登录");
        }
        let _ = tray.set_tooltip(Some(tip));
    }
    if let Some(w) = main_window(app) {
        let n = if prefs.badge && model.logged_in { model.unread } else { 0 };
        #[cfg(target_os = "macos")]
        let _ = w.set_badge_label((n > 0).then(|| if n > 99 { "99+".to_string() } else { n.to_string() }));
        #[cfg(target_os = "windows")]
        let _ = w.set_overlay_icon((n > 0).then(|| count_badge(n)));
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = w.set_badge_count((n > 0).then_some(n as i64));
    }
}

fn mute_label(until: i64) -> String {
    if until == -1 {
        return "已开启（直到手动关闭）".into();
    }
    let left = (until - now_ms()).max(0) / 60000;
    if left >= 60 {
        format!("还剩 {} 小时 {} 分", left / 60, left % 60)
    } else {
        format!("还剩 {} 分钟", left.max(1))
    }
}

fn build_menu(app: &AppHandle<Wry>) -> tauri::Result<Menu<Wry>> {
    let st = app.state::<Tray>();
    let model = st.model();
    let prefs = st.prefs();
    let sep = || PredefinedMenuItem::separator(app);
    let mut items: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();

    if model.logged_in {
        let head = if model.connected {
            format!("{} {} · {}", status_dot(&model.status), model.user, status_label(&model.status))
        } else {
            format!("⚪ {} · 未连接", model.user)
        };
        items.push(Box::new(MenuItem::with_id(app, "head", head, false, None::<&str>)?));
        items.push(Box::new(sep()?));
    }
    items.push(Box::new(MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?));

    if model.logged_in {
        let label = if model.unread > 0 { format!("未读消息（{}）", model.unread) } else { "未读消息".into() };
        let mut sub: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();
        if model.convos.is_empty() {
            sub.push(Box::new(MenuItem::with_id(app, "none", "没有未读消息", false, None::<&str>)?));
        } else {
            for c in &model.convos {
                let name: String = c.name.chars().take(18).collect();
                sub.push(Box::new(MenuItem::with_id(app, format!("conv:{}", c.id), format!("{name}（{}）", c.count), true, None::<&str>)?));
            }
            sub.push(Box::new(sep()?));
            sub.push(Box::new(MenuItem::with_id(app, "readall", "全部标为已读", true, None::<&str>)?));
        }
        let refs: Vec<&dyn IsMenuItem<Wry>> = sub.iter().map(|b| b.as_ref()).collect();
        items.push(Box::new(Submenu::with_id_and_items(app, "unread", label, true, &refs)?));
        items.push(Box::new(sep()?));

        let cur = if model.status.is_empty() { "online" } else { model.status.as_str() };
        let mut st_items: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();
        for s in ["online", "away", "busy", "dnd"] {
            st_items.push(Box::new(CheckMenuItem::with_id(
                app,
                format!("st:{s}"),
                format!("{} {}", status_dot(s), status_label(s)),
                true,
                cur == s,
                None::<&str>,
            )?));
        }
        let refs: Vec<&dyn IsMenuItem<Wry>> = st_items.iter().map(|b| b.as_ref()).collect();
        items.push(Box::new(Submenu::with_id_and_items(app, "status", format!("状态：{}", status_label(cur)), true, &refs)?));

        let muted = st.muted();
        let mut mute_items: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();
        if muted {
            mute_items.push(Box::new(MenuItem::with_id(app, "mute:info", mute_label(prefs.mute_until), false, None::<&str>)?));
            mute_items.push(Box::new(MenuItem::with_id(app, "mute:off", "恢复通知", true, None::<&str>)?));
            mute_items.push(Box::new(sep()?));
        }
        mute_items.push(Box::new(MenuItem::with_id(app, "mute:30m", "30 分钟", true, None::<&str>)?));
        mute_items.push(Box::new(MenuItem::with_id(app, "mute:1h", "1 小时", true, None::<&str>)?));
        mute_items.push(Box::new(MenuItem::with_id(app, "mute:8h", "8 小时", true, None::<&str>)?));
        mute_items.push(Box::new(MenuItem::with_id(app, "mute:tomorrow", "到明天早上 8 点", true, None::<&str>)?));
        mute_items.push(Box::new(MenuItem::with_id(app, "mute:forever", "直到我关闭", true, None::<&str>)?));
        let refs: Vec<&dyn IsMenuItem<Wry>> = mute_items.iter().map(|b| b.as_ref()).collect();
        items.push(Box::new(Submenu::with_id_and_items(
            app,
            "mute",
            if muted { "消息免打扰 ✓" } else { "消息免打扰" },
            true,
            &refs,
        )?));
    }
    items.push(Box::new(sep()?));

    let autostart = app.autolaunch().is_enabled().unwrap_or(false);
    items.push(Box::new(CheckMenuItem::with_id(app, "autostart", "开机自动启动", true, autostart, None::<&str>)?));
    items.push(Box::new(CheckMenuItem::with_id(app, "closetray", "关闭窗口时最小化到托盘", true, prefs.close_to_tray, None::<&str>)?));
    if model.logged_in {
        items.push(Box::new(MenuItem::with_id(app, "settings", "通知与托盘设置…", true, None::<&str>)?));
    }
    items.push(Box::new(MenuItem::with_id(app, "about", "关于 NANO MESH", true, None::<&str>)?));
    items.push(Box::new(sep()?));
    items.push(Box::new(MenuItem::with_id(app, "quit", "退出 NANO MESH", true, Some("CmdOrCtrl+Q"))?));

    let refs: Vec<&dyn IsMenuItem<Wry>> = items.iter().map(|b| b.as_ref()).collect();
    Menu::with_items(app, &refs)
}

fn refresh_menu(app: &AppHandle<Wry>) {
    if let (Some(tray), Ok(menu)) = (tray_handle(app), build_menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn refresh_all(app: &AppHandle<Wry>) {
    refresh_menu(app);
    refresh_look(app);
}

/// 下一个本地早上 8 点；时区偏移由前端随 `tray_sync` 提供。
fn next_morning_ms(tz_offset_min: i64) -> i64 {
    let offset = tz_offset_min * 60_000;
    let day = 86_400_000;
    let local = now_ms() + offset;
    let today8 = local - local.rem_euclid(day) + 8 * 3_600_000;
    let target = if local < today8 { today8 } else { today8 + day };
    target - offset
}

fn set_mute(app: &AppHandle<Wry>, until: i64) {
    let st = app.state::<Tray>();
    {
        let mut p = st.prefs.lock().unwrap();
        p.mute_until = until;
        save_prefs(app, &p);
    }
    if until != 0 {
        stop_flash(app);
    }
    refresh_all(app);
    emit_action(app, "prefs", json!(st.prefs()));
}

fn on_menu(app: &AppHandle<Wry>, id: &str) {
    match id {
        "show" => show_main(app),
        "readall" => emit_action(app, "readall", Value::Null),
        "settings" => {
            show_main(app);
            emit_action(app, "settings", Value::Null);
        }
        "about" => {
            show_main(app);
            emit_action(app, "about", Value::Null);
        }
        "quit" => request_quit(app),
        "autostart" => {
            let al = app.autolaunch();
            let on = al.is_enabled().unwrap_or(false);
            let _ = if on { al.disable() } else { al.enable() };
            refresh_menu(app);
        }
        "closetray" => {
            let st = app.state::<Tray>();
            let mut p = st.prefs.lock().unwrap();
            p.close_to_tray = !p.close_to_tray;
            save_prefs(app, &p);
            drop(p);
            refresh_menu(app);
        }
        "mute:off" => set_mute(app, 0),
        "mute:30m" => set_mute(app, now_ms() + 30 * 60_000),
        "mute:1h" => set_mute(app, now_ms() + 3_600_000),
        "mute:8h" => set_mute(app, now_ms() + 8 * 3_600_000),
        "mute:tomorrow" => {
            let tz = app.state::<Tray>().model().tz_offset_min;
            set_mute(app, next_morning_ms(tz))
        }
        "mute:forever" => set_mute(app, -1),
        _ => {
            if let Some(conv) = id.strip_prefix("conv:") {
                show_main(app);
                emit_action(app, "open", json!(conv));
            } else if let Some(s) = id.strip_prefix("st:") {
                emit_action(app, "status", json!(s));
                refresh_menu(app);
            }
        }
    }
}

/// 退出前让前端落盘聊天记录；前端不响应时 1.5 秒后强制退出。
fn request_quit(app: &AppHandle<Wry>) {
    let st = app.state::<Tray>();
    if st.quitting.swap(true, Ordering::SeqCst) {
        app.exit(0);
        return;
    }
    emit_action(app, "quit", Value::Null);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        app.exit(0);
    });
}

fn start_flash(app: &AppHandle<Wry>) {
    let st = app.state::<Tray>();
    if cfg!(target_os = "macos") || st.flashing.swap(true, Ordering::SeqCst) {
        return;
    }
    let round = st.flash_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let blank = png(include_bytes!("../icons/tray/blank.png"));
        let mut on = false;
        loop {
            let st = app.state::<Tray>();
            if st.flash_gen.load(Ordering::SeqCst) != round || !st.flashing.load(Ordering::SeqCst) {
                break;
            }
            on = !on;
            if let Some(tray) = tray_handle(&app) {
                let _ = tray.set_icon(Some(if on { blank.clone() } else { icon_for(st.look()) }));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}

fn stop_flash(app: &AppHandle<Wry>) {
    let st = app.state::<Tray>();
    if st.flashing.swap(false, Ordering::SeqCst) {
        st.flash_gen.fetch_add(1, Ordering::SeqCst);
        refresh_look(app);
    }
}

fn window_active(app: &AppHandle<Wry>) -> bool {
    main_window(app).is_some_and(|w| {
        w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false) && w.is_focused().unwrap_or(false)
    })
}

fn sound_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Glass"
    } else if cfg!(windows) {
        "IM"
    } else {
        "message-new-instant"
    }
}

fn notify(app: &AppHandle<Wry>, title: &str, body: &str, sound: bool) {
    let mut b = app.notification().builder().title(title).body(body);
    if sound {
        b = b.sound(sound_name());
    }
    let _ = b.show();
}

fn register_hotkey(app: &AppHandle<Wry>, accel: &str) -> Result<(), String> {
    let st = app.state::<Tray>();
    let gs = app.global_shortcut();
    if let Some(old) = st.hotkey.lock().unwrap().take() {
        let _ = gs.unregister(old);
    }
    if accel.trim().is_empty() {
        return Ok(());
    }
    let sc: Shortcut = accel.parse().map_err(|e| format!("快捷键格式不正确：{e}"))?;
    gs.register(sc).map_err(|e| format!("快捷键注册失败（可能已被其它程序占用）：{e}"))?;
    *st.hotkey.lock().unwrap() = Some(sc);
    Ok(())
}

pub fn shortcut_plugin() -> tauri::plugin::TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, sc, ev| {
            let Some(st) = app.try_state::<Tray>() else { return };
            let mine = st.hotkey.lock().unwrap().as_ref() == Some(sc);
            if mine && ev.state() == ShortcutState::Pressed {
                toggle_main(app);
            }
        })
        .build()
}

/// 在 `setup` 中调用：建托盘、载入偏好、注册快捷键、处理开机自启隐藏启动。
pub fn setup(app: &AppHandle<Wry>) -> tauri::Result<()> {
    let prefs = load_prefs(app);
    let hotkey = prefs.hotkey_on.then(|| prefs.hotkey.clone());
    app.manage(Tray { prefs: Mutex::new(prefs), ..Default::default() });

    let menu = build_menu(app)?;
    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon_for(Look::Offline))
        .tooltip("NANO MESH")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| on_menu(app, ev.id().as_ref()))
        .on_tray_icon_event(|tray, ev| {
            let app = tray.app_handle();
            match ev {
                TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
                    let st = app.state::<Tray>();
                    if st.flashing.load(Ordering::SeqCst) || (st.model().unread > 0 && !window_active(app)) {
                        show_main(app);
                        emit_action(app, "open_latest", Value::Null);
                    } else {
                        toggle_main(app);
                    }
                }
                TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => show_main(app),
                _ => {}
            }
        });
    #[cfg(target_os = "macos")]
    let builder = builder.icon_as_template(true);
    builder.build(app)?;
    refresh_look(app);

    let ticker = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let st = ticker.state::<Tray>();
            let until = st.prefs.lock().unwrap().mute_until;
            if until > 0 && until <= now_ms() {
                set_mute(&ticker, 0);
            } else if until != 0 {
                refresh_menu(&ticker);
            }
        }
    });

    if let Some(k) = hotkey {
        if let Err(e) = register_hotkey(app, &k) {
            eprintln!("[tray] {e}");
        }
    }
    if std::env::args().any(|a| a == HIDDEN_ARG) {
        if let Some(w) = main_window(app) {
            let _ = w.hide();
        }
    }
    Ok(())
}

/// 窗口事件：关闭到托盘、获得焦点时停止闪烁。
pub fn on_window_event(window: &tauri::Window<Wry>, ev: &tauri::WindowEvent) {
    if window.label() != "main" {
        return;
    }
    let app = window.app_handle();
    let Some(st) = app.try_state::<Tray>() else { return };
    match ev {
        tauri::WindowEvent::CloseRequested { api, .. } => {
            if st.quitting.load(Ordering::SeqCst) {
                return;
            }
            let prefs = st.prefs();
            if !prefs.close_to_tray {
                api.prevent_close();
                request_quit(app);
                return;
            }
            api.prevent_close();
            let _ = window.hide();
            if !prefs.close_hint_shown {
                let mut p = st.prefs.lock().unwrap();
                p.close_hint_shown = true;
                save_prefs(app, &p);
                drop(p);
                let place = if cfg!(target_os = "macos") { "菜单栏" } else { "系统托盘" };
                notify(app, "NANO MESH 仍在后台运行", &format!("新消息会照常提醒。可在{place}图标的菜单中退出，或在设置中改为关闭即退出。"), false);
            }
        }
        tauri::WindowEvent::Focused(true) => stop_flash(app),
        _ => {}
    }
}

/// 应用级事件：macOS 点 Dock 图标重新打开窗口；⌘Q 等系统退出走同一落盘流程。
pub fn on_run_event(app: &AppHandle<Wry>, ev: &tauri::RunEvent) {
    match ev {
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => show_main(app),
        tauri::RunEvent::ExitRequested { api, code, .. } => {
            let Some(st) = app.try_state::<Tray>() else { return };
            if code.is_none() && !st.quitting.load(Ordering::SeqCst) {
                api.prevent_exit();
                request_quit(app);
            }
        }
        _ => {}
    }
}

// ---------------- 前端命令 ----------------

/// 前端推送界面状态；未读增加且窗口不在前台时闪烁 + 提醒用户注意。
#[tauri::command]
pub fn tray_sync(app: AppHandle, state: State<'_, Tray>, model: Model) {
    let (changed, grew) = {
        let mut cur = state.model.lock().unwrap();
        let grew = model.unread > cur.unread;
        let changed = *cur != model;
        *cur = model;
        (changed, grew)
    };
    if !changed {
        return;
    }
    let unread = state.model().unread;
    if unread == 0 {
        stop_flash(&app);
    } else if grew && !window_active(&app) && !state.quiet() {
        if state.prefs().flash {
            start_flash(&app);
        }
        if let Some(w) = main_window(&app) {
            let _ = w.request_user_attention(Some(UserAttentionType::Informational));
        }
    }
    refresh_all(&app);
}

/// 新消息系统通知（前端已做同会话合并）。返回是否真的弹出。
#[tauri::command]
pub fn tray_notify(app: AppHandle, state: State<'_, Tray>, title: String, body: String, force: Option<bool>) -> bool {
    let prefs = state.prefs();
    if !prefs.notify || state.quiet() || (window_active(&app) && !force.unwrap_or(false)) {
        return false;
    }
    if prefs.preview {
        notify(&app, &title, &body, prefs.sound);
    } else {
        notify(&app, "NANO MESH", "你收到一条新消息", prefs.sound);
    }
    true
}

#[tauri::command]
pub fn tray_prefs_get(app: AppHandle, state: State<'_, Tray>) -> Value {
    let mut v = json!(state.prefs());
    v["autostart"] = json!(app.autolaunch().is_enabled().unwrap_or(false));
    v["muted"] = json!(state.muted());
    v["os"] = json!(std::env::consts::OS);
    v
}

/// 局部更新偏好：`patch` 里出现的字段才改；`autostart`/`hotkey` 会立即生效。
#[tauri::command]
pub fn tray_prefs_set(app: AppHandle, state: State<'_, Tray>, patch: Value) -> Result<Value, String> {
    if let Some(on) = patch.get("autostart").and_then(Value::as_bool) {
        let al = app.autolaunch();
        if on { al.enable() } else { al.disable() }.map_err(|e| format!("开机自启设置失败：{e}"))?;
    }
    let old = state.prefs();
    let mut merged = json!(old);
    if let (Some(dst), Some(src)) = (merged.as_object_mut(), patch.as_object()) {
        for (k, v) in src {
            if k != "autostart" && dst.contains_key(k) {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
    let new: Prefs = serde_json::from_value(merged).map_err(|e| e.to_string())?;
    if new.hotkey_on != old.hotkey_on || new.hotkey != old.hotkey {
        let accel = if new.hotkey_on { new.hotkey.clone() } else { String::new() };
        if let Err(e) = register_hotkey(&app, &accel) {
            if old.hotkey_on {
                let _ = register_hotkey(&app, &old.hotkey);
            }
            return Err(e);
        }
    }
    *state.prefs.lock().unwrap() = new.clone();
    save_prefs(&app, &new);
    if new.mute_until != 0 || state.model().status == "dnd" {
        stop_flash(&app);
    }
    refresh_all(&app);
    Ok(tray_prefs_get(app, state))
}

/// 前端落盘完成后真正退出。
#[tauri::command]
pub fn app_quit(app: AppHandle, state: State<'_, Tray>) {
    state.quitting.store(true, Ordering::SeqCst);
    app.exit(0);
}

/// 测试通知（设置页“发送测试通知”）。
#[tauri::command]
pub fn tray_test_notify(app: AppHandle, state: State<'_, Tray>) {
    let p = state.prefs();
    notify(&app, "NANO MESH", "这是一条测试通知，通知功能工作正常。", p.sound);
}

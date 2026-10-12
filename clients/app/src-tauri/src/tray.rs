//! 系统托盘：图标状态（正常/未读/离线/勿扰）、新消息闪烁、右键菜单、关闭到托盘、
//! Dock/任务栏未读角标、系统通知、全局快捷键、开机自启、单实例唤起。
//!
//! 前端经 `tray_sync` 推送界面状态（登录、在线状态、未读），托盘菜单的操作经
//! `tray://action` 事件回传前端执行。偏好存 app_data_dir/tray.json。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, UserAttentionType, WebviewWindow, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;

pub mod commands;
mod lifecycle;
mod menu;
mod notify;
mod state;
mod visual;

pub use lifecycle::{on_run_event, on_window_event, setup};
pub use notify::shortcut_plugin;
pub use state::{Model, Prefs, Tray};

use menu::{on_menu, refresh_all, refresh_menu, request_quit, set_mute};
use notify::{notify, register_hotkey, start_flash, stop_flash, window_active};
use state::{Look, load_prefs, now_ms, save_prefs, status_dot, status_label};
use visual::{icon_for, refresh_look};

const TRAY_ID: &str = "main";
pub const ACTION_EVENT: &str = "tray://action";
pub const HIDDEN_ARG: &str = "--hidden";

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

use tauri::Manager;

use crate::{
    AppState, auth_commands, connection, devices, directory, identity, local_store, media, pairing,
    registry, social,
};

#[cfg(desktop)]
use crate::tray;

/// 供前端 `platform.js` 探测系统。窗口按钮由标题栏自绘。
#[tauri::command]
fn platform() -> &'static str {
    std::env::consts::OS // "macos" | "windows" | "linux"
}

pub(crate) fn run() {
    let builder = tauri::Builder::default();
    // 单实例必须最先注册：再次启动时唤起已运行的窗口（Windows 点通知也会走这里）。
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main(app)
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![tray::HIDDEN_ARG]),
        ))
        .plugin(tray::shortcut_plugin())
        .on_window_event(tray::on_window_event);
    builder
        .plugin(tauri_plugin_notification::init())
        .manage(AppState::default())
        .setup(|_app| {
            // 系统交通灯改由标题栏右侧的红黄绿按钮承担，各平台都关掉原生装饰。
            if let Some(w) = _app.get_webview_window("main") {
                let _ = w.set_decorations(false);
                let _ = w.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));
            }
            #[cfg(desktop)]
            tray::setup(_app.handle())?;
            // 换网重连驱动（P1 入站主动重连）：建换网信号通道，存入 AppState 供哨兵发信号，
            // 启动长驻驱动任务收信号后主动重连。见 docs/NETWORK_ROAMING_RECOVERY_DESIGN.md §5.5。
            {
                let state = _app.state::<AppState>();
                let (roam_tx, roam_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
                let _ = state.roam_tx.set(roam_tx);
                tauri::async_runtime::spawn(connection::roam_reconnect_driver(
                    _app.handle().clone(),
                    roam_rx,
                ));
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connection::connect,
            connection::im_ready,
            connection::send_to,
            directory::directory_query,
            directory::update_profile,
            media::blob_put,
            media::blob_get,
            media::blob_cached,
            media::blob_put_ref,
            media::media_file,
            media::save_media,
            media::open_path,
            media::send_rich,
            media::capture_screen,
            media::list_windows,
            media::capture_window,
            social::presence_set,
            social::presence_query,
            social::roster_list,
            social::roster_add,
            social::roster_remove,
            social::roster_update,
            social::group_create,
            social::group_list,
            social::group_add,
            social::group_kick,
            social::group_promote,
            social::group_demote,
            social::group_rename,
            social::group_set_meta,
            social::group_dissolve,
            social::group_leave,
            social::send_group,
            social::channel_create,
            social::channel_list,
            social::channel_sub,
            social::channel_unsub,
            social::channel_publish,
            social::channel_set_meta,
            social::channel_backfill,
            registry::naming::name_claim,
            registry::naming::name_resolve,
            registry::vault::ensure_device,
            registry::account::account_domains,
            registry::account::account_register,
            registry::account::account_login,
            registry::account::account_passwd,
            registry::account::account_reset,
            pairing::pair_request,
            pairing::pair_ticket,
            pairing::pair_cancel,
            pairing::pair_accept_ticket,
            pairing::pair_approve,
            pairing::pair_reject,
            devices::device_self,
            devices::device_list,
            devices::device_rename,
            devices::device_revoke,
            devices::device_freeze,
            registry::account::account_suggest,
            registry::naming::name_reverse,
            directory::node_users,
            social::my_id,
            registry::naming::disconnect,
            platform,
            registry::ticker::ticker_feed,
            registry::ticker::open_https,
            identity::list_identities,
            identity::create_identity,
            auth_commands::auth_status,
            auth_commands::setup_master,
            auth_commands::unlock,
            auth_commands::lock,
            auth_commands::change_master,
            auth_commands::export_backup,
            auth_commands::import_backup,
            auth_commands::generate_recovery,
            auth_commands::recover,
            auth_commands::read_audit,
            local_store::ui_kv_get_all,
            local_store::ui_kv_set,
            local_store::catalog_load,
            local_store::catalog_save,
            local_store::server_load,
            local_store::server_save,
            local_store::profile_load,
            local_store::profile_save,
            local_store::chat_open,
            local_store::chat_tail,
            local_store::chat_mark,
            local_store::chat_before,
            local_store::chat_around,
            local_store::chat_append,
            local_store::chat_append_many,
            local_store::chat_unread_save,
            local_store::chat_search,
            #[cfg(desktop)]
            tray::commands::tray_sync,
            #[cfg(desktop)]
            tray::commands::tray_notify,
            #[cfg(desktop)]
            tray::commands::tray_prefs_get,
            #[cfg(desktop)]
            tray::commands::tray_prefs_set,
            #[cfg(desktop)]
            tray::commands::tray_test_notify,
            #[cfg(desktop)]
            tray::commands::app_quit
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _ev| {
            #[cfg(desktop)]
            tray::on_run_event(_app, &_ev);
        });
}

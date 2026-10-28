use tauri::Manager;

use crate::{
    AppState, auth_commands, cmx_host, connection, devices, directory, identity, llm_chat,
    llm_proxy, llm_share, local_store, media, pairing, registry, social,
};

#[cfg(desktop)]
use crate::tray;

/// 供前端 `platform.js` 探测系统。窗口按钮由标题栏自绘。
#[tauri::command]
fn platform() -> &'static str {
    std::env::consts::OS // "macos" | "windows" | "linux"
}

/// 非阻塞日志 worker 守卫：须活到进程结束，否则缓冲日志不落盘。进全局，随进程存活。
static TRACING_GUARD: std::sync::OnceLock<tracing_appender::non_blocking::WorkerGuard> =
    std::sync::OnceLock::new();

/// 初始化应用日志：滚动到 `app_data_dir/logs/app.log.<date>` + 控制台（终端启动可见）。
/// 默认 `info`，并把换网/重连相关 target 调到 debug；`RUST_LOG` 环境变量可整体覆盖。
/// 这样换网后"卡在连 relay / pkarr 解析 / online"哪一层，日志里一目了然（此前 app 根本没装
/// subscriber，nm-transport/nm-client 的 tracing 日志全被丢弃，无从排障）。
fn init_tracing(app: &tauri::AppHandle) {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let Ok(dir) = crate::data_dir(app) else { return };
    let logs = dir.join("logs");
    if std::fs::create_dir_all(&logs).is_err() {
        return;
    }
    let file = tracing_appender::rolling::daily(&logs, "app.log");
    let (nb, guard) = tracing_appender::non_blocking(file);
    let _ = TRACING_GUARD.set(guard);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,nmspace_app_lib=debug,nm_client=debug,nm_transport=debug,iroh=warn")
    });
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(nb).with_ansi(false))
        .with(fmt::layer().with_writer(std::io::stderr))
        .try_init();
    tracing::info!("nmspace app 日志已启动 → {}", logs.display());
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
            init_tracing(_app.handle());
            // 系统交通灯改由标题栏右侧的红黄绿按钮承担，各平台都关掉原生装饰。
            if let Some(w) = _app.get_webview_window("main") {
                let _ = w.set_decorations(false);
                let _ = w.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));
            }
            #[cfg(desktop)]
            tray::setup(_app.handle())?;
            // 云模型纳管:app 启动即收养上个生命周期遗留的 gatewayd 孤儿(pidfile+命令行指纹),
            // UI 第一屏即见真实在线态;不需 VK。见 llm_share::adopt_on_boot。
            {
                let apph = _app.handle().clone();
                tauri::async_runtime::spawn(llm_share::adopt_on_boot(apph));
            }
            // 智能体工作台:配置 autostart=true 时 app 启动即拉起(否则点标签懒启动)。
            {
                let apph = _app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let cfg = cmx_host::load_cfg(&apph);
                    if cfg.enabled && cfg.autostart {
                        let st = apph.state::<AppState>();
                        if let Err(e) = cmx_host::cmx_start(apph.clone(), st).await {
                            tracing::warn!("cmx workbench autostart failed: {e}");
                        }
                    }
                });
            }
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
            social::read_mark,
            social::read_list,
            social::invite_create,
            social::invite_parse,
            social::group_join,
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
            llm_share::llm_model_list,
            llm_share::llm_model_upsert,
            llm_share::llm_model_set_key,
            llm_share::llm_model_delete,
            llm_share::llm_model_online,
            llm_share::llm_model_offline,
            llm_share::llm_model_runtime,
            llm_share::llm_model_issue_grant,
            llm_proxy::llm_proxy_start,
            llm_proxy::llm_proxy_stop,
            llm_proxy::llm_proxy_list,
            llm_proxy::llm_proxy_status,
            llm_proxy::llm_proxy_detail,
            llm_proxy::llm_proxy_update,
            llm_proxy::llm_proxy_clear_stats,
            llm_proxy::llm_proxy_probe,
            llm_proxy::llm_grant_save,
            llm_chat::llm_chat_stream,
            llm_chat::llm_chat_cancel,
            cmx_host::cmx_start,
            cmx_host::cmx_status,
            cmx_host::cmx_restart,
            cmx_host::cmx_cfg_get,
            cmx_host::cmx_cfg_set,
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
            local_store::actbar_load,
            local_store::actbar_register,
            local_store::actbar_remove,
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

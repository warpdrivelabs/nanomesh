use super::*;

/// 在 `setup` 中调用：建托盘、载入偏好、注册快捷键、处理开机自启隐藏启动。
pub fn setup(app: &AppHandle<Wry>) -> tauri::Result<()> {
    let prefs = load_prefs(app);
    let hotkey = prefs.hotkey_on.then(|| prefs.hotkey.clone());
    app.manage(Tray {
        prefs: Mutex::new(prefs),
        ..Default::default()
    });

    let menu = menu::build_menu(app)?;
    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon_for(Look::Offline))
        .tooltip("NANO MESH")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| on_menu(app, ev.id().as_ref()))
        .on_tray_icon_event(|tray, ev| {
            let app = tray.app_handle();
            match ev {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => {
                    let st = app.state::<Tray>();
                    if st.flashing.load(Ordering::SeqCst)
                        || (st.model().unread > 0 && !window_active(app))
                    {
                        show_main(app);
                        emit_action(app, "open_latest", Value::Null);
                    } else {
                        toggle_main(app);
                    }
                }
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => show_main(app),
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
    let Some(st) = app.try_state::<Tray>() else {
        return;
    };
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
                let place = if cfg!(target_os = "macos") {
                    "菜单栏"
                } else {
                    "系统托盘"
                };
                notify(
                    app,
                    "NANO MESH 仍在后台运行",
                    &format!(
                        "新消息会照常提醒。可在{place}图标的菜单中退出，或在设置中改为关闭即退出。"
                    ),
                    false,
                );
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
            let Some(st) = app.try_state::<Tray>() else {
                return;
            };
            if code.is_none() && !st.quitting.load(Ordering::SeqCst) {
                api.prevent_exit();
                request_quit(app);
            }
        }
        _ => {}
    }
}

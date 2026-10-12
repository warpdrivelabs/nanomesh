use super::*;

pub(super) fn start_flash(app: &AppHandle<Wry>) {
    let st = app.state::<Tray>();
    if cfg!(target_os = "macos") || st.flashing.swap(true, Ordering::SeqCst) {
        return;
    }
    let round = st.flash_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let blank = visual::png(include_bytes!("../../icons/tray/blank.png"));
        let mut on = false;
        loop {
            let st = app.state::<Tray>();
            if st.flash_gen.load(Ordering::SeqCst) != round || !st.flashing.load(Ordering::SeqCst) {
                break;
            }
            on = !on;
            if let Some(tray) = tray_handle(&app) {
                let _ = tray.set_icon(Some(if on {
                    blank.clone()
                } else {
                    icon_for(st.look())
                }));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}

pub(super) fn stop_flash(app: &AppHandle<Wry>) {
    let st = app.state::<Tray>();
    if st.flashing.swap(false, Ordering::SeqCst) {
        st.flash_gen.fetch_add(1, Ordering::SeqCst);
        refresh_look(app);
    }
}

pub(super) fn window_active(app: &AppHandle<Wry>) -> bool {
    main_window(app).is_some_and(|w| {
        w.is_visible().unwrap_or(false)
            && !w.is_minimized().unwrap_or(false)
            && w.is_focused().unwrap_or(false)
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

pub(super) fn notify(app: &AppHandle<Wry>, title: &str, body: &str, sound: bool) {
    let mut b = app.notification().builder().title(title).body(body);
    if sound {
        b = b.sound(sound_name());
    }
    let _ = b.show();
}

pub(super) fn register_hotkey(app: &AppHandle<Wry>, accel: &str) -> Result<(), String> {
    let st = app.state::<Tray>();
    let gs = app.global_shortcut();
    if let Some(old) = st.hotkey.lock().unwrap().take() {
        let _ = gs.unregister(old);
    }
    if accel.trim().is_empty() {
        return Ok(());
    }
    let sc: Shortcut = accel
        .parse()
        .map_err(|e| format!("快捷键格式不正确：{e}"))?;
    gs.register(sc)
        .map_err(|e| format!("快捷键注册失败（可能已被其它程序占用）：{e}"))?;
    *st.hotkey.lock().unwrap() = Some(sc);
    Ok(())
}

pub fn shortcut_plugin() -> tauri::plugin::TauriPlugin<Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, sc, ev| {
            let Some(st) = app.try_state::<Tray>() else {
                return;
            };
            let mine = st.hotkey.lock().unwrap().as_ref() == Some(sc);
            if mine && ev.state() == ShortcutState::Pressed {
                toggle_main(app);
            }
        })
        .build()
}

use super::*;

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
pub fn tray_notify(
    app: AppHandle,
    state: State<'_, Tray>,
    title: String,
    body: String,
    force: Option<bool>,
) -> bool {
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
pub fn tray_prefs_set(
    app: AppHandle,
    state: State<'_, Tray>,
    patch: Value,
) -> Result<Value, String> {
    if let Some(on) = patch.get("autostart").and_then(Value::as_bool) {
        let al = app.autolaunch();
        if on { al.enable() } else { al.disable() }
            .map_err(|e| format!("开机自启设置失败：{e}"))?;
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
        let accel = if new.hotkey_on {
            new.hotkey.clone()
        } else {
            String::new()
        };
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
    notify(
        &app,
        "NANO MESH",
        "这是一条测试通知，通知功能工作正常。",
        p.sound,
    );
}

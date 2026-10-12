use super::*;

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

pub(super) fn build_menu(app: &AppHandle<Wry>) -> tauri::Result<Menu<Wry>> {
    let st = app.state::<Tray>();
    let model = st.model();
    let prefs = st.prefs();
    let sep = || PredefinedMenuItem::separator(app);
    let mut items: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();

    if model.logged_in {
        let head = if model.connected {
            format!(
                "{} {} · {}",
                status_dot(&model.status),
                model.user,
                status_label(&model.status)
            )
        } else {
            format!("⚪ {} · 未连接", model.user)
        };
        items.push(Box::new(MenuItem::with_id(
            app,
            "head",
            head,
            false,
            None::<&str>,
        )?));
        items.push(Box::new(sep()?));
    }
    items.push(Box::new(MenuItem::with_id(
        app,
        "show",
        "显示主窗口",
        true,
        None::<&str>,
    )?));

    if model.logged_in {
        let label = if model.unread > 0 {
            format!("未读消息（{}）", model.unread)
        } else {
            "未读消息".into()
        };
        let mut sub: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();
        if model.convos.is_empty() {
            sub.push(Box::new(MenuItem::with_id(
                app,
                "none",
                "没有未读消息",
                false,
                None::<&str>,
            )?));
        } else {
            for c in &model.convos {
                let name: String = c.name.chars().take(18).collect();
                sub.push(Box::new(MenuItem::with_id(
                    app,
                    format!("conv:{}", c.id),
                    format!("{name}（{}）", c.count),
                    true,
                    None::<&str>,
                )?));
            }
            sub.push(Box::new(sep()?));
            sub.push(Box::new(MenuItem::with_id(
                app,
                "readall",
                "全部标为已读",
                true,
                None::<&str>,
            )?));
        }
        let refs: Vec<&dyn IsMenuItem<Wry>> = sub.iter().map(|b| b.as_ref()).collect();
        items.push(Box::new(Submenu::with_id_and_items(
            app, "unread", label, true, &refs,
        )?));
        items.push(Box::new(sep()?));

        let cur = if model.status.is_empty() {
            "online"
        } else {
            model.status.as_str()
        };
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
        items.push(Box::new(Submenu::with_id_and_items(
            app,
            "status",
            format!("状态：{}", status_label(cur)),
            true,
            &refs,
        )?));

        let muted = st.muted();
        let mut mute_items: Vec<Box<dyn IsMenuItem<Wry>>> = Vec::new();
        if muted {
            mute_items.push(Box::new(MenuItem::with_id(
                app,
                "mute:info",
                mute_label(prefs.mute_until),
                false,
                None::<&str>,
            )?));
            mute_items.push(Box::new(MenuItem::with_id(
                app,
                "mute:off",
                "恢复通知",
                true,
                None::<&str>,
            )?));
            mute_items.push(Box::new(sep()?));
        }
        mute_items.push(Box::new(MenuItem::with_id(
            app,
            "mute:30m",
            "30 分钟",
            true,
            None::<&str>,
        )?));
        mute_items.push(Box::new(MenuItem::with_id(
            app,
            "mute:1h",
            "1 小时",
            true,
            None::<&str>,
        )?));
        mute_items.push(Box::new(MenuItem::with_id(
            app,
            "mute:8h",
            "8 小时",
            true,
            None::<&str>,
        )?));
        mute_items.push(Box::new(MenuItem::with_id(
            app,
            "mute:tomorrow",
            "到明天早上 8 点",
            true,
            None::<&str>,
        )?));
        mute_items.push(Box::new(MenuItem::with_id(
            app,
            "mute:forever",
            "直到我关闭",
            true,
            None::<&str>,
        )?));
        let refs: Vec<&dyn IsMenuItem<Wry>> = mute_items.iter().map(|b| b.as_ref()).collect();
        items.push(Box::new(Submenu::with_id_and_items(
            app,
            "mute",
            if muted {
                "消息免打扰 ✓"
            } else {
                "消息免打扰"
            },
            true,
            &refs,
        )?));
    }
    items.push(Box::new(sep()?));

    let autostart = app.autolaunch().is_enabled().unwrap_or(false);
    items.push(Box::new(CheckMenuItem::with_id(
        app,
        "autostart",
        "开机自动启动",
        true,
        autostart,
        None::<&str>,
    )?));
    items.push(Box::new(CheckMenuItem::with_id(
        app,
        "closetray",
        "关闭窗口时最小化到托盘",
        true,
        prefs.close_to_tray,
        None::<&str>,
    )?));
    if model.logged_in {
        items.push(Box::new(MenuItem::with_id(
            app,
            "settings",
            "通知与托盘设置…",
            true,
            None::<&str>,
        )?));
    }
    items.push(Box::new(MenuItem::with_id(
        app,
        "about",
        "关于 NANO MESH",
        true,
        None::<&str>,
    )?));
    items.push(Box::new(sep()?));
    items.push(Box::new(MenuItem::with_id(
        app,
        "quit",
        "退出 NANO MESH",
        true,
        Some("CmdOrCtrl+Q"),
    )?));

    let refs: Vec<&dyn IsMenuItem<Wry>> = items.iter().map(|b| b.as_ref()).collect();
    Menu::with_items(app, &refs)
}

pub(super) fn refresh_menu(app: &AppHandle<Wry>) {
    if let (Some(tray), Ok(menu)) = (tray_handle(app), build_menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}

pub(super) fn refresh_all(app: &AppHandle<Wry>) {
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

pub(super) fn set_mute(app: &AppHandle<Wry>, until: i64) {
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

pub(super) fn on_menu(app: &AppHandle<Wry>, id: &str) {
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
pub(super) fn request_quit(app: &AppHandle<Wry>) {
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

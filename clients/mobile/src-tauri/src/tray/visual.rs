use super::*;

pub(super) fn png(bytes: &'static [u8]) -> Image<'static> {
    Image::from_bytes(bytes).expect("内嵌托盘图标解码失败")
}

pub(super) fn icon_for(look: Look) -> Image<'static> {
    #[cfg(target_os = "macos")]
    let bytes: &'static [u8] = match look {
        Look::Normal => include_bytes!("../../icons/tray/mac-normal.png"),
        Look::Unread => include_bytes!("../../icons/tray/mac-unread.png"),
        Look::Offline => include_bytes!("../../icons/tray/mac-offline.png"),
        Look::Dnd => include_bytes!("../../icons/tray/mac-dnd.png"),
    };
    #[cfg(not(target_os = "macos"))]
    let bytes: &'static [u8] = match look {
        Look::Normal => include_bytes!("../../icons/tray/normal.png"),
        Look::Unread => include_bytes!("../../icons/tray/unread.png"),
        Look::Offline => include_bytes!("../../icons/tray/offline.png"),
        Look::Dnd => include_bytes!("../../icons/tray/dnd.png"),
    };
    png(bytes)
}

#[cfg(target_os = "windows")]
fn count_badge(n: u32) -> Image<'static> {
    png(match n {
        1 => include_bytes!("../../icons/tray/badge-1.png"),
        2 => include_bytes!("../../icons/tray/badge-2.png"),
        3 => include_bytes!("../../icons/tray/badge-3.png"),
        4 => include_bytes!("../../icons/tray/badge-4.png"),
        5 => include_bytes!("../../icons/tray/badge-5.png"),
        6 => include_bytes!("../../icons/tray/badge-6.png"),
        7 => include_bytes!("../../icons/tray/badge-7.png"),
        8 => include_bytes!("../../icons/tray/badge-8.png"),
        9 => include_bytes!("../../icons/tray/badge-9.png"),
        _ => include_bytes!("../../icons/tray/badge-more.png"),
    })
}

/// 按当前状态刷新图标、标题（macOS 菜单栏数字）、悬停提示与角标。
pub(super) fn refresh_look(app: &AppHandle<Wry>) {
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
            let title = (model.unread > 0).then(|| {
                if model.unread > 99 {
                    "99+".to_string()
                } else {
                    model.unread.to_string()
                }
            });
            let _ = tray.set_title(title);
        }
        let mut tip = String::from("NANO MESH");
        if model.logged_in {
            tip.push_str(&format!(
                "\n{} · {}",
                model.user,
                if model.connected {
                    status_label(&model.status)
                } else {
                    "未连接"
                }
            ));
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
        let n = if prefs.badge && model.logged_in {
            model.unread
        } else {
            0
        };
        #[cfg(target_os = "macos")]
        let _ = w.set_badge_label((n > 0).then(|| {
            if n > 99 {
                "99+".to_string()
            } else {
                n.to_string()
            }
        }));
        #[cfg(target_os = "windows")]
        let _ = w.set_overlay_icon((n > 0).then(|| count_badge(n)));
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = w.set_badge_count((n > 0).then_some(n as i64));
    }
}

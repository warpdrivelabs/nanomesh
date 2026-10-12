use super::*;

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
    pub(super) id: String,
    pub(super) name: String,
    pub(super) count: u32,
}

#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Model {
    pub(super) logged_in: bool,
    pub(super) user: String,
    pub(super) status: String,
    pub(super) connected: bool,
    pub(super) unread: u32,
    pub(super) convos: Vec<Conv>,
    /// 本地时区相对 UTC 的分钟数（东八区 = 480）。
    pub(super) tz_offset_min: i64,
}

#[derive(Default)]
pub struct Tray {
    pub(super) model: Mutex<Model>,
    pub(super) prefs: Mutex<Prefs>,
    pub(super) flash_gen: AtomicU64,
    pub(super) flashing: AtomicBool,
    pub(super) quitting: AtomicBool,
    pub(super) hotkey: Mutex<Option<Shortcut>>,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Look {
    Normal,
    Unread,
    Offline,
    Dnd,
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(super) fn status_label(s: &str) -> &'static str {
    match s {
        "away" => "离开",
        "busy" => "忙碌",
        "dnd" => "勿扰",
        _ => "在线",
    }
}

pub(super) fn status_dot(s: &str) -> &'static str {
    match s {
        "away" => "🟡",
        "busy" => "🔴",
        "dnd" => "⛔",
        _ => "🟢",
    }
}

pub(super) fn prefs_path<R: Runtime>(app: &AppHandle<R>) -> Option<std::path::PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("tray.json"))
}

pub(super) fn load_prefs<R: Runtime>(app: &AppHandle<R>) -> Prefs {
    prefs_path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub(super) fn save_prefs<R: Runtime>(app: &AppHandle<R>, p: &Prefs) {
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
    pub(super) fn prefs(&self) -> Prefs {
        self.prefs.lock().unwrap().clone()
    }

    pub(super) fn model(&self) -> Model {
        self.model.lock().unwrap().clone()
    }

    pub(super) fn muted(&self) -> bool {
        let m = self.prefs.lock().unwrap().mute_until;
        m == -1 || m > now_ms()
    }

    /// 勿扰状态或免打扰时段：不弹通知、不闪烁、不响铃，未读照常累计。
    pub(super) fn quiet(&self) -> bool {
        self.muted() || self.model.lock().unwrap().status == "dnd"
    }

    pub(super) fn look(&self) -> Look {
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

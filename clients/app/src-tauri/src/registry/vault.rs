use tauri::{AppHandle, State};

use crate::{AppState, auth, data_dir};

fn write_device_key(dir: &std::path::Path, pw: &str) -> Result<(), String> {
    let key_path = dir.join("device.key");
    std::fs::write(&key_path, pw).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn wipe_device_key_file(dir: &std::path::Path) {
    let key_path = dir.join("device.key");
    if let Ok(meta) = std::fs::metadata(&key_path) {
        let _ = std::fs::write(&key_path, vec![0u8; meta.len() as usize]);
        let _ = std::fs::remove_file(&key_path);
    }
}

/// 设备口令放在系统钥匙串（macOS/iOS Keychain、Windows 凭据管理器、Linux Secret Service）；
/// 没有钥匙串的平台（Android）返回 Unsupported，调用方退回 0600 文件。
mod devkey {
    pub enum Error {
        Unsupported,
        Denied(String),
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        windows,
        all(target_os = "linux", not(target_os = "android"))
    ))]
    fn entry(dir: &std::path::Path) -> Result<keyring::Entry, Error> {
        keyring::Entry::new("io.nmspace.app", &format!("device-vault:{}", dir.display())).map_err(
            |e| match e {
                keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_) => {
                    Error::Unsupported
                }
                other => Error::Denied(other.to_string()),
            },
        )
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        windows,
        all(target_os = "linux", not(target_os = "android"))
    ))]
    pub fn get(dir: &std::path::Path) -> Result<Option<String>, Error> {
        match entry(dir)?.get_password() {
            Ok(pw) => Ok(Some(pw)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(keyring::Error::PlatformFailure(_)) => Err(Error::Unsupported),
            Err(e) => Err(Error::Denied(e.to_string())),
        }
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        windows,
        all(target_os = "linux", not(target_os = "android"))
    ))]
    pub fn set(dir: &std::path::Path, pw: &str) -> Result<(), Error> {
        entry(dir)?.set_password(pw).map_err(|e| match e {
            keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_) => {
                Error::Unsupported
            }
            other => Error::Denied(other.to_string()),
        })
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        windows,
        all(target_os = "linux", not(target_os = "android"))
    )))]
    pub fn get(_dir: &std::path::Path) -> Result<Option<String>, Error> {
        Err(Error::Unsupported)
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        windows,
        all(target_os = "linux", not(target_os = "android"))
    )))]
    pub fn set(_dir: &std::path::Path, _pw: &str) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// 保存设备口令：优先钥匙串，成功后抹掉明文文件；钥匙串不可用时写 0600 文件。
fn store_device_key(dir: &std::path::Path, pw: &str) -> Result<(), String> {
    match devkey::set(dir, pw) {
        Ok(()) => {
            wipe_device_key_file(dir);
            Ok(())
        }
        Err(_) => write_device_key(dir, pw),
    }
}

pub(crate) fn open_device_vault(
    app: &AppHandle,
    state: &State<'_, AppState>,
) -> Result<(), String> {
    if state.vault.lock().unwrap().is_some() {
        return Ok(());
    }
    let dir = data_dir(app)?;
    let key_path = dir.join("device.key");
    if auth::vault_exists(&dir) {
        let mut denied = None;
        match devkey::get(&dir) {
            Ok(Some(pw)) => {
                if let Ok(vk) = auth::unlock(&dir, pw.trim()) {
                    *state.vault.lock().unwrap() = Some(vk);
                    wipe_device_key_file(&dir);
                    return Ok(());
                }
            }
            Ok(None) | Err(devkey::Error::Unsupported) => {}
            Err(devkey::Error::Denied(e)) => denied = Some(e),
        }
        if let Ok(pw) = std::fs::read_to_string(&key_path) {
            if let Ok(vk) = auth::unlock(&dir, pw.trim()) {
                *state.vault.lock().unwrap() = Some(vk);
                let _ = store_device_key(&dir, pw.trim());
                return Ok(());
            }
        }
        // 钥匙串拒绝访问不等于没有口令；此时重建保险库会让本机身份全部失联。
        if let Some(e) = denied {
            return Err(format!("无法读取系统钥匙串，请允许本应用访问后重试：{e}"));
        }
        // 旧主口令库无法在无界面下打开。挪走后改由本机设备密钥建库，不再弹出解锁页。
        let legacy = dir.join(format!(
            "vault.json.legacy-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        std::fs::rename(dir.join("vault.json"), legacy).map_err(|e| e.to_string())?;
    }
    let mut raw = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
    let pw = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw);
    let vk = auth::setup(&dir, &pw)?;
    store_device_key(&dir, &pw)?;
    *state.vault.lock().unwrap() = Some(vk);
    Ok(())
}

/// 打开应用时准备本机钥匙库，不再向用户要主口令。
#[tauri::command]
pub(crate) fn ensure_device(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    open_device_vault(&app, &state)
}

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

/// 旧库及其全部本机 VK 加密数据整体归档到 `vault.legacy-<ts>/`。identities/ devices/
/// llm_models/ llm_grants/ 都用旧库 VK 加密，只挪 vault.json 会留下解不开的孤儿数据
/// （2026-10-30 事故：静默重建后新旧数据混挂，登录报「解密失败」）。挪完记审计。
fn orphan_vault_data(dir: &std::path::Path) -> Result<(), String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("vault.legacy-{ts}");
    let legacy = dir.join(&name);
    std::fs::create_dir_all(&legacy).map_err(|e| e.to_string())?;
    let victims: Vec<String> = std::iter::once("vault.json".to_string())
        .chain(
            ["identities", "devices", "llm_models", "llm_grants"]
                .iter()
                .map(|s| s.to_string()),
        )
        // 历次旧版平铺的 vault.json.legacy-* 一并收进归档目录
        .chain(
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("vault.json.legacy-")),
        )
        .collect();
    let mut moved = false;
    for name in victims {
        let from = dir.join(&name);
        if !from.exists() {
            continue;
        }
        std::fs::rename(&from, legacy.join(&name)).map_err(|e| e.to_string())?;
        moved = true;
    }
    if moved {
        auth::audit(dir, "vault:legacy-reset");
    }
    Ok(())
}

/// 用随机设备口令新建保险库并解锁。
fn setup_device_vault(dir: &std::path::Path, state: &State<'_, AppState>) -> Result<(), String> {
    let mut raw = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
    let pw = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw);
    let vk = auth::setup(dir, &pw)?;
    store_device_key(dir, &pw)?;
    *state.vault.lock().unwrap() = Some(vk);
    Ok(())
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
        // 旧库无法在无界面下打开。绝不静默重建（会作废全部本机身份）：
        // 返回固定错误码，前端征得用户确认后再调 device_vault_reset。
        return Err("vault_orphaned".into());
    }
    setup_device_vault(&dir, state)
}

/// 打开应用时准备本机钥匙库，不再向用户要主口令。
#[tauri::command]
pub(crate) fn ensure_device(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    open_device_vault(&app, &state)
}

/// 用户确认后重置保险库：旧库及全部本机 VK 加密数据归档到 legacy 目录，再建新库。
/// 前端必须在收到 vault_orphaned 并征得用户同意后才能调用。
#[tauri::command]
pub(crate) fn device_vault_reset(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // 库还能打开（比如刚被别的命令解锁）就不是孤儿，拒绝归档健康数据。
    if state.vault.lock().unwrap().is_some() {
        return Err("vault_not_orphaned".into());
    }
    let dir = data_dir(&app)?;
    orphan_vault_data(&dir)?;
    setup_device_vault(&dir, &state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_moves_vault_and_vk_data() {
        let dir = std::env::temp_dir().join(format!(
            "nm-vault-orphan-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("identities")).unwrap();
        std::fs::create_dir_all(dir.join("devices")).unwrap();
        std::fs::create_dir_all(dir.join("identities.orphaned")).unwrap();
        std::fs::write(dir.join("vault.json"), "{}").unwrap();
        std::fs::write(dir.join("vault.json.legacy-123"), "{}").unwrap();
        std::fs::write(dir.join("identities").join("abc.enc"), "x").unwrap();
        std::fs::write(dir.join("devices").join("abc.dev.enc"), "x").unwrap();
        orphan_vault_data(&dir).unwrap();
        let legacy = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().starts_with("vault.legacy-"))
            .expect("legacy dir created")
            .path();
        assert!(legacy.join("vault.json").exists());
        assert!(legacy.join("vault.json.legacy-123").exists());
        assert!(legacy.join("identities").join("abc.enc").exists());
        assert!(legacy.join("devices").join("abc.dev.enc").exists());
        assert!(!dir.join("vault.json").exists());
        assert!(!dir.join("identities").exists());
        assert!(!dir.join("devices").exists());
        // 手工归档目录（identities.orphaned）不受牵连
        assert!(dir.join("identities.orphaned").exists());
        let audit = std::fs::read_to_string(dir.join("audit.log")).unwrap();
        assert!(audit.contains("vault:legacy-reset"));
        // 空跑：无可挪数据不报错、不重复记审计
        orphan_vault_data(&dir).unwrap();
        let audit = std::fs::read_to_string(dir.join("audit.log")).unwrap();
        assert_eq!(audit.lines().filter(|l| l.contains("vault:legacy-reset")).count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

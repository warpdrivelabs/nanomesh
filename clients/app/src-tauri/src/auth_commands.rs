use serde_json::{Value, json};
use tauri::{AppHandle, State};

use crate::{AppState, auth, data_dir, vk_of};

// ── 本地访问认证（P1）：主口令门 + 身份私钥信封加密。见 auth.rs / docs/CLIENT_AUTH_SECURITY.md。 ──

#[tauri::command]
pub(crate) fn auth_status(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let dir = data_dir(&app)?;
    Ok(json!({
        "masterSet": auth::vault_exists(&dir),
        "unlocked": state.vault.lock().unwrap().is_some(),
        "hasRecovery": auth::has_recovery(&dir),
    }))
}

/// 首次设置主口令：建库 + 把已有明文身份迁移为加密态。
#[tauri::command]
pub(crate) fn setup_master(
    app: AppHandle,
    state: State<'_, AppState>,
    password: String,
) -> Result<(), String> {
    let dir = data_dir(&app)?;
    let vk = auth::setup(&dir, &password)?;
    *state.vault.lock().unwrap() = Some(vk);
    auth::audit(&dir, "setup");
    Ok(())
}

/// 解锁（输入主口令）。
#[tauri::command]
pub(crate) fn unlock(
    app: AppHandle,
    state: State<'_, AppState>,
    password: String,
) -> Result<(), String> {
    let dir = data_dir(&app)?;
    match auth::unlock(&dir, &password) {
        Ok(vk) => {
            *state.vault.lock().unwrap() = Some(vk);
            auth::audit(&dir, "unlock:ok");
            // 云模型期望态自启:autostart 模型解锁后自动上线(需 VK 解 Key,故挂此处;后台跑不阻塞解锁)。
            tauri::async_runtime::spawn(crate::llm_share::autostart_models(app.clone()));
            Ok(())
        }
        Err(e) => {
            auth::audit(&dir, "unlock:fail");
            Err(e)
        }
    }
}

/// 上锁：清零内存 VK + 断开当前连接。
#[tauri::command]
pub(crate) async fn lock(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    {
        let mut g = state.vault.lock().unwrap();
        if let Some(mut vk) = g.take() {
            use zeroize::Zeroize;
            vk.zeroize();
        }
    }
    *state.conn.lock().await = None;
    if let Ok(dir) = data_dir(&app) {
        auth::audit(&dir, "lock");
    }
    Ok(())
}

/// 修改主口令（用旧口令验证并重封保险库密钥；身份种子不动）。
#[tauri::command]
pub(crate) fn change_master(app: AppHandle, old: String, new: String) -> Result<(), String> {
    let dir = data_dir(&app)?;
    auth::change(&dir, &old, &new)?;
    auth::audit(&dir, "change_master");
    Ok(())
}

/// 加密导出全部身份（独立备份口令，与主口令解耦）。返回 base64 备份串。
#[tauri::command]
pub(crate) fn export_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    password: String,
) -> Result<String, String> {
    let vk = vk_of(&state)?;
    let dir = data_dir(&app)?;
    let blob = auth::export(&dir, &vk, &password)?;
    auth::audit(&dir, "backup:export");
    Ok(blob)
}

/// 从备份串导入身份（用备份口令解密，再用当前 VK 重新加密写入）。返回导入条数。
#[tauri::command]
pub(crate) fn import_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    blob: String,
    password: String,
) -> Result<usize, String> {
    let vk = vk_of(&state)?;
    let dir = data_dir(&app)?;
    let n = auth::import(&dir, &vk, &blob, &password)?;
    auth::audit(&dir, &format!("backup:import:{n}"));
    Ok(n)
}

/// 生成恢复码（24 词助记词，仅此一次可见）。
#[tauri::command]
pub(crate) fn generate_recovery(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let vk = vk_of(&state)?;
    let dir = data_dir(&app)?;
    let m = auth::generate_recovery(&dir, &vk)?;
    auth::audit(&dir, "recovery:generate");
    Ok(m)
}

/// 用恢复码重置主口令（忘记主口令时）。
#[tauri::command]
pub(crate) fn recover(
    app: AppHandle,
    mnemonic: String,
    new_password: String,
) -> Result<(), String> {
    let dir = data_dir(&app)?;
    auth::recover(&dir, &mnemonic, &new_password)?;
    auth::audit(&dir, "recovery:reset");
    Ok(())
}

/// 读取最近的安全审计日志（新→旧）。
#[tauri::command]
pub(crate) fn read_audit(app: AppHandle) -> Result<Vec<String>, String> {
    Ok(auth::read_audit(&data_dir(&app)?, 60))
}

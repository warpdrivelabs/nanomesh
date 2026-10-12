use tauri::{AppHandle, State};

use crate::{AppState, auth, devices, hex, identities_dir, pubkey_of_seed, vk_of};

/// 列出全部用户身份（公钥 hex）。仅解锁后可用；种子加密存 `identities/<pubkey>.enc`。
#[tauri::command]
pub(crate) fn list_identities(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    vk_of(&state)?; // 门禁：未解锁拒绝
    let dir = identities_dir(&app)?;
    let mut out = devices::accounts(&app);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".enc") {
                if stem.len() == 64 {
                    out.push(stem.to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// 新建一个用户身份（生成密钥对，用 VK 加密种子后落盘），返回其公钥 hex。
#[tauri::command]
pub(crate) fn create_identity(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let vk = vk_of(&state)?;
    let mut seed = nm_transport::SecretKey::generate().to_bytes();
    let pk = pubkey_of_seed(&seed);
    let enc = auth::encrypt_seed(&vk, &seed)?;
    {
        use zeroize::Zeroize;
        seed.zeroize();
    }
    let path = identities_dir(&app)?.join(format!("{}.enc", hex(&pk)));
    std::fs::write(&path, enc).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(hex(&pk))
}

/// 载入指定身份的种子（用 VK 解密）。
pub(crate) fn load_identity_seed(
    app: &AppHandle,
    vk: &[u8; 32],
    pubkey: &str,
) -> Result<[u8; 32], String> {
    let data = std::fs::read(identities_dir(app)?.join(format!("{}.enc", pubkey)))
        .map_err(|_| "用户身份不存在（可能已删除）".to_string())?;
    auth::decrypt_seed(vk, &data)
}

//! 应用内更新：包装 tauri-plugin-updater 的 check / download+install。
//! 前端仍走 window.__TAURI__.core.invoke，不引入前端打包链。

use std::sync::atomic::{AtomicBool, Ordering};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

static INSTALLING: AtomicBool = AtomicBool::new(false);

#[tauri::command]
pub async fn app_check_update(app: AppHandle) -> Result<Value, String> {
    let update = app
        .updater()
        .map_err(|e| format!("更新器初始化失败：{e}"))?
        .check()
        .await
        .map_err(|e| format!("检查更新失败：{e}"))?;

    match update {
        Some(u) => Ok(json!({
            "available": true,
            "version": u.version,
            "notes": u.body,
            "force": u.raw_json.get("force").and_then(Value::as_bool).unwrap_or(false),
        })),
        None => Ok(json!({ "available": false })),
    }
}

#[tauri::command]
pub async fn app_download_install(app: AppHandle) -> Result<(), String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("更新已在进行中".into());
    }

    let result = async {
        let update = app
            .updater()
            .map_err(|e| format!("更新器初始化失败：{e}"))?
            .check()
            .await
            .map_err(|e| format!("检查更新失败：{e}"))?
            .ok_or_else(|| "当前已是最新版本".to_string())?;

        let emit_app = app.clone();
        let mut received: usize = 0;
        update
            .download_and_install(
                move |chunk, total| {
                    received += chunk;
                    let _ = emit_app.emit(
                        "update-progress",
                        json!({ "received": received, "total": total }),
                    );
                },
                || {},
            )
            .await
            .map_err(|e| format!("下载或安装更新失败：{e}"))?;

        Ok::<(), String>(())
    }
    .await;

    INSTALLING.store(false, Ordering::SeqCst);
    result?;

    app.restart()
}

use serde_json::{Value, json};
use tauri::{AppHandle, Manager, State};

use crate::{AppState, data_dir, hex, parse_id, send_with_retry, session_of};

/// 上传头像等内容寻址 blob。`data_b64` = 纯 base64（无 data: 前缀）。返回 "b3:<hash-hex>"。
#[tauri::command]
pub(crate) async fn blob_put(
    state: State<'_, AppState>,
    data_b64: String,
    mime: String,
) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.trim())
        .map_err(|e| format!("非法 base64: {e}"))?;
    let session = session_of(&state).await?;
    let (hash, _home) = session
        .blob_put(bytes, &mime)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("b3:{}", hex(&hash)))
}

fn blob_hex(reference: &str) -> Result<&str, String> {
    let hexh = reference.strip_prefix("b3:").unwrap_or(reference).trim();
    parse_id(hexh)?;
    Ok(hexh)
}

fn read_cached_uri(app: &AppHandle, reference: &str) -> Option<String> {
    let hexh = blob_hex(reference).ok()?;
    let path = data_dir(app)
        .ok()?
        .join("blobs")
        .join(format!("{hexh}.uri"));
    let s = std::fs::read_to_string(path).ok()?;
    if s.starts_with("data:") {
        Some(s)
    } else {
        None
    }
}

fn write_blob_cache(app: &AppHandle, hash_hex: &str, mime: &str, data: &[u8]) {
    use base64::Engine;
    let Ok(dir) = data_dir(app) else { return };
    let dir = dir.join("blobs");
    let _ = std::fs::create_dir_all(&dir);
    let mime = if mime.trim().is_empty() {
        "application/octet-stream"
    } else {
        mime.trim()
    };
    let uri = format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(data)
    );
    let _ = std::fs::write(dir.join(format!("{hash_hex}.uri")), uri);
}

fn read_cached_bytes(app: &AppHandle, reference: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let uri = read_cached_uri(app, reference)?;
    let b64 = uri.split_once(',')?.1;
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

/// 只读本机缓存。没有的项是空字符串，不会向节点请求。
#[tauri::command]
pub(crate) fn blob_cached(app: AppHandle, references: Vec<String>) -> Result<Vec<String>, String> {
    Ok(references
        .into_iter()
        .map(|r| read_cached_uri(&app, &r).unwrap_or_default())
        .collect())
}

/// 取 blob 并返回 data:URI（带 app_data_dir/blobs 本地缓存，避免重复拉取）。
/// `reference` = "b3:<hash-hex>"；`home_node` = 实体归属节点 hex（可空，用于跨节点回源）。
#[tauri::command]
pub(crate) async fn blob_get(
    app: AppHandle,
    state: State<'_, AppState>,
    reference: String,
    home_node: String,
) -> Result<String, String> {
    if let Some(s) = read_cached_uri(&app, &reference) {
        return Ok(s);
    }
    let hexh = blob_hex(&reference)?.to_string();
    let hash = parse_id(&hexh)?;
    let home = if home_node.trim().is_empty() {
        Vec::new()
    } else {
        parse_id(home_node.trim())
            .map(|h| h.to_vec())
            .unwrap_or_default()
    };
    let session = session_of(&state).await?;
    let (data, mime) = session
        .blob_get(hash.to_vec(), home)
        .await
        .map_err(|e| e.to_string())?;
    let mime = if mime.trim().is_empty() {
        "application/octet-stream".to_string()
    } else {
        mime
    };
    write_blob_cache(&app, &hexh, &mime, &data);
    read_cached_uri(&app, &reference).ok_or_else(|| "缓存写入失败".to_string())
}

/// 上传一块媒体，返回内容哈希和所在节点。单块须小于节点的 1MB 上限。
#[tauri::command]
pub(crate) async fn blob_put_ref(
    app: AppHandle,
    state: State<'_, AppState>,
    data_b64: String,
    mime: String,
) -> Result<Value, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.trim())
        .map_err(|e| format!("非法 base64: {e}"))?;
    let session = session_of(&state).await?;
    let (hash, home) = session
        .blob_put(bytes.clone(), &mime)
        .await
        .map_err(|e| e.to_string())?;
    let hash_hex = hex(&hash);
    write_blob_cache(&app, &hash_hex, &mime, &bytes);
    Ok(json!({ "ref": format!("b3:{hash_hex}"), "home": hex(&home) }))
}

const MEDIA_CAP: usize = 50 * 1024 * 1024;

async fn gather_blobs(
    app: &AppHandle,
    state: &State<'_, AppState>,
    references: &[String],
    home_node: &str,
) -> Result<Vec<u8>, String> {
    if references.is_empty() {
        return Err("没有可读取的内容".into());
    }
    let home = if home_node.trim().is_empty() {
        Vec::new()
    } else {
        parse_id(home_node.trim())
            .map(|h| h.to_vec())
            .unwrap_or_default()
    };
    let session = session_of(state).await?;
    let mut all = Vec::new();
    for reference in references {
        let data = if let Some(cached) = read_cached_bytes(app, reference) {
            cached
        } else {
            let hexh = blob_hex(reference)?.to_string();
            let hash = parse_id(&hexh)?;
            let (data, mime) = session
                .blob_get(hash.to_vec(), home.clone())
                .await
                .map_err(|e| e.to_string())?;
            write_blob_cache(app, &hexh, &mime, &data);
            data
        };
        if all.len().saturating_add(data.len()) > MEDIA_CAP {
            return Err("文件超过 50MB".into());
        }
        all.extend(data);
    }
    Ok(all)
}

fn media_cache_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = data_dir(app)?.join("media-cache");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn downloads_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = std::path::PathBuf::from(if home.is_empty() { "/tmp".into() } else { home })
        .join("Downloads");
    if dir.is_dir() {
        dir
    } else {
        std::env::temp_dir()
    }
}

fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        "文件".into()
    } else {
        cleaned.chars().take(120).collect()
    }
}

fn unique_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = std::path::Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("文件");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    for n in 2..1000 {
        let next = if ext.is_empty() {
            format!("{stem} {n}")
        } else {
            format!("{stem} {n}.{ext}")
        };
        let p = dir.join(next);
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{name}.new"))
}

fn path_under(root: &std::path::Path, target: &std::path::Path) -> bool {
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    let Ok(target) = target.canonicalize() else {
        return false;
    };
    target.starts_with(root)
}

/// 把分片拼成一个缓存文件，供播放或另存。已拼过的直接返回路径。
#[tauri::command]
pub(crate) async fn media_file(
    app: AppHandle,
    state: State<'_, AppState>,
    references: Vec<String>,
    home_node: String,
    ext: String,
) -> Result<String, String> {
    let ext = ext
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>();
    let ext = if ext.is_empty() {
        "bin".to_string()
    } else {
        ext
    };
    let key = references.join(",");
    let name = format!("{:016x}.{}", simple_key(&key), ext);
    let path = media_cache_dir(&app)?.join(name);
    if !path.is_file() {
        let bytes = gather_blobs(&app, &state, &references, &home_node).await?;
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    }
    Ok(path.to_string_lossy().into_owned())
}

/// 拼好后写入下载目录，返回最终路径。
#[tauri::command]
pub(crate) async fn save_media(
    app: AppHandle,
    state: State<'_, AppState>,
    references: Vec<String>,
    home_node: String,
    filename: String,
) -> Result<String, String> {
    let bytes = gather_blobs(&app, &state, &references, &home_node).await?;
    let path = unique_path(&downloads_dir(), &safe_filename(&filename));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// 用系统默认程序打开缓存或下载目录里的文件。
#[tauri::command]
pub(crate) async fn open_path(app: AppHandle, path: String) -> Result<(), String> {
    let target = std::path::PathBuf::from(&path);
    let cache = media_cache_dir(&app)?;
    let downloads = downloads_dir();
    if !path_under(&cache, &target) && !path_under(&downloads, &target) {
        return Err("只能打开已接收的文件".into());
    }
    let status = {
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open").arg(&target).status()
        }
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("xdg-open").arg(&target).status()
        }
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("cmd")
                .args(["/C", "start", "", &path])
                .status()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            return Err("当前系统不能打开文件".into());
        }
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("系统没有打开这个文件".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn simple_key(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 私聊或群发一条富消息。`body` 是 nmspace.v1/chat 的 JSON。
#[tauri::command]
pub(crate) async fn send_rich(
    app: AppHandle,
    state: State<'_, AppState>,
    target: String,
    body: String,
    group: bool,
) -> Result<(), String> {
    let id = parse_id(&target)?;
    send_with_retry(&app, &state, move |s| {
        let body = body.clone();
        async move { s.send_rich(id, &body, group).await.map(|_| ()) }
    })
    .await
}

#[tauri::command]
pub(crate) async fn capture_screen(app: AppHandle, hide: bool) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        capture_screen_mac(app, hide).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, hide);
        Err("当前系统还不能截图".into())
    }
}

#[tauri::command]
pub(crate) async fn list_windows(app: AppHandle) -> Result<Vec<Value>, String> {
    #[cfg(target_os = "macos")]
    {
        list_windows_mac(app).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err("当前系统还不能列出窗口".into())
    }
}

#[tauri::command]
pub(crate) async fn capture_window(id: u32) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        capture_window_mac(id).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = id;
        Err("当前系统还不能截取窗口".into())
    }
}

#[cfg(target_os = "macos")]
async fn capture_screen_mac(app: AppHandle, hide: bool) -> Result<String, String> {
    let win = app.get_webview_window("main");
    let restore = if hide {
        if let Some(w) = &win {
            let _ = w.hide();
        }
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        win
    } else {
        None
    };
    let joined = tokio::task::spawn_blocking(|| shot(&["-x", "-m", "-t", "jpg"])).await;
    if let Some(w) = restore {
        let _ = w.show();
        let _ = w.set_focus();
    }
    joined.map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
async fn capture_window_mac(id: u32) -> Result<String, String> {
    let flag = format!("-l{id}");
    tokio::task::spawn_blocking(move || shot(&[flag.as_str(), "-x", "-o", "-t", "jpg"]))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
fn shot(args: &[&str]) -> Result<String, String> {
    use base64::Engine;
    let path = std::env::temp_dir().join(format!(
        "nm-shot-{}-{}.jpg",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args(args)
        .arg(&path)
        .status()
        .map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    if !status.success() || bytes.is_empty() {
        return Err("截图失败。请在系统设置的隐私与安全性中允许本应用录制屏幕".into());
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

#[cfg(target_os = "macos")]
const WINDOW_LIST_SWIFT: &str = r#"import Cocoa
import Foundation
let opts = CGWindowListOption(arrayLiteral: .optionOnScreenOnly, .excludeDesktopElements)
guard let raw = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
    print("[]")
    exit(0)
}
var rows: [[String: Any]] = []
for w in raw {
    let layer = w["kCGWindowLayer"] as? Int ?? 0
    if layer != 0 { continue }
    let wid = w["kCGWindowNumber"] as? Int ?? 0
    if wid == 0 { continue }
    let owner = w["kCGWindowOwnerName"] as? String ?? ""
    let title = w["kCGWindowName"] as? String ?? ""
    if owner == "Window Server" || owner == "Dock" { continue }
    if title == "NANO MESH" || owner == "nmspace" { continue }
    if title.isEmpty && owner.isEmpty { continue }
    rows.append(["id": wid, "title": title, "app": owner])
    if rows.count >= 40 { break }
}
let data = try! JSONSerialization.data(withJSONObject: rows)
FileHandle.standardOutput.write(data)
"#;

#[cfg(target_os = "macos")]
async fn list_windows_mac(app: AppHandle) -> Result<Vec<Value>, String> {
    let dir = data_dir(&app)?;
    tokio::task::spawn_blocking(move || {
        let bin = dir.join("nm-windows");
        if !bin.is_file() {
            let src = dir.join("nm-windows.swift");
            std::fs::write(&src, WINDOW_LIST_SWIFT).map_err(|e| e.to_string())?;
            let st = std::process::Command::new("swiftc")
                .args(["-O", "-o"])
                .arg(&bin)
                .arg(&src)
                .status()
                .map_err(|_| "这台电脑没有 swiftc，不能列出窗口".to_string())?;
            if !st.success() {
                return Err("不能列出窗口".into());
            }
        }
        let out = std::process::Command::new(&bin)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("不能列出窗口".into());
        }
        let list: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
        Ok(list)
    })
    .await
    .map_err(|e| e.to_string())?
}

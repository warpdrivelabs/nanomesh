use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{AppState, data_dir, hex, parse_id, send_with_retry, session_of};

/// 上传头像等内容寻址 blob。`data_b64` = 纯 base64（无 data: 前缀）。返回 "b3:<hash-hex>"。
#[tauri::command]
pub(crate) async fn blob_put(
    app: AppHandle,
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
        .blob_put(bytes.clone(), &mime)
        .await
        .map_err(|e| e.to_string())?;
    let hash_hex = hex(&hash);
    // 上传同时写入本机缓存，避免随后 blob_get 再走一轮网络。
    write_blob_cache(&app, &hash_hex, &mime, &bytes);
    Ok(format!("b3:{hash_hex}"))
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
) -> Result<u64, String> {
    let id = parse_id(&target)?;
    send_with_retry(&app, &state, move |s| {
        let body = body.clone();
        // 返回 node ack 的 ref_gram_id = 本条消息 gram_id，供富媒体气泡标记单勾✓（与文本 send_to 一致）。
        async move { s.send_rich(id, &body, group).await.map(|ack| ack.ref_gram_id.unwrap_or(0)) }
    })
    .await
}

// ── 截图（xcap 三平台：整屏 / 窗口）+ 选区裁剪 + mac 授权预检 ──
// macOS：CGPreflightScreenCaptureAccess 预检；未授权返回 {needPermission:true}，前端弹引导卡。
// Windows/Linux：xcap(WGC) 抓物理像素整屏。
// 选区流程（微信式）：capture_screen 抓全屏 → 独立全屏透明窗口(shot-overlay)盖在一切之上
// 展示快照 + 拖选 → shot_finish 回传物理像素选区 → 裁剪 JPEG → 事件发给主窗口进待发区。
// 快照 RGBA 存 SHOT_SNAP（一次一份），overlay 页面经 shotoverlay:// 协议拉取 PNG。

/// 当前选区会话的整屏快照（RGBA base64 + 尺寸）。同一时刻只有一场。
static SHOT_SNAP: std::sync::Mutex<Option<(u32, u32, String)>> = std::sync::Mutex::new(None);
/// 本场快照落盘 PNG 的路径（overlay 经 asset 协议加载）。
static SHOT_PNG_PATH: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 选区截图：抓全屏 → 开遮罩窗口。返回 {w,h}（触发选区 UI）；结果经事件 `shot://done` 推送。
#[tauri::command]
pub(crate) async fn capture_screen(app: AppHandle, hide: bool) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        return capture_screen_mac(app, hide).await;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = hide;
        // 已有选区窗口在跑则先关掉（重复点截图 = 重开）。
        if let Some(old) = app.get_webview_window("shot-overlay") {
            let _ = old.close();
        }
        let (w, h, rgba) = capture_screen_xcap(false)?;
        // 快照一次性编码 PNG 落临时文件，overlay 用 convertFileSrc 直接当图片加载
        //（比 base64 fetch 快得多）；马赛克取像素由前端 drawImage 反推。
        let png_path = shot_snap_png_file(&w, &h, &rgba)?;
        *SHOT_SNAP.lock().unwrap() = Some((w, h, rgba));
        *SHOT_PNG_PATH.lock().unwrap() = Some(png_path.to_string_lossy().into_owned());
        // overlay 加载主 UI 同源的独立页面 ui/shot.html（标注编辑器代码都在那），
        // 经 shotoverlay:// 协议拿快照 PNG（20MB+ 的 RGBA 不过 invoke/URL）。
        let win = tauri::webview::WebviewWindowBuilder::new(
            &app,
            "shot-overlay",
            tauri::WebviewUrl::App("shot.html".into()),
        )
        .title("shot")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(true)
        // Windows 无边框窗口默认带 shadow = 7px 隐形边框，outer 尺寸会比设定大
        //（实测 2880x1800 → 2904x1814），快照铺底随之错位（左/上露边）。必须关。
        .shadow(false)
        .build()
        .map_err(|e| format!("打不开截图窗口: {e}"))?;
        // 铺满主显示器：直接用物理坐标，避免 逻辑=物理/缩放 反算的四舍五入差 1px。
        let mon = xcap::Monitor::all()
            .ok()
            .and_then(|ms| ms.into_iter().next())
            .ok_or("没有可用的显示器")?;
        let (mx, my) = (mon.x().unwrap_or(0), mon.y().unwrap_or(0));
        let (mw, mh) = (mon.width().unwrap_or(w), mon.height().unwrap_or(h));
        let _ = win.set_position(tauri::PhysicalPosition::new(mx, my));
        let _ = win.set_size(tauri::PhysicalSize::new(mw, mh));
        Ok(json!({ "w": w, "h": h, "png": png_path.to_string_lossy().into_owned() }))
    }
}

/// 选区完成（overlay 页面 invoke）。
/// jpegB64 有值 = overlay 前端已合成（含标注），直接采用；
/// 无值且 cw/ch 有效 = 无标注轻量路径，Rust 按 SHOT_SNAP 裁剪；
/// cw/ch 全 0 = 取消/复制/保存后关窗。
/// 结果经事件 `shot://done` 推给主窗口（cancelled 或 {jpeg,name}）。
#[tauri::command]
pub(crate) async fn shot_finish(
    app: AppHandle,
    window: tauri::Window,
    x: u32,
    y: u32,
    cw: u32,
    ch: u32,
    jpeg_b64: Option<String>,
) -> Result<(), String> {
    let _ = window;
    let snap = SHOT_SNAP.lock().unwrap().take();
    *SHOT_PNG_PATH.lock().unwrap() = None;
    let _ = app.get_webview_window("shot-overlay").map(|w| w.close());
    if let Some(jpeg) = jpeg_b64.filter(|s| !s.is_empty()) {
        let _ = app.emit("shot://done", json!({ "jpeg": jpeg, "name": "截图.jpg" }));
        return Ok(());
    }
    let Some((w, h, rgba)) = snap else {
        return Err("没有进行中的截图".into());
    };
    if cw == 0 || ch == 0 {
        let _ = app.emit("shot://done", json!({ "cancelled": true }));
        return Ok(());
    }
    let b64 = crop_rgba_to_jpeg(w, h, &rgba, x, y, cw, ch).await?;
    let _ = app.emit("shot://done", json!({ "jpeg": b64, "name": "截图.jpg" }));
    Ok(())
}

/// overlay「保存到本地」：JPEG 字节落下载目录，返回路径。
#[tauri::command]
pub(crate) async fn shot_save(data_b64: String) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.trim())
        .map_err(|e| format!("非法数据: {e}"))?;
    let name = format!(
        "截图 {}",
        chrono_now_stamp()
    );
    let path = unique_path(&downloads_dir(), &format!("{name}.jpg"));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

fn chrono_now_stamp() -> String {
    // 无 chrono 依赖：秒级 Unix 时间戳即可保唯一（unique_path 会再兜底加序号）。
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string()
}

/// 把 SHOT_SNAP 的 RGBA 编码 PNG 写进临时文件，返回路径（给 overlay 铺底图）。
fn shot_snap_png_file(w: &u32, h: &u32, rgba: &str) -> Result<std::path::PathBuf, String> {
    use base64::Engine;
    use image::ImageEncoder as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(rgba.as_bytes())
        .map_err(|e| format!("非法像素数据: {e}"))?;
    let path = std::env::temp_dir().join(format!("nm-shot-snap-{}.png", std::process::id()));
    let img: image::RgbaImage =
        image::ImageBuffer::from_raw(*w, *h, raw).ok_or("像素数据与尺寸不符")?;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(img.as_raw(), *w, *h, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("PNG 编码失败: {e}"))?;
    std::fs::write(&path, out).map_err(|e| format!("快照落盘失败: {e}"))?;
    Ok(path)
}

/// 把选区 RGBA 裁剪并编码为 JPEG（base64）。共享实现（shot_finish / 旧 shot_crop）。
async fn crop_rgba_to_jpeg(
    w: u32,
    h: u32,
    rgba: &str,
    x: u32,
    y: u32,
    cw: u32,
    ch: u32,
) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(rgba.as_bytes())
        .map_err(|e| format!("非法像素数据: {e}"))?;
    if (w as usize) * (h as usize) * 4 != bytes.len() {
        return Err("像素数据与尺寸不符".into());
    }
    let (x, y) = (x.min(w.saturating_sub(1)), y.min(h.saturating_sub(1)));
    let (cw, ch) = (cw.min(w - x), ch.min(h - y));
    if cw == 0 || ch == 0 {
        return Err("选区为空".into());
    }
    let mut cropped = Vec::with_capacity(cw as usize * ch as usize * 4);
    for row in y..y + ch {
        let start = (row as usize * w as usize + x as usize) * 4;
        cropped.extend_from_slice(&bytes[start..start + cw as usize * 4]);
    }
    tokio::task::spawn_blocking(move || {
        let img: image::RgbaImage =
            image::ImageBuffer::from_raw(cw, ch, cropped).ok_or("裁剪数据不完整")?;
        let mut out = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
        enc.encode_image(&image::DynamicImage::ImageRgba8(img))
            .map_err(|e| e.to_string())?;
        Ok(base64::engine::general_purpose::STANDARD.encode(out))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// macOS 专用：屏幕录制授权预检。返回 {granted, canAsk}。
#[cfg(target_os = "macos")]
#[tauri::command]
pub(crate) async fn screen_capture_status() -> Result<Value, String> {
    let granted = tokio::task::spawn_blocking(|| {
        let (preflight, _) = mac_capture_fns();
        preflight()
    })
    .await
    .unwrap_or(false);
    Ok(json!({ "granted": granted }))
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub(crate) async fn screen_capture_status() -> Result<Value, String> {
    Ok(json!({ "granted": true }))
}

/// shotoverlay:// 协议：给选区遮罩窗口(shot.html)提供快照元数据 /meta 与快照 PNG /png
///（/png 现仅作兜底；正常路径 capture_screen 已落盘 PNG，前端走 asset 协议）。
/// Windows 下自定义协议固定走 http://shotoverlay.local/ 域名（Tauri 约定）。
pub(crate) fn shot_overlay_protocol(
    req: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
    use base64::Engine;
    use image::ImageEncoder as _;
    let path = req.uri().path().to_string();
    let (ct, body) = match path.as_str() {
        "/meta" => {
            let (w, h, _) = SHOT_SNAP
                .lock()
                .unwrap()
                .clone()
                .unwrap_or((0, 0, String::new()));
            // png 路径由 capture_screen 存进会话状态；这里简化：前端只拿尺寸，
            // 路径走 capture_screen 返回值 → 主窗口不行（overlay 独立加载）。
            // 改为存在 SHOT_PNG_PATH。
            let png = SHOT_PNG_PATH.lock().unwrap().clone();
            ("application/json", json!({ "w": w, "h": h, "png": png }).to_string().into_bytes())
        }
        "/png" => {
            let snap = SHOT_SNAP.lock().unwrap().clone();
            let body = snap
                .and_then(|(w, h, rgba)| {
                    base64::engine::general_purpose::STANDARD
                        .decode(rgba)
                        .ok()
                        .map(|raw| (w, h, raw))
                })
                .and_then(|(w, h, raw)| {
                    let img: image::RgbaImage = image::ImageBuffer::from_raw(w, h, raw)?;
                    let mut out = Vec::new();
                    image::codecs::png::PngEncoder::new(&mut out)
                        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
                        .ok()?;
                    Some(out)
                })
                .unwrap_or_default();
            ("image/png", body)
        }
        _ => ("text/plain", b"not found".to_vec()),
    };
    tauri::http::Response::builder()
        .status(200)
        .header("Content-Type", ct)
        .header("Access-Control-Allow-Origin", "*")
        .body(body)
        .unwrap()
}

/// macOS：跳转到系统设置 → 隐私与安全性 → 屏幕录制。
#[cfg(target_os = "macos")]
#[tauri::command]
pub(crate) async fn mac_open_screen_settings() -> Result<(), String> {
    tokio::task::spawn_blocking(|| {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
            .status()
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
    .map(|_| ())
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub(crate) async fn mac_open_screen_settings() -> Result<(), String> {
    Err("仅 macOS 有系统录制授权".into())
}

/// 把选区 RGBA 裁剪并编码为 JPEG（base64）。入参 rgba 为 capture_screen 返回的整屏像素。
/// （诊断日志保留：对照 抓屏尺寸 / 主窗口物理视口 / scale。）

/// xcap 抓主显示器整屏。返回 (宽, 高, base64 RGBA)。
#[cfg(not(target_os = "macos"))]
fn capture_screen_xcap(hide_main: bool) -> Result<(u32, u32, String), String> {
    use base64::Engine;
    let _ = hide_main;
    let monitors = xcap::Monitor::all().map_err(|e| format!("枚举显示器失败: {e}"))?;
    let mon = monitors.into_iter().next().ok_or("没有可用的显示器")?;
    let img = mon.capture_image().map_err(|e| format!("截图失败: {e}"))?;
    let (w, h) = (img.width(), img.height());
    tracing::info!("capture_screen: 抓屏 {w}x{h}");
    Ok((
        w,
        h,
        base64::engine::general_purpose::STANDARD.encode(img.into_raw()),
    ))
}

/// 把 JPEG base64 解码为统一 RGBA（macOS screencapture 路径转统一管线用）。
#[cfg(target_os = "macos")]
fn decode_jpeg_meta(b64: &str) -> Result<(u32, u32, String), String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("非法截图数据: {e}"))?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("解码截图失败: {e}"))?;
    let (w, h) = (img.width(), img.height());
    Ok((
        w,
        h,
        base64::engine::general_purpose::STANDARD.encode(img.to_rgba8().into_raw()),
    ))
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
        list_windows_xcap()
    }
}

#[cfg(not(target_os = "macos"))]
fn list_windows_xcap() -> Result<Vec<Value>, String> {
    let windows = xcap::Window::all().map_err(|e| format!("枚举窗口失败: {e}"))?;
    let mut rows = Vec::new();
    for w in windows {
        // 0.9 的 getter 全返回 XCapResult；失败(权限/已销毁)的窗口直接跳过。
        let (Ok(app_name), Ok(title)) = (w.app_name(), w.title()) else {
            continue;
        };
        if title.trim().is_empty() && app_name.trim().is_empty() {
            continue;
        }
        // 排除自己（托盘/隐藏辅助窗口会混进来）。
        if app_name.contains("nmspace") || title == "NANO MESH" {
            continue;
        }
        let id = w.id().unwrap_or(0);
        if id == 0 {
            continue;
        }
        rows.push(json!({ "id": id, "title": title, "app": app_name }));
        if rows.len() >= 40 {
            break;
        }
    }
    Ok(rows)
}

#[tauri::command]
pub(crate) async fn capture_window(id: u32) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        capture_window_mac(id).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        capture_window_xcap(id).await
    }
}

#[cfg(not(target_os = "macos"))]
async fn capture_window_xcap(id: u32) -> Result<String, String> {
    use base64::Engine;
    tokio::task::spawn_blocking(move || {
        let w = xcap::Window::all()
            .map_err(|e| format!("枚举窗口失败: {e}"))?
            .into_iter()
            .find(|w| w.id().unwrap_or(0) == id)
            .ok_or("窗口已关闭")?;
        let img = w.capture_image().map_err(|e| format!("截取窗口失败: {e}"))?;
        let mut out = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
        enc.encode_image(&image::DynamicImage::ImageRgba8(img))
            .map_err(|e| e.to_string())?;
        Ok(base64::engine::general_purpose::STANDARD.encode(out))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
async fn capture_screen_mac(app: AppHandle, hide: bool) -> Result<Value, String> {
    // 预检在先：未授权时不进截屏流程，前端直接弹引导卡。
    let (preflight, _) = mac_capture_fns();
    if !preflight() {
        return Ok(json!({ "needPermission": true }));
    }
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
    let b64 = joined.map_err(|e| e.to_string())??;
    decode_jpeg_meta(&b64).map(|(w, h, rgba)| json!({ "w": w, "h": h, "rgba": rgba }))
}

#[cfg(target_os = "macos")]
fn mac_capture_fns() -> (
    unsafe extern "C" fn() -> bool,
    unsafe extern "C" fn() -> bool,
) {
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    (CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess)
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

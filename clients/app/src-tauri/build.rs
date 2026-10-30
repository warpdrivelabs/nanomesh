use std::path::{Path, PathBuf};

fn main() {
    // frontendDist = ../ui 在编译期由 generate_context! 内嵌；tauri_build 默认不跟踪它，
    // 导致纯前端(ui/)改动不触发重编译、dev 里看到旧界面。显式声明后 ui/ 改动即触发重建。
    println!("cargo:rerun-if-changed=../ui");
    if let Some(ui) = discover_cmx_ui_dir() {
        println!("cargo:rustc-env=CMX_AGENT_UI_DIR={}", ui.display());
        println!("cargo:rerun-if-changed={}", ui.join("index.html").display());
    }
    tauri_build::build();
}

/// 定位 `cmx-agent-web/ui`：本地 checkout → Cargo git checkouts（gitee 依赖）。
/// 合格目录须含 `index.html` + `embed/truemate-app.js`（与 cmx_host.rs 运行期校验一致；
/// 旧本地 checkout 缺 embed 会致组件化嵌入 404，fa17704 只修了运行期，此处补齐编译期）。
/// 注意：build.rs 内禁止再调 `cargo`（会与当前构建抢锁）。
fn discover_cmx_ui_dir() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    if let Some(ui) = fallback_local(&manifest_dir) {
        return Some(ui);
    }
    find_in_cargo_git_checkouts()
}

/// 与 cmx_host.rs resolve_ui_dir 的 `ok` 谓词保持同款语义。
fn ui_qualified(p: &Path) -> bool {
    p.join("index.html").is_file() && p.join("embed/truemate-app.js").is_file()
}

fn fallback_local(manifest_dir: &Path) -> Option<PathBuf> {
    let mut candidates = vec![
        manifest_dir.join("../../../cmx-agent/crates/cmx-agent-web/ui"),
        manifest_dir
            .join("../../../../../../cmx-workspace/backend/cmx-agent/crates/cmx-agent-web/ui"),
    ];
    if let Some(h) = home_dir() {
        candidates.push(h.join("cmx-agent/crates/cmx-agent-web/ui"));
        candidates.push(h.join("cmx-workspace/backend/cmx-agent/crates/cmx-agent-web/ui"));
    }
    for c in candidates {
        if ui_qualified(&c) {
            return Some(c);
        }
    }
    None
}

fn find_in_cargo_git_checkouts() -> Option<PathBuf> {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".cargo")))?;
    let checkouts = cargo_home.join("git").join("checkouts");
    let rd = std::fs::read_dir(&checkouts).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for ent in rd.flatten() {
        let name = ent.file_name();
        let name = name.to_string_lossy();
        // URL hash 目录名通常含仓库名片段，如 cmx-agent-<hash>
        if !name.contains("cmx-agent") {
            continue;
        }
        let Ok(revs) = std::fs::read_dir(ent.path()) else {
            continue;
        };
        for rev in revs.flatten() {
            let ui = rev.path().join("crates/cmx-agent-web/ui");
            if !ui_qualified(&ui) {
                continue;
            }
            let mtime = rev
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                best = Some((mtime, ui));
            }
        }
    }
    best.map(|(_, p)| p)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

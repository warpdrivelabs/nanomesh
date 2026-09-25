//! `xtask` — 开发者任务编排。
//! 用法：`cargo run -p xtask -- <task>`
//!
//! - `dist`：后台打包发布——交叉编译 + 组装解压即用的 zip（bin + 启停脚本 + 配置模板 + README）。
//!   `cargo xtask dist [--all | --target <triple>] [--bins a,b,c] [--no-build]`
//!   mac 目标用原生 cargo；linux(musl)/windows(gnu) 用 cargo-zigbuild（`brew install zig cargo-zigbuild`）。

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// 发布包默认包含的二进制（后台三件：节点 + 管理台 + 回声测试机器人）。
const DEFAULT_BINS: &[&str] = &["nmd", "nm-admind", "nm-echo"];

/// `--all` 的官方目标矩阵：mac 双架构 + linux 静态(musl) + windows。
const ALL_TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-musl",
    "x86_64-pc-windows-gnu",
];

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("dist") => dist(&args[1..]),
        Some("proto") => {
            println!("TODO: 用 prost-build 生成协议代码");
            Ok(())
        }
        Some("e2e") => {
            println!("TODO: 启动多节点集成测试");
            Ok(())
        }
        Some("mobile-init") => {
            println!("TODO: 在 clients/app 下执行 tauri android/ios init");
            Ok(())
        }
        _ => {
            println!("usage: cargo run -p xtask -- <dist|proto|e2e|mobile-init>");
            println!("  dist [--all | --target <triple>] [--bins a,b,c] [--no-build]");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- dist ----

fn dist(args: &[String]) -> Result<()> {
    let mut targets: Vec<String> = Vec::new();
    let mut bins: Vec<String> = DEFAULT_BINS.iter().map(|s| s.to_string()).collect();
    let mut no_build = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--all" => targets = ALL_TARGETS.iter().map(|s| s.to_string()).collect(),
            "--target" => {
                targets.push(it.next().context("--target 需要一个 triple 参数")?.clone())
            }
            "--bins" => {
                bins = it
                    .next()
                    .context("--bins 需要逗号分隔的二进制名")?
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            "--no-build" => no_build = true,
            other => bail!("未知参数: {other}"),
        }
    }
    if targets.is_empty() {
        targets.push(host_triple()?); // 缺省：当前主机平台
    }

    let root = workspace_root();
    let dist_dir = root.join("dist");
    fs::create_dir_all(&dist_dir)?;
    let target_dir = cargo_target_dir(&root)?;
    let version = env!("CARGO_PKG_VERSION"); // 与 workspace 同源

    let mut outputs = Vec::new();
    for triple in &targets {
        let label = platform_label(triple)?;
        println!("\n==== [{label}] {triple} ====");
        if !no_build {
            build(&root, triple, &bins)?;
        }
        let zip_path = package(&root, &target_dir, triple, label, version, &bins, &dist_dir)?;
        outputs.push(zip_path);
    }

    println!("\n==> 发布包就绪：");
    for p in &outputs {
        let mb = fs::metadata(p).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0);
        println!("    {} ({mb:.1} MB)", p.display());
    }
    Ok(())
}

/// 编译一个目标。工具链选择：
/// - `*-apple-*`：原生 `cargo build`（仅当前 mac 架构或同厂交叉）。
/// - `*-windows-gnu`：优先 `cargo-zigbuild`；否则回退 mingw-w64（`cargo build` + 指定 linker/CC）。
/// - 其余（如 linux musl）：`cargo-zigbuild`。
fn build(root: &Path, triple: &str, bins: &[String]) -> Result<()> {
    // rustup target add（幂等）
    run(Command::new("rustup").args(["target", "add", triple]).current_dir(root))?;

    let is_apple = triple.contains("apple");
    let is_win_gnu = triple.contains("windows-gnu");
    let have_zig = has_cmd("cargo-zigbuild");

    // 选定子命令与（mingw 回退时）额外环境变量。
    let (sub, envs): (&str, Vec<(String, String)>) = if is_apple {
        ("build", vec![])
    } else if is_win_gnu && !have_zig {
        // mingw-w64 回退：需 x86_64-w64-mingw32-gcc（ring 的 C 代码 + 链接）。
        let gcc = mingw_prefix(triple);
        if !has_cmd(&format!("{gcc}-gcc")) {
            bail!(
                "目标 {triple} 需要 cargo-zigbuild 或 mingw-w64：\n  \
                 brew install cargo-zigbuild   # 推荐（zig 一套搞定 musl+windows）\n  \
                 brew install mingw-w64        # 仅 windows-gnu"
            );
        }
        let up = triple.to_uppercase().replace('-', "_"); // X86_64_PC_WINDOWS_GNU
        (
            "build",
            vec![
                (format!("CARGO_TARGET_{up}_LINKER"), format!("{gcc}-gcc")),
                (format!("CC_{}", triple.replace('-', "_")), format!("{gcc}-gcc")),
                (format!("AR_{}", triple.replace('-', "_")), format!("{gcc}-ar")),
            ],
        )
    } else {
        if !have_zig {
            bail!(
                "目标 {triple} 需要 cargo-zigbuild（zig 做交叉链接器）：\n  brew install zig cargo-zigbuild"
            );
        }
        ("zigbuild", vec![])
    };

    let mut cmd = Command::new("cargo");
    cmd.arg(sub).arg("--release").args(["--target", triple]).current_dir(root);
    for (k, v) in &envs {
        cmd.env(k, v);
    }
    for b in bins {
        cmd.args(["-p", b]);
    }
    let tool = if envs.is_empty() { sub.to_string() } else { format!("{sub} (mingw)") };
    println!("$ cargo {tool} --release --target {triple} -p {}", bins.join(" -p "));
    run(&mut cmd)
}

/// windows-gnu triple → mingw 交叉工具前缀。
fn mingw_prefix(triple: &str) -> &'static str {
    if triple.starts_with("aarch64") {
        "aarch64-w64-mingw32"
    } else {
        "x86_64-w64-mingw32"
    }
}

/// 组装 stage 目录并压成 zip（顶层带同名文件夹，解压即得整目录）。
fn package(
    root: &Path,
    target_dir: &Path,
    triple: &str,
    label: &str,
    version: &str,
    bins: &[String],
    dist_dir: &Path,
) -> Result<PathBuf> {
    let name = format!("nanomesh-{version}-{label}");
    let stage = dist_dir.join("stage").join(&name);
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(stage.join("bin"))?;

    // 1) 二进制
    let is_win = triple.contains("windows");
    let release = target_dir.join(triple).join("release");
    for b in bins {
        let file = if is_win { format!("{b}.exe") } else { b.clone() };
        let src = release.join(&file);
        if !src.exists() {
            bail!("缺少产物 {}（先去掉 --no-build 或检查构建日志）", src.display());
        }
        fs::copy(&src, stage.join("bin").join(&file))?;
    }

    // 2) 脚本 + 配置模板 + README（按平台挑选）
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let common = ["nmd.toml.example", "README.md"];
    let unix_sh = ["start-all.sh", "stop-all.sh", "restart-all.sh"];
    let win_sh = [
        "start-all.ps1", "stop-all.ps1", "restart-all.ps1",
        "start-all.bat", "stop-all.bat", "restart-all.bat",
    ];
    let picked: Vec<&str> = common
        .iter()
        .chain(if is_win { win_sh.iter() } else { unix_sh.iter() })
        .copied()
        .collect();
    for f in &picked {
        fs::copy(assets.join(f), stage.join(f))
            .with_context(|| format!("缺少资产 xtask/assets/{f}"))?;
    }
    // 附上许可证
    let _ = fs::copy(root.join("LICENSE"), stage.join("LICENSE"));

    // 3) zip（bin/* 与 *.sh 置 0755）
    let zip_path = dist_dir.join(format!("{name}.zip"));
    let _ = fs::remove_file(&zip_path);
    let file = fs::File::create(&zip_path)?;
    let mut zw = zip::ZipWriter::new(file);
    let deflate = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    add_dir_recursive(&mut zw, &stage, &name, deflate)?;
    zw.finish()?;
    println!("打包完成: {}", zip_path.display());
    Ok(zip_path)
}

/// 递归把 stage 目录写入 zip，zip 内前缀为发布包目录名。
fn add_dir_recursive(
    zw: &mut zip::ZipWriter<fs::File>,
    dir: &Path,
    prefix: &str,
    opts: zip::write::SimpleFileOptions,
) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let zip_name = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        if path.is_dir() {
            zw.add_directory(&zip_name, opts)?;
            add_dir_recursive(zw, &path, &zip_name, opts)?;
        } else {
            let executable = zip_name.contains("/bin/") || zip_name.ends_with(".sh");
            let mode = if executable { 0o755 } else { 0o644 };
            zw.start_file(&zip_name, opts.unix_permissions(mode))?;
            zw.write_all(&fs::read(&path)?)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- util ----

/// triple → 发布包平台标签。
fn platform_label(triple: &str) -> Result<&'static str> {
    Ok(match triple {
        "aarch64-apple-darwin" => "macos-arm64",
        "x86_64-apple-darwin" => "macos-x64",
        "x86_64-unknown-linux-musl" | "x86_64-unknown-linux-gnu" => "linux-x64",
        "aarch64-unknown-linux-musl" | "aarch64-unknown-linux-gnu" => "linux-arm64",
        "x86_64-pc-windows-gnu" | "x86_64-pc-windows-msvc" => "windows-x64",
        other => bail!("未收录的 triple：{other}（在 platform_label 里加一行即可）"),
    })
}

fn workspace_root() -> PathBuf {
    // xtask 的 manifest 在 <root>/xtask/
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn host_triple() -> Result<String> {
    let out = Command::new("rustc").arg("-vV").output().context("运行 rustc -vV 失败")?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_string))
        .context("rustc -vV 未输出 host")
}

/// 解析 cargo 目标目录（尊重共享 target / CARGO_TARGET_DIR）。
fn cargo_target_dir(root: &Path) -> Result<PathBuf> {
    if let Ok(d) = std::env::var("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(d));
    }
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    let key = "\"target_directory\":\"";
    let start = s.find(key).context("cargo metadata 里未找到 target_directory")? + key.len();
    let end = s[start..].find('"').context("target_directory 解析失败")? + start;
    Ok(PathBuf::from(&s[start..end]))
}

fn has_cmd(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().with_context(|| format!("无法执行 {cmd:?}"))?;
    if !status.success() {
        bail!("命令失败（{status}）：{cmd:?}");
    }
    Ok(())
}

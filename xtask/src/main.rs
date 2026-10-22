//! `xtask` — 开发者任务编排。
//! 用法：`cargo run -p xtask -- <task>`
//!
//! - `dist`：后台打包发布——交叉编译 + 组装解压即用的 zip（bin + 启停脚本 + 配置模板 + README）。
//!   `cargo xtask dist [--all | --target <triple>] [--bins a,b,c] [--no-build]`
//!   mac 目标用原生 cargo；linux(musl)/windows(gnu) 用 cargo-zigbuild（`brew install zig cargo-zigbuild`）。

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

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
        Some("soak") => soak(&args[1..]),
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
            println!("usage: cargo run -p xtask -- <dist|soak|proto|e2e|mobile-init>");
            println!("  dist [--all | --target <triple>] [--bins a,b,c] [--no-build]");
            println!(
                "  soak [--nodes N] [--duration SECS] [--scenario lan|nat|selfhost|restart|churn|partition|mixed] [--plan-only] [--no-build]"
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- soak ----

struct SoakCfg {
    nodes: usize,
    duration_secs: u64,
    scenario: String,
    base_port: u16,
    admin_base_port: u16,
    seed_peers: usize,
    plan_only: bool,
    no_build: bool,
    relay_url: Option<String>,
    dns_url: Option<String>,
}

struct NodePlan {
    index: usize,
    dir: PathBuf,
    config: PathBuf,
    id: String,
    admin_addr: String,
    admin_token: String,
}

fn soak(args: &[String]) -> Result<()> {
    let cfg = parse_soak(args)?;
    if cfg.nodes == 0 {
        bail!("--nodes 必须大于 0");
    }
    if cfg.nodes > 10_000 {
        bail!("--nodes 过大：当前保护上限 10000");
    }
    if cfg.scenario == "selfhost"
        && !cfg.plan_only
        && (cfg.relay_url.is_none() || cfg.dns_url.is_none())
    {
        bail!("scenario=selfhost 实跑需要 --relay-url 与 --dns-url");
    }

    let root = workspace_root();
    let target_dir = cargo_target_dir(&root)?;
    let run_id = format!("{}", unix_secs()?);
    let run_dir = target_dir.join("soak").join(&run_id);
    fs::create_dir_all(&run_dir)?;

    let plans = write_soak_layout(&cfg, &run_dir)?;
    write_soak_fault_plan(&cfg, &run_dir)?;
    write_soak_summary(&cfg, &run_dir, &plans, "planned", 0, 0)?;

    println!("soak run dir: {}", run_dir.display());
    println!(
        "nodes: {}, scenario: {}, duration: {}s",
        cfg.nodes, cfg.scenario, cfg.duration_secs
    );
    if cfg.plan_only {
        println!("plan-only 完成：已生成配置、身份、故障计划与 soak-summary.json");
        return Ok(());
    }

    if !cfg.no_build {
        println!("$ cargo build -p nmd");
        run(Command::new("cargo")
            .args(["build", "-p", "nmd"])
            .current_dir(&root))?;
    }
    let nmd = debug_bin(&target_dir, "nmd");
    if !nmd.exists() {
        bail!(
            "缺少 {}：请去掉 --no-build 或先 cargo build -p nmd",
            nmd.display()
        );
    }

    let mut children: Vec<Option<Child>> = Vec::with_capacity(plans.len());
    for plan in &plans {
        children.push(Some(spawn_soak_node(plan, &nmd)?));
        thread::sleep(Duration::from_millis(25));
    }

    let (mut restarts, mut partitions) = (0usize, 0usize);
    let deadline = SystemTime::now() + Duration::from_secs(cfg.duration_secs);
    let mut tick = 0usize;
    while SystemTime::now() < deadline {
        thread::sleep(Duration::from_secs(5));
        tick += 1;
        reap_dead(&mut children);
        match cfg.scenario.as_str() {
            "restart" => {
                restart_one(&plans, &nmd, &mut children, tick % cfg.nodes)?;
                restarts += 1;
            }
            "churn" => {
                let idx = tick % cfg.nodes;
                if tick.is_multiple_of(2) {
                    stop_one(&mut children, idx);
                } else if children[idx].is_none() {
                    children[idx] = Some(spawn_soak_node(&plans[idx], &nmd)?);
                    restarts += 1;
                }
            }
            "partition" => {
                let third = (cfg.duration_secs / 3).max(5);
                let elapsed = tick as u64 * 5;
                if elapsed >= third && elapsed < third + 5 {
                    stop_partition(&mut children, cfg.nodes / 2);
                    partitions += 1;
                } else if elapsed >= third * 2 && elapsed < third * 2 + 5 {
                    for i in cfg.nodes / 2..cfg.nodes {
                        if children[i].is_none() {
                            children[i] = Some(spawn_soak_node(&plans[i], &nmd)?);
                            restarts += 1;
                        }
                    }
                }
            }
            "mixed" => {
                if tick.is_multiple_of(3) {
                    restart_one(&plans, &nmd, &mut children, tick % cfg.nodes)?;
                    restarts += 1;
                }
                if tick == 3 {
                    stop_partition(&mut children, cfg.nodes / 3);
                    partitions += 1;
                }
                if tick == 6 {
                    for i in cfg.nodes / 3..cfg.nodes {
                        if children[i].is_none() {
                            children[i] = Some(spawn_soak_node(&plans[i], &nmd)?);
                            restarts += 1;
                        }
                    }
                }
            }
            "lan" | "nat" | "selfhost" => {}
            other => bail!("未知 scenario: {other}"),
        }
    }

    let mut alive = 0usize;
    for child in &mut children {
        if child_alive(child) {
            alive += 1;
        }
    }
    stop_all(&mut children);
    write_soak_summary(
        &cfg,
        &run_dir,
        &plans,
        "completed",
        alive,
        restarts + partitions,
    )?;
    println!("soak 完成：alive_at_end={alive}, restarts={restarts}, partitions={partitions}");
    Ok(())
}

fn parse_soak(args: &[String]) -> Result<SoakCfg> {
    let mut cfg = SoakCfg {
        nodes: 100,
        duration_secs: 60,
        scenario: "nat".to_string(),
        base_port: 41_000,
        admin_base_port: 51_000,
        seed_peers: 3,
        plan_only: false,
        no_build: false,
        relay_url: None,
        dns_url: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--nodes" => cfg.nodes = it.next().context("--nodes 需要数值")?.parse()?,
            "--duration" => {
                cfg.duration_secs = it.next().context("--duration 需要秒数")?.parse()?
            }
            "--scenario" => cfg.scenario = it.next().context("--scenario 需要名称")?.to_string(),
            "--base-port" => cfg.base_port = it.next().context("--base-port 需要端口")?.parse()?,
            "--admin-base-port" => {
                cfg.admin_base_port = it.next().context("--admin-base-port 需要端口")?.parse()?
            }
            "--seed-peers" => {
                cfg.seed_peers = it.next().context("--seed-peers 需要数值")?.parse()?
            }
            "--relay-url" => {
                cfg.relay_url = Some(it.next().context("--relay-url 需要 URL")?.to_string())
            }
            "--dns-url" => cfg.dns_url = Some(it.next().context("--dns-url 需要 URL")?.to_string()),
            "--plan-only" => cfg.plan_only = true,
            "--no-build" => cfg.no_build = true,
            other => bail!("未知 soak 参数: {other}"),
        }
    }
    Ok(cfg)
}

fn write_soak_layout(cfg: &SoakCfg, run_dir: &Path) -> Result<Vec<NodePlan>> {
    let mut ids = Vec::with_capacity(cfg.nodes);
    for i in 0..cfg.nodes {
        let seed = soak_seed(i);
        let sk = nm_transport::SecretKey::from_bytes(&seed);
        ids.push(bytes_hex(sk.public().as_bytes()));
    }

    let mut plans = Vec::with_capacity(cfg.nodes);
    let mode = match cfg.scenario.as_str() {
        "lan" => "lan",
        "selfhost" => "selfhost",
        "nat" | "restart" | "churn" | "partition" | "mixed" => "nat",
        other => bail!("未知 scenario: {other}"),
    };
    let federation = format!("soak-{}", run_dir.file_name().unwrap().to_string_lossy());

    for i in 0..cfg.nodes {
        let dir = run_dir.join(format!("node-{i:04}"));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("nmd.identity"), soak_seed(i))?;
        let admin_port = checked_port(cfg.admin_base_port, i)?;
        let bind_port = checked_port(cfg.base_port, i)?;
        let admin_addr = format!("127.0.0.1:{admin_port}");
        let admin_token = format!("soak-token-{i:04}");
        let mut toml = String::new();
        toml.push_str(&format!(
            "mode = \"{mode}\"\nidentity = \"nmd.identity\"\ndb = \"nmd.redb\"\nbind_port = {bind_port}\n\n"
        ));
        for peer in seed_peer_ids(&ids, i, cfg.seed_peers) {
            toml.push_str("[[peers]]\n");
            toml.push_str(&format!("id = \"{peer}\"\n\n"));
        }
        if mode == "selfhost" {
            if let Some(relay) = &cfg.relay_url {
                toml.push_str("[relay]\n");
                toml.push_str(&format!("urls = [\"{}\"]\n", toml_escape(relay)));
            }
            if let Some(dns) = &cfg.dns_url {
                toml.push_str("[dns]\n");
                toml.push_str(&format!("url = \"{}\"\n", toml_escape(dns)));
            }
            toml.push('\n');
        }
        toml.push_str("[admin]\n");
        toml.push_str(&format!(
            "api_addr = \"{admin_addr}\"\napi_token = \"{admin_token}\"\n\n"
        ));
        toml.push_str("[membership]\n");
        toml.push_str(&format!(
            "federation = \"{federation}\"\nname = \"soak-{i:04}\"\nannounce_interval_secs = 5\nttl_secs = 20\n\n"
        ));
        toml.push_str("[federation]\nper_topic = true\npresence_scoped = true\n\n");
        toml.push_str("[limits]\nmax_connections = 20000\n");
        let config = dir.join("nmd.toml");
        fs::write(&config, toml)?;
        plans.push(NodePlan {
            index: i,
            dir,
            config,
            id: ids[i].clone(),
            admin_addr,
            admin_token,
        });
    }
    Ok(plans)
}

fn seed_peer_ids(ids: &[String], self_index: usize, count: usize) -> Vec<String> {
    let mut out = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        if i != self_index && out.len() < count {
            out.push(id.clone());
        }
    }
    out
}

fn write_soak_fault_plan(cfg: &SoakCfg, run_dir: &Path) -> Result<()> {
    let mut s = String::new();
    s.push_str("# Nano Mesh Soak Fault Plan\n\n");
    s.push_str(&format!(
        "- nodes: {}\n- duration_secs: {}\n- scenario: {}\n\n",
        cfg.nodes, cfg.duration_secs, cfg.scenario
    ));
    s.push_str("## Built-in Faults\n\n");
    s.push_str("- `restart`: every 5s one node is killed and restarted.\n");
    s.push_str("- `churn`: rotating nodes leave and rejoin every 5s.\n");
    s.push_str(
        "- `partition`: the upper half of nodes is stopped for the middle third of the run.\n",
    );
    s.push_str("- `mixed`: combines rotating restarts with a short partial partition.\n\n");
    s.push_str("## Network Jitter / Packet Loss\n\n");
    s.push_str("Use OS-level netem/pf around this generated layout for true latency/loss injection. Keep nmd configs unchanged; target the UDP bind port range shown in `soak-summary.json`.\n");
    fs::write(run_dir.join("FAULT_PLAN.md"), s)?;
    Ok(())
}

fn write_soak_summary(
    cfg: &SoakCfg,
    run_dir: &Path,
    plans: &[NodePlan],
    status: &str,
    alive_at_end: usize,
    faults_injected: usize,
) -> Result<()> {
    let mut s = String::new();
    s.push_str("{\n");
    s.push_str(&format!("  \"status\": \"{}\",\n", json_escape(status)));
    s.push_str(&format!(
        "  \"scenario\": \"{}\",\n",
        json_escape(&cfg.scenario)
    ));
    s.push_str(&format!("  \"nodes\": {},\n", cfg.nodes));
    s.push_str(&format!("  \"duration_secs\": {},\n", cfg.duration_secs));
    s.push_str(&format!("  \"base_port\": {},\n", cfg.base_port));
    s.push_str(&format!(
        "  \"admin_base_port\": {},\n",
        cfg.admin_base_port
    ));
    s.push_str(&format!("  \"alive_at_end\": {},\n", alive_at_end));
    s.push_str(&format!("  \"faults_injected\": {},\n", faults_injected));
    s.push_str("  \"node_sample\": [\n");
    for (n, plan) in plans.iter().take(10).enumerate() {
        let comma = if n + 1 == plans.len().min(10) {
            ""
        } else {
            ","
        };
        s.push_str(&format!(
            "    {{ \"index\": {}, \"id\": \"{}\", \"admin\": \"{}\", \"token\": \"{}\" }}{}\n",
            plan.index,
            json_escape(&plan.id),
            json_escape(&plan.admin_addr),
            json_escape(&plan.admin_token),
            comma
        ));
    }
    s.push_str("  ]\n}\n");
    fs::write(run_dir.join("soak-summary.json"), s)?;
    Ok(())
}

fn spawn_soak_node(plan: &NodePlan, nmd: &Path) -> Result<Child> {
    let stdout = fs::File::create(plan.dir.join("stdout.log"))?;
    let stderr = fs::File::create(plan.dir.join("stderr.log"))?;
    Command::new(nmd)
        .arg("--config")
        .arg(&plan.config)
        .current_dir(&plan.dir)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| format!("启动 node-{:04} 失败", plan.index))
}

fn restart_one(
    plans: &[NodePlan],
    nmd: &Path,
    children: &mut [Option<Child>],
    idx: usize,
) -> Result<()> {
    stop_one(children, idx);
    children[idx] = Some(spawn_soak_node(&plans[idx], nmd)?);
    Ok(())
}

fn stop_partition(children: &mut [Option<Child>], keep_first: usize) {
    for idx in keep_first..children.len() {
        stop_one(children, idx);
    }
}

fn stop_one(children: &mut [Option<Child>], idx: usize) {
    if let Some(mut child) = children[idx].take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn stop_all(children: &mut [Option<Child>]) {
    for idx in 0..children.len() {
        stop_one(children, idx);
    }
}

fn reap_dead(children: &mut [Option<Child>]) {
    for slot in children {
        if let Some(child) = slot
            && matches!(child.try_wait(), Ok(Some(_)))
        {
            *slot = None;
        }
    }
}

fn child_alive(slot: &mut Option<Child>) -> bool {
    match slot {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    }
}

fn soak_seed(index: usize) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut x = 0x9e37_79b9_7f4a_7c15u64 ^ index as u64;
    for chunk in out.chunks_mut(8) {
        x = splitmix64(x);
        chunk.copy_from_slice(&x.to_le_bytes());
    }
    out
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn checked_port(base: u16, offset: usize) -> Result<u16> {
    let p = base as usize + offset;
    if p > u16::MAX as usize {
        bail!("端口范围溢出：base={base}, offset={offset}");
    }
    Ok(p as u16)
}

fn debug_bin(target_dir: &Path, name: &str) -> PathBuf {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    target_dir.join("debug").join(exe)
}

fn bytes_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn json_escape(s: &str) -> String {
    toml_escape(s)
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn unix_secs() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
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
            "--target" => targets.push(it.next().context("--target 需要一个 triple 参数")?.clone()),
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
    run(Command::new("rustup")
        .args(["target", "add", triple])
        .current_dir(root))?;

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
                (
                    format!("CC_{}", triple.replace('-', "_")),
                    format!("{gcc}-gcc"),
                ),
                (
                    format!("AR_{}", triple.replace('-', "_")),
                    format!("{gcc}-ar"),
                ),
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
    cmd.arg(sub)
        .arg("--release")
        .args(["--target", triple])
        .current_dir(root);
    for (k, v) in &envs {
        cmd.env(k, v);
    }
    for b in bins {
        cmd.args(["-p", b]);
    }
    let tool = if envs.is_empty() {
        sub.to_string()
    } else {
        format!("{sub} (mingw)")
    };
    println!(
        "$ cargo {tool} --release --target {triple} -p {}",
        bins.join(" -p ")
    );
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
        let file = if is_win {
            format!("{b}.exe")
        } else {
            b.clone()
        };
        let src = release.join(&file);
        if !src.exists() {
            bail!(
                "缺少产物 {}（先去掉 --no-build 或检查构建日志）",
                src.display()
            );
        }
        fs::copy(&src, stage.join("bin").join(&file))?;
    }

    // 2) 脚本 + 配置模板 + README（按平台挑选）
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let common = ["nmd.toml.example", "README.md"];
    let unix_sh = ["start-all.sh", "stop-all.sh", "restart-all.sh"];
    let win_sh = [
        "start-all.ps1",
        "stop-all.ps1",
        "restart-all.ps1",
        "start-all.bat",
        "stop-all.bat",
        "restart-all.bat",
    ];
    let picked: Vec<&str> = common
        .iter()
        .chain(if is_win {
            win_sh.iter()
        } else {
            unix_sh.iter()
        })
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
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn host_triple() -> Result<String> {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .context("运行 rustc -vV 失败")?;
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
    let start = s
        .find(key)
        .context("cargo metadata 里未找到 target_directory")?
        + key.len();
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

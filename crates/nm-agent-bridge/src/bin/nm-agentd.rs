//! `nm-agentd` —— 在一个 nmspace 节点上**真跑一个 cmx-agent 智能体**：它的模型能力经 P2P 取得。
//!
//! 链路：连节点 → 发现 `model.*` provider（C2）→ `SessionInferChannel`→`P2pModelSeam`（A0/A1）
//! → `cmx_agent_core::Agent` → 跑回合。内核零改，模型调用全走 P2P。
//!
//! 用法（三进程本地联调）：
//!   1) `cargo run -p nmd`                                   # 打印 NM_NODE_ADDR=...
//!   2) 起一个模型 provider，如 `cargo run -p nm-compute -- --node '<ADDR>' --backend demo --model demo-llm`
//!   3) cargo run --manifest-path crates/nm-agent-bridge/Cargo.toml --features nm-client --bin nm-agentd -- \
//!        --node '<ADDR>' --model demo-llm --prompt "你好"
//!      省略 `--prompt` 则进入交互 REPL（空行/EOF 退出）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use cmx_agent_core::{Agent, ModelSeam, Session, StopReason};
use nm_agent_bridge::{P2pModelSeam, SessionInferChannel};
use nm_entity::kinds;
use nm_proto::pb::AgentProfile;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
#[command(name = "nm-agentd", about = "在节点上真跑一个 cmx-agent 智能体（模型走 P2P）")]
struct Args {
    /// 节点地址（nmd 启动时打印的 NM_NODE_ADDR JSON）。
    #[arg(long)]
    node: String,
    /// 持久化身份文件路径（首次运行时生成随机密钥并落盘；后续重启使用同一密钥 = 同一 entity_id）。
    /// 省略则退回 --seed（仅用于开发/测试；任何知道 seed 值的人都能冒充这个 agent）。
    #[arg(long)]
    identity_file: Option<String>,
    /// 开发/测试用种子（0-255），决定本 agent 的公钥。不提供 --identity-file 时生效。
    #[arg(long, default_value_t = 210)]
    seed: u8,
    /// 要使用的模型名（directory 过滤 + 请求体 model 字段）。
    #[arg(long, default_value = "demo-llm")]
    model: String,
    /// 展示名（directory 里的 agent 名）。
    #[arg(long, default_value = "nmspace-agent")]
    name: String,
    /// 可选系统提示（AGENTS.md 式项目指令）。
    #[arg(long, default_value = "")]
    system: String,
    /// 一次性：跑一个回合即退出；省略则进入交互 REPL（除非 --serve）。
    #[arg(long)]
    prompt: Option<String>,
    /// bot 模式：注册后收 P2P DM，每条交给 agent 回合处理并回包（服务他人）。与 --prompt/REPL 互斥。
    #[arg(long, default_value_t = false)]
    serve: bool,
}

/// 完整 64 hex entity_id。
fn hex_full(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 加载或生成持久化身份密钥（32 字节原始 ed25519 seed）。
/// 文件不存在 → 随机生成并写入；已存在 → 读回。
/// 文件格式：64 个 ASCII hex 字符，一行。
fn load_or_create_identity(path: &str) -> anyhow::Result<[u8; 32]> {
    use std::io::{Read, Write};
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        let hex = s.trim();
        anyhow::ensure!(hex.len() == 64, "身份文件应含 64 hex 字符，实际 {} 字符", hex.len());
        let bytes: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16))
            .collect::<Result<_, _>>()?;
        Ok(bytes.try_into().unwrap())
    } else {
        // 首次：生成随机 32 字节种子。
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).map_err(|e| anyhow::anyhow!("随机数生成失败: {e}"))?;
        let hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
        // 若父目录不存在，先建
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut f = std::fs::File::create(path)
            .map_err(|e| anyhow::anyhow!("创建身份文件 {path} 失败: {e}"))?;
        writeln!(f, "{hex}")?;
        eprintln!("nm-agentd: 新身份已生成并保存至 {path}");
        Ok(seed)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();

    // 身份：优先持久化文件，回退 seed。
    let seed: [u8; 32] = match &args.identity_file {
        Some(path) => load_or_create_identity(path)?,
        None => [args.seed; 32],
    };

    // 连节点。client 必须存活到进程结束——Session 只持 conn，client 持 iroh endpoint。
    let addr = nm_transport::addr_from_string(&args.node)?;
    let client = nm_client::Client::bind_local(seed).await?;
    let p2p = client.online(addr).await?;

    // 注册为 agent.assistant（presence；backend 非空以过 validate）。
    let mut attrs = HashMap::new();
    attrs.insert("model".to_string(), args.model.clone());
    p2p.register_as::<kinds::AgentAssistant>(
        &AgentProfile { backend: "nmspace-p2p".into(), ..Default::default() },
        &args.name,
        attrs,
    )
    .await?;

    // 发现模型 provider。
    let providers = p2p.find_model_providers(Some(&args.model)).await?;
    let provider = providers.first().ok_or_else(|| {
        anyhow::anyhow!(
            "网络里没发现 model={} 的 provider；先起一个，如：\n  \
             cargo run -p nm-compute -- --node '<ADDR>' --backend demo --model {}",
            args.model,
            args.model
        )
    })?;
    let provider_id: [u8; 32] = provider
        .entity_id
        .clone()
        .try_into()
        .map_err(|_| anyhow::anyhow!("provider entity_id 非 32 字节"))?;
    eprintln!(
        "nm-agentd: id={} 已上线；发现 provider {}（model={}）",
        hex_full(&client.id_bytes()),
        hex_full(&provider_id),
        args.model
    );

    // serve 模式：把本进程变成服务他人的 P2P agent bot（收 DM → 回合 → 回包）。
    let sys_opt = if args.system.is_empty() { None } else { Some(args.system.as_str()) };
    if args.serve {
        eprintln!(
            "nm-agentd: serve 模式已就绪\n  entity_id (完整): {}\n  别人向此 id 发消息即可（Ctrl-C 退出）。",
            hex_full(&client.id_bytes())
        );
        nm_agent_bridge::serve::serve(p2p, provider_id, &args.model, sys_opt)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        return Ok(());
    }

    // 本地模式：组装 P2P 模型缝（driver 独占 p2p 会话）→ cmx-agent Agent（工具/守卫/审批走内核默认）。
    let channel = SessionInferChannel::spawn(p2p, provider_id);
    let seam = P2pModelSeam::new(channel, &args.model);
    let agent = Agent::builder()
        .model(Arc::new(seam) as Arc<dyn ModelSeam>)
        .build()
        .map_err(|e| anyhow::anyhow!("构建 agent 失败: {e}"))?;

    // 一问一答的会话（纯文本回合，模型无 tool_calls → 一步完成）。
    let mut convo = Session::new("nm-agentd");
    if !args.system.is_empty() {
        convo = convo.with_system(args.system.clone());
    }

    match args.prompt {
        Some(prompt) => run_one(&agent, &mut convo, &prompt).await?,
        None => {
            eprintln!("nm-agentd: 进入交互（空行 / Ctrl-D 退出）。");
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            loop {
                eprint!("» ");
                let Some(line) = lines.next_line().await? else { break };
                let line = line.trim();
                if line.is_empty() {
                    break;
                }
                run_one(&agent, &mut convo, line).await?;
            }
        }
    }
    Ok(())
}

/// 跑一个回合：把输入交给 agent（模型经 P2P），打印助手最终文本。
async fn run_one(agent: &Agent, convo: &mut Session, input: &str) -> anyhow::Result<()> {
    let out = tokio::time::timeout(Duration::from_secs(60), agent.run_turn(convo, input))
        .await
        .map_err(|_| anyhow::anyhow!("回合超时（provider 无响应？）"))?
        .map_err(|e| anyhow::anyhow!("run_turn 失败: {e}"))?;
    println!("{}", out.final_text.unwrap_or_default());
    if out.reason != StopReason::Completed {
        eprintln!("[stop={:?} steps={}]", out.reason, out.steps);
    }
    Ok(())
}

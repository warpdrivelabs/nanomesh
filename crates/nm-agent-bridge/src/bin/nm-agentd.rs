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
    /// 自建中继 URL（可多次）。配了 --relay 或 --pkarr 即走 selfhost（自建relay+dns），否则默认 N0。
    #[arg(long)]
    relay: Vec<String>,
    /// 自建 iroh-dns-server 的 pkarr 端点（如 https://relay.example.com:8444/pkarr）。按公钥发现 home node。
    #[arg(long)]
    pkarr: Option<String>,
    /// 可选：额外走 DNS 查询解析的源域。
    #[arg(long)]
    dns_origin: Option<String>,
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
    // 换网自愈：按 home node 公钥 id 重连（而非缓存完整地址），发现服务换网后重解析其地址。
    // 见 docs/NETWORK_ROAMING_RECOVERY_DESIGN.md §5.3/§5.4。
    let addr = nm_transport::addr_from_string(&args.node)?;
    let home_id: [u8; 32] = *addr.id.as_bytes();
    // 配了自建 relay/pkarr → selfhost（Minimal 基座 + 自建中继/发现）；否则 N0 公共基础设施。
    // 自建 home node 只把地址发布到自建 pkarr，故 agent 也须用同一套 pkarr 才能按公钥发现它。
    let client = if !args.relay.is_empty() || args.pkarr.is_some() {
        let pkarr = args.pkarr.clone().ok_or_else(|| {
            anyhow::anyhow!("--relay 需配合 --pkarr（自建 home node 经 pkarr 发现）")
        })?;
        eprintln!("[nm-agentd] selfhost: relays={:?} pkarr={}", args.relay, pkarr);
        nm_client::Client::bind_selfhosted(seed, args.relay.clone(), pkarr, args.dns_origin.clone(), 0).await?
    } else {
        // N0 预设（n0 公共中继 + DNS/pkarr 发现）：按公钥发现远程 home node、穿透 NAT。
        nm_client::Client::bind(seed).await?
    };
    client.watch_addr_log("nm-agentd"); // 换网地址变化结构化日志
    let p2p = client.online_by_id(home_id).await?;

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
    // 断线后自动重连：serve() 在 inbox channel 关闭（QUIC 连接断开）时返回，
    // 此时重新 online() + 重注册 + 重找 provider，再次进入 serve loop，直到 Ctrl-C。
    let sys_opt = if args.system.is_empty() { None } else { Some(args.system.as_str()) };
    if args.serve {
        eprintln!(
            "nm-agentd: serve 模式已就绪\n  entity_id (完整): {}\n  别人向此 id 发消息即可（Ctrl-C 退出）。",
            hex_full(&client.id_bytes())
        );
        // 首次连接已在上方建立（p2p + provider_id），直接进循环。
        // 循环结构：先 serve（消耗 session），serve 退出后重连拿到新 session，再循环顶部继续。
        let mut next_p2p = Some(p2p);
        let mut cur_provider = provider_id;
        loop {
            // 取出本轮的 session（首次来自上方建立，后续来自重连）。
            let cur_p2p = next_p2p.take().expect("next_p2p always set before loop top");
            match nm_agent_bridge::serve::serve(cur_p2p, cur_provider, &args.model, sys_opt).await {
                Ok(()) => eprintln!("nm-agentd: serve loop 退出（连接断开/换网），自动重连…"),
                Err(e) => eprintln!("nm-agentd: serve 出错: {e}，自动重连…"),
            }
            // 重连：共享原语按 home node 公钥 id 重连 + 指数退避（换网后发现服务重解析地址）。
            loop {
                let new_p2p = nm_client::online_by_id_retry(&client, home_id, "nm-agentd").await;
                let mut attrs = HashMap::new();
                attrs.insert("model".to_string(), args.model.clone());
                if let Err(e) = new_p2p.register_as::<kinds::AgentAssistant>(
                    &AgentProfile { backend: "nmspace-p2p".into(), ..Default::default() },
                    &args.name,
                    attrs,
                ).await {
                    eprintln!("nm-agentd: 重注册失败: {e}，继续等待…");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                let providers = new_p2p.find_model_providers(Some(&args.model)).await.unwrap_or_default();
                if let Some(prov) = providers.first() {
                    if let Ok(pid) = <[u8; 32]>::try_from(prov.entity_id.clone()) {
                        cur_provider = pid;
                    }
                }
                next_p2p = Some(new_p2p);
                eprintln!("nm-agentd: 重连成功，继续 serve…");
                break;
            }
        }
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

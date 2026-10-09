//! nm-agent-bridge（A0 引用打通）：nanomesh 侧实现 cmx-agent 内核的 [`ModelSeam`]，
//! 把「具体大模型调用」接到 **P2P 模型能力网络**（见 `docs/COMPUTE_MODEL_P2P_DESIGN.md` 的
//! C2 消费层）。内核零改——core 以 `trait ModelSeam` 把模型与回合循环解耦，本 crate 只提供
//! 一个「走 P2P」的 `ModelSeam` 实现。
//!
//! 分层：
//! - [`P2pInferChannel`]：把一次 **OpenAI 兼容** 请求送出去 / 取回响应的通道抽象——前门与 P2P
//!   之间的唯一耦合点。A1 由 `nm-client` 的 C2（`Session::infer`）实现它、接上真实 P2P 节点。
//! - [`P2pModelSeam`]：编解码 §13 OpenAI 载荷（`openai.chat.v1` ⇆ `openai.chat.completion.v1`），
//!   复用内核的 [`ModelContext`] / [`ModelResponse`]，实现 [`ModelSeam`]。
//!
//! A0 以测试桩（`P2pInferChannel`）验证整条缝 + 证内核三件套之 core/tools 引用打通；真实 P2P
//! 节点的端到端在 A1 接 `nm-client` 后补。

use async_trait::async_trait;
use cmx_agent_core::{ModelContext, ModelError, ModelMessage, ModelResponse, ModelSeam, ModelUsage};
use serde_json::{json, Value};

/// 送出一次 OpenAI 兼容请求（`openai.chat.v1` 字节）并取回 completion（`openai.chat.completion.v1` 字节）。
/// 这是前门 façade 与 P2P 之间的唯一耦合点——A1 以 nm-client 的 C2（`Session::infer`）实现。
#[async_trait]
pub trait P2pInferChannel: Send + Sync {
    async fn infer(&self, request_json: Vec<u8>) -> Result<Vec<u8>, String>;
}

/// 走 P2P 的 [`ModelSeam`]：`complete` = 把内核上下文编为 OpenAI 请求 → 经 [`P2pInferChannel`]
/// 发给某模型 provider → 解析 completion 回 [`ModelResponse`]。
pub struct P2pModelSeam<C: P2pInferChannel> {
    channel: C,
    model: String,
}

impl<C: P2pInferChannel> P2pModelSeam<C> {
    /// 用一个推理通道 + 目标模型名构造。`model` 写入请求体 `model` 字段（provider 侧按此路由/回填）。
    pub fn new(channel: C, model: impl Into<String>) -> Self {
        Self { channel, model: model.into() }
    }
}

#[async_trait]
impl<C: P2pInferChannel> ModelSeam for P2pModelSeam<C> {
    async fn complete(&self, ctx: &ModelContext) -> Result<ModelResponse, ModelError> {
        let req = encode_openai_request(&self.model, ctx)?;
        let resp = self.channel.infer(req).await.map_err(ModelError)?;
        decode_openai_response(&resp)
    }
}

/// 内核 [`ModelContext`] → OpenAI `/chat/completions` 请求体字节（§13：type_url=`openai.chat.v1`）。
/// 工具清单（`ctx.tools`）的 OpenAI `tools` 映射留待 A1（demo provider 忽略 tools，不影响 A0 链路）。
pub fn encode_openai_request(model: &str, ctx: &ModelContext) -> Result<Vec<u8>, ModelError> {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(sys) = ctx.system.as_deref().filter(|s| !s.is_empty()) {
        messages.push(json!({ "role": "system", "content": sys }));
    }
    for m in &ctx.messages {
        match m {
            ModelMessage::User { text, .. } => {
                messages.push(json!({ "role": "user", "content": text }));
            }
            ModelMessage::Assistant { text, .. } => {
                messages.push(json!({ "role": "assistant", "content": text.clone().unwrap_or_default() }));
            }
            ModelMessage::Tool { call_id, output } => {
                messages.push(json!({ "role": "tool", "tool_call_id": call_id, "content": output.to_string() }));
            }
        }
    }
    let body = json!({ "model": model, "messages": messages });
    serde_json::to_vec(&body).map_err(|e| ModelError(format!("编码 OpenAI 请求失败: {e}")))
}

/// OpenAI `chat.completion` 响应字节 → 内核 [`ModelResponse`]（取 `choices[0].message.content` + `usage`）。
pub fn decode_openai_response(bytes: &[u8]) -> Result<ModelResponse, ModelError> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| ModelError(format!("解析 OpenAI 响应失败: {e}")))?;
    let text = v["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string();
    let usage = v.get("usage").filter(|u| !u.is_null()).map(|u| ModelUsage {
        input: u["prompt_tokens"].as_u64().unwrap_or(0),
        output: u["completion_tokens"].as_u64().unwrap_or(0),
        cached_input: None,
    });
    Ok(ModelResponse { text: Some(text), usage, ..Default::default() })
}

/// 内置工具平面的规格清单（证 `cmx-agent-tools` 引用打通；A1 用于把工具清单随请求下发给 provider）。
pub fn builtin_tool_specs() -> Vec<cmx_agent_core::ToolSpec> {
    cmx_agent_tools::default_registry().specs()
}

/// A1：经 nm-client 的 C2（`Session::infer`）走**真实 P2P 节点**的 [`P2pInferChannel`] 实现。
///
/// `nm_client::Session` 非 `Sync`（内部持 `mpsc::UnboundedReceiver`），不能直接塞进要求 `Send+Sync`
/// 的 channel。故用一个 **driver task** 独占持有 `Session`，经 `mpsc` 收推理作业、`oneshot` 回结果；
/// [`SessionInferChannel`] 只持 `mpsc::Sender`（`Send+Sync`），恰好满足 `P2pInferChannel: Send+Sync`。
#[cfg(feature = "nm-client")]
pub mod p2p {
    use super::P2pInferChannel;
    use async_trait::async_trait;
    use tokio::sync::{mpsc, oneshot};

    struct InferJob {
        request: Vec<u8>,
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    }

    /// 经 nm-client C2 走真实 P2P 的推理通道。用 [`SessionInferChannel::spawn`] 构造（须在 tokio 运行时内调用）。
    pub struct SessionInferChannel {
        tx: mpsc::Sender<InferJob>,
    }

    impl SessionInferChannel {
        /// 用一个**已在线**的 `Session` + 目标 provider id 起 driver task。
        /// driver 独占 `session`（move 进单任务，`Session: Send` 足矣），逐个作业调用 `session.infer`。
        /// 返回的句柄 `Send+Sync`，可作 [`P2pInferChannel`] 注入 [`super::P2pModelSeam`]。
        pub fn spawn(session: nm_client::Session, provider: [u8; 32]) -> Self {
            let (tx, mut rx) = mpsc::channel::<InferJob>(16);
            tokio::spawn(async move {
                while let Some(job) = rx.recv().await {
                    let r = session
                        .infer(provider, &job.request)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = job.reply.send(r);
                }
            });
            Self { tx }
        }
    }

    #[async_trait]
    impl P2pInferChannel for SessionInferChannel {
        async fn infer(&self, request_json: Vec<u8>) -> Result<Vec<u8>, String> {
            let (rtx, rrx) = oneshot::channel();
            self.tx
                .send(InferJob { request: request_json, reply: rtx })
                .await
                .map_err(|_| "P2P 推理通道已关闭（driver task 结束）".to_string())?;
            rrx.await.map_err(|_| "P2P 推理通道未回结果（reply 被丢弃）".to_string())?
        }
    }
}

#[cfg(feature = "nm-client")]
pub use p2p::SessionInferChannel;

/// ①：把 agent 作为 **P2P bot** 跑在节点上，服务他人——注册后收 DM，每条交给 cmx-agent 回合处理，
/// 结果回包给发送方。模型调用仍走 P2P。
///
/// 单一 `Session` 要同时：收 DM、回消息、调模型 provider（infer）——而 `Session` 非 `Sync`，
/// 且回合内会再调 infer。解法：
/// - **io task** 独占 `Session`，只服务统一的 [`AgentOp`]（`Infer` / `SendDelta`）队列；
/// - **serve loop** 用 `take_inbox()` 拿到的收件流 + 每对端一个 cmx 会话，**内联**跑回合——回合的
///   `infer` 发到 io task（**另一个** task），故不自锁；回复经 `SendDelta` 分块流式发回（打字机）。
///
/// v1 串行（一次一个回合）：模型时延期间不取下一条（收件箱无界会缓冲）。并发（每消息 spawn + 每对端
/// 会话加锁）留待后续。
#[cfg(feature = "nm-client")]
pub mod serve {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use cmx_agent_core::{Agent, ModelSeam, Session as Convo};
    use nm_proto::GramKind;
    use tokio::sync::{mpsc, oneshot};

    use super::{P2pInferChannel, P2pModelSeam};

    /// agent→对端**流式回复**的载荷类型：app 据此把 delta 帧增量长成**同一个**气泡（而非每帧新开消息）。
    /// 帧体 JSON：`{streamId, seq, text(累计全文), done}`。每帧带累计全文 → 乱序/丢帧自愈、done 帧收敛正确。
    pub const AGENT_DELTA_TYPE_URL: &str = "nmspace.agent.delta.v1";

    /// io task 的统一作业：向 provider 发一次推理，或给某对端发一帧流式回复。
    enum AgentOp {
        Infer { request: Vec<u8>, reply: oneshot::Sender<Result<Vec<u8>, String>> },
        SendDelta { to: [u8; 32], body: Vec<u8> },
    }

    /// `P2pInferChannel`：把推理作业投递给 io task（而非自己持 `Session`——`Session` 由 io task 独占）。
    struct OpInferChannel {
        ops: mpsc::Sender<AgentOp>,
    }

    #[async_trait]
    impl P2pInferChannel for OpInferChannel {
        async fn infer(&self, request_json: Vec<u8>) -> Result<Vec<u8>, String> {
            let (rtx, rrx) = oneshot::channel();
            self.ops
                .send(AgentOp::Infer { request: request_json, reply: rtx })
                .await
                .map_err(|_| "agent io task 已停".to_string())?;
            rrx.await.map_err(|_| "agent io task 未回结果".to_string())?
        }
    }

    /// io task：独占 `Session`，顺序执行 infer / 发送流式回复帧。
    async fn io_task(session: nm_client::Session, provider: [u8; 32], mut ops_rx: mpsc::Receiver<AgentOp>) {
        while let Some(op) = ops_rx.recv().await {
            match op {
                AgentOp::Infer { request, reply } => {
                    let r = session.infer(provider, &request).await.map_err(|e| e.to_string());
                    let _ = reply.send(r);
                }
                AgentOp::SendDelta { to, body } => {
                    let _ = session.send_typed(to, AGENT_DELTA_TYPE_URL, &body).await;
                }
            }
        }
    }

    /// 把一段回复按小块（**累计全文**）逐帧发给对端，制造打字机节奏；末帧 `done=true` 带完整文本。
    /// 空回复也至少发一帧 done。帧间小睡 30ms。返回 `Err` 表示 io task 已停。
    async fn stream_reply(
        ops_tx: &mpsc::Sender<AgentOp>,
        to: [u8; 32],
        stream_id: &str,
        text: &str,
    ) -> Result<(), ()> {
        let chars: Vec<char> = text.chars().collect();
        let step = 2usize;
        let mut idx = 0usize;
        let mut seq = 0u32;
        loop {
            idx = (idx + step).min(chars.len());
            let done = idx >= chars.len();
            let cumulative: String = chars[..idx].iter().collect();
            let frame = serde_json::json!({
                "streamId": stream_id, "seq": seq, "text": cumulative, "done": done,
            });
            let body = serde_json::to_vec(&frame).unwrap_or_default();
            ops_tx.send(AgentOp::SendDelta { to, body }).await.map_err(|_| ())?;
            if done {
                break;
            }
            seq += 1;
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        Ok(())
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// 把 agent 作为 bot 跑起来：`session` 须已 `online` 且注册完毕（presence 由调用方做）。
    /// `provider` = 模型 provider 的 entity_id；`model` 写入请求；`system` 为每对端会话的系统提示。
    /// 返回 = 收件流关闭（连接断）为止。
    pub async fn serve(
        mut session: nm_client::Session,
        provider: [u8; 32],
        model: &str,
        system: Option<&str>,
    ) -> Result<(), String> {
        let mut inbox = session.take_inbox().ok_or("inbox 已被取走")?;
        let my_tag = hex(&session.id_bytes()[..4]); // 流 id 前缀，避免多 agent 的流 id 撞车
        let (ops_tx, ops_rx) = mpsc::channel::<AgentOp>(32);
        tokio::spawn(io_task(session, provider, ops_rx));

        let seam = P2pModelSeam::new(OpInferChannel { ops: ops_tx.clone() }, model);
        let agent = Agent::builder()
            .model(Arc::new(seam) as Arc<dyn ModelSeam>)
            .build()
            .map_err(|e| format!("build agent: {e}"))?;

        // 每对端一个持久 cmx 会话（记住上下文）；串行处理。
        let mut convos: HashMap<[u8; 32], Convo> = HashMap::new();
        let mut stream_ctr: u64 = 0;
        while let Some(gram) = inbox.recv().await {
            if gram.kind() != GramKind::Message {
                continue;
            }
            let Ok(sender) = <[u8; 32]>::try_from(gram.sender.clone()) else { continue };
            let text = gram
                .payload
                .as_ref()
                .map(|p| String::from_utf8_lossy(&p.value).to_string())
                .unwrap_or_default();
            if text.is_empty() {
                continue;
            }

            let convo = convos.entry(sender).or_insert_with(|| {
                let c = Convo::new(hex(&sender));
                match system {
                    Some(s) => c.with_system(s),
                    None => c,
                }
            });
            let reply = match agent.run_turn(convo, &text).await {
                Ok(out) => out.final_text.unwrap_or_default(),
                Err(e) => format!("[agent 出错] {e}"),
            };
            // 流式回包：把最终文本按节奏分块发给对端（app 增量长出同一个气泡）。
            stream_ctr += 1;
            let stream_id = format!("{my_tag}-{stream_ctr}");
            if stream_reply(&ops_tx, sender, &stream_id, &reply).await.is_err() {
                break; // io task 没了
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试桩：断言请求形状，回一段固定 OpenAI completion（无网络、确定性）。
    struct CannedChannel;

    #[async_trait]
    impl P2pInferChannel for CannedChannel {
        async fn infer(&self, request_json: Vec<u8>) -> Result<Vec<u8>, String> {
            let v: Value = serde_json::from_slice(&request_json).map_err(|e| e.to_string())?;
            assert_eq!(v["model"], "demo-llm");
            assert_eq!(v["messages"][0]["role"], "system");
            assert_eq!(v["messages"][1]["role"], "user");
            assert_eq!(v["messages"][1]["content"], "你好");
            Ok(br#"{"object":"chat.completion","model":"demo-llm","choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#.to_vec())
        }
    }

    /// 整条缝：以 `&dyn ModelSeam` 调用，证 nanomesh 实现的确是内核 `ModelSeam`。
    #[tokio::test]
    async fn p2p_seam_is_a_real_model_seam() {
        let seam = P2pModelSeam::new(CannedChannel, "demo-llm");
        let ctx = ModelContext {
            system: Some("你是助手".into()),
            messages: vec![ModelMessage::user("你好")],
            tools: Vec::new(),
        };
        let m: &dyn ModelSeam = &seam; // 关键：类型层面证明实现了内核 trait
        let resp = m.complete(&ctx).await.expect("complete ok");
        assert_eq!(resp.text.as_deref(), Some("pong"));
        let usage = resp.usage.expect("usage");
        assert_eq!(usage.input, 3);
        assert_eq!(usage.output, 1);
    }

    /// `complete_bounded` 默认实现回退到 `complete`（内核默认行为，证可直接复用）。
    #[tokio::test]
    async fn complete_bounded_falls_back() {
        let seam = P2pModelSeam::new(CannedChannel, "demo-llm");
        let ctx = ModelContext { messages: vec![ModelMessage::user("你好")], system: Some("你是助手".into()), tools: Vec::new() };
        let resp = seam.complete_bounded(&ctx, 16).await.expect("bounded ok");
        assert_eq!(resp.text.as_deref(), Some("pong"));
    }

    /// 坏响应 → 结构化 ModelError（不 panic）。
    #[test]
    fn decode_rejects_bad_json() {
        assert!(decode_openai_response(b"not json").is_err());
    }

    /// 证 `cmx-agent-tools` 引用打通：内置工具平面非空（echo/clock/add/fs_read/… 已装配）。
    #[test]
    fn builtin_tools_reference_through() {
        let specs = builtin_tool_specs();
        assert!(!specs.is_empty(), "内置工具平面应非空");
        assert!(specs.iter().any(|s| s.name == "echo"), "应含 echo 工具");
    }
}

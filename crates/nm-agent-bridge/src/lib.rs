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

//! `nm-compute`（C1）：P2P 模型 / 算力 **provider 运行时** 的核心逻辑。
//!
//! provider 注册为 `model.llm` 实体，经 `next_command` 接 **OpenAI 兼容** 的 `model.infer` 请求，
//! 转给后端（`Demo` 离线 / `Ollama`），回 OpenAI 兼容 completion。载荷约定见
//! `docs/COMPUTE_MODEL_P2P_DESIGN.md` §13（请求 `openai.chat.v1` / 响应 `openai.chat.completion.v1`）。
//!
//! 后端用 **enum 分发**（Demo + Ollama 两种）而非 trait 对象——两档足够，省掉 async-trait 依赖。

use serde::{Deserialize, Serialize};

/// 命令方法名（§13 固化）。
pub const METHOD_INFER: &str = "model.infer";
/// 请求 / 响应载荷 type_url（§13 固化，OpenAI 兼容）。
pub const REQ_TYPE_URL: &str = "openai.chat.v1";
pub const RESP_TYPE_URL: &str = "openai.chat.completion.v1";

#[derive(Debug, Clone, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
}

/// OpenAI `/chat/completions` 请求子集。
#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    #[serde(default)]
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Serialize)]
pub struct RespMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct Choice {
    pub index: u32,
    pub message: RespMessage,
    pub finish_reason: String,
}

/// OpenAI `chat.completion` 响应。
#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub id: String,
    pub object: String,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Usage,
}

fn approx_tokens(s: &str) -> u32 {
    // 粗略 token 估算（计量占位；真实 provider 用模型实际 usage）。
    let words = s.split_whitespace().count();
    (words.max(s.chars().count() / 4) as u32).max(if s.is_empty() { 0 } else { 1 })
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// provider 后端：离线 `Demo`（无网络，便于测试/演示）或 `Ollama`（本机 OpenAI 兼容端点）。
pub enum Backend {
    Demo,
    Ollama { url: String, client: reqwest::Client },
}

impl Backend {
    pub fn ollama(url: impl Into<String>) -> Self {
        Backend::Ollama {
            url: url.into().trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// 处理一次推理：输入 OpenAI 请求 JSON 字节 → 输出 OpenAI `chat.completion` JSON 字节。
    /// `model_label` 为 provider 注册的模型名（请求未带 model 时用它回填）。
    pub async fn handle_infer(&self, model_label: &str, params_json: &[u8]) -> anyhow::Result<Vec<u8>> {
        let req: ChatRequest = serde_json::from_slice(params_json)
            .map_err(|e| anyhow::anyhow!("非法 OpenAI chat 请求: {e}"))?;
        let model = if req.model.is_empty() { model_label.to_string() } else { req.model.clone() };
        let (content, usage) = match self {
            Backend::Demo => demo_infer(&req),
            Backend::Ollama { url, client } => ollama_infer(client, url, params_json).await?,
        };
        let resp = ChatResponse {
            id: format!("cmpl-{}", now_ms()),
            object: "chat.completion".into(),
            model,
            choices: vec![Choice {
                index: 0,
                message: RespMessage { role: "assistant".into(), content },
                finish_reason: "stop".into(),
            }],
            usage,
        };
        Ok(serde_json::to_vec(&resp)?)
    }
}

/// 离线 Demo：回声最后一条 user 消息 + 粗略 usage。无网络，确定性，供测试/无 Ollama 时用。
pub fn demo_infer(req: &ChatRequest) -> (String, Usage) {
    let last_user = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone())
        .unwrap_or_default();
    let content = format!("[demo-llm] 收到 {} 条消息；回声最后一句：{last_user}", req.messages.len());
    let prompt_tokens: u32 = req.messages.iter().map(|m| approx_tokens(&m.content)).sum();
    let completion_tokens = approx_tokens(&content);
    (
        content,
        Usage { prompt_tokens, completion_tokens, total_tokens: prompt_tokens + completion_tokens },
    )
}

/// Ollama 的 OpenAI 兼容端点 `{url}/v1/chat/completions`：把消费方 JSON 透传，取回 content+usage。
async fn ollama_infer(client: &reqwest::Client, url: &str, params_json: &[u8]) -> anyhow::Result<(String, Usage)> {
    let endpoint = format!("{url}/v1/chat/completions");
    let body: serde_json::Value = serde_json::from_slice(params_json)?;
    let resp = client.post(&endpoint).json(&body).send().await?.error_for_status()?;
    let out: serde_json::Value = resp.json().await?;
    let content = out["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string();
    let usage = Usage {
        prompt_tokens: out["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
        completion_tokens: out["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
        total_tokens: out["usage"]["total_tokens"].as_u64().unwrap_or(0) as u32,
    };
    Ok((content, usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn demo_infer_openai_roundtrip() {
        let req = br#"{"model":"demo-llm","messages":[{"role":"user","content":"hello world"}]}"#;
        let out = Backend::Demo.handle_infer("demo-llm", req).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["model"], "demo-llm");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert!(v["choices"][0]["message"]["content"].as_str().unwrap().contains("hello world"));
        assert!(v["usage"]["total_tokens"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn bad_request_errs() {
        assert!(Backend::Demo.handle_infer("m", b"not json").await.is_err());
    }

    #[test]
    fn model_fallback_when_request_omits_it() {
        let req: ChatRequest = serde_json::from_slice(br#"{"messages":[{"role":"user","content":"hi"}]}"#).unwrap();
        assert!(req.model.is_empty());
        let (c, u) = demo_infer(&req);
        assert!(c.contains("hi"));
        assert!(u.total_tokens > 0);
    }
}

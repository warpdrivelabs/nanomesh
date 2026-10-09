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
/// 流式中途帧载荷类型：JSON `{"delta":"<文字片段>"}`（终帧仍为完整 RESP_TYPE_URL）。
pub const CHUNK_TYPE_URL: &str = "openai.chat.chunk.v1";

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
        Ok(serde_json::to_vec(&chat_response(&model, content, usage))?)
    }

    /// 流式推理：把 OpenAI 兼容结果**逐帧**产出到 `tx`——中途帧 `({"delta":"片段"}, false)`，
    /// 终帧 `(完整 chat.completion, true)`。Demo 切块伪流式（带节奏）；Ollama 走真 SSE。
    pub async fn stream_infer(
        &self,
        model_label: &str,
        params_json: &[u8],
        tx: tokio::sync::mpsc::UnboundedSender<(Vec<u8>, bool)>,
    ) -> anyhow::Result<()> {
        let req: ChatRequest = serde_json::from_slice(params_json)
            .map_err(|e| anyhow::anyhow!("非法 OpenAI chat 请求: {e}"))?;
        let model = if req.model.is_empty() { model_label.to_string() } else { req.model.clone() };
        match self {
            Backend::Demo => {
                let (content, usage) = demo_infer(&req);
                let chars: Vec<char> = content.chars().collect();
                let mut i = 0;
                while i < chars.len() {
                    let j = (i + 3).min(chars.len());
                    let piece: String = chars[i..j].iter().collect();
                    let frame = serde_json::json!({ "delta": piece });
                    if tx.send((serde_json::to_vec(&frame)?, false)).is_err() {
                        return Ok(()); // 消费端已断
                    }
                    i = j;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                let _ = tx.send((serde_json::to_vec(&chat_response(&model, content, usage))?, true));
            }
            Backend::Ollama { url, client } => {
                ollama_stream(client, url, params_json, &model, &tx).await?;
            }
        }
        Ok(())
    }
}

/// 组装 OpenAI `chat.completion` 响应（流式终帧与非流式共用）。
fn chat_response(model: &str, content: String, usage: Usage) -> ChatResponse {
    ChatResponse {
        id: format!("cmpl-{}", now_ms()),
        object: "chat.completion".into(),
        model: model.to_string(),
        choices: vec![Choice {
            index: 0,
            message: RespMessage { role: "assistant".into(), content },
            finish_reason: "stop".into(),
        }],
        usage,
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

/// Ollama SSE 流式：设 `stream:true`，逐行解析 `data: {...}` 的 `choices[0].delta.content`，逐片产出到 `tx`；
/// 末尾产出完整 `chat.completion`（done=true）。需 reqwest `stream` 特性 + `futures-util`。
async fn ollama_stream(
    client: &reqwest::Client,
    url: &str,
    params_json: &[u8],
    model: &str,
    tx: &tokio::sync::mpsc::UnboundedSender<(Vec<u8>, bool)>,
) -> anyhow::Result<()> {
    use futures_util::StreamExt;
    let endpoint = format!("{url}/v1/chat/completions");
    let mut body: serde_json::Value = serde_json::from_slice(params_json)?;
    body["stream"] = serde_json::json!(true);
    body["stream_options"] = serde_json::json!({ "include_usage": true });
    let resp = client.post(&endpoint).json(&body).send().await?.error_for_status()?;
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut content = String::new();
    let mut usage = Usage::default();
    while let Some(chunk) = stream.next().await {
        buf.push_str(&String::from_utf8_lossy(&chunk?));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let Some(data) = line.trim().strip_prefix("data:").map(|s| s.trim().to_string()) else {
                continue;
            };
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
            if let Some(d) = v["choices"][0]["delta"]["content"].as_str() {
                if !d.is_empty() {
                    content.push_str(d);
                    let frame = serde_json::json!({ "delta": d });
                    if tx.send((serde_json::to_vec(&frame)?, false)).is_err() {
                        return Ok(());
                    }
                }
            }
            if !v["usage"].is_null() {
                usage.prompt_tokens = v["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32;
                usage.completion_tokens = v["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32;
                usage.total_tokens = v["usage"]["total_tokens"].as_u64().unwrap_or(0) as u32;
            }
        }
    }
    let _ = tx.send((serde_json::to_vec(&chat_response(model, content, usage))?, true));
    Ok(())
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

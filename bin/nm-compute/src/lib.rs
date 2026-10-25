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

/// provider 后端：离线 `Demo`、本机 `Ollama`、或云厂商 OpenAI 兼容 API（Key 仅留本机）。
pub enum Backend {
    Demo,
    Ollama { url: String, client: reqwest::Client },
    /// 云厂商 / 自建 OpenAI 兼容端点；`api_key` 只在进程内存。
    OpenAi {
        url: String,
        api_key: String,
        client: reqwest::Client,
    },
}

/// 上游 HTTP 客户端：必须带超时，否则错误 URL / 防火墙会让 `model.infer` 挂到消费方 P2P 超时。
fn http_client() -> reqwest::Client {
    use std::time::Duration;
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(90))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 请求 JSON 缺 model 时回填 `model_label`，避免上游收到空 model 名。
fn params_with_model(params_json: &[u8], model_label: &str) -> anyhow::Result<Vec<u8>> {
    let mut body: serde_json::Value = serde_json::from_slice(params_json)?;
    let empty = body
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.is_empty())
        .unwrap_or(true);
    if empty {
        body["model"] = serde_json::json!(model_label);
    }
    Ok(serde_json::to_vec(&body)?)
}

impl Backend {
    pub fn ollama(url: impl Into<String>) -> Self {
        Backend::Ollama {
            url: url.into().trim_end_matches('/').to_string(),
            client: http_client(),
        }
    }

    /// 云厂商 OpenAI 兼容：`base_url` 形如 `https://api.openai.com/v1`（勿带尾斜杠）。
    pub fn openai(url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Backend::OpenAi {
            url: url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            client: http_client(),
        }
    }

    /// 处理一次推理：输入 OpenAI 请求 JSON 字节 → 输出 OpenAI `chat.completion` JSON 字节。
    /// `model_label` 为 provider 注册的模型名（请求未带 model 时用它回填）。
    pub async fn handle_infer(&self, model_label: &str, params_json: &[u8]) -> anyhow::Result<Vec<u8>> {
        let req: ChatRequest = serde_json::from_slice(params_json)
            .map_err(|e| anyhow::anyhow!("非法 OpenAI chat 请求: {e}"))?;
        let model = if req.model.is_empty() { model_label.to_string() } else { req.model.clone() };
        let forward = params_with_model(params_json, &model)?;
        let (content, usage) = match self {
            Backend::Demo => demo_infer(&req),
            Backend::Ollama { url, client } => {
                openai_compat_infer(client, url, None, &forward).await?
            }
            Backend::OpenAi { url, api_key, client } => {
                openai_compat_infer(client, url, Some(api_key.as_str()), &forward).await?
            }
        };
        Ok(serde_json::to_vec(&chat_response(&model, content, usage))?)
    }

    /// 流式推理：把 OpenAI 兼容结果**逐帧**产出到 `tx`——中途帧 `({"delta":"片段"}, false)`，
    /// 终帧 `(完整 chat.completion, true)`。Demo 切块伪流式；Ollama/云厂商走真 SSE。
    pub async fn stream_infer(
        &self,
        model_label: &str,
        params_json: &[u8],
        tx: tokio::sync::mpsc::UnboundedSender<(Vec<u8>, bool)>,
    ) -> anyhow::Result<()> {
        let req: ChatRequest = serde_json::from_slice(params_json)
            .map_err(|e| anyhow::anyhow!("非法 OpenAI chat 请求: {e}"))?;
        let model = if req.model.is_empty() { model_label.to_string() } else { req.model.clone() };
        let forward = params_with_model(params_json, &model)?;
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
                openai_compat_stream(client, url, None, &forward, &model, &tx).await?;
            }
            Backend::OpenAi { url, api_key, client } => {
                openai_compat_stream(client, url, Some(api_key.as_str()), &forward, &model, &tx)
                    .await?;
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

/// OpenAI 兼容端点 `{url}/chat/completions`（`url` 已含 `/v1`）。
/// `api_key=Some` 时带 `Authorization: Bearer`（云厂商）；`None` 时不带（本机 Ollama）。
async fn openai_compat_infer(
    client: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    params_json: &[u8],
) -> anyhow::Result<(String, Usage)> {
    let endpoint = chat_completions_url(url);
    let mut body: serde_json::Value = serde_json::from_slice(params_json)?;
    body["stream"] = serde_json::json!(false);
    let mut req = client.post(&endpoint).json(&body);
    if let Some(k) = api_key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let raw = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow::anyhow!("上游 HTTP {status}: {}", truncate(&raw, 400)));
    }
    let out: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("上游非 JSON: {e}; body={}", truncate(&raw, 200)))?;
    let content = out["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let usage = Usage {
        prompt_tokens: out["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
        completion_tokens: out["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
        total_tokens: out["usage"]["total_tokens"].as_u64().unwrap_or(0) as u32,
    };
    Ok((content, usage))
}

/// OpenAI 兼容 SSE：设 `stream:true`，解析 `data:` 行的 `delta.content`，终帧完整 completion。
async fn openai_compat_stream(
    client: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    params_json: &[u8],
    model: &str,
    tx: &tokio::sync::mpsc::UnboundedSender<(Vec<u8>, bool)>,
) -> anyhow::Result<()> {
    use futures_util::StreamExt;
    let endpoint = chat_completions_url(url);
    let mut body: serde_json::Value = serde_json::from_slice(params_json)?;
    body["stream"] = serde_json::json!(true);
    body["stream_options"] = serde_json::json!({ "include_usage": true });
    let mut req = client.post(&endpoint).json(&body);
    if let Some(k) = api_key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let raw = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("上游 HTTP {status}: {}", truncate(&raw, 400)));
    }
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
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else {
                continue;
            };
            if let Some(d) = v["choices"][0]["delta"]["content"].as_str()
                && !d.is_empty()
            {
                content.push_str(d);
                let frame = serde_json::json!({ "delta": d });
                if tx.send((serde_json::to_vec(&frame)?, false)).is_err() {
                    return Ok(());
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

/// `url` 可为 `https://api.openai.com/v1` 或已含 `/chat/completions` 的完整路径。
fn chat_completions_url(url: &str) -> String {
    let u = url.trim_end_matches('/');
    if u.ends_with("/chat/completions") {
        u.to_string()
    } else {
        format!("{u}/chat/completions")
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
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

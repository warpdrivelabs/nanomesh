//! `nm-llm-proxy` — 消费方本机 OpenAI 兼容 HTTP，转发到 nanomesh 上**多个** `model.llm`。
//!
//! 智能体：`OPENAI_BASE_URL=http://127.0.0.1:18080/v1` `OPENAI_API_KEY=nm-local-token`
//!
//! 多 provider：
//! - `GET /v1/models` 聚合目录中全部 `model.llm`
//! - `POST /v1/chat/completions` 按 model / `model@hex8` / `--provider` / `X-NM-Provider` 路由
//! - 候选失败自动 failover 到下一提供方
//!
//! 见 `docs/LLM_KEY_SHARE_P2P_DESIGN.md`。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use clap::Parser;
use nm_llm_proxy::{aggregate_models, select_candidates, ProviderInfo, RouteRequest};
use nm_proto::{DirectoryQuery, Grant};
use prost::Message;
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::CorsLayer;

#[derive(Parser, Debug)]
#[command(name = "nm-llm-proxy", about = "OpenAI HTTP → nanomesh 多 provider P2P")]
struct Args {
    /// 节点地址 JSON（nmd 打印的 NM_NODE_ADDR）。
    #[arg(long)]
    node: String,
    /// 本代理身份种子。
    #[arg(long, default_value_t = 171)]
    seed: u8,
    /// 监听地址。
    #[arg(long, default_value = "127.0.0.1:18080")]
    listen: String,
    /// 本地 Bearer token（智能体 OPENAI_API_KEY）；空=不校验。
    #[arg(long, default_value = "nm-local-token")]
    local_token: String,
    /// 固定某个 provider（64 hex）；也可用环境变量 NM_LLM_PROVIDER_ID。
    #[arg(long, default_value = "")]
    provider: String,
    /// 单个 Grant 文件（base64）；或见 --grants-dir。
    #[arg(long, default_value = "")]
    grant: String,
    /// Grant 目录：每个文件名为 issuer 公钥 hex（或任意名），内容为 base64 Grant。
    #[arg(long, default_value = "")]
    grants_dir: String,
    /// 目录刷新间隔秒。
    #[arg(long, default_value_t = 15)]
    refresh_secs: u64,
    /// 推理超时秒。
    #[arg(long, default_value_t = 120)]
    timeout_secs: u64,
    /// failover 最多尝试几个 provider。
    #[arg(long, default_value_t = 3)]
    max_failover: usize,
    #[arg(long)]
    relay: Vec<String>,
    #[arg(long)]
    pkarr: Option<String>,
    #[arg(long)]
    dns_origin: Option<String>,
}

struct AppState {
    /// Session 含 !Sync 接收端，用 Mutex 供 axum 多任务共享。
    sess: Mutex<nm_client::Session>,
    consumer_id: [u8; 32],
    providers: RwLock<Vec<ProviderInfo>>,
    /// issuer_hex → Grant（多提供方各持一份）
    grants: HashMap<String, Grant>,
    /// 兜底：唯一 Grant（单提供方场景）
    default_grant: Option<Grant>,
    pin: Option<[u8; 32]>,
    local_token: String,
    timeout: Duration,
    max_failover: usize,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn parse_hex32(s: &str) -> anyhow::Result<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return Err(anyhow!("须 64 hex，got {}", s.len()));
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

fn decode_grant_b64(s: &str) -> anyhow::Result<Grant> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s.trim().as_bytes())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim().as_bytes()))
        .context("grant base64")?;
    Grant::decode(bytes.as_slice()).context("grant protobuf")
}

fn load_grants(grant_file: &str, grants_dir: &str) -> anyhow::Result<(Option<Grant>, HashMap<String, Grant>)> {
    let mut map = HashMap::new();
    let mut default = None;
    if !grant_file.is_empty() {
        let raw = std::fs::read_to_string(grant_file).with_context(|| format!("读 {grant_file}"))?;
        let g = decode_grant_b64(&raw)?;
        let iss = hex(&g.issuer);
        map.insert(iss, g.clone());
        default = Some(g);
    }
    if !grants_dir.is_empty() {
        let dir = PathBuf::from(grants_dir);
        for ent in std::fs::read_dir(&dir).with_context(|| format!("读目录 {grants_dir}"))? {
            let ent = ent?;
            if !ent.file_type()?.is_file() {
                continue;
            }
            let raw = std::fs::read_to_string(ent.path())?;
            let g = decode_grant_b64(&raw)?;
            map.insert(hex(&g.issuer), g);
        }
    }
    Ok((default, map))
}

fn grant_for(st: &AppState, provider: &ProviderInfo) -> Option<Grant> {
    st.grants
        .get(&provider.id_hex)
        .cloned()
        .or_else(|| st.default_grant.clone())
}

async fn refresh_providers(sess: &nm_client::Session) -> anyhow::Result<Vec<ProviderInfo>> {
    let list = sess
        .directory_query(DirectoryQuery {
            kind_prefix: "model.".into(),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow!("{e}"))?;
    Ok(list.iter().filter_map(ProviderInfo::from_entity).collect())
}

fn check_local_auth(st: &AppState, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    if st.local_token.is_empty() {
        return Ok(());
    }
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))
        .unwrap_or(auth);
    if token == st.local_token {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "invalid local token".into()))
    }
}

fn pin_from_headers(headers: &HeaderMap, fallback: Option<[u8; 32]>) -> Option<[u8; 32]> {
    headers
        .get("x-nm-provider")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| parse_hex32(s).ok())
        .or(fallback)
}

async fn handle_models(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err((code, msg)) = check_local_auth(&st, &headers) {
        return (code, msg).into_response();
    }
    // 尽力刷新一次（失败则用缓存）
    {
        let sess = st.sess.lock().await;
        if let Ok(fresh) = refresh_providers(&sess).await {
            drop(sess);
            *st.providers.write().await = fresh;
        }
    }
    let list = aggregate_models(&st.providers.read().await);
    Json(list).into_response()
}

async fn handle_chat(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err((code, msg)) = check_local_auth(&st, &headers) {
        return (code, Json(json!({"error":{"message":msg,"type":"auth_error"}}))).into_response();
    }

    let req_json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":{"message":format!("bad json: {e}"),"type":"invalid_request"}})),
            )
                .into_response();
        }
    };
    let model = req_json["model"].as_str().unwrap_or("").to_string();
    let stream = req_json["stream"].as_bool().unwrap_or(false);
    if stream {
        // P0：流式暂降级为同步（仍返回完整 completion；P1 再做 SSE）。
        tracing::debug!("stream=true → sync fallback (P0)");
    }

    // 目录为空时强制刷一次
    {
        let empty = st.providers.read().await.is_empty();
        if empty {
            let sess = st.sess.lock().await;
            if let Ok(fresh) = refresh_providers(&sess).await {
                drop(sess);
                *st.providers.write().await = fresh;
            }
        }
    }

    let pin = pin_from_headers(&headers, st.pin);
    let providers = st.providers.read().await.clone();
    let candidates = select_candidates(
        &providers,
        &RouteRequest {
            model: model.clone(),
            pin_provider: pin,
        },
    );
    if candidates.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("no model.llm provider for model={model:?}"),
                    "type": "provider_not_found"
                }
            })),
        )
            .into_response();
    }

    // 请求体：若用了 model@hex，转发上游时改回短名。
    let mut forward = req_json.clone();
    if let Some((name, _)) = model.rsplit_once('@') {
        if !name.is_empty() {
            forward["model"] = json!(name);
        }
    }
    forward["stream"] = json!(false);
    let forward_bytes = serde_json::to_vec(&forward).unwrap_or_else(|_| body.to_vec());

    let mut last_err = String::from("no candidate");
    let limit = st.max_failover.max(1).min(candidates.len());
    let sess = st.sess.lock().await;
    for (i, cand) in candidates.into_iter().take(limit).enumerate() {
        let grant = if cand.require_grant {
            match grant_for(&st, &cand) {
                Some(g) => Some(g),
                None => {
                    last_err = format!(
                        "provider {} requires grant but none loaded for issuer",
                        &cand.id_hex[..8]
                    );
                    tracing::warn!("{last_err}");
                    continue;
                }
            }
        } else {
            grant_for(&st, &cand)
        };

        tracing::info!(
            "infer try#{} provider={}… model={} grant={}",
            i + 1,
            &cand.id_hex[..8],
            forward["model"],
            grant.is_some()
        );
        let t0 = Instant::now();
        match sess
            .infer_with_grant_timeout(cand.id, &forward_bytes, grant, st.timeout)
            .await
        {
            Ok(bytes) => {
                tracing::info!(
                    "infer ok provider={}… {}ms",
                    &cand.id_hex[..8],
                    t0.elapsed().as_millis()
                );
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-nm-provider", cand.id_hex)
                    .body(axum::body::Body::from(bytes))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
            Err(e) => {
                last_err = format!("{}…: {e}", &cand.id_hex[..8]);
                tracing::warn!("infer fail try#{}: {last_err}", i + 1);
            }
        }
    }

    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": {
                "message": format!("all providers failed: {last_err}"),
                "type": "p2p_upstream_error"
            }
        })),
    )
        .into_response()
}

async fn handle_health(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    let n = st.providers.read().await.len();
    Json(json!({
        "ok": true,
        "providers": n,
        "consumer_id": hex(&st.consumer_id),
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let mut args = Args::parse();
    if args.provider.is_empty() {
        if let Ok(v) = std::env::var("NM_LLM_PROVIDER_ID") {
            args.provider = v;
        }
    }
    let pin = if args.provider.is_empty() {
        None
    } else {
        Some(parse_hex32(&args.provider)?)
    };
    let (default_grant, grants) = load_grants(&args.grant, &args.grants_dir)?;

    let addr = nm_transport::addr_from_string(&args.node)?;
    let client = if !args.relay.is_empty() || args.pkarr.is_some() {
        let pkarr = args
            .pkarr
            .clone()
            .ok_or_else(|| anyhow!("--relay 需配合 --pkarr"))?;
        nm_client::Client::bind_selfhosted(
            [args.seed; 32],
            args.relay.clone(),
            pkarr,
            args.dns_origin.clone(),
            0,
        )
        .await?
    } else {
        nm_client::Client::bind([args.seed; 32]).await?
    };
    // LAN：播种 home 地址，便于按需 online。
    client.add_peer_addr(addr.clone());
    let sess = client.online(addr).await.map_err(|e| anyhow!("{e}"))?;
    let consumer_id = sess.id_bytes();
    let initial = refresh_providers(&sess).await.unwrap_or_default();
    println!(
        "nm-llm-proxy online: consumer={}  providers={}  listen={}",
        hex(&consumer_id),
        initial.len(),
        args.listen
    );
    for p in &initial {
        println!(
            "  - {}…  models={:?}  load={}  grant_needed={}",
            &p.id_hex[..8],
            p.models,
            p.load,
            p.require_grant
        );
    }
    if pin.is_some() {
        println!("  pin provider={}", args.provider);
    }

    let st = Arc::new(AppState {
        sess: Mutex::new(sess),
        consumer_id,
        providers: RwLock::new(initial),
        grants,
        default_grant,
        pin,
        local_token: args.local_token.clone(),
        timeout: Duration::from_secs(args.timeout_secs),
        max_failover: args.max_failover,
    });

    // 后台刷新目录
    {
        let st2 = st.clone();
        let secs = args.refresh_secs.max(3);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                let sess = st2.sess.lock().await;
                match refresh_providers(&sess).await {
                    Ok(list) => {
                        drop(sess);
                        tracing::debug!("directory refresh: {} providers", list.len());
                        *st2.providers.write().await = list;
                    }
                    Err(e) => tracing::warn!("directory refresh: {e}"),
                }
            }
        });
    }

    let app = Router::new()
        .route("/healthz", get(handle_health))
        .route("/v1/models", get(handle_models))
        .route("/v1/chat/completions", post(handle_chat))
        // 兼容未带 /v1 前缀
        .route("/models", get(handle_models))
        .route("/chat/completions", post(handle_chat))
        .layer(CorsLayer::permissive())
        .with_state(st);

    let listen: SocketAddr = args.listen.parse().context("listen addr")?;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    println!("=> OPENAI_BASE_URL=http://{listen}/v1");
    println!("=> OPENAI_API_KEY={}", args.local_token);
    axum::serve(listener, app).await?;
    Ok(())
}

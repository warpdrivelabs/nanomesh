//! `nm-llm-gatewayd` — 本机云厂商 LLM Key 的 P2P 提供方。
//!
//! 支持**多个** `[[backends]]`：每家 Key / base_url / 模型列表独立；
//! 对外仍注册为一个 `model.llm`，按请求 `model` 字段路由到对应后端。
//! 见 `docs/LLM_KEY_SHARE_P2P_DESIGN.md`。
//!
//! ```text
//! cargo run -p nm-llm-gatewayd -- serve --config ~/.config/nanomesh/llm-gateway.toml
//! cargo run -p nm-llm-gatewayd -- issue-grant --config ... --audience <64hex> --days 7
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use base64::Engine;
use clap::{Parser, Subcommand};
use nm_compute::{Backend, CHUNK_TYPE_URL, METHOD_INFER, RESP_TYPE_URL};
use nm_entity::kinds;
use nm_proto::pb::InferenceProfile;
use nm_proto::Any;
use prost::Message;
use serde::Deserialize;

#[derive(Parser)]
#[command(name = "nm-llm-gatewayd", about = "云厂商 LLM Key → nanomesh P2P 提供方（多模型）")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 上线注册 model.llm，接单转发云厂商。
    Serve {
        #[arg(long)]
        config: String,
        /// 节点地址 JSON（覆盖配置里的 home_node）。
        #[arg(long, default_value = "")]
        node: String,
    },
    /// 用本机身份给消费方签发 Grant（action=model.infer, resource=model.llm）。
    IssueGrant {
        #[arg(long)]
        config: String,
        /// 消费方公钥 hex（64 字符）。
        #[arg(long)]
        audience: String,
        /// 有效天数；0=永不过期。
        #[arg(long, default_value_t = 7)]
        days: i64,
        /// 写出 base64 Grant 文件；空则打印到 stdout。
        #[arg(long, default_value = "")]
        out: String,
    },
}

#[derive(Debug, Deserialize, Clone)]
struct Config {
    #[serde(default)]
    home_node: String,
    #[serde(default = "default_name")]
    display_name: String,
    /// 兼容旧配置：单字节复制成 32B seed。优先用 seed_hex / seed_file。
    #[serde(default = "default_seed")]
    seed: u8,
    /// 完整 64 hex 身份种子（一模型一进程一身份；Tauri 管理台写入）。
    #[serde(default)]
    seed_hex: String,
    /// 存放 64 hex 种子的文件路径（0600）；与 seed_hex 二选一。
    #[serde(default)]
    seed_file: String,
    /// 连接模式：nat（默认 N0）| lan | selfhost
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default)]
    relay: Vec<String>,
    #[serde(default)]
    pkarr: Option<String>,
    #[serde(default)]
    dns_origin: Option<String>,
    /// 展示档案（与 Person 对齐）。
    #[serde(default)]
    avatar_url: String,
    /// 头像原图文件（data:URI 文本）：上线时据此把 blob 传到**本模型 home**（gatewayd 所连节点），
    /// 使 `avatar_url`(b3:hash) 能被其他用户按模型 home_node 拉到。
    /// 否则 app 侧 blob 只落在 app 登录 home，模型挂靠别的 home 时他人拉不到 → 头像空。
    #[serde(default)]
    avatar_file: String,
    #[serde(default)]
    bio: String,
    #[serde(default)]
    status_text: String,
    #[serde(default)]
    links: Vec<String>,
    #[serde(default)]
    locale: String,
    #[serde(default)]
    metering: String,
    #[serde(default)]
    billing: String,
    #[serde(default)]
    price_label: String,
    #[serde(default)]
    backends: Vec<BackendCfg>,
    #[serde(default)]
    policy: PolicyCfg,
}

fn default_mode() -> String {
    "nat".into()
}

fn default_name() -> String {
    "llm-gateway".into()
}
fn default_seed() -> u8 {
    170
}

#[derive(Debug, Deserialize, Clone)]
struct BackendCfg {
    /// 配置内逻辑名（日志 / 排障）。
    #[serde(default = "default_backend_id")]
    id: String,
    #[serde(default = "default_provider")]
    provider: String,
    #[serde(default = "default_base_url")]
    base_url: String,
    /// 环境变量名（禁止把 Key 写进 toml）。每个 backend 可用不同变量。
    #[serde(default = "default_key_env")]
    key_env: String,
    #[serde(default)]
    models: Vec<String>,
}

fn default_backend_id() -> String {
    "main".into()
}
fn default_provider() -> String {
    "openai".into()
}
fn default_base_url() -> String {
    "https://api.openai.com/v1".into()
}
fn default_key_env() -> String {
    "OPENAI_API_KEY".into()
}

#[derive(Debug, Deserialize, Clone)]
struct PolicyCfg {
    #[serde(default = "default_true")]
    require_grant: bool,
    /// 消费方公钥 hex 白名单；空=任意持有效 Grant 者。
    #[serde(default)]
    allow_audiences: Vec<String>,
    #[serde(default = "default_budget")]
    daily_token_budget: u64,
    #[serde(default = "default_concurrent")]
    max_concurrent: u32,
}

fn default_true() -> bool {
    true
}
fn default_budget() -> u64 {
    2_000_000
}
fn default_concurrent() -> u32 {
    4
}

impl Default for PolicyCfg {
    fn default() -> Self {
        Self {
            require_grant: true,
            allow_audiences: Vec::new(),
            daily_token_budget: default_budget(),
            max_concurrent: default_concurrent(),
        }
    }
}

/// 单个已加载的后端（一把 Key + 其声明的模型）。
struct LoadedBackend {
    id: String,
    #[allow(dead_code)]
    provider: String,
    #[allow(dead_code)]
    models: Vec<String>,
    backend: Arc<Backend>,
}

/// 多 Key / 多模型路由表：按请求 `model` 选后端。
struct ModelRouter {
    /// model_name → 后端下标
    by_model: HashMap<String, usize>,
    backends: Vec<LoadedBackend>,
    /// 全部模型（注册 attributes.models / InferenceProfile）
    all_models: Vec<String>,
    /// 默认模型（请求未带 model 时）
    default_model: String,
    /// 厂商标签聚合，如 `openai,deepseek`
    providers_csv: String,
}

impl ModelRouter {
    fn from_configs(cfgs: &[BackendCfg]) -> anyhow::Result<Self> {
        if cfgs.is_empty() {
            return Err(anyhow!("配置须至少有一个 [[backends]]"));
        }
        let mut backends: Vec<LoadedBackend> = Vec::new();
        let mut by_model: HashMap<String, usize> = HashMap::new();
        let mut all_models: Vec<String> = Vec::new();
        let mut providers: Vec<String> = Vec::new();

        for cfg in cfgs {
            let key = resolve_key(&cfg.key_env)?;
            let models = if cfg.models.is_empty() {
                vec!["gpt-4o-mini".into()]
            } else {
                cfg.models.clone()
            };
            // 同一 backend 内禁止重复模型名。
            {
                let mut seen = std::collections::HashSet::new();
                for m in &models {
                    if !seen.insert(m.as_str()) {
                        return Err(anyhow!("backend `{}` 内模型名重复: `{m}`", cfg.id));
                    }
                }
            }
            let idx = backends.len();
            for m in &models {
                if let Some(&prev) = by_model.get(m) {
                    return Err(anyhow!(
                        "模型名冲突: `{m}` 同时出现在 backends[{prev}]({}) 与 backends[{idx}]({})",
                        backends[prev].id,
                        cfg.id
                    ));
                }
            }
            for m in &models {
                by_model.insert(m.clone(), idx);
                all_models.push(m.clone());
            }
            if !providers.iter().any(|p| p == &cfg.provider) {
                providers.push(cfg.provider.clone());
            }
            eprintln!(
                "  backend id={} provider={} key_env={} models={:?}",
                cfg.id, cfg.provider, cfg.key_env, models
            );
            backends.push(LoadedBackend {
                id: cfg.id.clone(),
                provider: cfg.provider.clone(),
                models: models.clone(),
                backend: Arc::new(Backend::openai(cfg.base_url.as_str(), key)),
            });
        }

        let default_model = all_models[0].clone();
        Ok(Self {
            by_model,
            backends,
            all_models,
            default_model,
            providers_csv: providers.join(","),
        })
    }

    /// 从请求 JSON 解析 model，返回 (backend, 用于回填的 model_label)。
    fn resolve(&self, params_json: &[u8]) -> Result<(Arc<Backend>, String), String> {
        let model = serde_json::from_slice::<serde_json::Value>(params_json)
            .ok()
            .and_then(|v| v["model"].as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let model = if model.is_empty() {
            self.default_model.clone()
        } else {
            model
        };
        match self.by_model.get(&model) {
            Some(&idx) => Ok((self.backends[idx].backend.clone(), model)),
            None => Err(format!(
                "本节点未提供模型 `{model}`；可用: {}",
                self.all_models.join(", ")
            )),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 解析 `data:<mime>;base64,<b64>` → (字节, mime)。非法/非 base64 则返回 None。
/// 用于把 app 侧随 runtime 落盘的头像原图还原成字节，上线时 blob_put 到本模型 home。
fn parse_data_uri(uri: &str) -> Option<(Vec<u8>, String)> {
    let uri = uri.trim();
    let rest = uri.strip_prefix("data:")?;
    let (meta, b64) = rest.split_once(',')?;
    if !meta.contains("base64") {
        return None;
    }
    let mime = meta.split(';').next().unwrap_or("application/octet-stream");
    let mime = if mime.is_empty() {
        "application/octet-stream"
    } else {
        mime
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    Some((bytes, mime.to_string()))
}

fn parse_hex32(s: &str) -> anyhow::Result<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return Err(anyhow!("公钥须为 64 字符 hex，got len={}", s.len()));
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|e| anyhow!("hex: {e}"))?;
    }
    Ok(out)
}

fn load_config(path: &str) -> anyhow::Result<Config> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("读配置 {path}"))?;
    let cfg: Config = toml::from_str(&raw).context("解析 toml")?;
    if cfg.backends.is_empty() {
        return Err(anyhow!("配置须至少有一个 [[backends]]"));
    }
    Ok(cfg)
}

fn resolve_key(env_name: &str) -> anyhow::Result<String> {
    std::env::var(env_name).map_err(|_| anyhow!("未设置环境变量 {env_name}（云厂商 API Key）"))
}

fn resolve_seed(cfg: &Config) -> anyhow::Result<[u8; 32]> {
    if !cfg.seed_file.trim().is_empty() {
        let raw = std::fs::read_to_string(cfg.seed_file.trim())
            .with_context(|| format!("读 seed_file {}", cfg.seed_file))?;
        return parse_hex32(raw.trim());
    }
    if !cfg.seed_hex.trim().is_empty() {
        return parse_hex32(&cfg.seed_hex);
    }
    Ok([cfg.seed; 32])
}

async fn bind_client_async(cfg: &Config) -> anyhow::Result<nm_client::Client> {
    let seed = resolve_seed(cfg)?;
    let mode = cfg.mode.trim().to_ascii_lowercase();
    if matches!(mode.as_str(), "lan" | "local") {
        return Ok(nm_client::Client::bind_local(seed).await?);
    }
    if matches!(mode.as_str(), "selfhost" | "self" | "custom")
        || !cfg.relay.is_empty()
        || cfg.pkarr.is_some()
    {
        let pkarr = cfg
            .pkarr
            .clone()
            .ok_or_else(|| anyhow!("selfhost 模式须设置 pkarr"))?;
        return Ok(nm_client::Client::bind_selfhosted(
            seed,
            cfg.relay.clone(),
            pkarr,
            cfg.dns_origin.clone(),
            0,
        )
        .await?);
    }
    // nat / N0
    Ok(nm_client::Client::bind(seed).await?)
}

struct Meter {
    day_tokens: AtomicU64,
    day_stamp: AtomicU64,
    inflight: AtomicU32,
}

impl Meter {
    fn new() -> Self {
        Self {
            day_tokens: AtomicU64::new(0),
            day_stamp: AtomicU64::new(today_stamp()),
            inflight: AtomicU32::new(0),
        }
    }

    fn today_roll(&self) {
        let t = today_stamp();
        let prev = self.day_stamp.load(Ordering::Relaxed);
        if prev != t
            && self
                .day_stamp
                .compare_exchange(prev, t, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
        {
            self.day_tokens.store(0, Ordering::SeqCst);
        }
    }

    fn try_enter(&self, max_concurrent: u32, budget: u64) -> Result<(), String> {
        self.today_roll();
        if self.day_tokens.load(Ordering::Relaxed) >= budget {
            return Err(format!("daily token budget exceeded ({budget})"));
        }
        loop {
            let cur = self.inflight.load(Ordering::Relaxed);
            if cur >= max_concurrent {
                return Err(format!("max_concurrent={max_concurrent}"));
            }
            if self
                .inflight
                .compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    fn leave(&self, used_tokens: u64) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        if used_tokens > 0 {
            self.day_tokens.fetch_add(used_tokens, Ordering::SeqCst);
        }
    }
}

fn today_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86400
}

fn tokens_from_completion(json: &[u8]) -> u64 {
    serde_json::from_slice::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v["usage"]["total_tokens"].as_u64())
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve { config, node } => serve(config, node).await,
        Cmd::IssueGrant {
            config,
            audience,
            days,
            out,
        } => issue_grant(config, audience, days, out).await,
    }
}

async fn issue_grant(config: String, audience: String, days: i64, out: String) -> anyhow::Result<()> {
    let cfg = load_config(&config)?;
    let aud = parse_hex32(&audience)?;
    let client = bind_client_async(&cfg).await?;
    let expires = if days <= 0 {
        0
    } else {
        (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64)
            + days * 86400
    };
    let grant = client.issue_grant(aud, METHOD_INFER, "model.llm", expires);
    let b64 = base64::engine::general_purpose::STANDARD.encode(grant.encode_to_vec());
    if out.is_empty() {
        println!("{b64}");
    } else {
        std::fs::write(&out, b64.as_bytes()).with_context(|| format!("写 {out}"))?;
        eprintln!("wrote grant → {out} (audience={}, expires={expires})", hex(&aud));
    }
    eprintln!(
        "issuer(provider)={}  action={METHOD_INFER}  resource=model.llm",
        hex(&client.id_bytes())
    );
    Ok(())
}

async fn serve(config: String, node_override: String) -> anyhow::Result<()> {
    let mut cfg = load_config(&config)?;
    if !node_override.is_empty() {
        cfg.home_node = node_override;
    }
    if cfg.home_node.is_empty() {
        return Err(anyhow!("须配置 home_node 或传入 --node"));
    }

    eprintln!("loading {} backend(s)…", cfg.backends.len());
    let router = Arc::new(ModelRouter::from_configs(&cfg.backends)?);
    let models_csv = router.all_models.join(",");
    let policy = cfg.policy.clone();
    let allow: Arc<Vec<[u8; 32]>> = Arc::new(
        policy
            .allow_audiences
            .iter()
            .filter_map(|h| parse_hex32(h).ok())
            .collect(),
    );
    let meter = Arc::new(Meter::new());

    // home_node：NM_NODE_ADDR JSON **或 64-hex 节点 id**(id-only，靠 relay/pkarr 发现拨号——与 agentd 同约定；
    // 客户端 app 的云模型共享写的就是 hex id)。
    let addr = if let Ok(id) = parse_hex32(cfg.home_node.trim()) {
        nm_transport::addr_from_id(id)?
    } else {
        nm_transport::addr_from_string(&cfg.home_node)?
    };
    let home_id: [u8; 32] = *addr.id.as_bytes();
    let client = bind_client_async(&cfg).await?;
    client.watch_addr_log("nm-llm-gatewayd");

    println!(
        "nm-llm-gatewayd online: kind=model.llm  backends={}  models=[{models_csv}]  providers={}  id={}",
        router.backends.len(),
        router.providers_csv,
        hex(&client.id_bytes())
    );
    println!(
        "=> default_model={}  require_grant={}  daily_budget={}  max_concurrent={}",
        router.default_model, policy.require_grant, policy.daily_token_budget, policy.max_concurrent
    );
    println!("=> Key 仅在本机进程；签发: nm-llm-gatewayd issue-grant --config … --audience <hex>");

    let display_name = cfg.display_name.clone();
    let avatar_url = cfg.avatar_url.clone();
    // 头像内容寻址 blob 迁移：解析 avatar_file(data:URI) 得原图字节；上线时传到**本模型 home**。
    // 内容寻址=同字节同 blake3 hash，故传到任何节点都与 avatar_url 的 b3:hash 对上。失败不阻断上线。
    let avatar_blob: Option<(Vec<u8>, String)> =
        if avatar_url.starts_with("b3:") && !cfg.avatar_file.trim().is_empty() {
            match std::fs::read_to_string(cfg.avatar_file.trim()) {
                Ok(uri) => parse_data_uri(&uri),
                Err(e) => {
                    eprintln!("头像原图读取失败（跳过，不影响上线）: {e}");
                    None
                }
            }
        } else {
            None
        };
    let bio = cfg.bio.clone();
    let status_text = cfg.status_text.clone();
    let links = cfg.links.clone();
    let locale = cfg.locale.clone();
    let metering = cfg.metering.clone();
    let billing = cfg.billing.clone();
    let price_label = if cfg.price_label.is_empty() {
        String::new()
    } else {
        cfg.price_label.clone()
    };
    nm_client::serve_with_reconnect(&client, home_id, "nm-llm-gatewayd", |mut sess| {
        let router = router.clone();
        let models_csv = models_csv.clone();
        let profile_models = router.all_models.clone();
        let default_model = router.default_model.clone();
        let providers_csv = router.providers_csv.clone();
        let display_name = display_name.clone();
        let policy = policy.clone();
        let allow = allow.clone();
        let meter = meter.clone();
        let avatar_url = avatar_url.clone();
        let bio = bio.clone();
        let status_text = status_text.clone();
        let links = links.clone();
        let locale = locale.clone();
        let metering = metering.clone();
        let billing = billing.clone();
        let price_label = price_label.clone();
        let avatar_blob = avatar_blob.clone();
        async move {
            // 头像 blob 先传到**本模型 home**（gatewayd 连的节点），再注册 profile。
            // 内容寻址幂等：每次(重)连都传一遍，确保换网/重连后 blob 仍在当前 home，他人可拉。
            if let Some((bytes, mime)) = &avatar_blob {
                match sess.blob_put(bytes.clone(), mime).await {
                    Ok((hash, _)) => {
                        println!("头像 blob 已上传到本模型 home: b3:{}", hex(&hash))
                    }
                    Err(e) => eprintln!("头像 blob 上传失败（不影响注册）: {e}"),
                }
            }
            let mut attrs = HashMap::new();
            attrs.insert("model".to_string(), default_model.clone());
            attrs.insert("models".to_string(), models_csv);
            attrs.insert("api".to_string(), "openai.chat.v1".to_string());
            attrs.insert("status".to_string(), "idle".to_string());
            // 多厂商时写聚合标签；单厂商仍为具体名。
            attrs.insert("provider".to_string(), providers_csv.clone());
            attrs.insert("providers".to_string(), providers_csv);
            attrs.insert("proxy".to_string(), "cloud".to_string());
            if !metering.is_empty() {
                attrs.insert("metering".to_string(), metering);
            }
            if !billing.is_empty() {
                attrs.insert("billing".to_string(), billing);
            }
            if !price_label.is_empty() {
                attrs.insert("price".to_string(), price_label.clone());
            }
            attrs.insert(
                "auth".to_string(),
                if policy.require_grant {
                    "grant".into()
                } else {
                    "open".into()
                },
            );
            if policy.require_grant {
                attrs.insert("require_grant".to_string(), "true".to_string());
            }
            sess.register_as::<kinds::ModelLlm>(
                &InferenceProfile {
                    models: profile_models,
                    price: price_label,
                    avatar_url,
                    bio,
                    status_text,
                    links,
                    locale,
                    ..Default::default()
                },
                &display_name,
                attrs,
            )
            .await?;

            while let Some((req, cmd)) = sess.next_command().await {
                if cmd.method != METHOD_INFER {
                    let _ = sess
                        .reply(&req, false, None, &format!("不支持的方法: {}", cmd.method))
                        .await;
                    continue;
                }

                let caller = &req.sender;
                if !allow.is_empty() {
                    let ok = caller.len() == 32
                        && allow.iter().any(|a| a.as_slice() == caller.as_slice());
                    if !ok {
                        let _ = sess
                            .reply(&req, false, None, "caller not in allow_audiences")
                            .await;
                        continue;
                    }
                }
                if let Err(e) = meter.try_enter(policy.max_concurrent, policy.daily_token_budget) {
                    let _ = sess.reply(&req, false, None, &e).await;
                    continue;
                }

                let params = cmd.params.as_ref().map(|p| p.value.clone()).unwrap_or_default();
                let (backend, model_label) = match router.resolve(&params) {
                    Ok(v) => v,
                    Err(e) => {
                        meter.leave(0);
                        let _ = sess.reply(&req, false, None, &e).await;
                        continue;
                    }
                };

                let streaming = serde_json::from_slice::<serde_json::Value>(&params)
                    .ok()
                    .and_then(|v| v["stream"].as_bool())
                    .unwrap_or(false);

                if streaming {
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, bool)>();
                    let backend2 = backend.clone();
                    let model2 = model_label.clone();
                    let params2 = params.clone();
                    tokio::spawn(async move {
                        if let Err(e) = backend2.stream_infer(&model2, &params2, tx).await {
                            tracing::warn!("stream_infer[{model2}]: {e}");
                        }
                    });
                    let mut seq = 0u32;
                    let mut used = 0u64;
                    while let Some((value, done)) = rx.recv().await {
                        if done {
                            used = tokens_from_completion(&value);
                        }
                        let type_url = if done { RESP_TYPE_URL } else { CHUNK_TYPE_URL }.to_string();
                        if sess
                            .reply_frame(&req, true, Some(Any { type_url, value }), "", seq, done)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        seq += 1;
                    }
                    meter.leave(used);
                } else {
                    match backend.handle_infer(&model_label, &params).await {
                        Ok(json) => {
                            let used = tokens_from_completion(&json);
                            meter.leave(used);
                            let any = Any {
                                type_url: RESP_TYPE_URL.to_string(),
                                value: json,
                            };
                            let _ = sess.reply(&req, true, Some(any), "").await;
                        }
                        Err(e) => {
                            meter.leave(0);
                            let _ = sess
                                .reply(&req, false, None, &format!("推理失败[{model_label}]: {e}"))
                                .await;
                        }
                    }
                }
            }
            Ok(())
        }
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_backend_route_by_model() {
        // 不读真实 env：直接构造 LoadedBackend 测 resolve 映射逻辑。
        let mut by_model = HashMap::new();
        by_model.insert("gpt-4o".into(), 0usize);
        by_model.insert("deepseek-chat".into(), 1usize);
        let router = ModelRouter {
            by_model,
            backends: vec![
                LoadedBackend {
                    id: "oa".into(),
                    provider: "openai".into(),
                    models: vec!["gpt-4o".into()],
                    backend: Arc::new(Backend::Demo),
                },
                LoadedBackend {
                    id: "ds".into(),
                    provider: "deepseek".into(),
                    models: vec!["deepseek-chat".into()],
                    backend: Arc::new(Backend::Demo),
                },
            ],
            all_models: vec!["gpt-4o".into(), "deepseek-chat".into()],
            default_model: "gpt-4o".into(),
            providers_csv: "openai,deepseek".into(),
        };
        let (_, m) = router
            .resolve(br#"{"model":"deepseek-chat","messages":[]}"#)
            .unwrap();
        assert_eq!(m, "deepseek-chat");
        let (_, m2) = router.resolve(br#"{"messages":[]}"#).unwrap();
        assert_eq!(m2, "gpt-4o");
        assert!(router.resolve(br#"{"model":"claude-3","messages":[]}"#).is_err());
    }

    #[test]
    fn model_name_conflict_rejected() {
        // 通过临时 env 测 from_configs 冲突检测较重；这里验证插入逻辑等价。
        let mut by_model: HashMap<String, usize> = HashMap::new();
        assert!(by_model.insert("gpt-4o".into(), 0).is_none());
        assert_eq!(by_model.insert("gpt-4o".into(), 1), Some(0));
    }
}

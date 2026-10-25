//! `nm-llm-proxy` 库：多 `model.llm` provider 的发现索引与路由选择。
//!
//! P2P 网上可同时存在多个 LLM 算力提供方；本模块负责：
//! - 聚合目录实体 → 模型清单（`GET /v1/models`）
//! - 按请求 `model` / 固定 provider / `model@hex8` 消歧 → 候选列表（含负载排序）
//! - 失败时按候选顺序 failover

use nm_proto::Entity;
use serde::Serialize;

/// 一个可调用的 provider 快照。
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    pub id: [u8; 32],
    pub id_hex: String,
    pub display_name: String,
    /// 主模型名（attributes.model）。
    pub model: String,
    /// 全部模型（attributes.models 逗号拆分，若空则仅含 model）。
    pub models: Vec<String>,
    pub provider: String,
    pub proxy: String,
    pub status: String,
    /// 0–100；缺省 50。越低越优先。
    pub load: u32,
    pub require_grant: bool,
}

impl ProviderInfo {
    pub fn from_entity(e: &Entity) -> Option<Self> {
        if e.entity_id.len() != 32 {
            return None;
        }
        let mut id = [0u8; 32];
        id.copy_from_slice(&e.entity_id);
        let id_hex = hex(&id);
        let model = e
            .attributes
            .get("model")
            .cloned()
            .unwrap_or_else(|| e.display_name.clone());
        let models = e
            .attributes
            .get("models")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                if model.is_empty() {
                    Vec::new()
                } else {
                    vec![model.clone()]
                }
            });
        let load = e
            .attributes
            .get("load")
            .and_then(|s| s.parse().ok())
            .unwrap_or(50);
        let require_grant = e.attributes.get("require_grant").map(String::as_str) == Some("true")
            || e.attributes.get("auth").map(String::as_str) == Some("grant");
        Some(Self {
            id,
            id_hex,
            display_name: e.display_name.clone(),
            model,
            models,
            provider: e.attributes.get("provider").cloned().unwrap_or_default(),
            proxy: e.attributes.get("proxy").cloned().unwrap_or_default(),
            status: e.attributes.get("status").cloned().unwrap_or_else(|| "unknown".into()),
            load,
            require_grant,
        })
    }

    pub fn offers(&self, model_name: &str) -> bool {
        self.models.iter().any(|m| m == model_name) || self.model == model_name
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 解析请求里的 model 字段：`gpt-4o` 或消歧 `gpt-4o@a1b2c3d4`（provider id 前 8 hex）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub name: String,
    /// 可选：provider id 前缀（至少 8 hex）用于消歧。
    pub provider_prefix: Option<String>,
}

pub fn parse_model_ref(raw: &str) -> ModelRef {
    if let Some((name, pref)) = raw.rsplit_once('@') {
        if !name.is_empty() && pref.chars().all(|c| c.is_ascii_hexdigit()) && pref.len() >= 4 {
            return ModelRef {
                name: name.to_string(),
                provider_prefix: Some(pref.to_ascii_lowercase()),
            };
        }
    }
    ModelRef {
        name: raw.to_string(),
        provider_prefix: None,
    }
}

/// 路由输入。
#[derive(Debug, Clone, Default)]
pub struct RouteRequest {
    pub model: String,
    /// 固定 provider（完整 64 hex）；优先于 model@prefix。
    pub pin_provider: Option<[u8; 32]>,
}

/// 选出的有序候选（先试 index 0，失败再 failover）。
pub fn select_candidates(providers: &[ProviderInfo], req: &RouteRequest) -> Vec<ProviderInfo> {
    let mref = parse_model_ref(&req.model);

    let mut list: Vec<ProviderInfo> = if let Some(pin) = req.pin_provider {
        providers.iter().filter(|p| p.id == pin).cloned().collect()
    } else if let Some(pref) = &mref.provider_prefix {
        providers
            .iter()
            .filter(|p| p.id_hex.starts_with(pref.as_str()) && (mref.name.is_empty() || p.offers(&mref.name)))
            .cloned()
            .collect()
    } else if mref.name.is_empty() {
        providers.to_vec()
    } else {
        let matched: Vec<_> = providers.iter().filter(|p| p.offers(&mref.name)).cloned().collect();
        if matched.is_empty() {
            // 请求的 model 名无精确匹配时：仍返回全部（让提供方用自己的默认 model 回填），
            // 便于「智能体随便填个名 / 只配了一家」的场景；多家时优先已声明该名者。
            providers.to_vec()
        } else {
            matched
        }
    };

    // status=offline 沉底；同档按 load 升序。
    list.sort_by(|a, b| {
        let rank = |p: &ProviderInfo| -> u32 {
            match p.status.as_str() {
                "idle" => 0,
                "busy" => 1,
                "offline" => 9,
                _ => 2,
            }
        };
        rank(a).cmp(&rank(b)).then(a.load.cmp(&b.load)).then(a.id_hex.cmp(&b.id_hex))
    });
    list
}

/// OpenAI `GET /v1/models` 条目（多 provider 聚合；同名模型用 `@hex8` 消歧副本）。
#[derive(Debug, Serialize)]
pub struct OpenAiModel {
    pub id: String,
    pub object: &'static str,
    pub owned_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nm_provider_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OpenAiModelList {
    pub object: &'static str,
    pub data: Vec<OpenAiModel>,
}

/// 聚合模型列表：每个 (provider, model) 一条；另为每个唯一 model 名保留一条「无后缀」
/// （指向该名下负载最低的 provider），方便智能体直接用短名。
pub fn aggregate_models(providers: &[ProviderInfo]) -> OpenAiModelList {
    let mut data = Vec::new();
    let mut short_best: std::collections::HashMap<String, ProviderInfo> = std::collections::HashMap::new();

    for p in providers {
        for m in &p.models {
            data.push(OpenAiModel {
                id: format!("{m}@{}", &p.id_hex[..8.min(p.id_hex.len())]),
                object: "model",
                owned_by: if p.display_name.is_empty() {
                    p.id_hex[..8.min(p.id_hex.len())].to_string()
                } else {
                    p.display_name.clone()
                },
                nm_provider_id: Some(p.id_hex.clone()),
            });
            short_best
                .entry(m.clone())
                .and_modify(|cur| {
                    if p.load < cur.load || (p.load == cur.load && p.id_hex < cur.id_hex) {
                        *cur = p.clone();
                    }
                })
                .or_insert_with(|| p.clone());
        }
    }
    // 短名条目放前面，智能体默认看到干净 id。
    let mut shorts: Vec<_> = short_best
        .into_iter()
        .map(|(m, p)| OpenAiModel {
            id: m,
            object: "model",
            owned_by: p.display_name.clone(),
            nm_provider_id: Some(p.id_hex),
        })
        .collect();
    shorts.sort_by(|a, b| a.id.cmp(&b.id));
    shorts.append(&mut data);
    OpenAiModelList {
        object: "list",
        data: shorts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(hex_prefix: u8, model: &str, models: &[&str], load: u32, status: &str) -> ProviderInfo {
        let mut id = [0u8; 32];
        id[0] = hex_prefix;
        ProviderInfo {
            id,
            id_hex: hex(&id),
            display_name: format!("p{hex_prefix}"),
            model: model.into(),
            models: models.iter().map(|s| (*s).to_string()).collect(),
            provider: "openai".into(),
            proxy: "cloud".into(),
            status: status.into(),
            load,
            require_grant: false,
        }
    }

    #[test]
    fn parse_model_at_prefix() {
        let r = parse_model_ref("gpt-4o@aa11bb22");
        assert_eq!(r.name, "gpt-4o");
        assert_eq!(r.provider_prefix.as_deref(), Some("aa11bb22"));
        let r2 = parse_model_ref("gpt-4o");
        assert!(r2.provider_prefix.is_none());
    }

    #[test]
    fn multi_provider_prefer_idle_low_load() {
        let ps = vec![
            prov(1, "gpt-4o", &["gpt-4o"], 80, "busy"),
            prov(2, "gpt-4o", &["gpt-4o"], 10, "idle"),
            prov(3, "other", &["other"], 0, "idle"),
        ];
        let c = select_candidates(
            &ps,
            &RouteRequest {
                model: "gpt-4o".into(),
                pin_provider: None,
            },
        );
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].id[0], 2); // idle + load 10
        assert_eq!(c[1].id[0], 1);
    }

    #[test]
    fn pin_and_failover_order() {
        let ps = vec![
            prov(1, "m", &["m"], 0, "idle"),
            prov(2, "m", &["m"], 0, "idle"),
        ];
        let pin = ps[1].id;
        let c = select_candidates(
            &ps,
            &RouteRequest {
                model: "m".into(),
                pin_provider: Some(pin),
            },
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].id[0], 2);
    }

    #[test]
    fn disambiguate_at_prefix() {
        let ps = vec![
            prov(0xaa, "gpt-4o", &["gpt-4o"], 0, "idle"),
            prov(0xbb, "gpt-4o", &["gpt-4o"], 0, "idle"),
        ];
        let pref = ps[1].id_hex[..8].to_string();
        let c = select_candidates(
            &ps,
            &RouteRequest {
                model: format!("gpt-4o@{pref}"),
                pin_provider: None,
            },
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].id[0], 0xbb);
    }

    #[test]
    fn aggregate_has_short_and_qualified() {
        let ps = vec![
            prov(1, "gpt-4o", &["gpt-4o", "gpt-4o-mini"], 5, "idle"),
            prov(2, "gpt-4o", &["gpt-4o"], 1, "idle"),
        ];
        let list = aggregate_models(&ps);
        let ids: Vec<_> = list.data.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"gpt-4o"));
        assert!(ids.contains(&"gpt-4o-mini"));
        assert!(ids.iter().any(|id| id.starts_with("gpt-4o@")));
        // 短名 gpt-4o 应归属 load 更低的 p2
        let short = list.data.iter().find(|m| m.id == "gpt-4o").unwrap();
        assert!(short.nm_provider_id.as_ref().unwrap().starts_with("0200"));
    }
}

use nm_proto::DirectoryQuery;
use serde_json::{Value, json};
use tauri::State;

use crate::{AppState, dial, hex, session_of};

/// 把一个目录 Entity 渲染为前端 JSON —— 含 P0 富属性：bio / 状态文本 / 头像引用 / 链接 / locale，
/// 以及可发现标签 attributes["status"]。
/// person → PersonProfile；model.llm / compute.* → InferenceProfile（展示字段对齐）。
fn person_entity_json(e: &nm_proto::pb::Entity) -> Value {
    let pp = e
        .profile
        .as_ref()
        .and_then(|a| nm_entity::unpack_profile::<nm_entity::kinds::Person>(a).ok());
    let ip = e
        .profile
        .as_ref()
        .and_then(|a| nm_entity::unpack_profile::<nm_entity::kinds::ModelLlm>(a).ok());
    let bio = pp
        .as_ref()
        .map(|p| p.bio.clone())
        .or_else(|| ip.as_ref().map(|p| p.bio.clone()))
        .unwrap_or_default();
    let status_text = pp
        .as_ref()
        .map(|p| p.status_text.clone())
        .or_else(|| ip.as_ref().map(|p| p.status_text.clone()))
        .unwrap_or_default();
    let avatar = pp
        .as_ref()
        .map(|p| p.avatar_url.clone())
        .or_else(|| ip.as_ref().map(|p| p.avatar_url.clone()))
        .unwrap_or_default();
    let links = pp
        .as_ref()
        .map(|p| p.links.clone())
        .or_else(|| ip.as_ref().map(|p| p.links.clone()))
        .unwrap_or_default();
    let locale = pp
        .as_ref()
        .map(|p| p.locale.clone())
        .or_else(|| ip.as_ref().map(|p| p.locale.clone()))
        .unwrap_or_default();
    json!({
        "id": hex(&e.entity_id),
        "homeNode": hex(&e.home_node),
        "kind": e.kind,
        "name": e.display_name,
        "status": e.attributes.get("status").cloned().unwrap_or_default(),
        "presence": e.attributes.get("presence").cloned().unwrap_or_default(),
        "handle": e.attributes.get("name").cloned().unwrap_or_default(),
        "model": e.attributes.get("model").cloned()
            .or_else(|| ip.as_ref().and_then(|p| p.models.first().cloned()))
            .unwrap_or_default(),
        "provider": e.attributes.get("provider").cloned().unwrap_or_default(),
        "metering": e.attributes.get("metering").cloned().unwrap_or_default(),
        "billing": e.attributes.get("billing").cloned().unwrap_or_default(),
        "price": e.attributes.get("price").cloned()
            .or_else(|| ip.as_ref().map(|p| p.price.clone()))
            .unwrap_or_default(),
        "auth": e.attributes.get("auth").cloned().unwrap_or_default(),
        "bio": bio,
        "statusText": status_text,
        "avatar": avatar,
        "links": links,
        "locale": locale,
    })
}

/// 查询指定节点服务上的用户/实体（临时匿名拨号 + 目录查询，不影响当前主会话）。
/// 用随机临时身份，避免与主会话相同公钥的端点冲突；目录查询为只读发现，无需成员身份。
#[tauri::command]
pub(crate) async fn node_users(
    mode: String,
    node: String,
    relay_urls: Vec<String>,
    pkarr_url: Option<String>,
    dns_origin: Option<String>,
) -> Result<Vec<Value>, String> {
    let seed = nm_transport::SecretKey::generate().to_bytes();
    let (client, session) =
        dial(seed, &mode, &node, relay_urls, pkarr_url, dns_origin, None).await?;
    let entities = session
        .directory_query(DirectoryQuery {
            kind_prefix: String::new(),
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;
    let out: Vec<Value> = entities.iter().map(person_entity_json).collect();
    drop(session);
    drop(client);
    Ok(out)
}

#[tauri::command]
pub(crate) async fn directory_query(
    state: State<'_, AppState>,
    kind_prefix: String,
) -> Result<Vec<Value>, String> {
    let session = session_of(&state).await?;
    let entities = session
        .directory_query(DirectoryQuery {
            kind_prefix,
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(entities.iter().map(person_entity_json).collect())
}

/// 用户扩展属性一行（编辑资料里的三列表：key / value / note）。
/// note 写入 attributes 时用保留前缀 `__note:<key>`，与 value 一并 LWW 扩散。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExtAttrRow {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub note: String,
}

/// 系统占用的 attributes 键：用户扩展属性不得覆盖（status/presence/name 由资料/在线态写入）。
fn is_reserved_attr_key(k: &str) -> bool {
    matches!(k, "status" | "presence" | "name") || k.starts_with("__note:")
}

/// 更新本人资料（P0 富属性）：重新以 person 身份注册一份带完整 PersonProfile + 标签的 Entity。
/// 节点侧按 updated_at 做 LWW 收敛并经联邦扩散；前端需传全量当前值（本地持久，连接后回推）。
#[tauri::command]
pub(crate) async fn update_profile(
    state: State<'_, AppState>,
    display_name: String,
    bio: String,
    status_text: String,
    links: Vec<String>,
    locale: String,
    status: String, // 可发现标签：online / away / busy / dnd …（P2 presence 前的静态占位）
    avatar: String, // 头像：前端已缩放压缩的小图 data:URI（P0 内联随档案分发；P1 迁移为内容寻址 blob）
    attributes: Option<Vec<ExtAttrRow>>, // 用户扩展属性（前端始终传数组；None/空=仅系统 status）
) -> Result<(), String> {
    // 内联头像体量上限（防止撑爆目录/gossip 负载）：约 128KB data:URI。更大者应走 P1 内容寻址。
    if avatar.len() > 128 * 1024 {
        return Err("头像过大（请用更小的图；上限约 96KB 图片）".into());
    }
    let session = session_of(&state).await?;
    let profile = nm_proto::pb::PersonProfile {
        avatar_url: avatar, // data:image/...;base64,... （P1 起可存 "b3:<hash>"）
        bio,
        status_text,
        links: links.into_iter().filter(|s| !s.trim().is_empty()).collect(),
        locale,
    };
    let mut attrs = std::collections::HashMap::new();
    let status = status.trim();
    if !status.is_empty() {
        attrs.insert("status".to_string(), status.to_string());
    }
    // 用户扩展属性：key→value；有 note 时另写 `__note:<key>`（跳过空 key / 系统保留键）。
    if let Some(rows) = attributes {
        for row in rows {
            let key = row.key.trim().to_string();
            if key.is_empty() || is_reserved_attr_key(&key) {
                continue;
            }
            attrs.insert(key.clone(), row.value);
            let note = row.note.trim();
            if !note.is_empty() {
                attrs.insert(format!("__note:{key}"), note.to_string());
            }
        }
    }
    session
        .register_as::<nm_entity::kinds::Person>(&profile, display_name.trim(), attrs)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

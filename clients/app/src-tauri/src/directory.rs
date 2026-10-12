use nm_proto::DirectoryQuery;
use serde_json::{Value, json};
use tauri::State;

use crate::{AppState, dial, hex, session_of};

/// 把一个目录 Entity 渲染为前端 JSON —— 含 P0 富属性：bio / 状态文本 / 头像引用 / 链接 / locale，
/// 以及可发现标签 attributes["status"]。非 person 类型解不出 PersonProfile 时相应字段为空。
fn person_entity_json(e: &nm_proto::pb::Entity) -> Value {
    let pp = e
        .profile
        .as_ref()
        .and_then(|a| nm_entity::unpack_profile::<nm_entity::kinds::Person>(a).ok());
    json!({
        "id": hex(&e.entity_id),
        "homeNode": hex(&e.home_node),
        "kind": e.kind,
        "name": e.display_name,
        "status": e.attributes.get("status").cloned().unwrap_or_default(),
        "presence": e.attributes.get("presence").cloned().unwrap_or_default(),
        "handle": e.attributes.get("name").cloned().unwrap_or_default(), // 去中心命名 local@domain（N1）
        "model": e.attributes.get("model").cloned().unwrap_or_default(), // 模型 provider / agent 所用模型（智能体标签副标题）
        "bio": pp.as_ref().map(|p| p.bio.clone()).unwrap_or_default(),
        "statusText": pp.as_ref().map(|p| p.status_text.clone()).unwrap_or_default(),
        "avatar": pp.as_ref().map(|p| p.avatar_url.clone()).unwrap_or_default(),
        "links": pp.as_ref().map(|p| p.links.clone()).unwrap_or_default(),
        "locale": pp.as_ref().map(|p| p.locale.clone()).unwrap_or_default(),
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
    session
        .register_as::<nm_entity::kinds::Person>(&profile, display_name.trim(), attrs)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

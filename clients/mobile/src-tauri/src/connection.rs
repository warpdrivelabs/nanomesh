use std::sync::Arc;

use nm_client::{Client, Session};
use prost::Message as _;
use serde_json::json;
use tauri::{AppHandle, Emitter, State, async_runtime};

use crate::{AppState, Conn, devices, hex, pair, pairing, parse_id, session_of, vk_of};

/// 完成会话建立：注册为 person、起消息推送循环、存入状态，返回自己的 id(hex)。
pub(crate) async fn finish_session(
    app: &AppHandle,
    state: &State<'_, AppState>,
    client: Client,
    mut session: Session,
    display_name: &str,
) -> Result<String, String> {
    session
        .register_as::<nm_entity::kinds::Person>(
            &nm_proto::pb::PersonProfile::default(),
            display_name.trim(),
            std::collections::HashMap::new(),
        )
        .await
        .map_err(|e| e.to_string())?;
    let my_id = session.id_bytes();
    let node_id = session.node_id();

    // 把消息流交给事件循环 → 推送到前端。
    let mut inbox = session.take_inbox().ok_or("inbox 已被占用")?;
    let session = Arc::new(session);
    let watch = session.clone();
    let apph = app.clone();
    let recv_session = session.clone();
    async_runtime::spawn(async move {
        while let Some(gram) = inbox.recv().await {
            if let Some(p) = gram
                .payload
                .as_ref()
                .filter(|p| p.type_url.starts_with(pair::PREFIX))
            {
                pairing::on_old_side(&apph, &p.type_url, &p.value, &gram.sender).await;
                continue;
            }
            if let Some(p) = gram
                .payload
                .as_ref()
                .filter(|p| p.type_url == devices::NODE_EVENT_TYPE)
            {
                if gram.sender.as_slice() == node_id.as_slice() {
                    devices::node_event(&apph, &p.value);
                }
                continue;
            }
            if let Some(p) = gram
                .payload
                .as_ref()
                .filter(|p| p.type_url == "nmspace.v1/group.updated")
            {
                let body: serde_json::Value =
                    serde_json::from_slice(&p.value).unwrap_or_else(|_| json!({}));
                let _ = apph.emit(
                    "core://event",
                    json!({
                        "type": "group_updated",
                        "groupId": body.get("groupId").and_then(|v| v.as_str()).unwrap_or(""),
                        "name": body.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "topic": body.get("topic").and_then(|v| v.as_str()).unwrap_or(""),
                        "avatar": body.get("avatar").and_then(|v| v.as_str()).unwrap_or(""),
                        "updatedAt": body.get("updatedAt").and_then(|v| v.as_i64()).unwrap_or(0),
                    }),
                );
                continue;
            }
            if matches!(gram.kind(), nm_proto::GramKind::Receipt) {
                if let Some(refid) = gram.ref_gram_id {
                    let _ = apph.emit("core://event", json!({
                        "type": "receipt", "ref": refid, "from": hex(&gram.sender), "ts": gram.timestamp_ms
                    }));
                }
                continue;
            }
            // RPC 协议帧绝不渲染为聊天消息：跨节点流式推理的 chunk 帧(CommandResult/openai.chat.chunk.v1)
            // 在流已关闭后迟到/重复投递时会落到 inbox——设计即「投不到则丢弃，可靠性=客户端 pending+超时」。
            // 此前缺这层过滤，被当普通消息显示成乱码气泡。
            if matches!(
                gram.kind(),
                nm_proto::GramKind::Command | nm_proto::GramKind::CommandResult
            ) {
                continue;
            }
            // P1-6 多设备已读：StateSync 的"已读标记"gram（payload 是 protobuf，非 UTF-8）。须在通用消息分支前拦截，
            // 否则会被当普通消息乱显示。解出会话+已读位，发独立 read 事件给前端推进未读。
            if let Some(p) = gram
                .payload
                .as_ref()
                .filter(|p| p.type_url == "nmspace.v1/read.marker")
            {
                if let Ok(m) = nm_proto::ReadMarker::decode(p.value.as_slice()) {
                    let _ = apph.emit(
                        "core://event",
                        json!({
                            "type": "read", "conversation": hex(&m.conversation), "readUpToMs": m.read_up_to_ms
                        }),
                    );
                }
                continue;
            }
            if matches!(gram.kind(), nm_proto::GramKind::Message) && gram.sender.as_slice() != my_id.as_slice() {
                let s = recv_session.clone();
                let g = gram.clone();
                async_runtime::spawn(async move { let _ = s.send_receipt(&g).await; });
            }
            let (type_url, body) = match gram.payload.as_ref() {
                Some(p) => (
                    p.type_url.clone(),
                    String::from_utf8_lossy(&p.value).to_string(),
                ),
                None => (String::new(), String::new()),
            };
            let ev = json!({
                "type": "message",
                "msg": {
                    // gram_id 是发送方每个连接从 1 起的计数，重连会重复；拼上发送方与时间戳才唯一
                    "id": format!("{}-{}-{}", hex(&gram.sender), gram.timestamp_ms, gram.gram_id),
                    "from": hex(&gram.sender),
                    "to": hex(&gram.receiver),                 // 群消息=群id；频道=频道id；私聊=本人id
                    "group": matches!(gram.kind(), nm_proto::GramKind::GroupMessage),
                    "channel": matches!(gram.kind(), nm_proto::GramKind::ChannelPublish),
                    "typeUrl": type_url,
                    "body": body,
                    "ts": gram.timestamp_ms,
                }
            });
            let _ = apph.emit("core://event", ev);
        }
        if watch
            .close_reason()
            .is_some_and(|r| r.contains("device_revoked"))
        {
            let _ = apph.emit(devices::EVENT, json!({ "event": "self_revoked" }));
        }
    });

    *state.conn.lock().await = Some(Conn {
        _client: client,
        session,
        my_id,
    });
    Ok(hex(&my_id))
}

/// 连接节点：**同网优先、穿透兜底**。
/// - `lan`：仅 Minimal 直连（按地址，无 NAT 兜底）。
/// - `nat`/`selfhost`：**先试同网直连**（Minimal，零基础设施、可离线）——`node` 为完整地址(JSON)时启用；
///   同网失败或只给了公钥，则**转穿透 NAT**（发现服务 + 中继，按节点公钥拨号）。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    user: String, // 选定用户身份公钥 hex（空=用/建默认身份，向后兼容）
    mode: String, // "nat" | "selfhost" | "lan"
    node: String, // 节点公钥hex(64) 或 NM_NODE_ADDR(JSON)
    display_name: String,
    relay_urls: Vec<String>,    // selfhost（其余模式传 []）
    pkarr_url: Option<String>,  // selfhost 必填
    dns_origin: Option<String>, // selfhost 可选
) -> Result<String, String> {
    let vk = vk_of(&state)?;
    let user = user.trim();
    if user.is_empty() {
        return Err("请先选择用户身份".into());
    }
    let (client, session) = devices::dial_account(
        &app, &vk, user, &mode, &node, relay_urls, pkarr_url, dns_origin,
    )
    .await?;
    finish_session(&app, &state, client, session, &display_name).await
}

/// 上线：给了设备证书则以设备密钥连接后出示证书（账号身份收发），否则按连接密钥本身上线。
pub(crate) async fn go_online(
    client: &Client,
    addr: nm_transport::Addr,
    cert: Option<&nm_proto::DeviceCert>,
) -> Result<Session, String> {
    match cert {
        Some(c) => client.online_as(addr, c).await,
        None => client.online(addr).await,
    }
    .map_err(|e| e.to_string())
}

/// 拨号到一个节点（同网优先、穿透兜底），返回 (客户端, 会话)。connect 与 node_users 共用。
pub(crate) async fn dial(
    seed: [u8; 32],
    mode: &str,
    node: &str,
    relay_urls: Vec<String>,
    pkarr_url: Option<String>,
    dns_origin: Option<String>,
    cert: Option<&nm_proto::DeviceCert>,
) -> Result<(Client, Session), String> {
    let m = mode.trim().to_ascii_lowercase();
    let node = node.trim();

    // 纯同网模式：Minimal + 按地址直连（无 NAT 兜底）。
    if matches!(m.as_str(), "lan" | "local") {
        let addr = nm_transport::addr_from_string(node).map_err(|e| e.to_string())?;
        let client = Client::bind_local(seed).await.map_err(|e| e.to_string())?;
        let session = go_online(&client, addr, cert).await?;
        return Ok((client, session));
    }

    // nat/selfhost：输入可为公钥(hex) 或完整地址(JSON)。地址先试同网直连，失败转穿透。
    let (id, lan_addr) = match nm_transport::addr_from_string(node) {
        Ok(a) => (*a.id.as_bytes(), Some(a)),
        Err(_) => (parse_id(node)?, None),
    };
    if let Some(addr) = &lan_addr {
        if let Ok(c1) = Client::bind_local(seed).await {
            match tokio::time::timeout(
                std::time::Duration::from_secs(3),
                go_online(&c1, addr.clone(), cert),
            )
            .await
            {
                Ok(Ok(session)) => return Ok((c1, session)),
                // 节点已明确拒绝（证书/吊销/版本），不必再走穿透重试。
                Ok(Err(e))
                    if e.contains("device_")
                        || e.contains("unknown method")
                        || e.contains("cert") =>
                {
                    return Err(e);
                }
                _ => {}
            }
        }
    }
    let client = match m.as_str() {
        "selfhost" | "self" | "custom" => {
            let pkarr = pkarr_url
                .filter(|s| !s.trim().is_empty())
                .ok_or("selfhost 模式需要填写 pkarr 端点(如 https://dns.example.com/pkarr)")?;
            let relays: Vec<String> = relay_urls
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect();
            Client::bind_selfhosted(
                seed,
                relays,
                pkarr,
                dns_origin.filter(|s| !s.trim().is_empty()),
                0,
            )
            .await
            .map_err(|e| e.to_string())?
        }
        _ => Client::bind(seed).await.map_err(|e| e.to_string())?, // nat(N0) 默认
    };
    let addr = nm_transport::addr_from_id(id).map_err(|e| e.to_string())?;
    let session = go_online(&client, addr, cert).await?;
    Ok((client, session))
}

#[tauri::command]
pub(crate) async fn send_to(
    state: State<'_, AppState>,
    target: String,
    text: String,
) -> Result<u64, String> {
    let id = parse_id(&target)?;
    let session = session_of(&state).await?;
    let ack = session
        .send_to(id, &text)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ack.ref_gram_id.unwrap_or(0))
}

//! App 聊天界面直聊大模型(本地直连 + 远端 P2P 统一):`llm_chat_stream` 按 target(模型 entityId)
//! 分派——命中本地模型表走 `nm_compute::Backend::openai(...).stream_infer`(SSE,Key 只在 Rust 解密);
//! 否则走 `Session::infer_stream_with_grant`(P2P → provider gatewayd,Grant/超时复用运行中代理配置)。
//! 两路都把增量折成 app 既有的 agent 流式帧 `nmspace.agent.delta.v1`(累计全文 + 递增 seq + done),
//! 经 `core://event` 以 `type:"message"` 形状发给前端——`handleAgentDelta` 原地长气泡、done 落库,
//! 渲染/持久化零新代码。取消:`llm_chat_cancel(streamId)`。

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;

use crate::state::{hex, parse_id, session_of, vk_of, AppState};
use crate::llm_share;

/// 与 nm-agent-bridge/im_events.js 对齐的流式帧 type_url。
const AGENT_DELTA_TYPE: &str = "nmspace.agent.delta.v1";
/// 远端分支默认超时(无运行中代理可借参数时)。
const DEFAULT_REMOTE_TIMEOUT_MS: u32 = 120_000;

/// 进行中的聊天流:streamId → 取消句柄。
pub(crate) type LlmChats = Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>;

#[derive(Debug, Deserialize)]
pub(crate) struct ChatMsgIn {
    pub role: String,
    pub content: String,
}

/// 把一帧(累计全文)按 connection.rs 的入站消息事件形状发给前端。
fn emit_delta(app: &AppHandle, from_hex: &str, to_hex: &str, stream_id: &str, seq: u32, text: &str, done: bool) {
    let body = json!({ "streamId": stream_id, "seq": seq, "text": text, "done": done }).to_string();
    let ts = nm_proto::now_ms();
    let ev = json!({
        "type": "message",
        "msg": {
            "id": format!("{from_hex}-{ts}-{seq}"),
            "from": from_hex,
            "to": to_hex,
            "group": false,
            "channel": false,
            "seq": seq,
            "typeUrl": AGENT_DELTA_TYPE,
            "body": body,
            "ts": ts,
        }
    });
    let _ = app.emit("core://event", ev);
}

/// 在本地模型表(llm_models/*.json)按 entityId 找 share;命中返回 (share, local_id)。
fn find_local_by_entity(app: &AppHandle, entity_hex: &str) -> Option<llm_share::ModelShare> {
    let dir = llm_share::models_dir(app).ok()?;
    let rd = std::fs::read_dir(&dir).ok()?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(id) = name.strip_suffix(".json") else { continue };
        if let Ok(s) = llm_share::load_share(&dir, id) {
            if s.entity_id.eq_ignore_ascii_case(entity_hex) {
                return Some(s);
            }
        }
    }
    None
}

/// 发起一次模型对话流。立即返回;增量经 core://event 推送(agent delta 帧,前端已有渲染)。
#[tauri::command]
pub(crate) async fn llm_chat_stream(
    app: AppHandle,
    state: State<'_, AppState>,
    target: String,
    messages: Vec<ChatMsgIn>,
    stream_id: String,
) -> Result<(), String> {
    let target_hex = target.trim().to_lowercase();
    let _ = parse_id(&target_hex)?; // 校验 64-hex
    if messages.is_empty() {
        return Err("messages 为空".into());
    }
    // 会话里的"我"(回帧 to=本人 id,与普通入站私聊一致;前端据 from 归会话)。
    let my_hex = match session_of(&state).await {
        Ok(s) => hex(&s.id_bytes()),
        Err(_) => String::new(), // 未连节点时本地模型仍可聊;to 留空不影响前端归属(按 from)
    };

    // 组装 OpenAI 请求体(stream:true;model 由各分支定)。
    let msgs: Vec<serde_json::Value> =
        messages.iter().map(|m| json!({ "role": m.role, "content": m.content })).collect();

    // 取消通道:先注册再 spawn,cancel 命令随时可达。
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    state.llm_chats.lock().await.insert(stream_id.clone(), cancel_tx);

    // ── 分支判定(拿齐参数后进 spawn,State 不入 'static 任务)──
    if let Some(share) = find_local_by_entity(&app, &target_hex) {
        // 本地直连:Key 仅此处(Rust)解密,绝不出进程。
        let vk = vk_of(&state)?;
        let dir = llm_share::models_dir(&app)?;
        let key = llm_share::load_key(&dir, &vk, &share.local_id)?;
        let backend = nm_compute::Backend::openai(share.base_url.clone(), key);
        let model = share.model.clone();
        let body = json!({ "model": model, "messages": msgs, "stream": true });
        let body_bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
        let (apph, sid, from_hex, to_hex) = (app.clone(), stream_id.clone(), target_hex.clone(), my_hex);
        tauri::async_runtime::spawn(async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, bool)>();
            let m2 = model.clone();
            let bk = backend;
            let pump = tauri::async_runtime::spawn(async move {
                if let Err(e) = bk.stream_infer(&m2, &body_bytes, tx).await {
                    tracing::warn!("local model stream_infer: {e}");
                }
            });
            let mut acc = String::new();
            let mut seq: u32 = 0;
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => break,
                    f = rx.recv() => {
                        let Some((bytes, done)) = f else { break };
                        if done {
                            // 终帧为完整 completion:以其 content 为准(防增量丢帧)。
                            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                                if let Some(c) = v["choices"][0]["message"]["content"].as_str() {
                                    acc = c.to_string();
                                }
                            }
                            break;
                        }
                        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                            if let Some(d) = v["delta"].as_str() {
                                if !d.is_empty() {
                                    acc.push_str(d);
                                    seq += 1;
                                    emit_delta(&apph, &from_hex, &to_hex, &sid, seq, &acc, false);
                                }
                            }
                        }
                    }
                }
            }
            pump.abort();
            if acc.is_empty() {
                acc = "⚠ 模型无输出(检查 Base URL/Key/模型名)".into();
            }
            emit_delta(&apph, &from_hex, &to_hex, &sid, seq + 1, &acc, true);
            if let Some(st) = apph.try_state::<AppState>() {
                st.llm_chats.lock().await.remove(&sid);
            }
        });
        return Ok(());
    }

    // 远端 P2P:须已连接节点;Grant/超时/model 借运行中代理的配置(有则)。
    let sess = session_of(&state).await.map_err(|e| format!("未连接节点: {e}"))?;
    let provider = parse_id(&target_hex)?;
    let (grant, timeout_ms, model_hint) = {
        let g = state.llm_proxies.lock().await;
        match g.get(&target_hex) {
            Some(p) => {
                let (grant, t, m) = p.chat_params().await;
                (grant, t.as_millis().min(u32::MAX as u128) as u32, m)
            }
            None => (None, DEFAULT_REMOTE_TIMEOUT_MS, String::new()),
        }
    };
    let mut body = json!({ "messages": msgs, "stream": true });
    if !model_hint.is_empty() {
        body["model"] = json!(model_hint);
    }
    let body_bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    let mut stream = sess
        .infer_stream_with_grant(provider, &body_bytes, grant, timeout_ms)
        .await
        .map_err(|e| e.to_string())?;
    let (apph, sid, from_hex, to_hex) = (app.clone(), stream_id.clone(), target_hex, my_hex);
    tauri::async_runtime::spawn(async move {
        let mut acc = String::new();
        let mut seq: u32 = 0;
        let mut err: Option<String> = None;
        loop {
            tokio::select! {
                _ = &mut cancel_rx => break,
                f = stream.next() => {
                    let Some(cr) = f else { break };
                    if !cr.ok {
                        err = Some(if cr.error.is_empty() { "provider 流式错误".into() } else { cr.error });
                        break;
                    }
                    if cr.done {
                        if let Some(a) = cr.result {
                            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&a.value) {
                                if let Some(c) = v["choices"][0]["message"]["content"].as_str() {
                                    acc = c.to_string();
                                }
                            }
                        }
                        break;
                    }
                    if let Some(a) = cr.result {
                        let d = serde_json::from_slice::<serde_json::Value>(&a.value)
                            .ok()
                            .and_then(|v| v["delta"].as_str().map(|s| s.to_string()));
                        if let Some(d) = d {
                            if !d.is_empty() {
                                acc.push_str(&d);
                                seq += 1;
                                emit_delta(&apph, &from_hex, &to_hex, &sid, seq, &acc, false);
                            }
                        }
                    }
                }
            }
        }
        if let Some(e) = err {
            acc = format!("⚠ {e}");
        } else if acc.is_empty() {
            acc = "⚠ 模型无输出(provider 未回内容或已超时)".into();
        }
        emit_delta(&apph, &from_hex, &to_hex, &sid, seq + 1, &acc, true);
        if let Some(st) = apph.try_state::<AppState>() {
            st.llm_chats.lock().await.remove(&sid);
        }
    });
    Ok(())
}

/// 取消进行中的对话流(气泡定格在已生成部分;done 帧由流任务收尾发出)。
#[tauri::command]
pub(crate) async fn llm_chat_cancel(
    state: State<'_, AppState>,
    stream_id: String,
) -> Result<bool, String> {
    Ok(state
        .llm_chats
        .lock()
        .await
        .remove(&stream_id)
        .map(|tx| tx.send(()).is_ok())
        .unwrap_or(false))
}

//! `nm-gateway` — browser WebSocket gateway for nmspace.
//!
//! Browsers cannot speak iroh/QUIC directly. This crate exposes a small JSON
//! WebSocket bridge at `/ws` and forwards requests through `nm-client`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use nm_client::{Client, Session};
use nm_proto::{Any, DirectoryQuery, Entity, Gram, Group};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad listen address: {0}")]
    BadListenAddr(String),
}

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub listen: SocketAddr,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:9620".parse().expect("valid default listen address"),
        }
    }
}

/// Start the WebSocket gateway using `NM_GATEWAY_LISTEN` or `127.0.0.1:9620`.
pub async fn serve() -> Result<(), GatewayError> {
    let listen = match std::env::var("NM_GATEWAY_LISTEN") {
        Ok(s) => s.parse().map_err(|_| GatewayError::BadListenAddr(s))?,
        Err(_) => GatewayConfig::default().listen,
    };
    serve_with(GatewayConfig { listen }).await
}

pub async fn serve_with(config: GatewayConfig) -> Result<(), GatewayError> {
    let app = Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws_handler));
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(listen = %config.listen, "nm-gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "service": "nm-gateway" }))
}

async fn ws_handler(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_socket).into_response()
}

struct GatewaySession {
    _client: Client,
    session: Arc<Session>,
    inbox_task: tokio::task::JoinHandle<()>,
}

type SharedSession = Arc<Mutex<Option<GatewaySession>>>;
type Outbound = mpsc::UnboundedSender<Value>;

async fn handle_socket(socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        while let Some(value) = out_rx.recv().await {
            if ws_tx.send(Message::Text(value.to_string().into())).await.is_err() {
                break;
            }
        }
    });

    let session: SharedSession = Arc::new(Mutex::new(None));
    while let Some(next) = ws_rx.next().await {
        let Ok(msg) = next else {
            break;
        };
        match msg {
            Message::Text(text) => {
                let out = out_tx.clone();
                let state = session.clone();
                handle_text(text.to_string(), state, out).await;
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
        }
    }

    if let Some(active) = session.lock().await.take() {
        active.inbox_task.abort();
        active.session.disconnect().await;
    }
    writer.abort();
}

async fn handle_text(text: String, state: SharedSession, out: Outbound) {
    let request = match serde_json::from_str::<GatewayRequest>(&text) {
        Ok(r) => r,
        Err(e) => {
            send_error(&out, Value::Null, format!("bad json: {e}"));
            return;
        }
    };
    let id = request.id.clone().unwrap_or(Value::Null);
    let result = dispatch(request, state, out.clone()).await;
    match result {
        Ok(value) => send_ok(&out, id, value),
        Err(err) => send_error(&out, id, err),
    }
}

#[derive(Debug, Deserialize)]
struct GatewayRequest {
    #[serde(default)]
    id: Option<Value>,
    cmd: String,
    #[serde(default)]
    args: Value,
}

async fn dispatch(request: GatewayRequest, state: SharedSession, out: Outbound) -> Result<Value, String> {
    match request.cmd.as_str() {
        "connect" => connect(request.args, state, out).await,
        "disconnect" => disconnect(state).await,
        "my_id" => with_session(&state, |s| json!(hex_encode(&s.session.id_bytes()))).await,
        "node_id" => with_session(&state, |s| json!(hex_encode(&s.session.node_id()))).await,
        "directory_query" => directory_query(request.args, state).await,
        "send_to" => send_to(request.args, state).await,
        "send_group" => send_group(request.args, state).await,
        "group_list" => group_list(state).await,
        other => Err(format!("unknown command: {other}")),
    }
}

async fn connect(args: Value, state: SharedSession, out: Outbound) -> Result<Value, String> {
    let args: ConnectArgs = serde_json::from_value(args).map_err(|e| format!("bad connect args: {e}"))?;
    let node = parse_node(&args.node)?;
    let seed = match args.seed_hex.as_deref() {
        Some(seed) if !seed.is_empty() => hex_decode_32(seed).ok_or("bad seedHex")?,
        _ => nm_transport::SecretKey::generate().to_bytes(),
    };

    let client = match args.mode.as_deref() {
        Some("local") => Client::bind_local(seed).await,
        Some("selfhost") => Client::bind_selfhosted(
            seed,
            args.relay_urls.unwrap_or_default(),
            args.pkarr_url.unwrap_or_default(),
            args.dns_origin,
            0,
        )
        .await,
        _ => Client::bind(seed).await,
    }
    .map_err(|e| e.to_string())?;

    if let Some(addr) = args.peer_addr.as_deref() {
        client.add_peer_addr(nm_transport::addr_from_string(addr).map_err(|e| e.to_string())?);
    }

    let mut session = client.online(node).await.map_err(|e| e.to_string())?;
    let inbox = session.take_inbox().ok_or("session inbox already taken")?;
    let session = Arc::new(session);
    let inbox_task = spawn_inbox(inbox, out, session.id_bytes());

    if let Some(old) = state.lock().await.replace(GatewaySession {
        _client: client,
        session: session.clone(),
        inbox_task,
    }) {
        old.inbox_task.abort();
        old.session.disconnect().await;
    }

    Ok(json!({
        "id": hex_encode(&session.id_bytes()),
        "deviceId": hex_encode(&session.device_id()),
        "nodeId": hex_encode(&session.node_id()),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectArgs {
    node: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    seed_hex: Option<String>,
    #[serde(default)]
    peer_addr: Option<String>,
    #[serde(default)]
    relay_urls: Option<Vec<String>>,
    #[serde(default)]
    pkarr_url: Option<String>,
    #[serde(default)]
    dns_origin: Option<String>,
}

async fn disconnect(state: SharedSession) -> Result<Value, String> {
    let active = state.lock().await.take();
    if let Some(active) = active {
        active.inbox_task.abort();
        active.session.disconnect().await;
    }
    Ok(json!(true))
}

async fn directory_query(args: Value, state: SharedSession) -> Result<Value, String> {
    let args: DirectoryArgs = serde_json::from_value(args).map_err(|e| format!("bad directory args: {e}"))?;
    let session = active_session(&state).await?;
    let items = session
        .directory_query(DirectoryQuery {
            kind_prefix: args.kind_prefix.unwrap_or_default(),
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(Value::Array(items.iter().map(entity_json).collect()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryArgs {
    #[serde(default)]
    kind_prefix: Option<String>,
}

async fn send_to(args: Value, state: SharedSession) -> Result<Value, String> {
    let args: SendArgs = serde_json::from_value(args).map_err(|e| format!("bad send args: {e}"))?;
    let target = hex_decode_32(&args.target).ok_or("bad target")?;
    let session = active_session(&state).await?;
    let ack = session.send_to(target, &args.text).await.map_err(|e| e.to_string())?;
    Ok(gram_json(&ack))
}

async fn send_group(args: Value, state: SharedSession) -> Result<Value, String> {
    let args: SendGroupArgs = serde_json::from_value(args).map_err(|e| format!("bad send_group args: {e}"))?;
    let group_id = hex_decode_32(&args.group_id).ok_or("bad groupId")?;
    let session = active_session(&state).await?;
    let ack = session.send_group(group_id, &args.text).await.map_err(|e| e.to_string())?;
    Ok(gram_json(&ack))
}

#[derive(Debug, Deserialize)]
struct SendArgs {
    target: String,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendGroupArgs {
    group_id: String,
    text: String,
}

async fn group_list(state: SharedSession) -> Result<Value, String> {
    let session = active_session(&state).await?;
    let groups = session.group_list().await.map_err(|e| e.to_string())?;
    Ok(Value::Array(groups.iter().map(group_json).collect()))
}

async fn active_session(state: &SharedSession) -> Result<Arc<Session>, String> {
    state
        .lock()
        .await
        .as_ref()
        .map(|s| s.session.clone())
        .ok_or_else(|| "not connected".to_string())
}

async fn with_session<F>(state: &SharedSession, f: F) -> Result<Value, String>
where
    F: FnOnce(&GatewaySession) -> Value,
{
    let guard = state.lock().await;
    let active = guard.as_ref().ok_or_else(|| "not connected".to_string())?;
    Ok(f(active))
}

fn spawn_inbox(
    mut inbox: mpsc::UnboundedReceiver<Gram>,
    out: Outbound,
    my_id: [u8; 32],
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(gram) = inbox.recv().await {
            let _ = out.send(json!({
                "type": "event",
                "event": gram_event_json(&gram, &my_id),
            }));
        }
    })
}

fn parse_node(node: &str) -> Result<nm_transport::Addr, String> {
    nm_transport::addr_from_string(node)
        .or_else(|_| {
            hex_decode_32(node)
                .ok_or_else(|| nm_transport::TransportError::Iroh("bad node id".into()))
                .and_then(nm_transport::addr_from_id)
        })
        .map_err(|e| e.to_string())
}

fn send_ok(out: &Outbound, id: Value, result: Value) {
    let _ = out.send(json!({ "type": "response", "id": id, "ok": true, "result": result }));
}

fn send_error(out: &Outbound, id: Value, error: String) {
    let _ = out.send(json!({ "type": "response", "id": id, "ok": false, "error": error }));
}

fn gram_event_json(gram: &Gram, my_id: &[u8; 32]) -> Value {
    let mut value = gram_json(gram);
    if let Value::Object(ref mut obj) = value {
        obj.insert("type".to_string(), json!("message"));
        obj.insert("mine".to_string(), json!(gram.sender.as_slice() == my_id));
    }
    value
}

fn gram_json(gram: &Gram) -> Value {
    let payload = gram.payload.as_ref();
    json!({
        "kind": format!("{:?}", gram.kind()),
        "gramId": gram.gram_id,
        "refGramId": gram.ref_gram_id,
        "from": hex_encode(&gram.sender),
        "to": hex_encode(&gram.receiver),
        "ts": gram.timestamp_ms,
        "payload": payload.map(any_json),
        "text": payload.and_then(any_text),
    })
}

fn any_json(any: &Any) -> Value {
    json!({
        "typeUrl": any.type_url,
        "valueHex": hex_encode(&any.value),
    })
}

fn any_text(any: &Any) -> Option<String> {
    if any.type_url == "text/plain" || any.type_url == "nmspace.v1/chat" {
        String::from_utf8(any.value.clone()).ok()
    } else {
        None
    }
}

fn entity_json(entity: &Entity) -> Value {
    json!({
        "id": hex_encode(&entity.entity_id),
        "kind": entity.kind,
        "displayName": entity.display_name,
        "attributes": entity.attributes,
        "updatedAt": entity.updated_at,
        "homeNode": hex_encode(&entity.home_node),
        "profile": entity.profile.as_ref().map(any_json),
    })
}

fn group_json(group: &Group) -> Value {
    json!({
        "id": hex_encode(&group.group_id),
        "name": group.name,
        "owner": hex_encode(&group.owner),
        "members": group.members.iter().map(|m| hex_encode(m)).collect::<Vec<_>>(),
        "admins": group.admins.iter().map(|m| hex_encode(m)).collect::<Vec<_>>(),
        "updatedAt": group.updated_at,
        "topic": group.topic,
        "avatarUrl": group.avatar_url,
        "homeNode": hex_encode(&group.home_node),
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex_decode_32(s: &str) -> Option<[u8; 32]> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nm_proto::GramKind;

    #[test]
    fn hex_decode_accepts_32_bytes() {
        let got = hex_decode_32(&"11".repeat(32)).expect("hex");
        assert_eq!(got, [0x11; 32]);
        assert!(hex_decode_32("11").is_none());
    }

    #[test]
    fn gram_json_exposes_text_payload() {
        let gram = Gram {
            kind: GramKind::Message as i32,
            sender: vec![1; 32],
            receiver: vec![2; 32],
            payload: Some(Any { type_url: "text/plain".into(), value: b"hi".to_vec() }),
            ..Default::default()
        };
        assert_eq!(gram_json(&gram)["text"], "hi");
    }
}

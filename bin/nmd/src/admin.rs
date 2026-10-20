//! nmd 后端管理控制 API：axum，绑定本机地址 + 共享 token，供独立的 `nm-admind` 取数/下发控制。
//! 只读端点返回实时指标（连接/用户/存储/流量/系统）；控制端点做拉黑/解黑/踢下线。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use nm_core::Directory;
use nm_node::Node;
use nm_proto::DirectoryQuery;
use serde_json::{json, Value};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn parse_id(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut o = [0u8; 32];
    for i in 0..32 {
        o[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(o)
}

/// 会话链路质量 JSON（连接监控 / 对等节点共用）。
fn session_json(r: &nm_node::SessionRow) -> Value {
    json!({
        "id": hex(&r.id),
        "since_unix_ms": r.since_unix_ms,
        "bytes_tx": r.bytes_tx,
        "bytes_rx": r.bytes_rx,
        "alpn": r.alpn,
        "rtt_ms": r.rtt_ms,
        "cwnd": r.cwnd,
        "mtu": r.mtu,
        "lost_packets": r.lost_packets,
        "lost_bytes": r.lost_bytes,
        "datagrams_tx": r.datagrams_tx,
        "datagrams_rx": r.datagrams_rx,
        "congestion_events": r.congestion_events,
        "black_holes": r.black_holes,
        "path_kind": r.path_kind,
        "remote_addr": r.remote_addr,
    })
}

/// 进程自指标（后台每 2s 采样）。
#[derive(Clone, Copy, Default)]
struct ProcMetrics {
    rss_bytes: u64,
    cpu_pct: f32,
}

#[derive(Clone)]
struct AppState {
    node: Arc<Node>,
    db_path: String,
    token: String,
    start: Instant,
    proc: Arc<Mutex<ProcMetrics>>,
    /// 本节点所属联邦（成员频道名）。展示用：peers 面板与新增对等归属此联邦。
    federation: String,
}

/// 启动控制 API。绑定 `addr`，所有请求需带 `X-Admin-Token: <token>`。
pub async fn serve(
    node: Arc<Node>,
    addr: String,
    token: String,
    db_path: String,
    federation: String,
) -> anyhow::Result<()> {
    let proc = Arc::new(Mutex::new(ProcMetrics::default()));
    spawn_proc_sampler(proc.clone());
    let shutdown = node.shutdown_token(); // 优雅关闭：节点关闭时一并停 axum
    let state = AppState {
        node,
        db_path,
        token,
        start: Instant::now(),
        proc,
        federation,
    };

    let app = Router::new()
        .route("/overview", get(overview))
        .route("/connections", get(connections))
        .route("/users", get(users))
        .route("/devices", get(devices))
        .route("/storage", get(storage))
        .route("/traffic", get(traffic))
        .route("/system", get(system))
        .route("/identity", get(identity))
        .route("/peers", get(peers))
        .route("/ban", post(ban))
        .route("/unban", post(unban))
        .route("/kick", post(kick))
        .route("/add-peer", post(add_peer))
        .route("/remove-peer", post(remove_peer))
        .route("/backup", post(backup))
        .route("/names/domains", get(names_domains))
        .route("/names/domain-add", post(names_domain_add))
        .route("/names/domain-decision", post(names_domain_decision))
        .route("/names/domain-notices", get(names_domain_notices))
        .route("/names/list", get(names_list))
        .route("/names/set", post(names_set))
        .route("/names/del", post(names_del))
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth))
        // 健康探针：免鉴权（供 systemd/容器探针访问），merge 在 auth layer 之外。
        // /healthz 存活（进程能响应即 200，不依赖后端，避免误杀）；/readyz 就绪（redb 可写 + endpoint 有地址，否则 503）。
        .merge(
            Router::new()
                .route("/healthz", get(healthz))
                .route("/readyz", get(readyz))
                .with_state(state.clone()),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "admin control API listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await?;
    Ok(())
}

/// token 校验中间件。
async fn auth(
    State(st): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    let ok = req
        .headers()
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        == Some(st.token.as_str());
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// 存活探针：进程能响应 HTTP 即健康。**不依赖**后端状态（redb/网络），避免依赖抖动误杀进程。
async fn healthz(State(st): State<AppState>) -> Json<Value> {
    Json(json!({ "status": "ok", "uptime_s": st.start.elapsed().as_secs() }))
}

/// 就绪探针：节点真能服务才返回 200；redb 不可访问 / endpoint 无地址时返回 503（供编排器暂不转流量）。
async fn readyz(State(st): State<AppState>) -> axum::response::Response {
    match st.node.readiness() {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({
                "status": "ready",
                "online": st.node.online_count(),
                "peers": st.node.peer_count(),
            })),
        )
            .into_response(),
        Err(reason) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": reason })),
        )
            .into_response(),
    }
}

async fn overview(State(st): State<AppState>) -> Json<Value> {
    let node = &st.node;
    let t = nm_transport::traffic_snapshot();
    let pm = *st.proc.lock().unwrap();
    let db_bytes = std::fs::metadata(&st.db_path).map(|m| m.len()).unwrap_or(0);
    Json(json!({
        "id": hex(node.id().as_bytes()),
        "online": node.online_count(),
        "peers": node.peer_count(),
        "entities": node.directory().len(),
        "groups": node.groups_count(),
        "banned": node.banned_count(),
        "uptime_s": st.start.elapsed().as_secs(),
        "rss_bytes": pm.rss_bytes,
        "cpu_pct": pm.cpu_pct,
        "db_bytes": db_bytes,
        "traffic": traffic_json(&t),
    }))
}

async fn connections(State(st): State<AppState>) -> Json<Value> {
    let rows: Vec<Value> = st
        .node
        .sessions_snapshot()
        .into_iter()
        .map(|r| session_json(&r))
        .collect();
    Json(json!({ "connections": rows }))
}

async fn users(State(st): State<AppState>) -> Json<Value> {
    let dir = st.node.directory();
    let entities = dir.query(&DirectoryQuery::default()).await.unwrap_or_default();
    let banned_vec = st.node.banned();
    let banned_set: std::collections::HashSet<[u8; 32]> = banned_vec.iter().copied().collect();
    let users: Vec<Value> = entities
        .into_iter()
        .map(|e| {
            let mut idarr = [0u8; 32];
            if e.entity_id.len() == 32 {
                idarr.copy_from_slice(&e.entity_id);
            }
            json!({
                "id": hex(&e.entity_id),
                "kind": e.kind,
                "name": e.display_name,
                "banned": banned_set.contains(&idarr),
            })
        })
        .collect();
    let banned: Vec<String> = banned_vec.iter().map(|b| hex(b)).collect();
    Json(json!({ "users": users, "banned": banned }))
}

/// 全部已登记设备（按账号分组由前端完成）：供「域内命名登记」展开某账号的设备列表 + 每个设备公钥。
async fn devices(State(st): State<AppState>) -> Json<Value> {
    let rows: Vec<Value> = st
        .node
        .devices_snapshot()
        .into_iter()
        .map(|d| {
            let cert = d.cert.unwrap_or_default();
            let label = if d.label.is_empty() { cert.label.clone() } else { d.label.clone() };
            json!({
                "account": hex(&cert.account),   // 所属账号公钥（= 命名登记里的 pubkey）
                "device": hex(&cert.device),     // 设备连接公钥
                "label": label,
                "role": cert.role,
                "online": d.online,
                "last_seen": d.last_seen,
                "revoked": d.revoked.is_some(),
            })
        })
        .collect();
    Json(json!({ "devices": rows }))
}

async fn storage(State(st): State<AppState>) -> Json<Value> {
    let s = st.node.store_stats();
    let db_bytes = std::fs::metadata(&st.db_path).map(|m| m.len()).unwrap_or(0);
    Json(json!({
        "entities": s.entities,
        "groups": s.groups,
        "db_bytes": db_bytes,
        "db_path": st.db_path,
    }))
}

async fn traffic(State(_st): State<AppState>) -> Json<Value> {
    Json(traffic_json(&nm_transport::traffic_snapshot()))
}

fn traffic_json(t: &nm_transport::TrafficStat) -> Value {
    json!({
        "grams_in": t.grams_in, "grams_out": t.grams_out,
        "bytes_in": t.bytes_in, "bytes_out": t.bytes_out,
    })
}

async fn system(State(st): State<AppState>) -> Json<Value> {
    let pm = *st.proc.lock().unwrap();
    let addr = st.node.addr();
    let (roam_changes, roam_last_ms) = st.node.roam_snapshot();
    let (grp_fanouts, grp_acks, grp_retx, grp_giveups, grp_ack_batches, grp_dead, grp_ratelimited) = st.node.group_metrics_snapshot();
    let tables: serde_json::Map<String, Value> = st
        .node
        .table_sizes()
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::from(v)))
        .collect();
    Json(json!({
        "pid": std::process::id(),
        "rss_bytes": pm.rss_bytes,
        "cpu_pct": pm.cpu_pct,
        "uptime_s": st.start.elapsed().as_secs(),
        "online": st.node.online_count(),
        // 换网/寻址可观测（P2 看板）：当前端点地址、relay、IP，及换网计数/最近换网时间。
        "node_addr": nm_transport::addr_to_string(&addr),
        "relays": addr.relay_urls().map(|u| u.to_string()).collect::<Vec<_>>(),
        "ip_addrs": addr.ip_addrs().map(|a| a.to_string()).collect::<Vec<_>>(),
        "roam_changes": roam_changes,
        "roam_last_ms": roam_last_ms,
        // P1 内存可观测：各热点表当前条目数（监控增长趋势，据此判断是否需淘汰）。
        "table_sizes": tables,
        // 群消息可靠扇出到达率观测：扇出跟踪/收到 ack/重传/超期放弃。acks≈fanouts 且 retransmits/giveups 低 = 健康。
        "group_fanouts": grp_fanouts,
        "group_acks": grp_acks,
        "group_retransmits": grp_retx,
        "group_giveups": grp_giveups,
        // 群 ack 聚合：本节点作为成员发出的 GroupAckBatch 块数（块数 ≪ 被确认消息数 = 聚合生效）。
        "group_ack_batches": grp_ack_batches,
        // P0-3/P0-4 可观测：死信累计(达期限放弃并落档)/被消息级限流丢弃累计/当前死信留存条数。
        "dead_letters": grp_dead,
        "msgs_rate_limited": grp_ratelimited,
        "dead_letter_pending": st.node.dead_letter_count(),
    }))
}

async fn identity(State(st): State<AppState>) -> Json<Value> {
    Json(json!({
        "node_id": hex(st.node.id().as_bytes()),
        "addr": nm_transport::addr_to_string(&st.node.addr()),
    }))
}

async fn peers(State(st): State<AppState>) -> Json<Value> {
    // 活动会话按 id 索引，与联邦 peers 目录 join → 连通性 / 链路质量。
    let sessions: std::collections::HashMap<String, nm_node::SessionRow> = st
        .node
        .sessions_snapshot()
        .into_iter()
        .map(|r| (hex(&r.id), r))
        .collect();
    let mut online = 0usize;
    let peers: Vec<Value> = st
        .node
        .peers_detail()
        .into_iter()
        .map(|(id, info)| {
            // 空 source（旧行/配置种子）按 manual 呈现。
            let source = if info.source.is_empty() {
                "manual"
            } else {
                info.source.as_str()
            };
            // 空 federation（旧行）按本节点联邦呈现。
            let federation = if info.federation.is_empty() {
                st.federation.as_str()
            } else {
                info.federation.as_str()
            };
            let sess = sessions.get(&id);
            let connected = sess.is_some();
            if connected {
                online += 1;
            }
            let mut row = json!({
                "id": id, "name": info.name, "address": info.address,
                "email": info.email, "mobile": info.mobile, "gps": info.gps,
                "source": source, "last_seen": info.last_seen, "federation": federation,
                "connected": connected,
            });
            if let Some(s) = sess {
                let obj = row.as_object_mut().unwrap();
                for (k, v) in session_json(s).as_object().unwrap() {
                    // 保留 peers 的 id；其余会话字段写入同行。
                    if k != "id" {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
            row
        })
        .collect();
    let total = peers.len();
    Json(json!({
        "peers": peers,
        "federation": st.federation,
        "self_id": hex(st.node.id().as_bytes()),
        "online": online,
        "offline": total.saturating_sub(online),
        "total": total,
    }))
}

#[derive(serde::Deserialize)]
struct IdReq {
    id: String,
}

async fn ban(State(st): State<AppState>, Json(r): Json<IdReq>) -> Result<Json<Value>, StatusCode> {
    let id = parse_id(&r.id).ok_or(StatusCode::BAD_REQUEST)?;
    st.node.ban(id);
    Ok(Json(json!({ "ok": true })))
}
async fn unban(State(st): State<AppState>, Json(r): Json<IdReq>) -> Result<Json<Value>, StatusCode> {
    let id = parse_id(&r.id).ok_or(StatusCode::BAD_REQUEST)?;
    st.node.unban(id);
    Ok(Json(json!({ "ok": true })))
}
async fn kick(State(st): State<AppState>, Json(r): Json<IdReq>) -> Result<Json<Value>, StatusCode> {
    let id = parse_id(&r.id).ok_or(StatusCode::BAD_REQUEST)?;
    let cut = st.node.kick(id);
    Ok(Json(json!({ "ok": true, "cut": cut })))
}

/// P0-2 在线备份：把当前 nmd.redb 一致快照拷到 `dest`（绝对路径或相对 nmd 工作目录）。
#[derive(serde::Deserialize)]
struct BackupReq { dest: String }

async fn backup(State(st): State<AppState>, Json(r): Json<BackupReq>) -> Result<Json<Value>, StatusCode> {
    let dest = std::path::PathBuf::from(&r.dest);
    match st.node.backup_db(&dest) {
        Ok(n) => { tracing::info!(dest = %r.dest, bytes = n, "db backup written"); Ok(Json(json!({ "ok": true, "bytes": n, "dest": r.dest }))) }
        Err(e) => { tracing::warn!("db backup failed: {e}"); Err(StatusCode::INTERNAL_SERVER_ERROR) }
    }
}

/// 运行时添加/编辑联邦对等（node id + 名称/物理地址/email/mobile/gps）——无需重启；重复 id 即编辑。
#[derive(serde::Deserialize, Default)]
struct PeerReq {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    address: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    mobile: String,
    #[serde(default)]
    gps: String,
}

async fn add_peer(
    State(st): State<AppState>,
    Json(r): Json<PeerReq>,
) -> Result<Json<Value>, StatusCode> {
    let node_id = parse_id(&r.id).ok_or(StatusCode::BAD_REQUEST)?;
    let info = nm_node::PeerInfo {
        name: r.name,
        address: r.address,
        email: r.email,
        mobile: r.mobile,
        gps: r.gps,
        source: "manual".to_string(),
        last_seen: 0,
        federation: st.federation.clone(),
    };
    st.node
        .add_peer_full(node_id, info)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    tracing::info!("peer added/updated at runtime");
    Ok(Json(json!({ "ok": true, "peers": st.node.peer_count() })))
}

async fn remove_peer(
    State(st): State<AppState>,
    Json(r): Json<IdReq>,
) -> Result<Json<Value>, StatusCode> {
    let id = parse_id(&r.id).ok_or(StatusCode::BAD_REQUEST)?;
    let existed = st.node.remove_peer(id);
    Ok(Json(json!({ "ok": true, "existed": existed, "peers": st.node.peer_count() })))
}

// ── 去中心命名：本节点域名 + 域内命名 CRUD（注册商）──
async fn names_domains(State(st): State<AppState>) -> Json<Value> {
    Json(json!({ "domains": st.node.owned_domains() }))
}

#[derive(serde::Deserialize)]
struct DomainReq {
    domain: String,
}
#[derive(serde::Deserialize)]
struct DecisionReq {
    node_id: String,
    domain: String,
    approved: bool,
}
async fn names_domain_decision(State(st): State<AppState>, Json(r): Json<DecisionReq>) -> Json<Value> {
    let Some(id) = parse_id(&r.node_id) else {
        return Json(json!({ "ok": false, "error": "node id 须为 64 位十六进制" }));
    };
    st.node.publish_domain_decision(id, &r.domain, r.approved);
    Json(json!({ "ok": true }))
}
async fn names_domain_notices(State(st): State<AppState>) -> Json<Value> {
    let mut notices: Vec<Value> = st
        .node
        .domain_notices()
        .into_iter()
        .map(|(domain, approved, at_ms)| json!({ "domain": domain, "approved": approved, "at_ms": at_ms }))
        .collect();
    notices.sort_by(|a, b| b["at_ms"].as_u64().cmp(&a["at_ms"].as_u64()));
    Json(json!({ "notices": notices }))
}
async fn names_domain_add(State(st): State<AppState>, Json(r): Json<DomainReq>) -> Json<Value> {
    match st.node.add_domain(&r.domain) {
        Ok(added) => Json(json!({ "ok": true, "added": added, "domains": st.node.owned_domains() })),
        Err(e) => Json(json!({ "ok": false, "error": e })),
    }
}

/// 列出本节点签发的命名（local_part → pubkey），按域名分组由前端处理。
async fn names_list(State(st): State<AppState>) -> Json<Value> {
    let recs: Vec<Value> = st
        .node
        .names_owned(None)
        .into_iter()
        .map(|r| {
            json!({
                "local_part": r.local_part,
                "domain": r.domain,
                "name": format!("{}@{}", r.local_part, r.domain),
                "pubkey": hex(&r.client_pubkey),
                "serial": r.serial,
            })
        })
        .collect();
    Json(json!({ "names": recs }))
}

#[derive(serde::Deserialize)]
struct NameSetReq {
    domain: String,
    local_part: String,
    pubkey: String,
}
async fn names_set(State(st): State<AppState>, Json(r): Json<NameSetReq>) -> Json<Value> {
    let pk = match parse_id(&r.pubkey) {
        Some(x) => x,
        None => return Json(json!({ "ok": false, "error": "非法公钥（需 64 位 hex）" })),
    };
    match st.node.admin_set_name(&r.domain, &r.local_part, pk) {
        Ok(rec) => Json(json!({ "ok": true, "name": format!("{}@{}", rec.local_part, rec.domain) })),
        Err(e) => Json(json!({ "ok": false, "error": e })),
    }
}

#[derive(serde::Deserialize)]
struct NameDelReq {
    domain: String,
    local_part: String,
}
async fn names_del(State(st): State<AppState>, Json(r): Json<NameDelReq>) -> Json<Value> {
    match st.node.admin_del_name(&r.domain, &r.local_part) {
        Ok(()) => Json(json!({ "ok": true })),
        Err(e) => Json(json!({ "ok": false, "error": e })),
    }
}

/// 后台每 2s 采样本进程 CPU%/RSS。
fn spawn_proc_sampler(slot: Arc<Mutex<ProcMetrics>>) {
    tokio::spawn(async move {
        use sysinfo::{Pid, ProcessesToUpdate, System};
        let mut sys = System::new();
        let pid = Pid::from_u32(std::process::id());
        loop {
            sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
            if let Some(p) = sys.process(pid)
                && let Ok(mut m) = slot.lock() {
                m.rss_bytes = p.memory();
                m.cpu_pct = p.cpu_usage();
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

//! nmd 后端管理控制 API：axum，绑定本机地址 + 共享 token，供独立的 `nm-admind` 取数/下发控制。
//! 只读端点返回实时指标（连接/用户/存储/流量/系统）；控制端点做拉黑/解黑/踢下线。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use nm_core::Directory;
use nm_node::Node;
use nm_proto::DirectoryQuery;
use serde::Deserialize;
use serde_json::{Value, json};

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

// ── P1-8 审计：安全敏感的管理操作统一记一条结构化审计事件（target=audit，带 audit=true 字段）。
// 走既有 tracing 订阅器(两个二进制都已初始化)，journald/文件按 target 或字段即可筛出审计流，无需新依赖。
// actor = nm-admind 经 `X-Admin-Operator` 头转发的登录操作员名(nmd 的 token 本身匿名)；缺失则 "token-holder"。
/// 从请求头取转发来的操作员身份（nm-admind 登录会话名）。
fn operator(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("x-admin-operator")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("token-holder")
        .to_string()
}

/// 记一条管理审计事件：`action`=操作名，`target`=作用对象(id/域名等)，`ok`=是否成功，`detail`=补充。
fn audit(actor: &str, action: &str, target: &str, ok: bool, detail: &str) {
    tracing::info!(
        audit = true,
        actor,
        action,
        target,
        ok,
        detail,
        "ADMIN AUDIT"
    );
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

const DEFAULT_PAGE_LIMIT: usize = 200;
const MAX_PAGE_LIMIT: usize = 1000;

#[derive(Deserialize, Default)]
struct PageQuery {
    offset: Option<usize>,
    limit: Option<usize>,
}

fn page_params(q: &PageQuery) -> (usize, usize) {
    let offset = q.offset.unwrap_or(0);
    let limit = q
        .limit
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .clamp(1, MAX_PAGE_LIMIT);
    (offset, limit)
}

fn page_meta(total: usize, offset: usize, limit: usize) -> Value {
    let next = offset.checked_add(limit).filter(|n| *n < total);
    json!({
        "offset": offset,
        "limit": limit,
        "total": total,
        "next_offset": next,
    })
}

fn page_slice<T>(rows: &[T], offset: usize, limit: usize) -> &[T] {
    let start = offset.min(rows.len());
    let end = start.saturating_add(limit).min(rows.len());
    &rows[start..end]
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
    /// P1-10：配置文件路径（供 POST /reload 重读并热应用可变更子集）。
    config_path: String,
}

/// 启动控制 API。绑定 `addr`，所有请求需带 `X-Admin-Token: <token>`。
pub async fn serve(
    node: Arc<Node>,
    addr: String,
    token: String,
    db_path: String,
    federation: String,
    config_path: String,
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
        config_path,
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
        .route("/reload", post(reload))
        .route("/admission/issue", post(admission_issue))
        .route("/names/domains", get(names_domains))
        .route("/names/domain-add", post(names_domain_add))
        .route("/names/domain-decision", post(names_domain_decision))
        .route("/names/domain-notices", get(names_domain_notices))
        .route("/names/list", get(names_list))
        .route("/names/set", post(names_set))
        .route("/names/del", post(names_del))
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth))
        // 健康探针 + 指标：免鉴权（供 systemd/容器探针、Prometheus 抓取），merge 在 auth layer 之外。
        // /healthz 存活（进程能响应即 200，不依赖后端，避免误杀）；/readyz 就绪（redb 可写 + endpoint 有地址，否则 503）；
        // /metrics OpenMetrics 文本（admin 口默认绑 127.0.0.1，抓取方无需 token）。
        .merge(
            Router::new()
                .route("/healthz", get(healthz))
                .route("/readyz", get(readyz))
                .route("/metrics", get(metrics))
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

/// P1-9 Prometheus/OpenMetrics 文本导出。免鉴权（admin 口默认 127.0.0.1）。涵盖：到达率计数器、
/// 送达时延直方图(send→ack)、连接拒绝错误率、死信/限流、换网、流量、各表 gauge、进程/在线。
async fn metrics(State(st): State<AppState>) -> axum::response::Response {
    use std::fmt::Write;
    let mut o = String::with_capacity(4096);
    let pm = *st.proc.lock().unwrap();
    let (roam_changes, roam_last_ms) = st.node.roam_snapshot();
    let (f, acks, retx, giveups, batches, dead, ratelimited) = st.node.group_metrics_snapshot();
    let (rj_ban, rj_rev, rj_busy, rj_rate) = st.node.conn_rejects_snapshot();
    let (buckets, lat_count, lat_sum_ms) = st.node.delivery_hist_snapshot();
    let tr = nm_transport::traffic_snapshot();

    let c = |o: &mut String, name: &str, help: &str, v: u64| {
        let _ = writeln!(o, "# HELP {name} {help}\n# TYPE {name} counter\n{name} {v}");
    };
    let g = |o: &mut String, name: &str, help: &str, v: f64| {
        let _ = writeln!(o, "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {v}");
    };
    // 到达率/可靠性计数器
    c(
        &mut o,
        "nmd_group_fanouts_total",
        "group messages fanned out & tracked",
        f,
    );
    c(&mut o, "nmd_group_acks_total", "group acks received", acks);
    c(
        &mut o,
        "nmd_group_retransmits_total",
        "group retransmits",
        retx,
    );
    c(
        &mut o,
        "nmd_group_giveups_total",
        "group messages given up after horizon",
        giveups,
    );
    c(
        &mut o,
        "nmd_group_ack_batches_total",
        "GroupAckBatch grams emitted",
        batches,
    );
    c(
        &mut o,
        "nmd_dead_letters_total",
        "messages dead-lettered after horizon",
        dead,
    );
    c(
        &mut o,
        "nmd_msgs_rate_limited_total",
        "inbound messages dropped by rate limit",
        ratelimited,
    );
    // 连接拒绝错误率
    c(
        &mut o,
        "nmd_conn_rejected_banned_total",
        "connections rejected: banned",
        rj_ban,
    );
    c(
        &mut o,
        "nmd_conn_rejected_revoked_total",
        "connections rejected: revoked device",
        rj_rev,
    );
    c(
        &mut o,
        "nmd_conn_rejected_server_busy_total",
        "connections rejected: conn cap",
        rj_busy,
    );
    c(
        &mut o,
        "nmd_conn_rejected_rate_limited_total",
        "connections rejected: reconnect storm",
        rj_rate,
    );
    // 流量
    c(&mut o, "nmd_grams_in_total", "grams received", tr.grams_in);
    c(&mut o, "nmd_grams_out_total", "grams sent", tr.grams_out);
    c(&mut o, "nmd_bytes_in_total", "bytes received", tr.bytes_in);
    c(&mut o, "nmd_bytes_out_total", "bytes sent", tr.bytes_out);
    // 换网
    c(
        &mut o,
        "nmd_roam_changes_total",
        "endpoint address changes (roam)",
        roam_changes,
    );
    g(
        &mut o,
        "nmd_roam_last_change_ms",
        "unix ms of last roam (0=none)",
        roam_last_ms as f64,
    );
    // 送达时延直方图（send→ack，毫秒）
    let _ = writeln!(
        o,
        "# HELP nmd_delivery_latency_ms send→ack delivery latency (ms)\n# TYPE nmd_delivery_latency_ms histogram"
    );
    for (le, cum) in &buckets {
        let _ = writeln!(o, "nmd_delivery_latency_ms_bucket{{le=\"{le}\"}} {cum}");
    }
    let _ = writeln!(
        o,
        "nmd_delivery_latency_ms_bucket{{le=\"+Inf\"}} {lat_count}"
    );
    let _ = writeln!(o, "nmd_delivery_latency_ms_sum {lat_sum_ms}");
    let _ = writeln!(o, "nmd_delivery_latency_ms_count {lat_count}");
    // 进程 / 在线 / 死信留存
    g(
        &mut o,
        "nmd_online_sessions",
        "current online sessions",
        st.node.online_count() as f64,
    );
    g(
        &mut o,
        "nmd_peers",
        "known federation peers",
        st.node.peer_count() as f64,
    );
    g(
        &mut o,
        "nmd_rss_bytes",
        "process resident memory",
        pm.rss_bytes as f64,
    );
    g(
        &mut o,
        "nmd_cpu_pct",
        "process cpu percent",
        pm.cpu_pct as f64,
    );
    g(
        &mut o,
        "nmd_uptime_seconds",
        "process uptime",
        st.start.elapsed().as_secs() as f64,
    );
    g(
        &mut o,
        "nmd_dead_letter_pending",
        "dead letters currently retained",
        st.node.dead_letter_count() as f64,
    );
    // 各表 gauge（内存/存储增长观测）
    for (name, v) in st.node.table_sizes() {
        let _ = writeln!(
            o,
            "# TYPE nmd_table_size gauge\nnmd_table_size{{table=\"{name}\"}} {v}"
        );
    }
    let _ = writeln!(o, "# EOF");
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )],
        o,
    )
        .into_response()
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

async fn connections(State(st): State<AppState>, Query(q): Query<PageQuery>) -> Json<Value> {
    let (offset, limit) = page_params(&q);
    let rows = st.node.sessions_snapshot();
    let total = rows.len();
    let rows: Vec<Value> = page_slice(&rows, offset, limit)
        .iter()
        .map(session_json)
        .collect();
    Json(json!({ "connections": rows, "page": page_meta(total, offset, limit) }))
}

async fn users(State(st): State<AppState>, Query(q): Query<PageQuery>) -> Json<Value> {
    let (offset, limit) = page_params(&q);
    let dir = st.node.directory();
    let entities = dir
        .query(&DirectoryQuery::default())
        .await
        .unwrap_or_default();
    let total = entities.len();
    let banned_vec = st.node.banned();
    let banned_set: std::collections::HashSet<[u8; 32]> = banned_vec.iter().copied().collect();
    let users: Vec<Value> = page_slice(&entities, offset, limit)
        .iter()
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
    Json(json!({ "users": users, "banned": banned, "page": page_meta(total, offset, limit) }))
}

/// 全部已登记设备（按账号分组由前端完成）：供「域内命名登记」展开某账号的设备列表 + 每个设备公钥。
async fn devices(State(st): State<AppState>, Query(q): Query<PageQuery>) -> Json<Value> {
    let (offset, limit) = page_params(&q);
    let rows = st.node.devices_snapshot();
    let total = rows.len();
    let rows: Vec<Value> = page_slice(&rows, offset, limit)
        .iter()
        .map(|d| {
            let cert = d.cert.clone().unwrap_or_default();
            let label = if d.label.is_empty() {
                cert.label.clone()
            } else {
                d.label.clone()
            };
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
    Json(json!({ "devices": rows, "page": page_meta(total, offset, limit) }))
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
    let (grp_fanouts, grp_acks, grp_retx, grp_giveups, grp_ack_batches, grp_dead, grp_ratelimited) =
        st.node.group_metrics_snapshot();
    let (presence_pulls, presence_pull_cache_hits, presence_pull_errors) =
        st.node.presence_stat_snapshot();
    let (pkarr_resolve_ok, pkarr_resolve_fail) = st.node.pkarr_stat_snapshot();
    let (admission_enabled, admission_anchors) = st.node.admission_status();
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
        // P2-11 presence 去火管化观测：实际是否已退 overlay（强制 or 自动）、强制开关、已广告支持的 peer 数、拉取/命中/失败累计。
        "presence_scoped": st.node.presence_retired(),
        "presence_scoped_forced": st.node.presence_scoped_on(),
        "presence_scoped_peers": st.node.presence_scoped_peer_count(),
        "presence_pulls": presence_pulls,
        "presence_pull_cache_hits": presence_pull_cache_hits,
        "presence_pull_errors": presence_pull_errors,
        // P2-12 Phase A 去中心发现观测：配置的 pkarr 中继数（>1=已退单点）+ 多中继解析命中/全失败累计。
        "pkarr_relays": st.node.pkarr_relay_count(),
        "pkarr_resolve_ok": pkarr_resolve_ok,
        "pkarr_resolve_fail": pkarr_resolve_fail,
        // P2-13 成员准入观测：是否启用 + 本地信任锚点数。
        "admission_enabled": admission_enabled,
        "admission_anchors": admission_anchors,
    }))
}

async fn identity(State(st): State<AppState>) -> Json<Value> {
    Json(json!({
        "node_id": hex(st.node.id().as_bytes()),
        "addr": nm_transport::addr_to_string(&st.node.addr()),
        // P2 协议版本握手：本节点声明的协议版本与可兼容下限（客户端/对端据此自检）。
        "protocol_version": nm_proto::PROTOCOL_VERSION,
        "min_supported_version": nm_proto::MIN_SUPPORTED_VERSION,
    }))
}

async fn peers(State(st): State<AppState>, Query(q): Query<PageQuery>) -> Json<Value> {
    let (offset, limit) = page_params(&q);
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
    let page_rows: Vec<Value> = page_slice(&peers, offset, limit).to_vec();
    Json(json!({
        "peers": page_rows,
        "federation": st.federation,
        "self_id": hex(st.node.id().as_bytes()),
        "online": online,
        "offline": total.saturating_sub(online),
        "total": total,
        "page": page_meta(total, offset, limit),
    }))
}

#[derive(serde::Deserialize)]
struct IdReq {
    id: String,
}

async fn ban(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(r): Json<IdReq>,
) -> Result<Json<Value>, StatusCode> {
    let op = operator(&headers);
    let id = parse_id(&r.id).ok_or_else(|| {
        audit(&op, "ban", &r.id, false, "bad id");
        StatusCode::BAD_REQUEST
    })?;
    st.node.ban(id);
    audit(&op, "ban", &r.id, true, "");
    Ok(Json(json!({ "ok": true })))
}
async fn unban(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(r): Json<IdReq>,
) -> Result<Json<Value>, StatusCode> {
    let op = operator(&headers);
    let id = parse_id(&r.id).ok_or_else(|| {
        audit(&op, "unban", &r.id, false, "bad id");
        StatusCode::BAD_REQUEST
    })?;
    st.node.unban(id);
    audit(&op, "unban", &r.id, true, "");
    Ok(Json(json!({ "ok": true })))
}
async fn kick(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(r): Json<IdReq>,
) -> Result<Json<Value>, StatusCode> {
    let op = operator(&headers);
    let id = parse_id(&r.id).ok_or_else(|| {
        audit(&op, "kick", &r.id, false, "bad id");
        StatusCode::BAD_REQUEST
    })?;
    let cut = st.node.kick(id);
    audit(
        &op,
        "kick",
        &r.id,
        true,
        if cut {
            "cut live session"
        } else {
            "no live session"
        },
    );
    Ok(Json(json!({ "ok": true, "cut": cut })))
}

/// P0-2 在线备份：把当前 nmd.redb 一致快照拷到 `dest`（绝对路径或相对 nmd 工作目录）。
#[derive(serde::Deserialize)]
struct BackupReq {
    dest: String,
}

async fn backup(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(r): Json<BackupReq>,
) -> Result<Json<Value>, StatusCode> {
    let op = operator(&headers);
    let dest = std::path::PathBuf::from(&r.dest);
    match st.node.backup_db(&dest) {
        Ok(n) => {
            audit(&op, "backup", &r.dest, true, &format!("{n} bytes"));
            Ok(Json(json!({ "ok": true, "bytes": n, "dest": r.dest })))
        }
        Err(e) => {
            audit(&op, "backup", &r.dest, false, &e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// P2-13：锚点签发准入凭证。本节点用自身私钥签（issuer=本节点公钥）；返回 hex 供运维下发给 subject 节点。
#[derive(serde::Deserialize)]
struct AdmissionIssueReq {
    subject: String, // 被准入节点公钥 hex(64)
    #[serde(default)]
    federation: String, // 空=nmspace
    #[serde(default)]
    ttl_secs: u64, // 0=永不过期
}

async fn admission_issue(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(r): Json<AdmissionIssueReq>,
) -> Result<Json<Value>, StatusCode> {
    let op = operator(&headers);
    let subject = parse_id(&r.subject).ok_or_else(|| {
        audit(&op, "admission.issue", &r.subject, false, "bad subject id");
        StatusCode::BAD_REQUEST
    })?;
    let federation = if r.federation.trim().is_empty() {
        "nmspace".to_string()
    } else {
        r.federation.trim().to_string()
    };
    let expires_ms = if r.ttl_secs == 0 {
        0
    } else {
        nm_proto::now_ms() + r.ttl_secs * 1000
    };
    let (grant_hex, issuer_hex) = st
        .node
        .issue_admission_hex(subject, &federation, expires_ms);
    audit(
        &op,
        "admission.issue",
        &r.subject,
        true,
        &format!("fed={federation} exp={expires_ms}"),
    );
    Ok(Json(json!({
        "ok": true,
        "grant": grant_hex,   // 下发给 subject 节点：其 set_admission_grant(此 hex) 后出示
        "issuer": issuer_hex,  // 本节点(锚点)公钥；需被 subject 的对端信任列表收录
        "subject": r.subject,
        "federation": federation,
        "expires_ms": expires_ms,
    })))
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
    headers: axum::http::HeaderMap,
    Json(r): Json<PeerReq>,
) -> Result<Json<Value>, StatusCode> {
    let op = operator(&headers);
    let node_id = parse_id(&r.id).ok_or_else(|| {
        audit(&op, "add-peer", &r.id, false, "bad id");
        StatusCode::BAD_REQUEST
    })?;
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
    st.node.add_peer_full(node_id, info).map_err(|_| {
        audit(&op, "add-peer", &r.id, false, "add failed");
        StatusCode::BAD_REQUEST
    })?;
    audit(&op, "add-peer", &r.id, true, "");
    Ok(Json(json!({ "ok": true, "peers": st.node.peer_count() })))
}

async fn remove_peer(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(r): Json<IdReq>,
) -> Result<Json<Value>, StatusCode> {
    let op = operator(&headers);
    let id = parse_id(&r.id).ok_or_else(|| {
        audit(&op, "remove-peer", &r.id, false, "bad id");
        StatusCode::BAD_REQUEST
    })?;
    let existed = st.node.remove_peer(id);
    audit(
        &op,
        "remove-peer",
        &r.id,
        true,
        if existed { "removed" } else { "not present" },
    );
    Ok(Json(
        json!({ "ok": true, "existed": existed, "peers": st.node.peer_count() }),
    ))
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
async fn names_domain_decision(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(r): Json<DecisionReq>,
) -> Json<Value> {
    let op = operator(&headers);
    let Some(id) = parse_id(&r.node_id) else {
        audit(&op, "domain-decision", &r.domain, false, "bad node id");
        return Json(json!({ "ok": false, "error": "node id 须为 64 位十六进制" }));
    };
    st.node.publish_domain_decision(id, &r.domain, r.approved);
    audit(
        &op,
        "domain-decision",
        &r.domain,
        true,
        if r.approved { "approved" } else { "rejected" },
    );
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
async fn names_domain_add(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(r): Json<DomainReq>,
) -> Json<Value> {
    let op = operator(&headers);
    match st.node.add_domain(&r.domain) {
        Ok(added) => {
            audit(
                &op,
                "domain-add",
                &r.domain,
                true,
                if added { "claimed" } else { "already owned" },
            );
            Json(json!({ "ok": true, "added": added, "domains": st.node.owned_domains() }))
        }
        Err(e) => {
            audit(&op, "domain-add", &r.domain, false, &e);
            Json(json!({ "ok": false, "error": e }))
        }
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
async fn names_set(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(r): Json<NameSetReq>,
) -> Json<Value> {
    let op = operator(&headers);
    let full = format!("{}@{}", r.local_part, r.domain);
    let pk = match parse_id(&r.pubkey) {
        Some(x) => x,
        None => {
            audit(&op, "name-set", &full, false, "bad pubkey");
            return Json(json!({ "ok": false, "error": "非法公钥（需 64 位 hex）" }));
        }
    };
    match st.node.admin_set_name(&r.domain, &r.local_part, pk) {
        Ok(rec) => {
            audit(&op, "name-set", &full, true, "");
            Json(json!({ "ok": true, "name": format!("{}@{}", rec.local_part, rec.domain) }))
        }
        Err(e) => {
            audit(&op, "name-set", &full, false, &e);
            Json(json!({ "ok": false, "error": e }))
        }
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

/// P1-10 配置热加载：重读配置文件，热应用可运行时变更的子集（黑名单/对等/per_topic/pkarr/域名）；
/// 需重绑的项（relay URL/端口/身份/db/max_connections）在返回 detail 里提示需重启。
async fn reload(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    let op = operator(&headers);
    match crate::reload_config_from(&st.config_path, &st.node) {
        Ok(summary) => {
            audit(&op, "reload", &st.config_path, true, &summary);
            Ok(Json(json!({ "ok": true, "summary": summary })))
        }
        Err(e) => {
            audit(&op, "reload", &st.config_path, false, &e.to_string());
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
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
                && let Ok(mut m) = slot.lock()
            {
                m.rss_bytes = p.memory();
                m.cpu_pct = p.cpu_usage();
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

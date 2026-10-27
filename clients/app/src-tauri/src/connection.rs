use std::sync::Arc;

use nm_client::{Client, Session};
use prost::Message;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State, async_runtime};

use crate::{
    AppState, Conn, ConnectArgs, devices, hex, pair, pairing, parse_id, session_of, vk_of,
};

/// 完成会话建立：注册为 person、起消息推送循环、存入状态，返回自己的 id(hex)。
/// `wait_ready`：首连 true（保留 im_ready 8s 门控，防登录瞬间补投的离线消息早于前端监听器丢失）；
/// 重连 false（前端监听器早已在，立即放水，不白等 8s）。`gen`：写入 Conn 的连接世代，供单飞重连判重。
pub(crate) async fn finish_session(
    app: &AppHandle,
    state: &State<'_, AppState>,
    client: Client,
    mut session: Session,
    display_name: &str,
    wait_ready: bool,
    conn_gen: u64,
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
    let recv_session = session.clone(); // 收到他人私聊后回送达回执用
    // 前端就绪闸门：首连等 imStart 完成后调用 im_ready 放水，避免节点在「连接建立」瞬间补投的
    // 离线消息早于前端监听器而被丢弃。最多等 8s 兜底。重连时前端监听器早在，跳过门控立即放水。
    let ready_rx = if wait_ready {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
        *state.im_ready.lock().await = Some(ready_tx);
        Some(ready_rx)
    } else {
        None
    };
    async_runtime::spawn(async move {
        if let Some(ready_rx) = ready_rx {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(8), ready_rx).await;
        }
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
            // 群元数据/成员变更：节点推送，须在回执逻辑前拦截（不进聊天、不回执）。
            if let Some(p) = gram
                .payload
                .as_ref()
                .filter(|p| p.type_url == "nmspace.v1/group.updated")
            {
                let body: Value = serde_json::from_slice(&p.value).unwrap_or_else(|_| json!({}));
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
            // 送达回执（双对号）：
            // (a) 转回的回执(kind=Receipt) → 发独立 receipt 事件给前端，升本人气泡为双勾。
            if matches!(gram.kind(), nm_proto::GramKind::Receipt) {
                if let Some(refid) = gram.ref_gram_id {
                    let _ = apph.emit("core://event", json!({
                        "type": "receipt", "ref": refid, "from": hex(&gram.sender), "ts": gram.timestamp_ms
                    }));
                }
                continue;
            }
            // (b) 收到「他人」发来的私聊 → 回送达回执给节点（sender!=自己：排除 deliver_account 的本人同步副本，防自造双勾）。
            if matches!(gram.kind(), nm_proto::GramKind::Message) && gram.sender.as_slice() != my_id.as_slice() {
                let s = recv_session.clone();
                let g = gram.clone();
                async_runtime::spawn(async move { let _ = s.send_receipt(&g).await; });
            }
            // P1-6 多设备已读：StateSync 的"已读标记"gram（payload 是 protobuf，非 UTF-8）。须在通用消息分支前拦截，
            // 否则会被当普通消息乱显示。解出会话+已读位，发独立 read 事件给前端推进未读。
            if let Some(p) = gram
                .payload
                .as_ref()
                .filter(|p| p.type_url == "nmspace.v1/read.marker")
            {
                if let Ok(m) = nm_proto::ReadMarker::decode(p.value.as_slice()) {
                    let _ = apph.emit("core://event", json!({
                        "type": "read", "conversation": hex(&m.conversation), "readUpToMs": m.read_up_to_ms
                    }));
                }
                continue;
            }
            let (type_url, body) = match gram.payload.as_ref() {
                Some(p) => (
                    p.type_url.clone(),
                    String::from_utf8_lossy(&p.value).to_string(),
                ),
                None => (String::new(), String::new()),
            };
            let is_channel = matches!(gram.kind(), nm_proto::GramKind::ChannelPublish);
            // 频道消息：seq 是 home 定序器分配的全局权威序号 → 用 "<cid>:<seq>" 做稳定 id，
            // 令实时广播与 backfill 回填指向同一条（修「同一条消息两套 id → SQLite 重复显示」的 bug）。
            // 私聊/群：gram_id 是发送方每连接从 1 起的计数，重连会重复 → 拼发送方+时间戳才唯一。
            let msg_id = if is_channel {
                format!("{}:{}", hex(&gram.receiver), gram.gram_id)
            } else {
                format!("{}-{}-{}", hex(&gram.sender), gram.timestamp_ms, gram.gram_id)
            };
            let ev = json!({
                "type": "message",
                "msg": {
                    "id": msg_id,
                    "from": hex(&gram.sender),
                    "to": hex(&gram.receiver),                 // 群消息=群id；频道=频道id；私聊=本人id
                    "group": matches!(gram.kind(), nm_proto::GramKind::GroupMessage),
                    "channel": is_channel,
                    "seq": gram.gram_id,                       // 频道：全局 seq（排序/回填游标）；其它忽略
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
        conn_gen,
    });

    // 换网哨兵（P1 入站主动重连）：本端点地址(IP/relay)一变即发换网信号，由 setup 的重连驱动主动重连，
    // 恢复入站推送通道——不必等用户下一次发送才发现连接已死。重连走单飞锁 + gen 判重（见 reconnect），
    // 天然防与发送重连打架。哨兵只发信号（Send 安全）：重连 future 含 tauri State，不能进裸 spawn。
    // 见 docs/NETWORK_ROAMING_RECOVERY_DESIGN.md §5.5。
    if let Some(conn) = state.conn.lock().await.as_ref() {
        if let Some(tx) = state.roam_tx.get().cloned() {
            conn._client.spawn_addr_change_task(move || {
                let _ = tx.send(());
            });
        }
    }

    // 应用层心跳保活：每 20s ping 当前会话；ping 失败(连接死)→ 发换网信号走单飞重连（不自己拨号，
    // 与换网哨兵/发送重连共用一条重连路径，防风暴）。任务随世代退出：重连换了 session(conn_gen 变)
    // 后旧任务自动结束，避免多开。QUIC 层已有 3s keep-alive，这里 20s 应用层探测补"半死/假活"场景。
    if let Some(tx) = state.roam_tx.get().cloned() {
        let app_hb = app.clone();
        let my_gen = conn_gen;
        async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                let Some(st) = app_hb.try_state::<AppState>() else { break };
                // 取当前会话 + 校验世代：世代已变=本任务对应的连接已被重连取代，退出。
                let (sess, cur_gen) = {
                    let g = st.conn.lock().await;
                    match g.as_ref() {
                        Some(c) => (c.session.clone(), c.conn_gen),
                        None => break,
                    }
                };
                if cur_gen != my_gen {
                    break;
                }
                // ping 带超时判死；成功=连接活，失败/超时=发信号触发单飞重连。
                let dead = match tokio::time::timeout(SEND_TIMEOUT, sess.ping()).await {
                    Ok(Ok(())) => false,
                    Ok(Err(e)) => looks_dead(&sess, &e),
                    Err(_) => true, // 超时
                };
                if dead {
                    let _ = tx.send(());
                }
            }
        });
    }

    Ok(hex(&my_id))
}

/// 换网重连驱动：在 setup 里启动的长驻任务，从换网信号通道收信号，去重节流后**按需**自愈重连。
/// 持 `AppHandle`（Send），每次现取 `State` 调 `reconnect`，避开"重连 future 非 Send 不能进裸 spawn"。
///
/// 两道闸 + 一个自愈循环：
/// 1) **节流**：合并 300ms 内的连环地址抖动。
/// 2) **主动验活（ping）**：换网信号不等于连接已死。用一次带超时的 `ping` 探活——ping 通就**不重连**
///    （根除"地址一抖就断连重连"的自激风暴），仅 ping 超时/失败（含换网后的 QUIC 假死，此时
///    `close_reason` 常仍为 `None`，旧的 close_reason 验活会漏判）才认定需要重连。
/// 3) **无限退避自愈**：一旦认定失活，就 2s→30s 退避**反复重拨直到连上**（对齐节点侧
///    `online_by_id_retry` 的"永不放弃"语义），而非单次失败即停——这正是 app 换网后"一直连不上、
///    重启才好"的根因修复。终止条件：连上、或用户已断开（conn=None）/已锁库。
pub(crate) async fn roam_reconnect_driver(
    app: AppHandle,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    while rx.recv().await.is_some() {
        // 节流：合并 300ms 内的连环地址变化（换网瞬间常多次抖动），只处理一次。
        while tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv())
            .await
            .is_ok()
        {}
        let Some(state) = app.try_state::<AppState>() else { continue };
        // 取当前会话 + 世代；未连接则忽略本次信号。
        let (cur_gen, session) = {
            let g = state.conn.lock().await;
            match g.as_ref() {
                Some(c) => (c.conn_gen, c.session.clone()),
                None => continue,
            }
        };
        // 主动验活：ping 通=连接仍活，忽略本次换网信号（交由发送路径按需重连）。
        // ping 超时/失败=真失活（含换网后假死，close_reason 可能仍为 None）→ 进入自愈。
        let alive = matches!(
            tokio::time::timeout(SEND_TIMEOUT, session.ping()).await,
            Ok(Ok(()))
        );
        if alive {
            continue;
        }
        drop(session);
        tracing::info!("roam: 连接失活（ping 超时/失败），进入自愈重连循环");
        let _ = app.emit("core://event", json!({ "type": "roaming" }));

        // 无限退避自愈：反复重拨直到连上 / 用户断开 / 锁库。
        let mut backoff = std::time::Duration::from_secs(2);
        loop {
            // 终止：用户已断开连接（disconnect/lock 把 conn 置 None）。
            if state.conn.lock().await.is_none() {
                tracing::info!("roam: 连接已被清除（断开/切换账号），停止自愈");
                break;
            }
            // 终止：库已上锁——重连需 vault 现解身份，锁库下反复试无意义。
            if vk_of(&state).is_err() {
                tracing::warn!("roam: 库已锁，停止自愈（解锁后由后续信号重新触发）");
                break;
            }
            // 用进入自愈时的世代触发重连；若期间已被发送路径连上（gen 前进），reconnect 内判重即 Ok。
            match reconnect(&app, &state, cur_gen).await {
                Ok(()) => {
                    tracing::info!("roam: 自愈重连成功");
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        "roam: 自愈重连失败：{e}；{}s 后重试",
                        backoff.as_secs()
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                }
            }
        }
    }
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
    // 存 AppHandle（供重连 helper 在任意位置取），缓存入参供待机唤醒后自动重连。
    let _ = state.app_handle.set(app.clone());
    let args = ConnectArgs {
        user: user.to_string(),
        mode: mode.clone(),
        node: node.clone(),
        display_name: display_name.clone(),
        relay_urls: relay_urls.clone(),
        pkarr_url: pkarr_url.clone(),
        dns_origin: dns_origin.clone(),
    };
    let (client, session) = devices::dial_account(
        &app, &vk, user, &mode, &node, relay_urls, pkarr_url, dns_origin,
    )
    .await?;
    let out = finish_session(&app, &state, client, session, &display_name, true, 0).await?;
    *state.last_connect.lock().await = Some(args);
    Ok(out)
}

/// 发送短超时：待机唤醒后 QUIC 假死时，send 会一直等到底层 idle timeout(10s) 才报错；
/// 用 5s 超时提前判死 → 触发重连，而非让用户干等。
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 判断一次失败是否意味着"连接死了、该重连"（而非业务错误如目标非法/节点拒绝）。
fn looks_dead(session: &Session, err: &nm_client::ClientError) -> bool {
    session.close_reason().is_some() || matches!(err, nm_client::ClientError::Timeout)
}

/// 发送包装：短超时执行一次；超时或连接判死 → 自动重连一次 → 用新 session 重发一次。
/// 业务错误（非连接问题）直接返回，不触发重连。`op` 是"给一个 &Session 执行一次发送"的闭包。
pub(crate) async fn send_with_retry<T, F, Fut>(
    app: &AppHandle,
    state: &State<'_, AppState>,
    op: F,
) -> Result<T, String>
where
    F: Fn(Arc<Session>) -> Fut,
    Fut: std::future::Future<Output = Result<T, nm_client::ClientError>>,
{
    // 快照当前 session + gen。
    let (session, cur_gen) = {
        let g = state.conn.lock().await;
        let c = g.as_ref().ok_or("尚未连接")?;
        (c.session.clone(), c.conn_gen)
    };

    // 第一次尝试。
    match tokio::time::timeout(SEND_TIMEOUT, op(session.clone())).await {
        Ok(Ok(v)) => return Ok(v),
        Ok(Err(e)) if !looks_dead(&session, &e) => return Err(e.to_string()), // 业务错误，不重连
        _ => {} // 超时 或 连接死 → 重连
    }

    // 重连（单飞 + gen 判重），再用新 session 重发一次。
    if let Err(e) = reconnect(app, state, cur_gen).await {
        // 这次重连没成——触发后台自愈驱动接管（退避重试直到连上），别让用户这条失败后就无人重连。
        if let Some(tx) = state.roam_tx.get() {
            let _ = tx.send(());
        }
        return Err(e);
    }
    let s2 = session_of(state).await?;
    match tokio::time::timeout(SEND_TIMEOUT, op(s2)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => {
            if let Some(tx) = state.roam_tx.get() {
                let _ = tx.send(());
            }
            Err("发送失败：重连后仍无法送达".into())
        }
    }
}

/// 自动重连：用缓存的 connect 入参 + vault 身份重新拨号，替换当前死连接。
/// 单飞——并发发送同时触发时只有一个真正拨号，其余发现 gen 已前进即复用。
/// 先拨号成功再换手：拨号失败时旧（死）Conn 原样保留，不清成 None。
pub(crate) async fn reconnect(
    app: &AppHandle,
    state: &State<'_, AppState>,
    failed_gen: u64,
) -> Result<(), String> {
    let _guard = state.reconnect_lock.lock().await; // 单飞守卫

    // 别人已经重连过了（gen 前进）→ 直接复用，不再拨号。
    {
        let g = state.conn.lock().await;
        if let Some(c) = g.as_ref() {
            if c.conn_gen != failed_gen {
                return Ok(());
            }
        }
    }

    let args = state
        .last_connect
        .lock()
        .await
        .clone()
        .ok_or("无重连上下文（尚未成功连接过）")?;
    let vk = vk_of(state)?; // 库已锁 → 直接失败，提示解锁

    let _ = app.emit("core://event", json!({ "type": "reconnecting" }));

    // 换网重连：home node 一律**按公钥 id** 拨号（经发现服务 pkarr/DNS 重解析当前地址）。
    // 缓存的完整地址换网后必失效，直接用它会把连接导向死地址；dial 还会先浪费数秒试「同网直连旧地址」。
    // 故把 node 收敛成纯公钥 hex，让 dial 跳过同网直连、直奔发现重解析（对齐节点侧 online_by_id）。
    let redial_node = match nm_transport::addr_from_string(args.node.trim()) {
        Ok(a) => hex(a.id.as_bytes()),
        Err(_) => args.node.clone(),
    };

    // 先拨号（12s 兜底）成功，再换手。
    let dialed = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        devices::dial_account(
            app,
            &vk,
            &args.user,
            &args.mode,
            &redial_node,
            args.relay_urls.clone(),
            args.pkarr_url.clone(),
            args.dns_origin.clone(),
        ),
    )
    .await;
    let (client, session) = match dialed {
        Ok(Ok(cs)) => cs,
        // 失败不发终态 reconnect_failed：自愈驱动会继续退避重试，发送路径会返回错误给用户。
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err("重连超时（12s）".into()),
    };

    // gram_id 续号：从旧 session 高水位接着走，避免被节点 SeenGrams 误去重（群/跨节点消息静默丢失）。
    let old = state.conn.lock().await.take();
    if let Some(old) = &old {
        session.seed_corr(old.session.current_corr());
    }

    // 装配新 session（重连跳过 im_ready 门控，立即放水）；gen+1。
    let new_gen = failed_gen.wrapping_add(1);
    finish_session(
        app,
        state,
        client,
        session,
        &args.display_name,
        false,
        new_gen,
    )
    .await?;

    // 关旧连接 → 旧收流任务退出 → 旧 inbox 发送端 drop → 旧事件循环 recv()=None 退出，不双投。
    // disconnect(&self) 自带 800ms 超时，死连接也不会挂住。
    if let Some(old) = old {
        old.session.disconnect().await;
    }

    let _ = app.emit("core://event", json!({ "type": "reconnected" }));
    Ok(())
}

/// 前端在 `imStart` 完成（已注册 core://event 监听、已设 MY_ID、本地库就绪）后调用，放开消息事件循环，
/// 使节点在连接瞬间补投的离线消息被可靠接收 + 入库 + 展示（修复「登录后看不到离线消息」）。
#[tauri::command]
pub(crate) async fn im_ready(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(tx) = state.im_ready.lock().await.take() {
        let _ = tx.send(());
    }
    Ok(())
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
    app: AppHandle,
    state: State<'_, AppState>,
    target: String,
    text: String,
) -> Result<u64, String> {
    let id = parse_id(&target)?; // 业务校验先做，错了直接回，不触发重连
    send_with_retry(&app, &state, move |s| {
        let text = text.clone();
        // 返回节点 ack 回执里的 ref_gram_id = 本条消息 gram_id，供前端标记单勾✓。
        async move { s.send_to(id, &text).await.map(|ack| ack.ref_gram_id.unwrap_or(0)) }
    })
    .await
}

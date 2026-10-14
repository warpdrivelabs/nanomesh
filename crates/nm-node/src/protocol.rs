use std::sync::Arc;

use dashmap::DashSet;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh_gossip::Gossip;
use nm_core::Directory;
use nm_proto::{
    now_ms, Any, Command, CommandResult, DeviceRevoke, DirectoryQuery, Entity, EntityList,
    FedSyncResp, Gram, GramKind, Group, GroupGossip, GroupList, GroupOp, NameList, NameOp,
    NameQuery, NameRecord,
};
use nm_transport::{read_gram, write_gram, IrohConnection};
use prost::Message;

use crate::blobs::{blob_get, blob_put};
use crate::channels::{
    channel_backfill, channel_create, channel_list, channel_publish, channel_set_meta,
    channel_sub, channel_unsub,
};
use crate::delivery::{
    account_online, clear_inbox_on_receipt, deliver_account, deliver_direct, deliver_group_here,
    deliver_or_store,
    drain_inbox_to, group_bootstrap,
};
use crate::devices::{
    apply_revoke, device_freeze, device_hello, device_list, device_rename, is_admin_conn,
    is_revoked,
};
use crate::federation_runtime::announce_group;
use crate::grams::{command_result_gram, login_ok, receipt_for, reply_gram};
use crate::groups::{group_create, group_dissolve, group_mutate, GroupMut};
use crate::naming::{
    acct_fail, acct_text, login_name_account, lookup_name_account, name_canonical,
    passwd_name_account, register_name_account, reset_name_account, valid_local_part,
};
use crate::naming_runtime::apply_domain_decision;
use crate::presence::{presence_status, PresenceRec};
use crate::records::hex_decode_n;
use crate::retire::group_topic_retireable;
use crate::roster::{roster_add, roster_list, roster_remove, roster_update};
use crate::{now_secs, Ctx, SessionMeta, LIVE_TTL_MS};

/// nmspace 单播/命令/联邦协议处理器：包住既有的每连接处理逻辑，注册到 `Router` 的 `nmspace/0`。
pub(crate) struct ImspaceProto {
    pub(crate) ctx: Ctx,
}

impl std::fmt::Debug for ImspaceProto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImspaceProto")
    }
}

impl ProtocolHandler for ImspaceProto {
    async fn accept(&self, conn: IrohConnection) -> Result<(), AcceptError> {
        let rid_arr = *conn.remote_id().as_bytes();
        // 黑名单闸门：被禁公钥的连接直接拒绝（无白名单——其余一律放行）。
        if self.ctx.blacklist.contains(&rid_arr) {
            tracing::info!(peer = %conn.remote_id().fmt_short(), "rejected banned peer");
            conn.close(0u32.into(), b"banned");
            return Ok(());
        }
        if is_revoked(&self.ctx, &rid_arr, None) {
            tracing::info!(peer = %conn.remote_id().fmt_short(), "rejected revoked device");
            conn.close(0u32.into(), b"device_revoked");
            return Ok(());
        }
        // 限流闸①：全局并发连接上限。超则拒绝新连接，防 FD/内存耗尽（DoS 防护）。
        // 用 sessions.len() 作权威——它随连接关闭自动回落，无需额外计数器同步。
        if self.ctx.sessions.len() >= self.ctx.max_connections {
            tracing::warn!(online = self.ctx.sessions.len(), max = self.ctx.max_connections,
                peer = %conn.remote_id().fmt_short(), "rejected: connection limit reached");
            conn.close(0u32.into(), b"server_busy");
            return Ok(());
        }
        // 限流闸②：单公钥重连速率门（滑动计数窗口）。拦持续重连风暴/churn，
        // 但放行 app 重启/切后台/断线后的正常重连（1s 内也允许，不再一刀切拒绝）。
        {
            let now = crate::now_secs();
            let gate = &self.ctx.conn_gate;
            // 容量兜底：表满则整表清空（惰性，牺牲一轮限流精度换无界防护）。放在读改之前，避免清掉刚写的条目。
            if gate.len() >= crate::CONN_GATE_CAP {
                gate.clear();
            }
            let mut rejected = false;
            gate.entry(rid_arr)
                .and_modify(|(win_start, count)| {
                    if now.saturating_sub(*win_start) >= crate::CONN_RATE_WINDOW_SECS {
                        *win_start = now; // 窗口过期 → 重置
                        *count = 1;
                    } else if *count >= crate::CONN_RATE_MAX_IN_WINDOW {
                        rejected = true; // 窗口内已达上限 → 拒绝（持续风暴）
                    } else {
                        *count += 1;
                    }
                })
                .or_insert((now, 1));
            if rejected {
                tracing::warn!(peer = %conn.remote_id().fmt_short(),
                    "rejected: reconnect rate limited (storm)");
                conn.close(0u32.into(), b"rate_limited");
                return Ok(());
            }
        }
        let rid = rid_arr.to_vec();
        self.ctx.sessions.insert(rid.clone(), conn.clone());
        self.ctx
            .sessions_meta
            .insert(rid.clone(), SessionMeta { since: std::time::SystemTime::now() });
        tracing::info!(online = self.ctx.sessions.len(), "session up");

        // 连接关闭事件驱动地清理会话表（仅当表中仍是「这条」连接时移除，避免误删重连后的新会话）。
        {
            let sessions = self.ctx.sessions.clone();
            let principals = self.ctx.principals.clone();
            let watch_conn = conn.clone();
            let watch_rid = rid.clone();
            let closed_sid = watch_conn.stable_id();
            tokio::spawn(async move {
                let _ = watch_conn.closed().await;
                let stale = sessions
                    .get(&watch_rid)
                    .map(|c| c.stable_id() == closed_sid)
                    .unwrap_or(false);
                if stale {
                    sessions.remove(&watch_rid);
                    principals.remove(&watch_rid);
                    tracing::info!(online = sessions.len(), "session closed");
                }
            });
        }

        // 上线补投：把该连接（设备）的离线消息经 uni 流推送后清空。
        drain_inbox_to(&self.ctx, &rid, &conn);

        // 运行该连接的 gram 处理循环，直到连接关闭（handle_conn 内部会清理会话表）。
        handle_conn(conn, self.ctx.clone(), rid).await;
        Ok(())
    }
}

/// gossip 连接的黑名单闸门：被禁公钥的 gossip 连接直接拒，其余转交 iroh-gossip 处理。
/// （注意：只能在**连接/邻居层**拦截；gossip 广播消息不带作者级签名，无法按原始作者逐条过滤。）
pub(crate) struct GossipGate {
    pub(crate) gossip: Gossip,
    pub(crate) blacklist: Arc<DashSet<[u8; 32]>>,
}

impl std::fmt::Debug for GossipGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GossipGate")
    }
}

impl ProtocolHandler for GossipGate {
    async fn accept(&self, conn: IrohConnection) -> Result<(), AcceptError> {
        let rid = *conn.remote_id().as_bytes();
        if self.blacklist.contains(&rid) {
            tracing::info!(peer = %conn.remote_id().fmt_short(), "rejected banned peer (gossip)");
            conn.close(0u32.into(), b"banned");
            return Ok(());
        }
        self.gossip
            .handle_connection(conn)
            .await
            .map_err(AcceptError::from_err)?;
        Ok(())
    }
}

async fn handle_conn(conn: IrohConnection, ctx: Ctx, rid: Vec<u8>) {
    loop {
        let (mut send, mut recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(_) => break, // 连接关闭
        };
        let gram = match read_gram(&mut recv).await {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("read gram failed: {e}");
                break;
            }
        };
        tracing::info!(kind = ?gram.kind(), gram_id = gram.gram_id, "recv gram");
        let principal = ctx.principals.get(&rid).map(|p| p.clone()).unwrap_or_else(|| rid.clone());
        // 黑名单（纵深防御）：连接建立后才被拉黑的对端，其后续 gram 一律**静默**丢弃——
        // 不回任何响应、直接关流，不向被拉黑者泄露"你被拉黑了"（区别于下方的明确拒绝）。
        let blacklisted = [principal.as_slice(), rid.as_slice()].iter().any(|k| {
            <[u8; 32]>::try_from(*k).map(|c| ctx.blacklist.contains(&c)).unwrap_or(false)
        });
        if blacklisted {
            let _ = send.finish(); // 静默：关流不写，被拉黑方读到空流（符合"拉黑=隐身"）
            continue;
        }
        // 契约：非黑名单的请求必有响应。handle_gram 返回 None = 消息被拒（防冒名/未知 kind），
        // 若直接关流，client 的 route_send 读到空流会报 "stream finished early(0 bytes read)"——
        // 既误导用户（像连接坏了，实为被拒），又让重连无法自愈。故回明确的拒绝 Reply。
        let reply = match handle_gram(&gram, &ctx, &principal, &rid).await {
            Some(r) => r,
            None => reply_gram(
                &gram,
                GramKind::Reply,
                Some(nm_proto::pb::Any {
                    type_url: "nmspace.v1/error".into(),
                    value: b"rejected: message dropped by node (sender-principal mismatch / unsupported)".to_vec(),
                }),
            ),
        };
        if let Err(e) = write_gram(&mut send, &reply).await {
            tracing::warn!("write reply failed: {e}");
        }
        let _ = send.finish();
    }
    let my_sid = conn.stable_id();
    let stale = ctx.sessions.get(&rid).map(|c| c.stable_id() == my_sid).unwrap_or(false);
    if stale {
        ctx.sessions.remove(&rid);
        ctx.principals.remove(&rid);
        tracing::info!(online = ctx.sessions.len(), "session down");
    }
}

/// `caller` 是账号身份（设备连接 hello 后为账号公钥，否则等于连接公钥）；`rid` 是连接公钥。
async fn handle_gram(gram: &Gram, ctx: &Ctx, caller: &[u8], rid: &[u8]) -> Option<Gram> {
    // 黑名单：主拦截在 handle_conn（静默关流、不泄露拉黑）。此处为纵深防御冗余——
    // 正常走连接路径到不了（上游已拦），保留以防未来有其它调用方绕过 handle_conn。
    for k in [caller, rid] {
        if let Ok(c) = <[u8; 32]>::try_from(k) {
            if ctx.blacklist.contains(&c) {
                return None;
            }
        }
    }
    // 防冒名：聊天消息的 sender 必须是本连接的账号身份。
    // caller 是本连接 principal：device.hello 成功后 = 账号；未完成时 fallback = 设备密钥(rid)。
    // 故 sender==caller 不够——online_as 的多设备连接里 sender 是账号、caller 可能还是设备密钥，
    // 会把合法消息误判为冒名并静默丢弃（client 侧表现为 "stream finished early"）。
    // 正确判据：sender 是本连接 principal，或是本连接设备(rid)的证书所声明的账号。
    if matches!(gram.kind(), GramKind::Message | GramKind::GroupMessage) {
        let sender_ok = gram.sender.as_slice() == caller
            || ctx
                .devices
                .get(rid)
                .and_then(|d| d.cert.as_ref().map(|c| c.account == gram.sender))
                .unwrap_or(false);
        if !sender_ok {
            tracing::warn!("message sender != connection principal/device-account; dropped");
            return None;
        }
    }
    // 群消息：receiver 是 group_id，由节点扇出（不是直接路由目标）。
    // 扇出可能对每个离线/跨节点成员做目录查询/中继（各带网络超时），若同步等待会
    // 阻塞给发送方的 ack → 前端「发送很迟钝」。故后台扇出、立即回执（存转发语义）。
    if matches!(gram.kind(), GramKind::GroupMessage) {
        let (g, c, who, r) = (gram.clone(), ctx.clone(), caller.to_vec(), rid.to_vec());
        ctx.tracker.spawn(async move { fanout_group(&g, &c, &who, &r).await });
        return Some(receipt_for(gram));
    }
    // Relay 信封（来自对等节点的跨节点转发）：解出内层 gram，按本地投递。
    if matches!(gram.kind(), GramKind::Relay) {
        if let Some(inner) = gram.payload.as_ref().and_then(|p| Gram::decode(p.value.as_slice()).ok()) {
            deliver_or_store(&inner.receiver, &inner, ctx).await;
        }
        return Some(reply_gram(gram, GramKind::Receipt, None));
    }
    // 送达回执：client 收到推送的私聊后回的 ack（kind=Receipt, ref=原gram_id, receiver=原sender）。
    // 1) 清理该设备在本节点的离线副本（Tier2）；2) 把回执转回原 sender（支撑双勾，可跨节点）；
    // 3) 回 Reply 而非 Receipt —— 断开"回执再触发回执"的环（防环三闸之二）。
    if matches!(gram.kind(), GramKind::Receipt) {
        // 防伪造：回执的 sender 必须是本连接身份。
        if gram.sender.as_slice() != caller {
            return Some(reply_gram(gram, GramKind::Reply, None));
        }
        if let Some(orig_id) = gram.ref_gram_id {
            clear_inbox_on_receipt(ctx, rid, caller, orig_id);
        }
        // receiver 已是原 sender → deliver_direct 本地直投 / 离线入库 / 远端 gossip 转回。
        let (g, c) = (gram.clone(), ctx.clone());
        ctx.tracker.spawn(async move { deliver_direct(&g, &c, false).await; }); // Receipt 转回不入 outbox
        return Some(reply_gram(gram, GramKind::Reply, None));
    }
    let to = &gram.receiver;
    let node_id = &ctx.node_id;
    let addressed_to_node = to.is_empty() || to.as_slice() == node_id.as_slice();
    if !addressed_to_node {
        // 路由命令若目标要求授权(require_grant)，先校验 Grant，不合格则拒绝并回推错误。
        if matches!(gram.kind(), GramKind::Command) {
            if let Ok(Some(target)) = ctx.dir.get(to).await {
                if target.attributes.get("require_grant").map(String::as_str) == Some("true") {
                    if let Err(reason) = authorize_routed(gram, caller, &target) {
                        tracing::warn!(%reason, "routed command denied");
                        push_command_denied(gram, rid, ctx, &reason);
                        return Some(reply_gram(gram, GramKind::Receipt, None));
                    }
                }
            }
        }
        // Message 可离线暂存；Command/CommandResult 仅在线路由。
        // 单播私聊改走 gossip（本地投递不了则联邦广播，对端节点投递）——取代 NAT 下常不通的 s2s 中继。
        if matches!(gram.kind(), GramKind::Message) {
            let (g, c, who, r) = (gram.clone(), ctx.clone(), caller.to_vec(), rid.to_vec());
            ctx.tracker.spawn(async move {
                deliver_direct(&g, &c, true).await; // 真实 Message：跟踪可靠投递
                // 已发同步：同账号的其它设备也收到一份（客户端按 sender==自己 归入会话）。
                if g.receiver != who {
                    deliver_account(&who, &g, &c, true, Some(&r)).await;
                }
            });
        } else {
            deliver_account(to, gram, ctx, false, None).await;
        }
        return Some(reply_gram(gram, GramKind::Receipt, None)); // 给发送方回 ack
    }
    // 面向节点本身的请求。
    match gram.kind() {
        GramKind::Command => handle_command(gram, ctx, caller, rid).await,
        GramKind::Message => Some(receipt_for(gram)),
        GramKind::Ping => Some(reply_gram(gram, GramKind::Pong, None)), // 应用层心跳
        GramKind::Login => Some(login_ok(gram)),
        GramKind::Logout => {
            ctx.sessions.remove(rid); // 显式下线：同步移除会话
            ctx.principals.remove(rid);
            tracing::info!(online = ctx.sessions.len(), "session logout");
            Some(reply_gram(gram, GramKind::Reply, None))
        }
        _ => None,
    }
}

/// 群消息扇出：`receiver` 为 group_id；对每个成员(除发送方)——在线则路由、离线则入库。
async fn fanout_group(gram: &Gram, ctx: &Ctx, caller: &[u8], rid: &[u8]) {
    let Some(group) = ctx.groups.get(&gram.receiver).map(|g| g.clone()) else {
        tracing::warn!("group message to unknown group");
        return;
    };
    // 仅群成员可发。
    if !group.members.iter().any(|m| m.as_slice() == caller) {
        tracing::warn!("non-member group message denied");
        return;
    }
    // 本地投递：本节点在线成员直投；本节点为其 home 的离线成员入库补投。
    deliver_group_here(gram, &group.members, rid, ctx).await;
    // 跨节点：发布到群联邦 gossip，各成员节点收到后各自投递「本地成员」。
    // 取代此前的 s2s 中继（NAT 下常超时不通）——与频道同走 gossip 叠加网。
    let gg = GroupGossip {
        origin: ctx.node_id.to_vec(),
        body: Some(nm_proto::pb::group_gossip::Body::Msg(gram.clone())),
    };
    let bytes = gg.encode_to_vec();
    let per_topic = ctx.per_topic.load(std::sync::atomic::Ordering::Relaxed);
    // F5：当「本群所有远端成员(home!=本节点)的 home 都在该群主题上存活」时，省略火管的群消息副本——
    // 该消息仅经每群主题投递(仅成员节点收)，实现真正「仅成员可达」。保守：任一远端 home 存活未知/过期
    // → 仍发火管(自愈，不漏投)。注意仅针对群「消息」；announce 仍走火管(发现/收敛)，直至 F2 DHT 落地。
    let retireable = per_topic
        && group_topic_retireable(&ctx.node_id, &ctx.dir.entities, &ctx.per_group_live, &group, now_ms(), LIVE_TTL_MS);
    // 每群主题双写（仅成员节点收；收端按 gram_id 去重 F1-2a）。
    let mut per_topic_ok = false;
    if per_topic {
        let _ = ctx.fed.join_group(&gram.receiver, group_bootstrap(&group, ctx)).await;
        per_topic_ok = ctx.fed.publish(&gram.receiver, bytes.clone()).await.is_ok();
    }
    // 火管兜底：仅当「可退火管」且每群主题确已发出时才省略；否则(含 per-topic 发布失败)仍发火管，绝不漏投。
    if retireable && per_topic_ok {
        tracing::debug!("firehose suppressed for group message (all remote homes live on topic)");
    } else {
        let _ = ctx.group_pub.send(bytes);
    }
}

async fn handle_command(gram: &Gram, ctx: &Ctx, caller: &[u8], rid: &[u8]) -> Option<Gram> {
    let dir = &*ctx.dir;
    let groups = &*ctx.groups;
    let store = &ctx.store;
    let cmd = Command::decode(gram.payload.as_ref()?.value.as_slice()).ok()?;
    let (ok, result, error) = match cmd.method.as_str() {
        "domain.decision" => {
            apply_domain_decision(gram, &ctx.domains, &ctx.store, &ctx.domain_notices);
            (true, None, String::new())
        }
        "directory.register" | "profile.update" => match cmd
            .params
            .as_ref()
            .and_then(|p| Entity::decode(p.value.as_slice()).ok())
        {
            // 认证：只能注册/更新与连接身份一致的实体（防冒名）。
            Some(entity) if entity.entity_id != caller => {
                (false, None, "entity_id != connection identity".to_string())
            }
            Some(mut entity) => {
                let kind = entity.kind.clone();
                entity.home_node = ctx.node_id.to_vec(); // 归属本节点（联邦寻址）
                // LWW 收敛：拒绝比现有更旧的更新；仅接受时才持久化，避免旧副本回写存储。
                if dir.merge_lww(entity.clone()) {
                    if let Some(s) = store {
                        if let Err(e) = s.put_entity(&entity) {
                            tracing::warn!("persist entity failed: {e}");
                        }
                    }
                    tracing::info!(%kind, total = dir.len(), "entity upserted (register/profile.update)");
                    (true, None, String::new())
                } else {
                    (false, None, "stale update: older than current updated_at".to_string())
                }
            }
            None => (false, None, "invalid entity".to_string()),
        },
        "directory.query" => {
            let q = cmd
                .params
                .as_ref()
                .and_then(|p| DirectoryQuery::decode(p.value.as_slice()).ok())
                .unwrap_or_default();
            let mut list = dir.query(&q).await.unwrap_or_default();
            // P2：给每个实体盖上实时 presence（本地会话/gossip 缓存 + TTL）到 attributes["presence"]。
            // N1：反向盖上命名 name=local@domain（命名缓存里 client_pubkey 命中者）。
            let name_by_pk: std::collections::HashMap<Vec<u8>, String> = ctx
                .names
                .iter()
                .map(|r| (r.client_pubkey.clone(), format!("{}@{}", r.local_part, r.domain)))
                .collect();
            for e in list.iter_mut() {
                let p = presence_status(&ctx.sessions, &ctx.principals, &ctx.devices, &ctx.presence, &ctx.status_intent, &e.entity_id);
                e.attributes.insert("presence".to_string(), p);
                if let Some(n) = name_by_pk.get(&e.entity_id) {
                    e.attributes.insert("name".to_string(), n.clone());
                }
            }
            let el = EntityList { entities: list };
            (
                true,
                Some(Any {
                    type_url: "nmspace.v1.EntityList".to_string(),
                    value: el.encode_to_vec(),
                }),
                String::new(),
            )
        }
        // P2：设置本人在线状态（away/busy/dnd/online）；params.value = 状态字符串(UTF-8)。
        "presence.set" => {
            let status = cmd
                .params
                .as_ref()
                .map(|p| String::from_utf8_lossy(&p.value).trim().to_string())
                .unwrap_or_default();
            if status.is_empty() || status == "online" {
                ctx.status_intent.remove(caller);
            } else {
                ctx.status_intent.insert(caller.to_vec(), status.clone());
            }
            if account_online(&ctx.sessions, &ctx.principals, &ctx.devices, caller) {
                ctx.presence.insert(
                    caller.to_vec(),
                    PresenceRec {
                        status: if status.is_empty() { "online".into() } else { status },
                        last_seen: now_secs(),
                        home_node: ctx.node_id.to_vec(),
                    },
                );
            }
            (true, None, String::new())
        }
        // A：按 id 批量查在线状态——供 app 给「跨节点好友」（不在本地 directory）盖 presence。
        // params.value = JSON {"ids":["<hex>",...]}；结果 = JSON {"<hex>":"online|away|...|offline"}。
        // 复用 presence_of 同源（本地会话priority + gossip 缓存 + TTL），节点间 presence 不额外 RPC。
        "presence.query" => {
            let ids: Vec<String> = cmd
                .params
                .as_ref()
                .and_then(|p| serde_json::from_slice::<serde_json::Value>(&p.value).ok())
                .and_then(|v| v.get("ids").and_then(|x| x.as_array()).map(|a| {
                    a.iter().filter_map(|e| e.as_str().map(|s| s.to_string())).collect()
                }))
                .unwrap_or_default();
            let mut out = serde_json::Map::new();
            for id_hex in ids.into_iter().take(1024) {
                if let Some(id) = hex_decode_n::<32>(&id_hex) {
                    let st = presence_status(&ctx.sessions, &ctx.principals, &ctx.devices, &ctx.presence, &ctx.status_intent, &id);
                    out.insert(id_hex, serde_json::Value::String(st));
                }
            }
            let body = serde_json::Value::Object(out).to_string();
            (true, Some(Any { type_url: "text/plain".into(), value: body.into_bytes() }), String::new())
        }
        "roster.list" => roster_list(ctx, caller),
        "roster.add" => roster_add(ctx, caller, &cmd),
        "roster.remove" => roster_remove(ctx, caller, &cmd),
        "roster.update" => roster_update(ctx, caller, &cmd),
        "blob.put" => blob_put(ctx, &cmd),
        "blob.get" => blob_get(ctx, &cmd).await,
        "group.create" => group_create(&cmd, groups, store, caller, ctx.node_id.as_slice()),
        "group.join" => group_mutate(&cmd, groups, store, caller, GroupMut::Join),
        "group.leave" => group_mutate(&cmd, groups, store, caller, GroupMut::Leave),
        "group.add" => group_mutate(&cmd, groups, store, caller, GroupMut::Add),
        "group.kick" => group_mutate(&cmd, groups, store, caller, GroupMut::Kick),
        "group.promote" => group_mutate(&cmd, groups, store, caller, GroupMut::Promote),
        "group.demote" => group_mutate(&cmd, groups, store, caller, GroupMut::Demote),
        "group.rename" => group_mutate(&cmd, groups, store, caller, GroupMut::Rename),
        "group.set_meta" => group_mutate(&cmd, groups, store, caller, GroupMut::SetMeta),
        "group.dissolve" => group_dissolve(&cmd, groups, store, caller),
        // ── 去中心命名（N1：home node 委派 local@domain）──
        "name.claim" => match cmd.params.as_ref().and_then(|p| NameOp::decode(p.value.as_slice()).ok()) {
            Some(op) => {
                let domain = ctx.domains.read().unwrap().first().cloned().unwrap_or_default();
                let local = op.local_part.trim().to_lowercase();
                if domain.is_empty() {
                    (false, None, "本节点未配置域名（无法签发命名）".into())
                } else if !valid_local_part(&local) {
                    (false, None, "非法 local-part（仅 a-z 0-9 . - _，≤63）".into())
                } else {
                    let full = format!("{local}@{domain}");
                    let taken_by_other = ctx.names.get(&full).map(|r| r.client_pubkey != caller).unwrap_or(false);
                    if taken_by_other {
                        (false, None, "该名字已被占用".into())
                    } else {
                        let serial = ctx.names.get(&full).map(|r| r.serial + 1).unwrap_or(1);
                        let mut rec = NameRecord {
                            local_part: local, domain, client_pubkey: caller.to_vec(), serial,
                            issued_at: now_ms() as i64, ttl: 3600, home_node: ctx.node_id.to_vec(),
                            home_sig: Vec::new(),
                        };
                        rec.home_sig = nm_crypto::sign_bytes(ctx.ep.secret_key(), &name_canonical(&rec)).to_vec();
                        if let Some(s) = &ctx.store {
                            let _ = s.put_name(&rec);
                        }
                        ctx.names.insert(full.clone(), rec.clone());
                        // P1: back-fill Entity.attributes["name"] with the just-claimed handle so it
                        // survives in persistent storage and appears offline (not just on live query).
                        if let Some(mut e) = ctx.dir.entities.get(caller).map(|e| e.clone()) {
                            e.attributes.insert("name".to_string(), full.clone());
                            e.updated_at = now_ms() as i64;
                            ctx.dir.merge_lww(e.clone());
                            if let Some(s) = &ctx.store { let _ = s.put_entity(&e); }
                        }
                        // 立即经联邦 gossip 广播，各节点填充命名缓存 → 全网可解析。
                        let gg = GroupGossip {
                            origin: ctx.node_id.to_vec(),
                            body: Some(nm_proto::pb::group_gossip::Body::Name(rec.clone())),
                        };
                        let _ = ctx.group_pub.send(gg.encode_to_vec());
                        (true, Some(Any { type_url: "nmspace.v1.NameRecord".into(), value: rec.encode_to_vec() }), String::new())
                    }
                }
            }
            None => (false, None, "invalid name op".into()),
        },
        "name.resolve" => match cmd.params.as_ref().and_then(|p| NameQuery::decode(p.value.as_slice()).ok()) {
            Some(q) => {
                let name = q.name.trim().to_lowercase();
                let records: Vec<NameRecord> = ctx.names.get(&name).map(|r| r.clone()).into_iter().collect();
                (true, Some(Any { type_url: "nmspace.v1.NameList".into(), value: NameList { records }.encode_to_vec() }), String::new())
            }
            None => (false, None, "invalid name query".into()),
        },
        "name.reverse" => match cmd.params.as_ref().and_then(|p| NameQuery::decode(p.value.as_slice()).ok()) {
            Some(q) => {
                let records: Vec<NameRecord> = ctx.names.iter().find(|r| r.client_pubkey == q.pubkey).map(|r| r.clone()).into_iter().collect();
                (true, Some(Any { type_url: "nmspace.v1.NameList".into(), value: NameList { records }.encode_to_vec() }), String::new())
            }
            None => (false, None, "invalid name query".into()),
        },
        "name.list" => {
            let records: Vec<NameRecord> = ctx.names.iter().map(|r| r.clone()).collect();
            (true, Some(Any { type_url: "nmspace.v1.NameList".into(), value: NameList { records }.encode_to_vec() }), String::new())
        }
        "name.register" => register_name_account(ctx, caller, &cmd),
        "name.login" => login_name_account(ctx, caller, &cmd),
        "name.lookup" => lookup_name_account(ctx, &cmd),
        "name.passwd" | "name.reset" if !is_admin_conn(ctx, caller, rid) => acct_fail("not_admin_device"),
        "name.passwd" => passwd_name_account(ctx, caller, &cmd),
        "name.reset" => reset_name_account(ctx, caller, &cmd),
        // ── 多设备 ──
        "device.hello" => device_hello(ctx, rid, &cmd),
        "device.list" => device_list(ctx, caller, rid),
        "device.rename" => device_rename(ctx, caller, &cmd),
        "device.revoke" => match cmd.params.as_ref().and_then(|p| DeviceRevoke::decode(p.value.as_slice()).ok()) {
            Some(r) if !r.by_node.is_empty() => acct_fail("by_node_not_allowed"),
            Some(r) => match apply_revoke(ctx, r, true) {
                Ok(_) => acct_text("ok"),
                Err(e) => acct_fail(e),
            },
            None => acct_fail("invalid_revoke"),
        },
        "device.freeze" => device_freeze(ctx, &cmd),
        // ── 频道 / 主题（P4）──
        "channel.create" => channel_create(ctx, caller, rid, &cmd).await,
        "channel.sub" => channel_sub(ctx, rid, &cmd).await,
        "channel.unsub" => channel_unsub(ctx, rid, &cmd),
        "channel.publish" => channel_publish(ctx, caller, &cmd),
        "channel.backfill" => channel_backfill(ctx, &cmd),
        "channel.set_meta" => channel_set_meta(ctx, caller, &cmd),
        "channel.list" => channel_list(ctx),
        "group.list" => {
            let mine: Vec<Group> = groups
                .iter()
                .filter(|g| g.members.iter().any(|m| m.as_slice() == caller))
                .map(|g| g.clone())
                .collect();
            (
                true,
                Some(Any {
                    type_url: "nmspace.v1.GroupList".to_string(),
                    value: GroupList { groups: mine }.encode_to_vec(),
                }),
                String::new(),
            )
        }
        "fed.sync" => {
            // 返回本节点归属的实体（供对等节点并入其目录）。
            let mine: Vec<Entity> = dir
                .entities
                .iter()
                .filter(|e| e.home_node == ctx.node_id.as_slice())
                .map(|e| e.clone())
                .collect();
            (
                true,
                Some(Any {
                    type_url: "nmspace.v1.FedSyncResp".to_string(),
                    value: FedSyncResp { entities: mine }.encode_to_vec(),
                }),
                String::new(),
            )
        }
        other => (false, None, format!("unknown method: {other}")),
    };
    // 群状态变更：立即向联邦 gossip 广播一次群公告，让成员节点尽快学到群（发现 + 成员表），
    // 免等 20s 周期——发现驱动跨节点群消息投递。
    if ok && cmd.method.starts_with("group.") && cmd.method != "group.list" {
        if let Some(op) = cmd.params.as_ref().and_then(|p| GroupOp::decode(p.value.as_slice()).ok()) {
            announce_group(ctx, &op.group_id);
        }
    }
    let cr = CommandResult {
        correlation_id: cmd.correlation_id,
        ok,
        result,
        error,
        seq: 0,
        done: true,
    };
    Some(command_result_gram(gram, &cr))
}

/// 校验一条路由命令是否被授权：命令须带 Grant，且授权 caller 对 target.kind 执行该 method。
fn authorize_routed(gram: &Gram, caller: &[u8], target: &Entity) -> std::result::Result<(), String> {
    let cmd = gram
        .payload
        .as_ref()
        .and_then(|p| Command::decode(p.value.as_slice()).ok())
        .ok_or_else(|| "malformed command".to_string())?;
    let grant = cmd.grant.as_ref().ok_or_else(|| "missing grant".to_string())?;
    let now = (now_ms() / 1000) as i64;
    // resource 约定为目标实体的 kind；action 为命令 method。
    nm_crypto::authorize(grant, caller, &cmd.method, &target.kind, now)
        .map_err(|e| e.to_string())?;
    // grant 须由目标实体(资源所有者)签发（issuer == target.entity_id）。
    if grant.issuer != target.entity_id {
        return Err("grant issuer != resource owner".to_string());
    }
    Ok(())
}

/// 把「命令被拒」作为 CommandResult 推回调用方在线会话（若在线）。
fn push_command_denied(gram: &Gram, caller: &[u8], ctx: &Ctx, reason: &str) {
    let sessions = &ctx.sessions;
    let corr = gram
        .payload
        .as_ref()
        .and_then(|p| Command::decode(p.value.as_slice()).ok())
        .map(|c| c.correlation_id)
        .unwrap_or(0);
    let Some(conn) = sessions.get(caller).map(|r| r.clone()) else {
        return;
    };
    let cr = CommandResult { correlation_id: corr, ok: false, result: None, error: format!("denied: {reason}"), seq: 0, done: true };
    let mut reply = reply_gram(gram, GramKind::CommandResult, Some(Any {
        type_url: "nmspace.v1.CommandResult".to_string(),
        value: cr.encode_to_vec(),
    }));
    reply.ref_gram_id = Some(gram.gram_id);
    reply.receiver = caller.to_vec();
    ctx.tracker.spawn(async move {
        if let Ok(mut sname) = conn.open_uni().await {
            let _ = write_gram(&mut sname, &reply).await;
            let _ = sname.finish();
        }
    });
}

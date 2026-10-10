//! `nm-client` — 客户端 SDK。任意实体类型（人 / Agent / 设备 / 车辆 / 算力）都用它上线。
//! 原生端(Tauri)在进程内直接调用它（内嵌 iroh）；`nm-gateway` 也复用它为浏览器服务。
//!
//! - 短连接 RPC（面向节点）：`register_as` / `directory_query` / `send_message`。
//! - 持久会话 [`Session`]（`online()`）：保持连接、后台接收推送；支持
//!   `send_to`（消息）、`call`（路由到某实体的命令 RPC）、`next_command`/`reply`（作为被调方响应命令）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use nm_entity::{pack_profile, EntityKind};
use nm_proto::{
    now_ms, Any, BlobData, BlobPut, BlobRef, Channel, ChannelBackfillReq, ChannelList, ChannelLog,
    ChannelMsg, ChannelOp, ChannelPub, Command, CommandResult, DeviceCert, DeviceInfo, DeviceList,
    DeviceRevoke, DirectoryQuery, Entity, EntityList, Gram, GramKind, Group, GroupList, GroupOp,
    NameList, NameOp, NameQuery, NameRecord, PROTOCOL_VERSION,
};
use nm_transport::{read_gram, write_gram, Addr, IrohConnection, NodeEndpoint};
use prost::Message;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{0}")]
    Other(String),
    #[error("call timed out")]
    Timeout,
}

fn err(e: impl std::fmt::Display) -> ClientError {
    ClientError::Other(e.to_string())
}

/// 客户端句柄（持有本地 iroh 端点身份）。
pub struct Client {
    ep: NodeEndpoint,
}

impl Client {
    pub async fn bind_local(seed: [u8; 32]) -> Result<Self, ClientError> {
        let ep = NodeEndpoint::bind_local_from_seed(seed).await.map_err(err)?;
        Ok(Self { ep })
    }

    /// 随机身份 + 本地(Minimal)绑定（GUI 首次连接、无持久身份时用）。
    pub async fn bind_local_random() -> Result<Self, ClientError> {
        let ep = NodeEndpoint::bind_local_random().await.map_err(err)?;
        Ok(Self { ep })
    }
    pub async fn bind(seed: [u8; 32]) -> Result<Self, ClientError> {
        let ep = NodeEndpoint::bind_from_seed(seed).await.map_err(err)?;
        Ok(Self { ep })
    }

    /// 自建基础设施绑定（自定义 iroh-relay + 自建 iroh-dns-server(pkarr)）：客户端须与目标节点
    /// 指向**同一套** relay/dns，方能按公钥发现并穿透 NAT。`port=0` 用临时端口。
    pub async fn bind_selfhosted(
        seed: [u8; 32],
        relay_urls: Vec<String>,
        pkarr_url: String,
        dns_origin: Option<String>,
        port: u16,
    ) -> Result<Self, ClientError> {
        let ep =
            NodeEndpoint::bind_selfhosted_from_seed(seed, relay_urls, pkarr_url, dns_origin, port)
                .await
                .map_err(err)?;
        Ok(Self { ep })
    }

    pub fn id(&self) -> nm_transport::Id {
        self.ep.id()
    }
    pub fn id_bytes(&self) -> [u8; 32] {
        self.ep.id_bytes()
    }

    /// 播种一个已知节点地址到本端点地址簿：供 [`Self::online_by_id`] 在 LAN/无发现时按公钥解析。
    /// N0/selfhost 有发现服务时无需调用（发现服务会自动解析）。
    pub fn add_peer_addr(&self, addr: Addr) {
        self.ep.add_addr(addr);
    }

    /// 作为资源所有者，用本地私钥签发一份能力授权(Grant)：
    /// 允许 `audience` 在 `expires`(Unix 秒；0=永不过期) 前对 `resource` 执行 `action`。
    pub fn issue_grant(
        &self,
        audience: [u8; 32],
        action: &str,
        resource: &str,
        expires: i64,
    ) -> nm_proto::Grant {
        nm_crypto::issue_grant(self.ep.secret_key(), audience, action, resource, expires, Vec::new())
    }

    /// 建立持久会话：保持连接、后台接收节点推送并分派（消息 / 命令 / 命令结果）。
    pub async fn online(&self, to: impl Into<Addr>) -> Result<Session, ClientError> {
        let conn = self.ep.connect(to).await.map_err(err)?;
        let pending: Arc<DashMap<u64, oneshot::Sender<Gram>>> = Arc::new(DashMap::new());
        let pending_streams: Arc<DashMap<u64, mpsc::UnboundedSender<Gram>>> = Arc::new(DashMap::new());
        let (in_tx, in_rx) = mpsc::unbounded_channel(); // 消息 & 未匹配
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel(); // 收到的命令（供被调方）

        let rconn = conn.clone();
        let pend = pending.clone();
        let streams = pending_streams.clone();
        let task = tokio::spawn(async move {
            // 节点经 uni 流推送 gram；按类型分派。
            while let Ok(mut recv) = rconn.accept_uni().await {
                let Ok(gram) = read_gram(&mut recv).await else {
                    break;
                };
                match gram.kind() {
                    GramKind::CommandResult => {
                        // 用 ref_gram_id 关联请求：先命中单响应 pending（oneshot），否则命中流式 pending_streams。
                        if let Some(n) = gram.ref_gram_id {
                            if let Some((_, tx)) = pend.remove(&n) {
                                let _ = tx.send(gram);
                                continue;
                            }
                            if let Some(entry) = streams.get(&n) {
                                // 流式帧：转发；done 帧后移除流（接收端随之关闭）。
                                let done = gram
                                    .payload
                                    .as_ref()
                                    .and_then(|p| CommandResult::decode(p.value.as_slice()).ok())
                                    .map(|cr| cr.done)
                                    .unwrap_or(true);
                                let _ = entry.value().send(gram);
                                drop(entry);
                                if done {
                                    streams.remove(&n);
                                }
                                continue;
                            }
                        }
                        let _ = in_tx.send(gram);
                    }
                    GramKind::Command => {
                        if let Some(cmd) = gram
                            .payload
                            .as_ref()
                            .and_then(|p| Command::decode(p.value.as_slice()).ok())
                        {
                            let _ = cmd_tx.send((gram, cmd));
                        }
                    }
                    _ => {
                        let _ = in_tx.send(gram);
                    }
                }
            }
        });

        Ok(Session {
            conn,
            my_id: self.ep.id_bytes(),
            device_id: self.ep.id_bytes(),
            inbox: Some(in_rx),
            commands: cmd_rx,
            pending,
            pending_streams,
            next_corr: Arc::new(AtomicU64::new(1)),
            _task: task,
        })
    }

    /// 按 **node id(公钥)** 建立持久会话：由发现服务(N0/selfhost)解析节点当前地址，从而穿透 NAT——
    /// 跨公网只需知道节点公钥（稳定），无需其会变的地址。LAN/无发现时须先 [`Self::add_peer_addr`] 播种。
    pub async fn online_by_id(&self, node_id: [u8; 32]) -> Result<Session, ClientError> {
        let addr = nm_transport::addr_from_id(node_id).map_err(err)?;
        self.online(addr).await
    }

    /// 以设备密钥连接、以账号身份收发：连上后出示账号签发的设备证书（`device.hello`）。
    /// 节点不支持多设备时返回的错误含 `unknown method`，调用方可回退为账号密钥直连。
    pub async fn online_as(&self, to: impl Into<Addr>, cert: &DeviceCert) -> Result<Session, ClientError> {
        let account: [u8; 32] =
            cert.account.as_slice().try_into().map_err(|_| ClientError::Other("bad cert account".into()))?;
        let mut s = self.online(to).await?;
        let params = Any { type_url: "nmspace.v1.DeviceCert".into(), value: cert.encode_to_vec() };
        let res = match rpc_over(&s.conn, s.device_id, "device.hello", Some(params)).await {
            Ok(r) => r,
            Err(e) => {
                // 节点在连接层拒绝（如设备已吊销）时，关闭原因比流错误更有用。
                let closed = tokio::time::timeout(Duration::from_millis(500), s.conn.closed()).await;
                return Err(match closed {
                    Ok(reason) => ClientError::Other(reason.to_string()),
                    Err(_) => e,
                });
            }
        };
        if !res.ok {
            s.conn.close(0u32.into(), b"hello rejected");
            return Err(ClientError::Other(res.error));
        }
        s.my_id = account;
        Ok(s)
    }

    pub async fn online_as_by_id(&self, node_id: [u8; 32], cert: &DeviceCert) -> Result<Session, ClientError> {
        let addr = nm_transport::addr_from_id(node_id).map_err(err)?;
        self.online_as(addr, cert).await
    }

    // ---- 短连接便捷方法（面向节点；每次自建连接）----
    pub async fn register_as<K: EntityKind>(
        &self,
        to: impl Into<Addr>,
        profile: &K::Profile,
        display_name: &str,
        attributes: HashMap<String, String>,
    ) -> Result<(), ClientError> {
        let conn = self.ep.connect(to).await.map_err(err)?;
        register_over::<K>(&conn, self.id_bytes(), profile, display_name, attributes).await
    }
    pub async fn directory_query(
        &self,
        to: impl Into<Addr>,
        query: DirectoryQuery,
    ) -> Result<Vec<Entity>, ClientError> {
        let conn = self.ep.connect(to).await.map_err(err)?;
        query_over(&conn, self.id_bytes(), query).await
    }
    pub async fn send_message(
        &self,
        to: impl Into<Addr>,
        receiver: [u8; 32],
        text: &str,
    ) -> Result<Gram, ClientError> {
        let conn = self.ep.connect(to).await.map_err(err)?;
        let msg = text_message(self.id_bytes(), receiver, text, 1);
        route_send(&conn, &msg).await
    }
}

/// 持久会话：复用同一连接收发，并后台接收节点推送。
pub struct Session {
    conn: IrohConnection,
    /// 账号身份（消息 sender）；`online_as` 时与连接密钥 `device_id` 不同。
    my_id: [u8; 32],
    device_id: [u8; 32],
    inbox: Option<mpsc::UnboundedReceiver<Gram>>,
    commands: mpsc::UnboundedReceiver<(Gram, Command)>,
    pending: Arc<DashMap<u64, oneshot::Sender<Gram>>>,
    /// 流式命令关联：一个请求的多帧 CommandResult 转发到此（done 帧后移除）。供 `call_stream`/`infer_stream`。
    pending_streams: Arc<DashMap<u64, mpsc::UnboundedSender<Gram>>>,
    next_corr: Arc<AtomicU64>,
    _task: tokio::task::JoinHandle<()>,
}

impl Session {
    pub fn id_bytes(&self) -> [u8; 32] {
        self.my_id
    }

    pub fn device_id(&self) -> [u8; 32] {
        self.device_id
    }

    /// 所连节点的公钥（用于认定节点发来的系统事件）。
    pub fn node_id(&self) -> [u8; 32] {
        *self.conn.remote_id().as_bytes()
    }

    /// 连接被节点关闭时的原因（如 `device_revoked`）；仍在线返回 None。
    pub fn close_reason(&self) -> Option<String> {
        self.conn.close_reason().map(|r| r.to_string())
    }

    // ---- 多设备 ----

    pub async fn device_list(&self) -> Result<Vec<DeviceInfo>, ClientError> {
        let res = rpc_over(&self.conn, self.my_id, "device.list", None).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let p = res.result.ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(DeviceList::decode(p.value.as_slice()).map_err(err)?.devices)
    }

    pub async fn device_rename(&self, device: [u8; 32], label: &str) -> Result<(), ClientError> {
        let hex: String = device.iter().map(|b| format!("{b:02x}")).collect();
        let body = serde_json::json!({ "device": hex, "label": label }).to_string();
        self.name_account("device.rename", &body).await.map(|_| ())
    }

    /// 提交账号私钥签好的吊销记录。
    pub async fn device_revoke(&self, revoke: &DeviceRevoke) -> Result<(), ClientError> {
        let params = Any { type_url: "nmspace.v1.DeviceRevoke".into(), value: revoke.encode_to_vec() };
        let res = rpc_over(&self.conn, self.my_id, "device.revoke", Some(params)).await?;
        if res.ok { Ok(()) } else { Err(ClientError::Other(res.error)) }
    }

    /// 紧急冻结：凭账号口令请家节点代签吊销。`device` 为 None 时冻结该账号全部设备；返回冻结台数。
    pub async fn device_freeze(
        &self,
        local: &str,
        domain: &str,
        password: &str,
        device: Option<[u8; 32]>,
    ) -> Result<usize, ClientError> {
        let hex: String = device.map(|d| d.iter().map(|b| format!("{b:02x}")).collect()).unwrap_or_default();
        let body = serde_json::json!({ "local": local, "domain": domain, "password": password, "device": hex }).to_string();
        let n = self.name_account("device.freeze", &body).await?;
        Ok(n.trim().parse().unwrap_or(0))
    }

    /// 等待下一条节点推送的消息（如别人发来的 Message）。
    pub async fn recv(&mut self) -> Option<Gram> {
        self.inbox.as_mut()?.recv().await
    }

    /// 取走 inbox 接收端（用于把消息流交给上层事件循环，如 Tauri 事件桥）。
    /// 取走后 `recv()` 将返回 None，改由取走方消费。
    pub fn take_inbox(&mut self) -> Option<mpsc::UnboundedReceiver<Gram>> {
        self.inbox.take()
    }

    /// 主动下线：先发 Logout 让节点同步移除会话（后续推送转离线队列），再关连接。
    pub async fn close(self) {
        let bye = build_gram(GramKind::Logout, self.my_id, Vec::new(), None);
        let _ = route_send(&self.conn, &bye).await; // 等节点确认已处理
        self.conn.close(0u32.into(), b"bye");
        self._task.abort();
    }

    /// 主动下线（按引用，供 `Arc<Session>` 调用）：发 Logout 让节点即时移除会话，再关连接。
    /// 带超时——`route_send` 无超时，节点无响应时不致挂死调用方（如 app 退出路径）。
    pub async fn disconnect(&self) {
        let bye = build_gram(GramKind::Logout, self.my_id, Vec::new(), None);
        let _ = tokio::time::timeout(Duration::from_millis(800), route_send(&self.conn, &bye)).await;
        self.conn.close(0u32.into(), b"bye");
    }

    /// 作为被调方，等待下一条发给自己的命令（返回请求 gram 与解析后的 Command）。
    pub async fn next_command(&mut self) -> Option<(Gram, Command)> {
        self.commands.recv().await
    }

    /// 对某条命令请求作出响应（CommandResult 经节点路由回请求方）。
    /// 对某条命令请求回**一帧**响应。`seq` 为帧序（0 基），`done=true` 表示终帧。
    /// 单响应（非流式）用 [`Self::reply`]；流式 provider 逐帧用本方法（`done=false` 多帧 + 终帧 `done=true`）。
    pub async fn reply_frame(
        &self,
        request: &Gram,
        ok: bool,
        result: Option<Any>,
        error: &str,
        seq: u32,
        done: bool,
    ) -> Result<(), ClientError> {
        let corr = request
            .payload
            .as_ref()
            .and_then(|p| Command::decode(p.value.as_slice()).ok())
            .map(|c| c.correlation_id)
            .unwrap_or(0);
        let cr = CommandResult {
            correlation_id: corr,
            ok,
            result,
            error: error.to_string(),
            seq,
            done,
        };
        let mut gram = build_gram(
            GramKind::CommandResult,
            self.my_id,
            request.sender.clone(),
            Some(Any {
                type_url: "nmspace.v1.CommandResult".to_string(),
                value: cr.encode_to_vec(),
            }),
        );
        // 用 ref_gram_id 携带请求的 gram_id 作为关联键。
        gram.ref_gram_id = Some(request.gram_id);
        route_send(&self.conn, &gram).await.map(|_| ())
    }

    /// 对某条命令请求作出单个（终帧）响应（CommandResult 经节点路由回请求方）。
    pub async fn reply(
        &self,
        request: &Gram,
        ok: bool,
        result: Option<Any>,
        error: &str,
    ) -> Result<(), ClientError> {
        self.reply_frame(request, ok, result, error, 0, true).await
    }

    pub async fn register_as<K: EntityKind>(
        &self,
        profile: &K::Profile,
        display_name: &str,
        attributes: HashMap<String, String>,
    ) -> Result<(), ClientError> {
        register_over::<K>(&self.conn, self.my_id, profile, display_name, attributes).await
    }
    pub async fn directory_query(&self, query: DirectoryQuery) -> Result<Vec<Entity>, ClientError> {
        query_over(&self.conn, self.my_id, query).await
    }

    /// 向目标实体发送一条文本消息（节点路由到其在线会话），返回节点 ack。
    pub async fn send_to(&self, target: [u8; 32], text: &str) -> Result<Gram, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let msg = text_message(self.my_id, target, text, n);
        route_send(&self.conn, &msg).await
    }

    // ---- 群组（node RPC；见 docs/PLAN_C §8.3）----

    /// 创建群（caller 为 owner 兼首成员）；返回 group_id。
    pub async fn group_create(&self, group_id: [u8; 32], name: &str) -> Result<[u8; 32], ClientError> {
        let op = GroupOp { group_id: group_id.to_vec(), name: name.to_string(), ..Default::default() };
        self.group_op("group.create", op).await?;
        Ok(group_id)
    }
    /// F2：创建「带密钥群」——group_id = 新生成的 Ed25519 公钥；私钥随 create 命令交给 home 节点
    /// （经加密的 client→home 信道），home 节点据此用 pkarr 发布「群公钥 → seed 节点」发现记录，
    /// 使成员节点无锚（不依赖 home_node 常驻）也能发现并 join 该群主题。返回 group_id（= 群公钥）。
    /// 传统 [`Self::group_create`]（调用方给定随机 id、仅火管发现）保持不变。
    pub async fn group_create_keyed(&self, name: &str) -> Result<[u8; 32], ClientError> {
        let sk = nm_transport::SecretKey::generate();
        let group_id = *sk.public().as_bytes();
        let op = GroupOp {
            group_id: group_id.to_vec(),
            name: name.to_string(),
            secret: sk.to_bytes().to_vec(),
            ..Default::default()
        };
        self.group_op("group.create", op).await?;
        Ok(group_id)
    }
    pub async fn group_join(&self, group_id: [u8; 32]) -> Result<(), ClientError> {
        self.group_op("group.join", GroupOp { group_id: group_id.to_vec(), ..Default::default() }).await
    }
    pub async fn group_leave(&self, group_id: [u8; 32]) -> Result<(), ClientError> {
        self.group_op("group.leave", GroupOp { group_id: group_id.to_vec(), ..Default::default() }).await
    }
    pub async fn group_kick(&self, group_id: [u8; 32], target: [u8; 32]) -> Result<(), ClientError> {
        self.group_op(
            "group.kick",
            GroupOp { group_id: group_id.to_vec(), target: target.to_vec(), ..Default::default() },
        )
        .await
    }
    /// owner/admin 添加成员。
    pub async fn group_add(&self, group_id: [u8; 32], target: [u8; 32]) -> Result<(), ClientError> {
        self.group_op(
            "group.add",
            GroupOp { group_id: group_id.to_vec(), target: target.to_vec(), ..Default::default() },
        )
        .await
    }
    /// owner 提升成员为管理员。
    pub async fn group_promote(&self, group_id: [u8; 32], target: [u8; 32]) -> Result<(), ClientError> {
        self.group_op(
            "group.promote",
            GroupOp { group_id: group_id.to_vec(), target: target.to_vec(), ..Default::default() },
        )
        .await
    }
    /// owner 取消某人的管理员。
    pub async fn group_demote(&self, group_id: [u8; 32], target: [u8; 32]) -> Result<(), ClientError> {
        self.group_op(
            "group.demote",
            GroupOp { group_id: group_id.to_vec(), target: target.to_vec(), ..Default::default() },
        )
        .await
    }
    /// owner/admin 改群名。
    pub async fn group_rename(&self, group_id: [u8; 32], name: &str) -> Result<(), ClientError> {
        self.group_op(
            "group.rename",
            GroupOp { group_id: group_id.to_vec(), name: name.to_string(), ..Default::default() },
        )
        .await
    }
    /// owner/admin 设置群信息（名称/简介/头像）。
    pub async fn group_set_meta(&self, group_id: [u8; 32], name: &str, topic: &str, avatar_url: &str) -> Result<(), ClientError> {
        self.group_op(
            "group.set_meta",
            GroupOp {
                group_id: group_id.to_vec(),
                name: name.to_string(),
                topic: topic.to_string(),
                avatar_url: avatar_url.to_string(),
                ..Default::default()
            },
        )
        .await
    }
    /// owner 解散群。
    pub async fn group_dissolve(&self, group_id: [u8; 32]) -> Result<(), ClientError> {
        self.group_op(
            "group.dissolve",
            GroupOp { group_id: group_id.to_vec(), ..Default::default() },
        )
        .await
    }
    pub async fn group_list(&self) -> Result<Vec<Group>, ClientError> {
        let res = rpc_over(&self.conn, self.my_id, "group.list", None).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let payload = res.result.ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(GroupList::decode(payload.value.as_slice()).map_err(err)?.groups)
    }

    /// 上传一个内容寻址 blob（P1 头像等）；返回 (hash, home_node)。hash 由节点按 blake3 计算。
    pub async fn blob_put(&self, data: Vec<u8>, mime: &str) -> Result<(Vec<u8>, Vec<u8>), ClientError> {
        let params = Any {
            type_url: "nmspace.v1.BlobPut".to_string(),
            value: BlobPut { data, mime: mime.to_string() }.encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "blob.put", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let payload = res.result.ok_or_else(|| ClientError::Other("no payload".into()))?;
        let br = BlobRef::decode(payload.value.as_slice()).map_err(err)?;
        Ok((br.hash, br.home_node))
    }

    /// 按 hash 取一个 blob（本节点未命中时，节点据 home_node 回源）；返回 (data, mime)。
    pub async fn blob_get(&self, hash: Vec<u8>, home_node: Vec<u8>) -> Result<(Vec<u8>, String), ClientError> {
        let params = Any {
            type_url: "nmspace.v1.BlobRef".to_string(),
            value: BlobRef { hash, home_node }.encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "blob.get", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let payload = res.result.ok_or_else(|| ClientError::Other("no payload".into()))?;
        let bd = BlobData::decode(payload.value.as_slice()).map_err(err)?;
        Ok((bd.data, bd.mime))
    }

    /// 设置本人在线状态（P2）：online / away / busy / dnd（空/online = 清除意图）。
    pub async fn presence_set(&self, status: &str) -> Result<(), ClientError> {
        let params = Any {
            type_url: "nmspace.v1/presence.status".to_string(),
            value: status.as_bytes().to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "presence.set", Some(params)).await?;
        if res.ok {
            Ok(())
        } else {
            Err(ClientError::Other(res.error))
        }
    }

    /// A：按 id 批量查询在线状态（含跨节点好友）。返回 {hex: "online|away|busy|dnd|offline"}。
    pub async fn presence_query(&self, ids: &[[u8; 32]]) -> Result<std::collections::HashMap<String, String>, ClientError> {
        let hexed: Vec<String> = ids.iter().map(|b| b.iter().map(|x| format!("{x:02x}")).collect()).collect();
        let body = serde_json::json!({ "ids": hexed }).to_string();
        let params = Any { type_url: "text/plain".to_string(), value: body.into_bytes() };
        let res = rpc_over(&self.conn, self.my_id, "presence.query", Some(params)).await?;
        if !res.ok { return Err(ClientError::Other(res.error)); }
        let raw = res.result.map(|p| p.value).unwrap_or_default();
        Ok(serde_json::from_slice(&raw).unwrap_or_default())
    }

    // ── Roster（联系人册）────────────────────────────────────────────────────

    /// 返回调用方账号的全部联系人。
    pub async fn roster_list(&self) -> Result<Vec<std::collections::HashMap<String, serde_json::Value>>, ClientError> {
        let params = Any { type_url: "text/plain".to_string(), value: b"{}".to_vec() };
        let res = rpc_over(&self.conn, self.my_id, "roster.list", Some(params)).await?;
        if !res.ok { return Err(ClientError::Other(res.error)); }
        let raw = res.result.map(|p| p.value).unwrap_or_default();
        Ok(serde_json::from_slice(&raw).unwrap_or_default())
    }

    /// 添加或更新一条联系人记录；返回服务端写入后的完整条目。
    pub async fn roster_add(&self, id: [u8; 32], kind: &str, name: &str, handle: &str, remark: &str)
        -> Result<serde_json::Value, ClientError> {
        let id_hex: String = id.iter().map(|x| format!("{x:02x}")).collect();
        let body = serde_json::json!({ "id": id_hex, "kind": kind, "name": name, "handle": handle, "remark": remark }).to_string();
        let params = Any { type_url: "text/plain".to_string(), value: body.into_bytes() };
        let res = rpc_over(&self.conn, self.my_id, "roster.add", Some(params)).await?;
        if !res.ok { return Err(ClientError::Other(res.error)); }
        let raw = res.result.map(|p| p.value).unwrap_or_default();
        Ok(serde_json::from_slice(&raw).unwrap_or_else(|_| serde_json::Value::Null))
    }

    /// 删除一条联系人记录（幂等）。
    pub async fn roster_remove(&self, id: [u8; 32]) -> Result<(), ClientError> {
        let id_hex: String = id.iter().map(|x| format!("{x:02x}")).collect();
        let body = serde_json::json!({ "id": id_hex }).to_string();
        let params = Any { type_url: "text/plain".to_string(), value: body.into_bytes() };
        let res = rpc_over(&self.conn, self.my_id, "roster.remove", Some(params)).await?;
        if !res.ok { return Err(ClientError::Other(res.error)); } Ok(())
    }

    /// 更新联系人的 name、handle 或 remark（空字符串 = 不覆盖现有值；remark 可清空）。
    pub async fn roster_update(&self, id: [u8; 32], name: Option<&str>, handle: Option<&str>, remark: Option<&str>)
        -> Result<serde_json::Value, ClientError> {
        let id_hex: String = id.iter().map(|x| format!("{x:02x}")).collect();
        let mut m = serde_json::json!({ "id": id_hex });
        if let Some(n) = name  { m["name"]   = serde_json::Value::String(n.to_string()); }
        if let Some(h) = handle { m["handle"] = serde_json::Value::String(h.to_string()); }
        if let Some(r) = remark { m["remark"] = serde_json::Value::String(r.to_string()); }
        let params = Any { type_url: "text/plain".to_string(), value: m.to_string().into_bytes() };
        let res = rpc_over(&self.conn, self.my_id, "roster.update", Some(params)).await?;
        if !res.ok { return Err(ClientError::Other(res.error)); }
        let raw = res.result.map(|p| p.value).unwrap_or_default();
        Ok(serde_json::from_slice(&raw).unwrap_or_else(|_| serde_json::Value::Null))
    }

    // ── 频道 / 主题（P4）──
    async fn channel_op(&self, method: &str, op: ChannelOp) -> Result<(), ClientError> {
        let params = Any { type_url: "nmspace.v1.ChannelOp".to_string(), value: op.encode_to_vec() };
        let res = rpc_over(&self.conn, self.my_id, method, Some(params)).await?;
        if res.ok { Ok(()) } else { Err(ClientError::Other(res.error)) }
    }
    pub async fn channel_create(&self, channel_id: [u8; 32], name: &str, topic: &str, avatar_url: &str) -> Result<(), ClientError> {
        self.channel_op("channel.create", ChannelOp { channel_id: channel_id.to_vec(), name: name.to_string(), topic: topic.to_string(), avatar_url: avatar_url.to_string() }).await
    }
    /// owner 设置频道信息（名称/简介/头像）→ gossip 广播给订阅者。
    pub async fn channel_set_meta(&self, channel_id: [u8; 32], name: &str, topic: &str, avatar_url: &str) -> Result<(), ClientError> {
        self.channel_op("channel.set_meta", ChannelOp { channel_id: channel_id.to_vec(), name: name.to_string(), topic: topic.to_string(), avatar_url: avatar_url.to_string() }).await
    }
    pub async fn channel_sub(&self, channel_id: [u8; 32], name: &str) -> Result<(), ClientError> {
        self.channel_op("channel.sub", ChannelOp { channel_id: channel_id.to_vec(), name: name.to_string(), ..Default::default() }).await
    }
    pub async fn channel_unsub(&self, channel_id: [u8; 32]) -> Result<(), ClientError> {
        self.channel_op("channel.unsub", ChannelOp { channel_id: channel_id.to_vec(), ..Default::default() }).await
    }
    pub async fn channel_publish(&self, channel_id: [u8; 32], body: &str) -> Result<(), ClientError> {
        let params = Any { type_url: "nmspace.v1.ChannelPub".to_string(), value: ChannelPub { channel_id: channel_id.to_vec(), body: body.to_string() }.encode_to_vec() };
        let res = rpc_over(&self.conn, self.my_id, "channel.publish", Some(params)).await?;
        if res.ok { Ok(()) } else { Err(ClientError::Other(res.error)) }
    }
    pub async fn channel_list(&self) -> Result<Vec<Channel>, ClientError> {
        let res = rpc_over(&self.conn, self.my_id, "channel.list", None).await?;
        if !res.ok { return Err(ClientError::Other(res.error)); }
        let p = res.result.ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(ChannelList::decode(p.value.as_slice()).map_err(err)?.channels)
    }
    pub async fn channel_backfill(&self, channel_id: [u8; 32], since_seq: u64) -> Result<Vec<ChannelMsg>, ClientError> {
        let params = Any { type_url: "nmspace.v1.ChannelBackfillReq".to_string(), value: ChannelBackfillReq { channel_id: channel_id.to_vec(), since_seq }.encode_to_vec() };
        let res = rpc_over(&self.conn, self.my_id, "channel.backfill", Some(params)).await?;
        if !res.ok { return Err(ClientError::Other(res.error)); }
        let p = res.result.ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(ChannelLog::decode(p.value.as_slice()).map_err(err)?.msgs)
    }

    // ---- 去中心命名（N1）----
    /// 在 home node 认领本地名 local-part（→ 本客户端公钥）；返回签发的 NameRecord。
    /// 家节点账号口令命令。`name.register` / `name.login` 的参数是 JSON 文本。
    /// 成功时 `name.login` 的结果正文是该名字登记的客户端公钥（hex）。
    pub async fn name_account(&self, method: &str, body: &str) -> Result<String, ClientError> {
        let params = Any {
            type_url: "text/plain".to_string(),
            value: body.as_bytes().to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, method, Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        Ok(res
            .result
            .map(|p| String::from_utf8_lossy(&p.value).into_owned())
            .unwrap_or_default())
    }
    pub async fn name_claim(&self, local_part: &str) -> Result<NameRecord, ClientError> {
        let params = Any { type_url: "nmspace.v1.NameOp".to_string(), value: NameOp { local_part: local_part.to_string() }.encode_to_vec() };
        let res = rpc_over(&self.conn, self.my_id, "name.claim", Some(params)).await?;
        if !res.ok { return Err(ClientError::Other(res.error)); }
        let p = res.result.ok_or_else(|| ClientError::Other("no payload".into()))?;
        NameRecord::decode(p.value.as_slice()).map_err(err)
    }
    /// 解析 name（local@domain 或域名）→ NameRecord（含目标公钥）；无则 None。
    pub async fn name_resolve(&self, name: &str) -> Result<Option<NameRecord>, ClientError> {
        let params = Any { type_url: "nmspace.v1.NameQuery".to_string(), value: NameQuery { name: name.to_string(), pubkey: Vec::new() }.encode_to_vec() };
        let res = rpc_over(&self.conn, self.my_id, "name.resolve", Some(params)).await?;
        if !res.ok { return Err(ClientError::Other(res.error)); }
        let p = res.result.ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(NameList::decode(p.value.as_slice()).map_err(err)?.records.into_iter().next())
    }
    /// 反向解析 公钥 → NameRecord（规范名）；无则 None。
    pub async fn name_reverse(&self, pubkey: [u8; 32]) -> Result<Option<NameRecord>, ClientError> {
        let params = Any { type_url: "nmspace.v1.NameQuery".to_string(), value: NameQuery { name: String::new(), pubkey: pubkey.to_vec() }.encode_to_vec() };
        let res = rpc_over(&self.conn, self.my_id, "name.reverse", Some(params)).await?;
        if !res.ok { return Err(ClientError::Other(res.error)); }
        let p = res.result.ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(NameList::decode(p.value.as_slice()).map_err(err)?.records.into_iter().next())
    }

    /// 向群发消息（节点扇出到各成员，在线路由/离线入库）。返回节点 ack。
    pub async fn send_group(&self, group_id: [u8; 32], text: &str) -> Result<Gram, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let mut msg = text_message(self.my_id, group_id, text, n);
        msg.kind = GramKind::GroupMessage as i32;
        route_send(&self.conn, &msg).await
    }

    /// 私聊或群的富消息。`json` 为 `nmspace.v1/chat` 载荷，媒体本体在 blob 里。
    pub async fn send_rich(&self, target: [u8; 32], json: &str, group: bool) -> Result<Gram, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let mut msg = build_gram(
            if group { GramKind::GroupMessage } else { GramKind::Message },
            self.my_id,
            target.to_vec(),
            Some(Any {
                type_url: "nmspace.v1/chat".to_string(),
                value: json.as_bytes().to_vec(),
            }),
        );
        msg.gram_id = n;
        msg.crc = nm_crypto::content_hash(json.as_bytes()).to_vec();
        route_send(&self.conn, &msg).await
    }

    /// 私聊通道上的自定义载荷（如设备迁移握手），接收方按 `type_url` 分派。
    pub async fn send_typed(&self, target: [u8; 32], type_url: &str, body: &[u8]) -> Result<Gram, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let mut msg = build_gram(
            GramKind::Message,
            self.my_id,
            target.to_vec(),
            Some(Any { type_url: type_url.to_string(), value: body.to_vec() }),
        );
        msg.gram_id = n;
        msg.crc = nm_crypto::content_hash(body).to_vec();
        route_send(&self.conn, &msg).await
    }

    // ── C2（P2P 模型消费便捷层）：发现 model.* provider + 一次 OpenAI 兼容推理 ──

    /// C2：发现网络里的模型 provider（kind=model.*）。给了 `model` 名则按 attributes["model"]/["models"] 过滤。
    pub async fn find_model_providers(&self, model: Option<&str>) -> Result<Vec<Entity>, ClientError> {
        let mut list = self
            .directory_query(DirectoryQuery { kind_prefix: "model.".into(), ..Default::default() })
            .await?;
        if let Some(m) = model {
            list.retain(|e| {
                e.attributes.get("model").map(|v| v == m).unwrap_or(false)
                    || e.attributes
                        .get("models")
                        .map(|v| v.split(',').any(|x| x.trim() == m))
                        .unwrap_or(false)
            });
        }
        Ok(list)
    }

    /// C2：向某 provider 发一次 OpenAI 兼容推理（method=model.infer）。
    /// 入参 `openai_request_json` 为 OpenAI `/chat/completions` 请求体字节；返回 completion JSON 字节。
    pub async fn infer(&self, provider: [u8; 32], openai_request_json: &[u8]) -> Result<Vec<u8>, ClientError> {
        self.infer_with_grant(provider, openai_request_json, None).await
    }

    /// C2：带 Grant 的推理（provider 要求授权时用）。
    pub async fn infer_with_grant(
        &self,
        provider: [u8; 32],
        openai_request_json: &[u8],
        grant: Option<nm_proto::Grant>,
    ) -> Result<Vec<u8>, ClientError> {
        let params = Any { type_url: "openai.chat.v1".to_string(), value: openai_request_json.to_vec() };
        let res = self.call_with_grant(provider, "model.infer", Some(params), grant).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        Ok(res.result.map(|a| a.value).unwrap_or_default())
    }

    async fn group_op(&self, method: &str, op: GroupOp) -> Result<(), ClientError> {
        let params = Any { type_url: "nmspace.v1.GroupOp".to_string(), value: op.encode_to_vec() };
        let res = rpc_over(&self.conn, self.my_id, method, Some(params)).await?;
        if res.ok {
            Ok(())
        } else {
            Err(ClientError::Other(res.error))
        }
    }

    /// 路由命令 RPC：向目标实体发命令，等待其 CommandResult（超时 10s）。
    pub async fn call(
        &self,
        target: [u8; 32],
        method: &str,
        params: Option<Any>,
    ) -> Result<CommandResult, ClientError> {
        self.call_with_grant(target, method, params, None).await
    }

    /// 带能力授权(Grant)的路由命令 RPC（目标要求鉴权时需提供）。
    pub async fn call_with_grant(
        &self,
        target: [u8; 32],
        method: &str,
        params: Option<Any>,
        grant: Option<nm_proto::Grant>,
    ) -> Result<CommandResult, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let cmd = Command {
            method: method.to_string(),
            params,
            correlation_id: n,
            timeout_ms: 10_000,
            grant,
        };
        let gram = {
            let mut g = build_gram(
                GramKind::Command,
                self.my_id,
                target.to_vec(),
                Some(Any {
                    type_url: "nmspace.v1.Command".to_string(),
                    value: cmd.encode_to_vec(),
                }),
            );
            g.gram_id = n; // 关联键
            g
        };
        let (tx, rx) = oneshot::channel();
        self.pending.insert(n, tx);
        // 发出命令（节点回 ack；结果稍后作为推送经 pending 唤醒）。
        if let Err(e) = route_send(&self.conn, &gram).await {
            self.pending.remove(&n);
            return Err(e);
        }
        let result_gram = match tokio::time::timeout(Duration::from_secs(10), rx).await {
            Ok(Ok(g)) => g,
            Ok(Err(_)) => return Err(ClientError::Other("call channel closed".into())),
            Err(_) => {
                self.pending.remove(&n);
                return Err(ClientError::Timeout);
            }
        };
        let payload = result_gram
            .payload
            .ok_or_else(|| ClientError::Other("command result has no payload".into()))?;
        CommandResult::decode(payload.value.as_slice()).map_err(err)
    }

    /// 流式路由命令 RPC：发出命令后立即返回一个 [`InferStream`]；多帧 `CommandResult` 陆续到达（done 帧止）。
    /// 对端须按流式回（`reply_frame` 多帧）；否则只会收到单个终帧。帧乱序由上层按 `seq` 处理。
    pub async fn call_stream(
        &self,
        target: [u8; 32],
        method: &str,
        params: Option<Any>,
    ) -> Result<InferStream, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let cmd = Command {
            method: method.to_string(),
            params,
            correlation_id: n,
            timeout_ms: 0,
            grant: None,
        };
        let gram = {
            let mut g = build_gram(
                GramKind::Command,
                self.my_id,
                target.to_vec(),
                Some(Any { type_url: "nmspace.v1.Command".to_string(), value: cmd.encode_to_vec() }),
            );
            g.gram_id = n; // 关联键
            g
        };
        let (tx, rx) = mpsc::unbounded_channel();
        self.pending_streams.insert(n, tx);
        if let Err(e) = route_send(&self.conn, &gram).await {
            self.pending_streams.remove(&n);
            return Err(e);
        }
        Ok(InferStream { rx })
    }

    /// 流式推理（C2）：OpenAI 兼容请求（应带 `"stream": true`），返回逐帧 [`InferStream`]。
    pub async fn infer_stream(
        &self,
        provider: [u8; 32],
        openai_request_json: &[u8],
    ) -> Result<InferStream, ClientError> {
        let params = Any { type_url: "openai.chat.v1".to_string(), value: openai_request_json.to_vec() };
        self.call_stream(provider, "model.infer", Some(params)).await
    }
}

/// 流式命令的接收端：逐帧取 [`CommandResult`]（`done` 帧后通道关闭 → `next` 返回 `None`）。
pub struct InferStream {
    rx: mpsc::UnboundedReceiver<Gram>,
}

impl InferStream {
    /// 取下一帧的 `CommandResult`；流结束（done 帧已消费或连接断）返回 `None`。
    pub async fn next(&mut self) -> Option<CommandResult> {
        let gram = self.rx.recv().await?;
        gram.payload.and_then(|p| CommandResult::decode(p.value.as_slice()).ok())
    }
}

// ---- 底层收发 ----

/// 面向节点的同步 RPC（directory.* 等）：请求与响应走同一 bi 流。
async fn rpc_over(
    conn: &IrohConnection,
    sender: [u8; 32],
    method: &str,
    params: Option<Any>,
) -> Result<CommandResult, ClientError> {
    let cmd = Command {
        method: method.to_string(),
        params,
        correlation_id: 1,
        timeout_ms: 10_000,
        grant: None,
    };
    let gram = build_gram(
        GramKind::Command,
        sender,
        Vec::new(), // 空 receiver = 面向节点
        Some(Any {
            type_url: "nmspace.v1.Command".to_string(),
            value: cmd.encode_to_vec(),
        }),
    );
    let resp = route_send(conn, &gram).await?;
    let payload = resp
        .payload
        .ok_or_else(|| ClientError::Other("command result has no payload".into()))?;
    CommandResult::decode(payload.value.as_slice()).map_err(err)
}

/// 在一条新 bi 流上发送一个 gram 并读取对端的单条响应。
async fn route_send(conn: &IrohConnection, gram: &Gram) -> Result<Gram, ClientError> {
    let (mut send, mut recv) = conn.open_bi().await.map_err(err)?;
    write_gram(&mut send, gram).await.map_err(err)?;
    send.finish().map_err(err)?;
    read_gram(&mut recv).await.map_err(err)
}

async fn register_over<K: EntityKind>(
    conn: &IrohConnection,
    my_id: [u8; 32],
    profile: &K::Profile,
    display_name: &str,
    attributes: HashMap<String, String>,
) -> Result<(), ClientError> {
    let entity = Entity {
        entity_id: my_id.to_vec(),
        kind: K::KIND.to_string(),
        display_name: display_name.to_string(),
        attributes,
        capabilities: K::capabilities().iter().map(|c| *c as i32).collect(),
        profile: Some(pack_profile::<K>(profile)),
        updated_at: now_ms() as i64,
        signature: Vec::new(), // TODO(E2+)：用 endpoint 私钥自签名
        home_node: Vec::new(), // 由归属节点在 register 时盖章
    };
    let params = Any {
        type_url: "nmspace.v1.Entity".to_string(),
        value: entity.encode_to_vec(),
    };
    let res = rpc_over(conn, my_id, "directory.register", Some(params)).await?;
    if res.ok {
        Ok(())
    } else {
        Err(ClientError::Other(res.error))
    }
}

async fn query_over(
    conn: &IrohConnection,
    my_id: [u8; 32],
    query: DirectoryQuery,
) -> Result<Vec<Entity>, ClientError> {
    let params = Any {
        type_url: "nmspace.v1.DirectoryQuery".to_string(),
        value: query.encode_to_vec(),
    };
    let res = rpc_over(conn, my_id, "directory.query", Some(params)).await?;
    if !res.ok {
        return Err(ClientError::Other(res.error));
    }
    let payload = res
        .result
        .ok_or_else(|| ClientError::Other("query result has no payload".into()))?;
    let list = EntityList::decode(payload.value.as_slice()).map_err(err)?;
    Ok(list.entities)
}

fn text_message(sender: [u8; 32], receiver: [u8; 32], text: &str, gram_id: u64) -> Gram {
    let mut msg = build_gram(
        GramKind::Message,
        sender,
        receiver.to_vec(),
        Some(Any {
            type_url: "nmspace.v1/text".to_string(),
            value: text.as_bytes().to_vec(),
        }),
    );
    msg.gram_id = gram_id;
    msg.crc = nm_crypto::content_hash(text.as_bytes()).to_vec();
    msg
}

fn build_gram(kind: GramKind, sender: [u8; 32], receiver: Vec<u8>, payload: Option<Any>) -> Gram {
    Gram {
        version: PROTOCOL_VERSION,
        kind: kind as i32,
        gram_id: 1,
        ref_gram_id: None,
        sender: sender.to_vec(),
        receiver,
        timestamp_ms: now_ms(),
        payload,
        crc: Vec::new(),
    }
}

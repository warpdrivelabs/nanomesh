//! `nm-client` — 客户端 SDK。任意实体类型（人 / Agent / 设备 / 车辆 / 算力）都用它上线。
//! 原生端(Tauri)在进程内直接调用它（内嵌 iroh）；`nm-gateway` 也复用它为浏览器服务。
//!
//! - 短连接 RPC（面向节点）：`register_as` / `directory_query` / `send_message`。
//! - 持久会话 [`Session`]（`online()`）：保持连接、后台接收推送；支持
//!   `send_to`（消息）、`call`（路由到某实体的命令 RPC）、`next_command`/`reply`（作为被调方响应命令）。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
mod error;
mod gram;
mod rpc;
mod session_blob;
mod session_calls;
mod session_channels;
mod session_devices;
mod session_groups;
mod session_messages;
mod session_naming;
mod session_presence;
mod session_roster;
mod stream;

pub use error::ClientError;
pub use stream::InferStream;

use error::err;
use gram::{build_gram, text_message};
use nm_entity::EntityKind;
use nm_proto::{
    Any, BlobData, BlobPut, BlobRef, Channel, ChannelBackfillReq, ChannelList, ChannelLog,
    ChannelMsg, ChannelOp, ChannelPub, Command, CommandResult, DeviceCert, DeviceInfo, DeviceList,
    DeviceRevoke, DirectoryQuery, Entity, Gram, GramKind, Group, GroupList, GroupOp, NameList,
    NameOp, NameQuery, NameRecord,
};
use nm_transport::{Addr, IrohConnection, NodeEndpoint, read_gram};
use prost::Message;
use rpc::{query_over, register_over, route_send, rpc_over};
use tokio::sync::{mpsc, oneshot};

/// 客户端句柄（持有本地 iroh 端点身份）。
pub struct Client {
    ep: NodeEndpoint,
}

impl Client {
    pub async fn bind_local(seed: [u8; 32]) -> Result<Self, ClientError> {
        let ep = NodeEndpoint::bind_local_from_seed(seed)
            .await
            .map_err(err)?;
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
        nm_crypto::issue_grant(
            self.ep.secret_key(),
            audience,
            action,
            resource,
            expires,
            Vec::new(),
        )
    }

    /// 建立持久会话：保持连接、后台接收节点推送并分派（消息 / 命令 / 命令结果）。
    pub async fn online(&self, to: impl Into<Addr>) -> Result<Session, ClientError> {
        let conn = self.ep.connect(to).await.map_err(err)?;
        let pending: Arc<DashMap<u64, oneshot::Sender<Gram>>> = Arc::new(DashMap::new());
        let pending_streams: Arc<DashMap<u64, mpsc::UnboundedSender<Gram>>> =
            Arc::new(DashMap::new());
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
    pub async fn online_as(
        &self,
        to: impl Into<Addr>,
        cert: &DeviceCert,
    ) -> Result<Session, ClientError> {
        let account: [u8; 32] = cert
            .account
            .as_slice()
            .try_into()
            .map_err(|_| ClientError::Other("bad cert account".into()))?;
        let mut s = self.online(to).await?;
        let params = Any {
            type_url: "nmspace.v1.DeviceCert".into(),
            value: cert.encode_to_vec(),
        };
        let res = match rpc_over(&s.conn, s.device_id, "device.hello", Some(params)).await {
            Ok(r) => r,
            Err(e) => {
                // 节点在连接层拒绝（如设备已吊销）时，关闭原因比流错误更有用。
                let closed =
                    tokio::time::timeout(Duration::from_millis(500), s.conn.closed()).await;
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

    pub async fn online_as_by_id(
        &self,
        node_id: [u8; 32],
        cert: &DeviceCert,
    ) -> Result<Session, ClientError> {
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

    /// 当前 gram_id 计数（下一条发送将用的值）。供重连时把高水位续到新 session，
    /// 避免新 session 从 1 重置后与节点 `SeenGrams`（按 (sender,gram_id) 去重、无 TTL）里
    /// 的旧值相撞而被静默丢弃（群消息 / 跨节点私聊）。
    pub fn current_corr(&self) -> u64 {
        self.next_corr.load(Ordering::SeqCst)
    }

    /// 把 gram_id 计数续到指定高水位（重连后调用，接着旧 session 的号往下发）。
    pub fn seed_corr(&self, v: u64) {
        self.next_corr.store(v, Ordering::SeqCst);
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
        let _ =
            tokio::time::timeout(Duration::from_millis(800), route_send(&self.conn, &bye)).await;
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
}

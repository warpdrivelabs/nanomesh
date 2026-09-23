//! `im-client` — 客户端 SDK。任意实体类型（人 / Agent / 设备 / 车辆 / 算力）都用它上线。
//! 原生端(Tauri)在进程内直接调用它（内嵌 iroh）；`im-gateway` 也复用它为浏览器服务。
//!
//! - 短连接 RPC（面向节点）：`register_as` / `directory_query` / `send_message`。
//! - 持久会话 [`Session`]（`online()`）：保持连接、后台接收推送；支持
//!   `send_to`（消息）、`call`（路由到某实体的命令 RPC）、`next_command`/`reply`（作为被调方响应命令）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use im_entity::{pack_profile, EntityKind};
use im_proto::{
    now_ms, Any, Command, CommandResult, DirectoryQuery, Entity, EntityList, Gram, GramKind, Group,
    GroupList, GroupOp, PROTOCOL_VERSION,
};
use im_transport::{read_gram, write_gram, Addr, IrohConnection, NodeEndpoint};
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

    pub fn id(&self) -> im_transport::Id {
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
    ) -> im_proto::Grant {
        im_crypto::issue_grant(self.ep.secret_key(), audience, action, resource, expires, Vec::new())
    }

    /// 建立持久会话：保持连接、后台接收节点推送并分派（消息 / 命令 / 命令结果）。
    pub async fn online(&self, to: impl Into<Addr>) -> Result<Session, ClientError> {
        let conn = self.ep.connect(to).await.map_err(err)?;
        let pending: Arc<DashMap<u64, oneshot::Sender<Gram>>> = Arc::new(DashMap::new());
        let (in_tx, in_rx) = mpsc::unbounded_channel(); // 消息 & 未匹配
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel(); // 收到的命令（供被调方）

        let rconn = conn.clone();
        let pend = pending.clone();
        let task = tokio::spawn(async move {
            // 节点经 uni 流推送 gram；按类型分派。
            while let Ok(mut recv) = rconn.accept_uni().await {
                let Ok(gram) = read_gram(&mut recv).await else {
                    break;
                };
                match gram.kind() {
                    GramKind::CommandResult => {
                        // 用 ref_gram_id 关联请求；命中 pending 则唤醒等待者。
                        if let Some(n) = gram.ref_gram_id {
                            if let Some((_, tx)) = pend.remove(&n) {
                                let _ = tx.send(gram);
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
            inbox: Some(in_rx),
            commands: cmd_rx,
            pending,
            next_corr: Arc::new(AtomicU64::new(1)),
            _task: task,
        })
    }

    /// 按 **node id(公钥)** 建立持久会话：由发现服务(N0/selfhost)解析节点当前地址，从而穿透 NAT——
    /// 跨公网只需知道节点公钥（稳定），无需其会变的地址。LAN/无发现时须先 [`Self::add_peer_addr`] 播种。
    pub async fn online_by_id(&self, node_id: [u8; 32]) -> Result<Session, ClientError> {
        let addr = im_transport::addr_from_id(node_id).map_err(err)?;
        self.online(addr).await
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
    my_id: [u8; 32],
    inbox: Option<mpsc::UnboundedReceiver<Gram>>,
    commands: mpsc::UnboundedReceiver<(Gram, Command)>,
    pending: Arc<DashMap<u64, oneshot::Sender<Gram>>>,
    next_corr: Arc<AtomicU64>,
    _task: tokio::task::JoinHandle<()>,
}

impl Session {
    pub fn id_bytes(&self) -> [u8; 32] {
        self.my_id
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

    /// 作为被调方，等待下一条发给自己的命令（返回请求 gram 与解析后的 Command）。
    pub async fn next_command(&mut self) -> Option<(Gram, Command)> {
        self.commands.recv().await
    }

    /// 对某条命令请求作出响应（CommandResult 经节点路由回请求方）。
    pub async fn reply(
        &self,
        request: &Gram,
        ok: bool,
        result: Option<Any>,
        error: &str,
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
        };
        let mut gram = build_gram(
            GramKind::CommandResult,
            self.my_id,
            request.sender.clone(),
            Some(Any {
                type_url: "imspace.v1.CommandResult".to_string(),
                value: cr.encode_to_vec(),
            }),
        );
        // 用 ref_gram_id 携带请求的 gram_id 作为关联键。
        gram.ref_gram_id = Some(request.gram_id);
        route_send(&self.conn, &gram).await.map(|_| ())
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
        let op = GroupOp { group_id: group_id.to_vec(), name: name.to_string(), target: Vec::new() };
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
    pub async fn group_list(&self) -> Result<Vec<Group>, ClientError> {
        let res = rpc_over(&self.conn, self.my_id, "group.list", None).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let payload = res.result.ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(GroupList::decode(payload.value.as_slice()).map_err(err)?.groups)
    }

    /// 向群发消息（节点扇出到各成员，在线路由/离线入库）。返回节点 ack。
    pub async fn send_group(&self, group_id: [u8; 32], text: &str) -> Result<Gram, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let mut msg = text_message(self.my_id, group_id, text, n);
        msg.kind = GramKind::GroupMessage as i32;
        route_send(&self.conn, &msg).await
    }

    async fn group_op(&self, method: &str, op: GroupOp) -> Result<(), ClientError> {
        let params = Any { type_url: "imspace.v1.GroupOp".to_string(), value: op.encode_to_vec() };
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
        grant: Option<im_proto::Grant>,
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
                    type_url: "imspace.v1.Command".to_string(),
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
            type_url: "imspace.v1.Command".to_string(),
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
        type_url: "imspace.v1.Entity".to_string(),
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
        type_url: "imspace.v1.DirectoryQuery".to_string(),
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
            type_url: "imspace.v1/text".to_string(),
            value: text.as_bytes().to_vec(),
        }),
    );
    msg.gram_id = gram_id;
    msg.crc = im_crypto::content_hash(text.as_bytes()).to_vec();
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

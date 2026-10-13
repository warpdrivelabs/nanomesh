use std::sync::Arc;

use nm_transport::IrohConnection;

use crate::delivery::online_rids;
use crate::{MemDirectory, Node, NodeError, PeerInfo};

/// 一条活动会话的管理快照（后端管理台「连接监控 / 对等节点」用）。
pub struct SessionRow {
    pub id: [u8; 32],
    pub since_unix_ms: u64,
    pub bytes_tx: u64,
    pub bytes_rx: u64,
    pub alpn: String,
    /// 当前选中路径 RTT（毫秒）；无路径时为 `None`。
    pub rtt_ms: Option<f64>,
    /// 拥塞窗口（字节）。
    pub cwnd: Option<u64>,
    /// 当前路径 MTU（UDP 载荷）。
    pub mtu: Option<u16>,
    pub lost_packets: u64,
    pub lost_bytes: u64,
    pub datagrams_tx: u64,
    pub datagrams_rx: u64,
    pub congestion_events: u64,
    pub black_holes: u64,
    /// `"direct"` / `"relay"` / `""`（未知）。
    pub path_kind: String,
    /// 选中路径远端传输地址（IP/中继 URL）。
    pub remote_addr: String,
}

/// 存储统计（后端管理台「存储管理」用）。
pub struct StoreStat {
    pub entities: usize,
    pub groups: usize,
}

impl Node {
    /// 添加联邦对等节点（bootstrap）。之后本节点会周期性拉取其目录、并向其转发跨节点消息。
    pub fn add_peer(&self, node_id: [u8; 32], addr: nm_transport::Addr) {
        self.peers.insert(node_id.to_vec(), addr);
    }

    /// 由对等节点地址添加（node id 从地址里取，配置更省事）。
    pub fn add_peer_addr(&self, addr: nm_transport::Addr) {
        let id = *addr.id.as_bytes();
        // 播种到 endpoint 地址簿：让「按公钥拨号」（gossip 引导）在 LAN/无发现时也能解析地址。
        self.ep.add_addr(addr.clone());
        self.peers.insert(id.to_vec(), addr);
    }

    /// 按 node id(公钥) 添加对等（N0 模式：发现服务解析地址；跨 NAT 只需交换公钥）。
    pub fn add_peer_by_id(&self, node_id: [u8; 32]) -> Result<(), NodeError> {
        let addr = nm_transport::addr_from_id(node_id).map_err(|e| NodeError::Other(e.to_string()))?;
        self.peers.insert(node_id.to_vec(), addr);
        Ok(())
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// 封禁一个公钥：加入黑名单、持久化，并**立即切断**其在线会话。此后其连接一律被拒。
    pub fn ban(&self, pubkey: [u8; 32]) {
        self.blacklist.insert(pubkey);
        if let Some(s) = &self.store {
            if let Err(e) = s.ban(&pubkey) {
                tracing::warn!("persist ban failed: {e}");
            }
        }
        // 立即切断在线会话（若有，含该账号下所有设备连接），并主动关闭连接。
        for rid in online_rids(&self.sessions, &self.principals, &self.devices, &pubkey) {
            if let Some((_, conn)) = self.sessions.remove(&rid) {
                self.sessions_meta.remove(&rid);
                self.principals.remove(&rid);
                conn.close(0u32.into(), b"banned");
                tracing::info!(online = self.sessions.len(), "banned peer session cut");
            }
        }
    }

    /// 解封一个公钥（幂等）。
    pub fn unban(&self, pubkey: [u8; 32]) {
        self.blacklist.remove(&pubkey);
        if let Some(s) = &self.store {
            if let Err(e) = s.unban(&pubkey) {
                tracing::warn!("persist unban failed: {e}");
            }
        }
    }

    /// 该公钥是否被封禁。
    pub fn is_banned(&self, pubkey: &[u8; 32]) -> bool {
        self.blacklist.contains(pubkey)
    }

    /// 当前黑名单（全部被禁公钥）。
    pub fn banned(&self) -> Vec<[u8; 32]> {
        self.blacklist.iter().map(|k| *k).collect()
    }

    pub fn banned_count(&self) -> usize {
        self.blacklist.len()
    }

    pub fn directory(&self) -> Arc<MemDirectory> {
        self.dir.clone()
    }

    pub fn online_count(&self) -> usize {
        self.sessions.len()
    }

    /// 活动会话快照（连接监控 / 对等连通性）。顺带清理陈旧的会话元数据。
    pub fn sessions_snapshot(&self) -> Vec<SessionRow> {
        self.sessions_meta.retain(|k, _| self.sessions.contains_key(k));
        self.sessions
            .iter()
            .map(|e| {
                let key = e.key();
                let conn = e.value();
                let mut id = [0u8; 32];
                if key.len() == 32 {
                    id.copy_from_slice(key);
                }
                let since_unix_ms = self
                    .sessions_meta
                    .get(key)
                    .and_then(|m| m.since.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                session_row_from_conn(id, since_unix_ms, conn)
            })
            .collect()
    }

    /// 群组数量。
    pub fn groups_count(&self) -> usize {
        self.groups.len()
    }

    /// 全部已登记设备快照（后端管理台「域内命名登记」下展开某账号的设备列表用）。
    /// 每条含所属账号、设备公钥、标签、角色、实时在线态与吊销状态；在线态按当前会话/映射重算
    /// （与 device_list 一致：principals 映射 + 本连接存活）。设备仅存在于账号 home 所在节点。
    pub fn devices_snapshot(&self) -> Vec<nm_proto::DeviceInfo> {
        self.devices
            .iter()
            .map(|d| {
                let mut i = d.clone();
                let rid = d.key();
                let acct = i.cert.as_ref().map(|c| c.account.clone()).unwrap_or_default();
                i.online = self.sessions.contains_key(rid)
                    && self.principals.get(rid).is_some_and(|p| p.as_slice() == acct.as_slice());
                i.revoked = self
                    .revoked
                    .get(rid)
                    .filter(|r| r.account == acct)
                    .map(|r| r.clone());
                i
            })
            .collect()
    }

    /// 当前联邦对等节点公钥列表（管理台「对等节点」用）。
    pub fn peers_list(&self) -> Vec<[u8; 32]> {
        self.peers
            .iter()
            .filter_map(|e| <[u8; 32]>::try_from(e.key().as_slice()).ok())
            .collect()
    }

    /// 添加/更新一个带元数据的对等（名称/物理地址/email/mobile/gps）——运行时生效 + 持久化。
    /// 连接一律按 node id（公钥）发现；`info.address` 仅为物理位置元数据。重复 id 即为编辑(覆盖)。
    pub fn add_peer_full(&self, node_id: [u8; 32], info: PeerInfo) -> Result<(), NodeError> {
        let a = nm_transport::addr_from_id(node_id).map_err(|e| NodeError::Other(e.to_string()))?;
        self.peers.insert(node_id.to_vec(), a);
        self.peer_info.insert(node_id.to_vec(), info.clone());
        if let Some(s) = &self.store {
            if let Ok(js) = serde_json::to_string(&info) {
                if let Err(e) = s.put_peer(&node_id, &js) {
                    tracing::warn!("persist peer failed: {e}");
                }
            }
        }
        Ok(())
    }

    /// 标注一个对等为手工/种子来源（不改拨号信息，仅登记 peer_info）：
    /// 用于配置文件 `[[peers]]` 种子——使其在管理台显示为「手工」+ 所属联邦，且不被自动发现降级/清扫。
    pub fn note_manual_peer(&self, node_id: [u8; 32], federation: &str) {
        let mut info = self.peer_info.get(&node_id[..]).map(|i| i.clone()).unwrap_or_default();
        info.source = "manual".to_string();
        info.federation = federation.to_string();
        self.peer_info.insert(node_id.to_vec(), info);
    }

    /// 移除一个对等（运行时 + 持久化）。返回是否存在过。
    pub fn remove_peer(&self, node_id: [u8; 32]) -> bool {
        let existed = self.peers.remove(&node_id[..]).is_some();
        self.peer_info.remove(&node_id[..]);
        if let Some(s) = &self.store {
            let _ = s.remove_peer(&node_id);
        }
        existed
    }

    /// 对等详情（node id hex + 元数据）——管理台列表用。
    pub fn peers_detail(&self) -> Vec<(String, PeerInfo)> {
        self.peers
            .iter()
            .map(|e| {
                let id = e.key();
                let hexid: String = id.iter().map(|x| format!("{x:02x}")).collect();
                let info = self.peer_info.get(id).map(|i| i.clone()).unwrap_or_default();
                (hexid, info)
            })
            .collect()
    }

    /// 存储统计（实体/群组数，内存计数，与持久层一致）。
    pub fn store_stats(&self) -> StoreStat {
        StoreStat {
            entities: self.dir.len(),
            groups: self.groups.len(),
        }
    }

    /// 踢下线：断开某公钥的会话，但**不**拉黑（区别于 `ban`）。返回是否有会话被断。
    pub fn kick(&self, pubkey: [u8; 32]) -> bool {
        self.sessions_meta.remove(&pubkey[..]);
        if let Some((_, conn)) = self.sessions.remove(&pubkey[..]) {
            conn.close(0u32.into(), b"kicked");
            tracing::info!(online = self.sessions.len(), "peer kicked");
            true
        } else {
            false
        }
    }
}

/// 从活动连接提取管理台用的链路质量快照（字节/RTT/丢包/路径类型等）。
fn session_row_from_conn(id: [u8; 32], since_unix_ms: u64, conn: &IrohConnection) -> SessionRow {
    let stats = conn.stats();
    let paths = conn.paths();
    let selected = paths.iter().find(|p| p.is_selected()).or_else(|| paths.iter().next());
    let (rtt_ms, cwnd, mtu, congestion_events, black_holes, path_kind, remote_addr) =
        if let Some(p) = selected {
            let ps = p.stats();
            let kind = if p.is_relay() {
                "relay"
            } else if p.is_ip() {
                "direct"
            } else {
                ""
            };
            (
                Some(ps.rtt.as_secs_f64() * 1000.0),
                Some(ps.cwnd),
                Some(ps.current_mtu),
                ps.congestion_events,
                ps.black_holes_detected,
                kind.to_string(),
                p.remote_addr().to_string(),
            )
        } else {
            (None, None, None, 0, 0, String::new(), String::new())
        };
    SessionRow {
        id,
        since_unix_ms,
        bytes_tx: stats.udp_tx.bytes,
        bytes_rx: stats.udp_rx.bytes,
        alpn: String::from_utf8_lossy(conn.alpn()).into_owned(),
        rtt_ms,
        cwnd,
        mtu,
        lost_packets: stats.lost_packets,
        lost_bytes: stats.lost_bytes,
        datagrams_tx: stats.udp_tx.datagrams,
        datagrams_rx: stats.udp_rx.datagrams,
        congestion_events,
        black_holes,
        path_kind,
        remote_addr,
    }
}

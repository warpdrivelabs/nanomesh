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

    /// 各内存热点表当前大小（P1 内存可观测）：随用户/设备/群/名增长的表，供运维监控增长趋势、
    /// 据此判断是否需要淘汰策略。返回 (表名, 条目数) 列表。
    pub fn table_sizes(&self) -> Vec<(&'static str, usize)> {
        vec![
            ("sessions", self.sessions.len()),
            ("principals", self.principals.len()),
            ("devices", self.devices.len()),
            ("revoked", self.revoked.len()),
            ("entities", self.dir.len()),
            ("groups", self.groups.len()),
            ("names", self.names.len()),
            ("peers", self.peers.len()),
            ("channels", self.channels.len()),
            ("presence", self.presence.len()),
            ("outbox_pending", self.outbox_pending()),
            ("group_outbox_pending", self.group_outbox_pending()),
            ("channel_outbox_pending", self.channel_outbox_pending()),
        ]
    }

    /// 跨节点可靠投递：当前待对端 home 确认（DirectAck）的发件箱条目数。
    /// 稳态近 0（ack 几秒内清）；持续偏高 = 某对端 home 长期不可达，运维可据此告警。
    /// 测试也用它断言"发送后非空 → 收到 ack 后清零"的闭环。
    pub fn outbox_pending(&self) -> usize {
        self.store
            .as_ref()
            .and_then(|s| s.outbox_all().ok())
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// 跨节点群消息可靠扇出：当前待成员节点确认（GroupAck）的发件箱条目数。
    /// 稳态近 0；持续偏高 = 某成员 home 节点长期不可达。测试据此断言「发群消息后非空 → 成员节点 ack 后清零」。
    pub fn group_outbox_pending(&self) -> usize {
        self.store
            .as_ref()
            .and_then(|s| s.group_outbox_all().ok())
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// 频道发布待确认发件箱条目数（publisher 侧，远端 home）。稳态近 0；
    /// 持续偏高 = 某频道 home 长期不可达。测试据此断言「远端发布后非空 → 收到 Msg/Reject 后清零」。
    pub fn channel_outbox_pending(&self) -> usize {
        self.store
            .as_ref()
            .and_then(|s| s.channel_outbox_all().ok())
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// 群消息可靠扇出观测快照：(扇出, acks, 重传, 放弃, 批量块, 死信, 被限流)。供 admin /system 展示到达率。
    pub fn group_metrics_snapshot(&self) -> (u64, u64, u64, u64, u64, u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        let m = &self.group_metrics;
        (
            m.fanouts.load(Relaxed),
            m.acks.load(Relaxed),
            m.retransmits.load(Relaxed),
            m.giveups.load(Relaxed),
            m.ack_batches_sent.load(Relaxed),
            m.dead_letters.load(Relaxed),
            m.msgs_rate_limited.load(Relaxed),
        )
    }

    /// 死信当前留存条数（P0-3，供 admin 展示；无 store 则 0）。
    pub fn dead_letter_count(&self) -> usize {
        self.store.as_ref().and_then(|s| s.dead_letter_count().ok()).unwrap_or(0)
    }

    /// P1-9 送达时延直方图快照：(累积桶[(le_ms, 累计计数)], 样本数, 毫秒之和)。供 /metrics 导出。
    pub fn delivery_hist_snapshot(&self) -> (Vec<(u64, u64)>, u64, u64) {
        self.delivery_hist.snapshot()
    }

    /// P1-9 连接拒绝计数快照：(banned, revoked, server_busy, rate_limited)。
    pub fn conn_rejects_snapshot(&self) -> (u64, u64, u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        let r = &self.conn_rejects;
        (r.banned.load(Relaxed), r.revoked.load(Relaxed), r.server_busy.load(Relaxed), r.rate_limited.load(Relaxed))
    }

    /// 在线一致备份数据库到 dest（P0-2）。返回拷贝字节数；无 store（内存模式）则 Err。
    pub fn backup_db(&self, dest: &std::path::Path) -> std::result::Result<u64, String> {
        match &self.store {
            Some(s) => s.backup(dest).map_err(|e| e.to_string()),
            None => Err("no store (in-memory mode)".into()),
        }
    }

    /// 全部已登记设备快照（后端管理台「域内命名登记」下展开某账号的设备列表用）。
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

    /// 就绪探针（/readyz）：节点是否真能服务。检查 (1) redb 可访问（有 store 时），
    /// (2) endpoint 已绑定地址。返回 Ok(()) 为就绪；Err(原因) 供探针返回 503。
    pub fn readiness(&self) -> std::result::Result<(), String> {
        if let Some(s) = &self.store {
            s.health_check().map_err(|e| format!("store: {e}"))?;
        }
        if self.ep.addr().addrs.is_empty() && self.ep.addr().relay_urls().next().is_none() {
            return Err("endpoint has no address yet".into());
        }
        Ok(())
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

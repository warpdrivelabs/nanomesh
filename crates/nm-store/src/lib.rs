//! `nm-store` — 存储抽象的 `redb` 实现：离线队列(inbox) + 实体目录持久化。
//! 见 `docs/PLAN_B` §8.6 与 `docs/PLAN_C` §6。
//!
//! 表设计：
//! - `INBOX`：key = `receiver(32) ++ gram_id(8, BE)`，value = prost 编码的 `Gram`。
//!   前缀 range 扫描即可取某实体的全部离线消息，`gram_id` 单调保证 FIFO。
//! - `ENTITIES`：key = `entity_id(32)`，value = prost 编码的 `Entity`（目录持久化）。

use nm_proto::{BlobData, DeviceInfo, DeviceRevoke, Entity, Gram, Group, NameRecord};
use prost::Message;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

const INBOX: TableDefinition<&[u8], &[u8]> = TableDefinition::new("inbox");
const ENTITIES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("entities");
const GROUPS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("groups");
/// 黑名单：key = 被禁公钥(32)，value 置空（存在即被禁）。无白名单——默认放行，仅拒绝名单内公钥。
const BLACKLIST: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blacklist");
/// 联邦对等：key = 对端公钥(32)，value = 元数据 JSON（name/addr/email/mobile/gps，可扩展）。
const PEERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("peers");
/// 内容寻址 blob（P1 头像等）：key = blake3 hash(32)，value = prost 编码的 `BlobData`。内容寻址=天然去重/无冲突。
const BLOBS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blobs");
// 命名记录（N1）：key = 完整名 "local@domain" 的字节，value = NameRecord 编码。
const NAMES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("names");
/// 账号口令校验器：key = "local@domain"，value = Argon2 PHC 字符串。只存哈希，不明文。
const NAME_SECRETS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("name_secrets");
/// F2：本节点为「群 home」时持有的群签名私钥：key = group_id(群公钥,32)，value = 群私钥(32)。
/// 仅本地、绝不广播；用于 pkarr 发布 `群公钥 → seed 节点` 发现记录。
const GROUP_SECRETS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("group_secrets");
// 本节点拥有的域名（N1，TOFU）：key = 域名字节，value = 占位（申请时间戳字符串）。
const DOMAINS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("domains");
const ROSTER: TableDefinition<&[u8], &[u8]> = TableDefinition::new("roster");
/// 已登记设备：key = 设备公钥(32)，value = DeviceInfo 编码（证书 + 改名 + 最近在线）。
const DEVICES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("devices");
/// 已吊销设备：key = 设备公钥(32)，value = DeviceRevoke 编码。只增不删。
const REVOKED: TableDefinition<&[u8], &[u8]> = TableDefinition::new("revoked");
/// 跨节点可靠投递·待确认发件箱：key = sender(32)++receiver(32)++gram_id(8BE)，
/// val = first_ms(8)++retries(4)++next_due_ms(8)++gram_bytes。收到 DirectAck 前持久保留，重启续传。
const OUTBOX: TableDefinition<&[u8], &[u8]> = TableDefinition::new("outbox");
/// 跨节点 Direct 持久去重：key = sender(32)++gram_id(8BE)++crc(8)，val = first_seen_ms(8BE)。
/// 并入 crc 防 client 重启 gram_id 回绕导致的误去重。SeenGrams 内存版的持久权威兜底。
const DEDUP_DIRECT: TableDefinition<&[u8], &[u8]> = TableDefinition::new("dedup_direct");
/// 跨节点群消息可靠扇出·待确认发件箱：key = gid(32)++sender(32)++gram_id(8BE)，
/// val = first_ms(8)++retries(4)++next_due_ms(8)++n_pending(2)++[node(32)*n]++gram_bytes。
/// 按「待 ack 成员节点集」跟踪：收到某节点 GroupAck 即移除该 node，集合空则删、停重传。
const GROUP_OUTBOX: TableDefinition<&[u8], &[u8]> = TableDefinition::new("group_outbox");

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("db error: {0}")]
    Db(String),
    #[error("decode error: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// 待确认发件箱一行（跨节点可靠投递重传任务用）。
pub struct OutboxRow {
    pub sender: Vec<u8>,
    pub receiver: Vec<u8>,
    pub gram_id: u64,
    pub first_ms: u64,
    pub retries: u32,
    pub next_due_ms: u64,
    pub gram_bytes: Vec<u8>,
}

/// 群消息待确认发件箱一行（跨节点可靠扇出重传任务用）。`pending_nodes`=尚未回 GroupAck 的成员 home 节点。
pub struct GroupOutboxRow {
    pub gid: Vec<u8>,
    pub sender: Vec<u8>,
    pub gram_id: u64,
    pub first_ms: u64,
    pub retries: u32,
    pub next_due_ms: u64,
    pub pending_nodes: Vec<Vec<u8>>,
    pub gram_bytes: Vec<u8>,
}

fn db_err(e: impl std::fmt::Display) -> StoreError {
    StoreError::Db(e.to_string())
}

/// redb 后端。
pub struct RedbStore {
    db: Database,
}

impl RedbStore {
    /// 打开/创建数据库文件，并确保表存在。
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let db = Database::create(path).map_err(db_err)?;
        // 初始化表（首次打开时创建）。
        let wtx = db.begin_write().map_err(db_err)?;
        {
            wtx.open_table(INBOX).map_err(db_err)?;
            wtx.open_table(ENTITIES).map_err(db_err)?;
            wtx.open_table(ROSTER).map_err(db_err)?;
            wtx.open_table(GROUPS).map_err(db_err)?;
            wtx.open_table(BLACKLIST).map_err(db_err)?;
            wtx.open_table(PEERS).map_err(db_err)?;
            wtx.open_table(BLOBS).map_err(db_err)?;
            wtx.open_table(NAMES).map_err(db_err)?;
            wtx.open_table(NAME_SECRETS).map_err(db_err)?;
            wtx.open_table(DOMAINS).map_err(db_err)?;
            wtx.open_table(DEVICES).map_err(db_err)?;
            wtx.open_table(REVOKED).map_err(db_err)?;
            wtx.open_table(OUTBOX).map_err(db_err)?;
            wtx.open_table(DEDUP_DIRECT).map_err(db_err)?;
            wtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(Self { db })
    }

    // ---- 离线队列 ----

    /// 健康探测：开一个读事务并打开一张表，确认底层 db 可访问（供 /readyz 就绪探针）。
    /// 轻量只读，不写、无副作用；db 损坏/锁死时返回 Err。
    pub fn health_check(&self) -> Result<()> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        rtx.open_table(ENTITIES).map_err(db_err)?;
        Ok(())
    }

    /// 把一条消息存入收件人的离线队列。单条路径保持默认 Immediate 持久（崩溃不丢）。
    pub fn push_inbox(&self, receiver: &[u8], gram: &Gram) -> Result<()> {
        let key = inbox_key(receiver, gram.gram_id);
        let val = gram.encode_to_vec();
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(INBOX).map_err(db_err)?;
            t.insert(key.as_slice(), val.as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 批量把多条消息存入离线队列——单事务单 commit。
    /// 群/多设备扇出热路径用：N 个离线设备从 N 次事务+N 次 fsync 降为 1 次事务。
    /// 批量场景多为可重发的群消息，且单事务已大幅降 fsync，这里用 `None` 免除本事务的 fsync
    /// （后续任一 Immediate 写——drain/身份变更——会把它一并落盘，作天然检查点）。
    /// `items`：(收件人, gram) 列表。
    pub fn push_inbox_batch(&self, items: &[(&[u8], &Gram)]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let mut wtx = self.db.begin_write().map_err(db_err)?;
        wtx.set_durability(redb::Durability::None).map_err(db_err)?;
        {
            let mut t = wtx.open_table(INBOX).map_err(db_err)?;
            for (receiver, gram) in items {
                let key = inbox_key(receiver, gram.gram_id);
                let val = gram.encode_to_vec();
                t.insert(key.as_slice(), val.as_slice()).map_err(db_err)?;
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 读取某实体的全部离线消息（按 gram_id 升序），不删除。
    pub fn peek_inbox(&self, receiver: &[u8]) -> Result<Vec<Gram>> {
        let (lo, hi) = inbox_prefix_range(receiver);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(INBOX).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.range(lo.as_slice()..hi.as_slice()).map_err(db_err)? {
            let (_k, v) = item.map_err(db_err)?;
            let g = Gram::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?;
            out.push(g);
        }
        Ok(out)
    }

    /// 取出并清空某实体的离线消息（补投用）。
    pub fn drain_inbox(&self, receiver: &[u8]) -> Result<Vec<Gram>> {
        let (lo, hi) = inbox_prefix_range(receiver);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let mut out = Vec::new();
        {
            let mut t = wtx.open_table(INBOX).map_err(db_err)?;
            for item in t
                .extract_from_if(lo.as_slice()..hi.as_slice(), |_, _| true)
                .map_err(db_err)?
            {
                let (_k, v) = item.map_err(db_err)?;
                let g = Gram::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?;
                out.push(g);
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok(out)
    }

    /// 删除某收件人名下指定 gram_id 的一条离线消息（送达回执到达后清理）。返回是否确有删除。
    /// 配合 Tier2：drain_inbox_to 改 peek 推送不删，收到对应 client 回执后由本方法删除。
    pub fn del_inbox_one(&self, receiver: &[u8], gram_id: u64) -> Result<bool> {
        let key = inbox_key(receiver, gram_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let existed;
        {
            let mut t = wtx.open_table(INBOX).map_err(db_err)?;
            existed = t.remove(key.as_slice()).map_err(db_err)?.is_some();
        }
        wtx.commit().map_err(db_err)?;
        Ok(existed)
    }

    // ── 跨节点可靠投递：待确认发件箱 OUTBOX ──

    fn outbox_key(sender: &[u8], receiver: &[u8], gram_id: u64) -> Vec<u8> {
        let mut k = Vec::with_capacity(sender.len() + receiver.len() + 8);
        k.extend_from_slice(sender);
        k.extend_from_slice(receiver);
        k.extend_from_slice(&gram_id.to_be_bytes());
        k
    }

    /// 存/覆盖一条待确认条目（Immediate 落盘，崩溃不丢）。
    pub fn outbox_put(&self, sender: &[u8], receiver: &[u8], gram_id: u64,
                      first_ms: u64, retries: u32, next_due_ms: u64, gram_bytes: &[u8]) -> Result<()> {
        let key = Self::outbox_key(sender, receiver, gram_id);
        let mut val = Vec::with_capacity(20 + gram_bytes.len());
        val.extend_from_slice(&first_ms.to_be_bytes());
        val.extend_from_slice(&retries.to_be_bytes());
        val.extend_from_slice(&next_due_ms.to_be_bytes());
        val.extend_from_slice(gram_bytes);
        let wtx = self.db.begin_write().map_err(db_err)?;
        { let mut tb = wtx.open_table(OUTBOX).map_err(db_err)?;
          tb.insert(key.as_slice(), val.as_slice()).map_err(db_err)?; }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 删一条待确认条目（收到 DirectAck / 达期限放弃）。幂等。
    pub fn outbox_del(&self, sender: &[u8], receiver: &[u8], gram_id: u64) -> Result<bool> {
        let key = Self::outbox_key(sender, receiver, gram_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let existed;
        { let mut tb = wtx.open_table(OUTBOX).map_err(db_err)?;
          existed = tb.remove(key.as_slice()).map_err(db_err)?.is_some(); }
        wtx.commit().map_err(db_err)?;
        Ok(existed)
    }

    /// 全量加载待确认条目（重传任务每 tick 扫；稳态近空）。key 后 72 字节=sender32+recv32+gram8。
    pub fn outbox_all(&self) -> Result<Vec<OutboxRow>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let tb = rtx.open_table(OUTBOX).map_err(db_err)?;
        let mut out = Vec::new();
        for item in tb.iter().map_err(db_err)? {
            let (k, v) = item.map_err(db_err)?;
            let k = k.value();
            let v = v.value();
            if k.len() != 72 || v.len() < 20 { continue; }
            let sender = k[0..32].to_vec();
            let receiver = k[32..64].to_vec();
            let gram_id = u64::from_be_bytes(k[64..72].try_into().unwrap());
            let first_ms = u64::from_be_bytes(v[0..8].try_into().unwrap());
            let retries = u32::from_be_bytes(v[8..12].try_into().unwrap());
            let next_due_ms = u64::from_be_bytes(v[12..20].try_into().unwrap());
            let gram_bytes = v[20..].to_vec();
            out.push(OutboxRow { sender, receiver, gram_id, first_ms, retries, next_due_ms, gram_bytes });
        }
        Ok(out)
    }

    // ── 跨节点 Direct 持久去重 DEDUP_DIRECT ──

    fn dedup_key(sender: &[u8], gram_id: u64, crc: &[u8]) -> Vec<u8> {
        let mut k = sender.to_vec();
        k.extend_from_slice(&gram_id.to_be_bytes());
        k.extend_from_slice(&crc[..crc.len().min(8)]);
        k
    }

    /// 查询是否已见过该 (sender,gram_id,crc)——只读，不标记。true=已见。
    pub fn dedup_seen(&self, sender: &[u8], gram_id: u64, crc: &[u8]) -> Result<bool> {
        let key = Self::dedup_key(sender, gram_id, crc);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let tb = rtx.open_table(DEDUP_DIRECT).map_err(db_err)?;
        Ok(tb.get(key.as_slice()).map_err(db_err)?.is_some())
    }

    /// 标记 (sender,gram_id,crc) 已见（投递成功后调用——先投出才标记，防"标记了却没投出"的空洞）。
    pub fn dedup_mark(&self, sender: &[u8], gram_id: u64, crc: &[u8], now_ms: u64) -> Result<()> {
        let key = Self::dedup_key(sender, gram_id, crc);
        let wtx = self.db.begin_write().map_err(db_err)?;
        { let mut tb = wtx.open_table(DEDUP_DIRECT).map_err(db_err)?;
          tb.insert(key.as_slice(), now_ms.to_be_bytes().as_slice()).map_err(db_err)?; }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 清理早于 before_ms 的去重记录（按龄，对齐重传上限）。返回删除条数。
    pub fn dedup_prune(&self, before_ms: u64) -> Result<usize> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        let mut n = 0usize;
        { let mut tb = wtx.open_table(DEDUP_DIRECT).map_err(db_err)?;
          for item in tb.extract_if(|_, v| {
              v.len() >= 8 && u64::from_be_bytes(v[0..8].try_into().unwrap()) < before_ms
          }).map_err(db_err)? { item.map_err(db_err)?; n += 1; } }
        wtx.commit().map_err(db_err)?;
        Ok(n)
    }

    // ── 跨节点群消息可靠扇出：待确认发件箱 GROUP_OUTBOX（按待 ack 成员节点集跟踪）──

    fn group_outbox_key(gid: &[u8], sender: &[u8], gram_id: u64) -> Vec<u8> {
        let mut k = Vec::with_capacity(gid.len() + sender.len() + 8);
        k.extend_from_slice(gid);
        k.extend_from_slice(sender);
        k.extend_from_slice(&gram_id.to_be_bytes());
        k
    }

    fn group_outbox_encode(first_ms: u64, retries: u32, next_due_ms: u64,
                           pending: &[Vec<u8>], gram_bytes: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(22 + pending.len() * 32 + gram_bytes.len());
        v.extend_from_slice(&first_ms.to_be_bytes());
        v.extend_from_slice(&retries.to_be_bytes());
        v.extend_from_slice(&next_due_ms.to_be_bytes());
        v.extend_from_slice(&(pending.len() as u16).to_be_bytes());
        for n in pending { v.extend_from_slice(n); } // 每个节点 id 定长 32
        v.extend_from_slice(gram_bytes);
        v
    }

    /// 解出 (first_ms, retries, next_due_ms, pending_nodes, gram_bytes)；格式不符返回 None。
    fn group_outbox_decode(v: &[u8]) -> Option<(u64, u32, u64, Vec<Vec<u8>>, Vec<u8>)> {
        if v.len() < 22 { return None; }
        let first_ms = u64::from_be_bytes(v[0..8].try_into().ok()?);
        let retries = u32::from_be_bytes(v[8..12].try_into().ok()?);
        let next_due_ms = u64::from_be_bytes(v[12..20].try_into().ok()?);
        let n = u16::from_be_bytes(v[20..22].try_into().ok()?) as usize;
        let nodes_end = 22 + n * 32;
        if v.len() < nodes_end { return None; }
        let mut pending = Vec::with_capacity(n);
        for i in 0..n { pending.push(v[22 + i * 32..22 + (i + 1) * 32].to_vec()); }
        let gram_bytes = v[nodes_end..].to_vec();
        Some((first_ms, retries, next_due_ms, pending, gram_bytes))
    }

    /// 存/覆盖一条群待确认条目（Immediate 落盘）。`pending_nodes`=期望回 ack 的成员 home 节点集。
    pub fn group_outbox_put(&self, gid: &[u8], sender: &[u8], gram_id: u64, first_ms: u64,
                            retries: u32, next_due_ms: u64, pending_nodes: &[Vec<u8>],
                            gram_bytes: &[u8]) -> Result<()> {
        let key = Self::group_outbox_key(gid, sender, gram_id);
        let val = Self::group_outbox_encode(first_ms, retries, next_due_ms, pending_nodes, gram_bytes);
        let wtx = self.db.begin_write().map_err(db_err)?;
        { let mut tb = wtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
          tb.insert(key.as_slice(), val.as_slice()).map_err(db_err)?; }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 收到某成员节点的 GroupAck：从待 ack 集移除该 node；集合空则删条目。
    /// 返回 (found=条目存在, cleared=移除后已全清并删除)。幂等（node 不在集中也不报错）。
    pub fn group_outbox_ack_node(&self, gid: &[u8], sender: &[u8], gram_id: u64, node: &[u8]) -> Result<(bool, bool)> {
        let key = Self::group_outbox_key(gid, sender, gram_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let mut found = false;
        let mut cleared = false;
        {
            let mut tb = wtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
            let existing = tb.get(key.as_slice()).map_err(db_err)?.map(|g| g.value().to_vec());
            if let Some(bytes) = existing {
                found = true;
                if let Some((first_ms, retries, next_due_ms, mut pending, gram_bytes)) =
                    Self::group_outbox_decode(&bytes)
                {
                    pending.retain(|n| n.as_slice() != node);
                    if pending.is_empty() {
                        tb.remove(key.as_slice()).map_err(db_err)?;
                        cleared = true;
                    } else {
                        let val = Self::group_outbox_encode(first_ms, retries, next_due_ms, &pending, &gram_bytes);
                        tb.insert(key.as_slice(), val.as_slice()).map_err(db_err)?;
                    }
                }
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok((found, cleared))
    }

    /// 删一条群待确认条目（达期限放弃）。幂等。
    pub fn group_outbox_del(&self, gid: &[u8], sender: &[u8], gram_id: u64) -> Result<bool> {
        let key = Self::group_outbox_key(gid, sender, gram_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let existed;
        { let mut tb = wtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
          existed = tb.remove(key.as_slice()).map_err(db_err)?.is_some(); }
        wtx.commit().map_err(db_err)?;
        Ok(existed)
    }

    /// 全量加载群待确认条目（重传任务每 tick 扫；稳态近空）。key 前 72 字节=gid32+sender32+gram8。
    pub fn group_outbox_all(&self) -> Result<Vec<GroupOutboxRow>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let tb = rtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
        let mut out = Vec::new();
        for item in tb.iter().map_err(db_err)? {
            let (k, v) = item.map_err(db_err)?;
            let k = k.value();
            if k.len() != 72 { continue; }
            let Some((first_ms, retries, next_due_ms, pending_nodes, gram_bytes)) =
                Self::group_outbox_decode(v.value()) else { continue };
            out.push(GroupOutboxRow {
                gid: k[0..32].to_vec(),
                sender: k[32..64].to_vec(),
                gram_id: u64::from_be_bytes(k[64..72].try_into().unwrap()),
                first_ms, retries, next_due_ms, pending_nodes, gram_bytes,
            });
        }
        Ok(out)
    }


    /// 某实体离线消息条数。range 直接计数，不解码 value（避免 O(n) prost 解码）。
    pub fn inbox_len(&self, receiver: &[u8]) -> Result<usize> {
        let (lo, hi) = inbox_prefix_range(receiver);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(INBOX).map_err(db_err)?;
        Ok(t.range(lo.as_slice()..hi.as_slice()).map_err(db_err)?.count())
    }

    /// 删除所有收件人中 `timestamp_ms` 早于 `before_ms` 的离线消息，返回删除条数。
    pub fn prune_inbox(&self, before_ms: u64) -> Result<usize> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        let mut n = 0usize;
        {
            let mut t = wtx.open_table(INBOX).map_err(db_err)?;
            for item in t
                .extract_if(|_, v| Gram::decode(v).map(|g| g.timestamp_ms < before_ms).unwrap_or(true))
                .map_err(db_err)?
            {
                item.map_err(db_err)?;
                n += 1;
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok(n)
    }

    // ---- 多设备 ----

    pub fn put_device(&self, info: &DeviceInfo) -> Result<()> {
        let Some(cert) = info.cert.as_ref() else {
            return Err(StoreError::Db("device info without cert".into()));
        };
        let val = info.encode_to_vec();
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(DEVICES).map_err(db_err)?;
            t.insert(cert.device.as_slice(), val.as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    pub fn all_devices(&self) -> Result<Vec<DeviceInfo>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(DEVICES).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (_k, v) = item.map_err(db_err)?;
            out.push(DeviceInfo::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn put_revoke(&self, r: &DeviceRevoke) -> Result<()> {
        let val = r.encode_to_vec();
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(REVOKED).map_err(db_err)?;
            t.insert(r.device.as_slice(), val.as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    pub fn all_revokes(&self) -> Result<Vec<DeviceRevoke>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(REVOKED).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (_k, v) = item.map_err(db_err)?;
            out.push(DeviceRevoke::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?);
        }
        Ok(out)
    }

    // ---- 目录持久化 ----

    pub fn put_entity(&self, entity: &Entity) -> Result<()> {
        let val = entity.encode_to_vec();
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(ENTITIES).map_err(db_err)?;
            t.insert(entity.entity_id.as_slice(), val.as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    pub fn get_entity(&self, entity_id: &[u8]) -> Result<Option<Entity>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(ENTITIES).map_err(db_err)?;
        match t.get(entity_id).map_err(db_err)? {
            Some(v) => {
                let e = Entity::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?;
                Ok(Some(e))
            }
            None => Ok(None),
        }
    }

    /// 加载全部实体（节点启动时回填内存目录）。
    pub fn all_entities(&self) -> Result<Vec<Entity>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(ENTITIES).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (_k, v) = item.map_err(db_err)?;
            let e = Entity::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?;
            out.push(e);
        }
        Ok(out)
    }

    // ---- 内容寻址 blob（P1：头像等媒体）----

    /// 存一个 blob（key = blob.hash，应等于 blake3(blob.data)，由调用方保证/校验）。内容寻址=幂等去重。
    pub fn put_blob(&self, blob: &BlobData) -> Result<()> {
        let val = blob.encode_to_vec();
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(BLOBS).map_err(db_err)?;
            t.insert(blob.hash.as_slice(), val.as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 按 hash 取一个 blob。
    pub fn get_blob(&self, hash: &[u8]) -> Result<Option<BlobData>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(BLOBS).map_err(db_err)?;
        match t.get(hash).map_err(db_err)? {
            Some(v) => Ok(Some(
                BlobData::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    // ---- 群组持久化 ----

    pub fn put_group(&self, g: &Group) -> Result<()> {
        let val = g.encode_to_vec();
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(GROUPS).map_err(db_err)?;
            t.insert(g.group_id.as_slice(), val.as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 删除一个群（解散）。
    pub fn del_group(&self, group_id: &[u8]) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(GROUPS).map_err(db_err)?;
            t.remove(group_id).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    pub fn get_group(&self, group_id: &[u8]) -> Result<Option<Group>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(GROUPS).map_err(db_err)?;
        match t.get(group_id).map_err(db_err)? {
            Some(v) => Ok(Some(
                Group::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    pub fn all_groups(&self) -> Result<Vec<Group>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(GROUPS).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (_k, v) = item.map_err(db_err)?;
            out.push(Group::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?);
        }
        Ok(out)
    }

    // ---- 命名记录持久化（N1）----
    pub fn put_name(&self, r: &NameRecord) -> Result<()> {
        let key = format!("{}@{}", r.local_part, r.domain);
        let val = r.encode_to_vec();
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(NAMES).map_err(db_err)?;
            t.insert(key.as_bytes(), val.as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    pub fn all_names(&self) -> Result<Vec<NameRecord>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(NAMES).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (_k, v) = item.map_err(db_err)?;
            out.push(NameRecord::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?);
        }
        Ok(out)
    }
    pub fn put_name_secret(&self, full: &str, phc: &str) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(NAME_SECRETS).map_err(db_err)?;
            t.insert(full.as_bytes(), phc.as_bytes()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    pub fn name_secret(&self, full: &str) -> Result<Option<String>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(NAME_SECRETS).map_err(db_err)?;
        match t.get(full.as_bytes()).map_err(db_err)? {
            Some(v) => Ok(Some(String::from_utf8_lossy(v.value()).into_owned())),
            None => Ok(None),
        }
    }
    /// F2：存本节点为 home 的群签名私钥（32 字节）。仅本地、绝不广播；镜像 [`Self::name_secret`]。
    pub fn put_group_secret(&self, group_id: &[u8], secret: &[u8]) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(GROUP_SECRETS).map_err(db_err)?;
            t.insert(group_id, secret).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    pub fn group_secret(&self, group_id: &[u8]) -> Result<Option<Vec<u8>>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(GROUP_SECRETS).map_err(db_err)?;
        match t.get(group_id).map_err(db_err)? {
            Some(v) => Ok(Some(v.value().to_vec())),
            None => Ok(None),
        }
    }
    pub fn del_name(&self, full: &str) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(NAMES).map_err(db_err)?;
            t.remove(full.as_bytes()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    // 本节点拥有的域名（N1）。
    pub fn put_domain(&self, domain: &str) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(DOMAINS).map_err(db_err)?;
            t.insert(domain.as_bytes(), b"1".as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    pub fn del_domain(&self, domain: &str) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(DOMAINS).map_err(db_err)?;
            t.remove(domain.as_bytes()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    pub fn all_domains(&self) -> Result<Vec<String>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(DOMAINS).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (k, _v) = item.map_err(db_err)?;
            out.push(String::from_utf8_lossy(k.value()).to_string());
        }
        Ok(out)
    }

    // ---- 黑名单持久化（无白名单：默认放行，仅拒绝名单内公钥）----

    /// 把一个公钥加入黑名单（幂等）。
    pub fn ban(&self, pubkey: &[u8]) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(BLACKLIST).map_err(db_err)?;
            t.insert(pubkey, [].as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 把一个公钥移出黑名单（幂等）。
    pub fn unban(&self, pubkey: &[u8]) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(BLACKLIST).map_err(db_err)?;
            t.remove(pubkey).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 加载全部被禁公钥（节点启动时回填内存黑名单）。
    pub fn all_banned(&self) -> Result<Vec<[u8; 32]>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(BLACKLIST).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (k, _v) = item.map_err(db_err)?;
            if let Ok(arr) = <[u8; 32]>::try_from(k.value()) {
                out.push(arr);
            }
        }
        Ok(out)
    }

    // ---- 联邦对等持久化（value 为不透明元数据 JSON 串，上层自解释）----

    /// 写入/更新一个对等（公钥 → 元数据 JSON）。
    pub fn put_peer(&self, id: &[u8], meta_json: &str) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(PEERS).map_err(db_err)?;
            t.insert(id, meta_json.as_bytes()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 移除一个对等（幂等）。
    pub fn remove_peer(&self, id: &[u8]) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(PEERS).map_err(db_err)?;
            t.remove(id).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 加载全部对等（node id + 元数据 JSON 串）。
    pub fn all_peers(&self) -> Result<Vec<([u8; 32], String)>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(PEERS).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (k, v) = item.map_err(db_err)?;
            if let Ok(id) = <[u8; 32]>::try_from(k.value()) {
                out.push((id, String::from_utf8_lossy(v.value()).into_owned()));
            }
        }
        Ok(out)
    }

    // ---- Roster: per-account contact list ----
    // key = account_pubkey(32) ++ entry_id(32), value = RosterEntry JSON.

    /// Write or replace one roster entry.
    pub fn put_roster(&self, account: &[u8], entry_id: &[u8], json: &str) -> Result<()> {
        let mut key = Vec::with_capacity(account.len() + entry_id.len());
        key.extend_from_slice(account); key.extend_from_slice(entry_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        { let mut t = wtx.open_table(ROSTER).map_err(db_err)?;
          t.insert(key.as_slice(), json.as_bytes()).map_err(db_err)?; }
        wtx.commit().map_err(db_err)?; Ok(())
    }

    /// Remove one roster entry (idempotent).
    pub fn del_roster(&self, account: &[u8], entry_id: &[u8]) -> Result<()> {
        let mut key = Vec::with_capacity(account.len() + entry_id.len());
        key.extend_from_slice(account); key.extend_from_slice(entry_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        { let mut t = wtx.open_table(ROSTER).map_err(db_err)?;
          t.remove(key.as_slice()).map_err(db_err)?; }
        wtx.commit().map_err(db_err)?; Ok(())
    }

    /// Return all roster entries for an account as JSON strings.
    pub fn get_roster_all(&self, account: &[u8]) -> Result<Vec<String>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(ROSTER).map_err(db_err)?;
        let lo = account.to_vec();
        let mut hi = lo.clone();
        for i in (0..hi.len()).rev() {
            if hi[i] < 0xFF { hi[i] += 1; break; } else { hi[i] = 0; }
        }
        let mut out = Vec::new();
        for item in t.range::<&[u8]>(lo.as_slice()..hi.as_slice()).map_err(db_err)? {
            let (_, v) = item.map_err(db_err)?;
            out.push(String::from_utf8_lossy(v.value()).into_owned());
        }
        Ok(out)
    }
}

fn inbox_key(receiver: &[u8], gram_id: u64) -> Vec<u8> {
    let mut k = Vec::with_capacity(receiver.len() + 8);
    k.extend_from_slice(receiver);
    k.extend_from_slice(&gram_id.to_be_bytes());
    k
}

/// `[receiver ++ 0x00*8, receiver ++ 0xFF*8]` 的半开区间上界用 receiver 递增后缀实现。
fn inbox_prefix_range(receiver: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let lo = inbox_key(receiver, u64::MIN);
    // 上界：receiver ++ 8*0xFF 之后再 +1，等价于 receiver 后接 9 字节 0xFF...但用
    // “receiver ++ u64::MAX 的下一个 key”表达为 receiver ++ 0xFF*8，再把区间设为闭上界不便，
    // 这里改用 receiver 前缀的“递增”上界：把 receiver 视作大整数 +1。
    let mut hi = receiver.to_vec();
    for i in (0..hi.len()).rev() {
        if hi[i] == 0xFF {
            hi[i] = 0;
        } else {
            hi[i] += 1;
            return (lo, hi);
        }
    }
    // receiver 全为 0xFF：上界用一个比任何 key 都大的值（多加一字节）。
    let mut hi = inbox_key(receiver, u64::MAX);
    hi.push(0xFF);
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nm_proto::{now_ms, GramKind};

    fn msg(receiver: [u8; 32], id: u64, text: &str) -> Gram {
        Gram {
            version: 1,
            kind: GramKind::Message as i32,
            gram_id: id,
            ref_gram_id: None,
            sender: [1u8; 32].to_vec(),
            receiver: receiver.to_vec(),
            timestamp_ms: now_ms(),
            payload: Some(nm_proto::Any {
                type_url: "nmspace.v1/text".into(),
                value: text.as_bytes().to_vec(),
            }),
            crc: Vec::new(),
        }
    }

    #[test]
    fn inbox_fifo_and_drain() {
        let dir = tempfile::tempdir().unwrap();
        let s = RedbStore::open(dir.path().join("t.redb")).unwrap();
        let r = [7u8; 32];
        let other = [8u8; 32];
        s.push_inbox(&r, &msg(r, 2, "second")).unwrap();
        s.push_inbox(&r, &msg(r, 1, "first")).unwrap();
        s.push_inbox(&other, &msg(other, 1, "other")).unwrap();

        // 只取 r 的、且按 gram_id 升序。
        let peek = s.peek_inbox(&r).unwrap();
        assert_eq!(peek.len(), 2);
        assert_eq!(peek[0].gram_id, 1);
        assert_eq!(peek[1].gram_id, 2);

        // drain 清空。
        let drained = s.drain_inbox(&r).unwrap();
        assert_eq!(drained.len(), 2);
        assert_eq!(s.inbox_len(&r).unwrap(), 0);
        // 未触及 other。
        assert_eq!(s.inbox_len(&other).unwrap(), 1);
    }

    #[test]
    fn inbox_prune_by_age() {
        let dir = tempfile::tempdir().unwrap();
        let s = RedbStore::open(dir.path().join("t.redb")).unwrap();
        let r = [7u8; 32];
        let mut old = msg(r, 1, "old");
        old.timestamp_ms = 1_000;
        s.push_inbox(&r, &old).unwrap();
        s.push_inbox(&r, &msg(r, 2, "new")).unwrap();
        assert_eq!(s.prune_inbox(10_000).unwrap(), 1);
        let left = s.peek_inbox(&r).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].gram_id, 2);
    }

    #[test]
    fn devices_and_revokes_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.redb");
        let cert = nm_proto::DeviceCert { account: vec![1; 32], device: vec![2; 32], role: "member".into(), ..Default::default() };
        {
            let s = RedbStore::open(&path).unwrap();
            s.put_device(&DeviceInfo { cert: Some(cert.clone()), label: "a".into(), ..Default::default() }).unwrap();
            s.put_device(&DeviceInfo { cert: Some(cert.clone()), label: "b".into(), ..Default::default() }).unwrap();
            s.put_revoke(&DeviceRevoke { account: vec![1; 32], device: vec![2; 32], ..Default::default() }).unwrap();
        }
        let s = RedbStore::open(&path).unwrap();
        let devs = s.all_devices().unwrap();
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].label, "b");
        assert_eq!(s.all_revokes().unwrap().len(), 1);
    }

    #[test]
    fn entity_persist_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.redb");
        let e = Entity {
            entity_id: [9u8; 32].to_vec(),
            kind: "agent.assistant".into(),
            display_name: "A".into(),
            ..Default::default()
        };
        {
            let s = RedbStore::open(&path).unwrap();
            s.put_entity(&e).unwrap();
        }
        // 重新打开 → 数据仍在（持久化）。
        let s = RedbStore::open(&path).unwrap();
        assert_eq!(s.all_entities().unwrap().len(), 1);
        assert_eq!(s.get_entity(&[9u8; 32]).unwrap().unwrap().kind, "agent.assistant");
    }

    #[test]
    fn blacklist_persist_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.redb");
        let k = [5u8; 32];
        {
            let s = RedbStore::open(&path).unwrap();
            s.ban(&k).unwrap();
            assert_eq!(s.all_banned().unwrap(), vec![k]);
            s.unban(&k).unwrap(); // 幂等解封
            assert!(s.all_banned().unwrap().is_empty());
            s.ban(&k).unwrap();
        }
        // 重开 → 封禁仍在（持久化）。
        let s = RedbStore::open(&path).unwrap();
        assert_eq!(s.all_banned().unwrap(), vec![k]);
    }
}

#[cfg(test)]
mod blob_tests {
    use super::*;
    use nm_proto::BlobData;

    // P1：内容寻址 blob 存取往返 + 未命中返回 None。
    #[test]
    fn blob_roundtrip() {
        let path = std::env::temp_dir().join(format!("nmstore-blob-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = RedbStore::open(&path).unwrap();
        let hash = vec![7u8; 32];
        let blob = BlobData { hash: hash.clone(), data: b"hello-avatar".to_vec(), mime: "image/jpeg".into() };
        store.put_blob(&blob).unwrap();
        let got = store.get_blob(&hash).unwrap().expect("blob present");
        assert_eq!(got.data, b"hello-avatar");
        assert_eq!(got.mime, "image/jpeg");
        assert!(store.get_blob(&[9u8; 32]).unwrap().is_none(), "未知 hash 应 None");
        let _ = std::fs::remove_file(&path);
    }
}

//! `nm-store` — 存储抽象的 `redb` 实现：离线队列(inbox) + 实体目录持久化。
//! 见 `docs/PLAN_B` §8.6 与 `docs/PLAN_C` §6。
//!
//! 表设计：
//! - `INBOX`：key = `receiver(32) ++ gram_id(8, BE)`，value = prost 编码的 `Gram`。
//!   前缀 range 扫描即可取某实体的全部离线消息，`gram_id` 单调保证 FIFO。
//! - `ENTITIES`：key = `entity_id(32)`，value = prost 编码的 `Entity`（目录持久化）。

use nm_proto::{
    BlobData, Channel, ChannelMsg, DeviceInfo, DeviceRevoke, Entity, Gram, Group, NameRecord,
    now_ms,
};
use prost::Message;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

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
/// P1-8 管理操作员凭据：key = 操作员名(utf8)，value = JSON {phc, scope:"read"|"control", disabled}。
/// 只存 Argon2 PHC 哈希(非明文)，可随 /backup 入快照；镜像 NAME_SECRETS 的 KV 形状。
const OPERATORS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("operators");
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
/// OUTBOX 到期索引：key = next_due_ms(8BE)++OUTBOX.key，value=空。重传循环按时间 range 扫，避免全表扫描。
const OUTBOX_DUE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("outbox_due");
/// 跨节点 Direct 持久去重：key = sender(32)++gram_id(8BE)++crc(8)，val = first_seen_ms(8BE)。
/// 并入 crc 防 client 重启 gram_id 回绕导致的误去重。SeenGrams 内存版的持久权威兜底。
const DEDUP_DIRECT: TableDefinition<&[u8], &[u8]> = TableDefinition::new("dedup_direct");
/// 跨节点群消息可靠扇出·待确认发件箱：key = gid(32)++sender(32)++gram_id(8BE)，
/// val = first_ms(8)++retries(4)++next_due_ms(8)++n_pending(2)++[node(32)*n]++gram_bytes。
/// 按「待 ack 成员节点集」跟踪：收到某节点 GroupAck 即移除该 node，集合空则删、停重传。
const GROUP_OUTBOX: TableDefinition<&[u8], &[u8]> = TableDefinition::new("group_outbox");
/// GROUP_OUTBOX 到期索引：key = next_due_ms(8BE)++GROUP_OUTBOX.key，value=空。
const GROUP_OUTBOX_DUE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("group_outbox_due");
// ── 频道（P4 升级：持久化 + 全局定序 + 可靠广播）──
/// 频道元信息：key = channel_id(32)，value = prost 编码的 Channel。重启回填频道目录。
const CHANNELS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("channels");
/// 账号级订阅：key = account(32)++channel_id(32)，value = 订阅时间 Unix ms(8BE)。
/// 账号级（非连接级）→ 重启/换设备仍订阅；前缀扫某账号的全部订阅。
const CHANNEL_SUBS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("channel_subs");
/// 频道消息持久日志：key = channel_id(32)++seq(8BE)，value = prost 编码的 ChannelMsg。
/// 前缀区间扫做回填；按龄+条数裁剪。(channel_id,seq) 唯一 → 日志本身即去重（点查）。
const CHANNEL_LOG: TableDefinition<&[u8], &[u8]> = TableDefinition::new("channel_log");
/// 频道 seq 计数器（仅 home）：key = channel_id(32)，value = 下一个待分配 seq(8BE)。
/// 永不裁剪（日志会裁，故不能用 log-max 推算，否则 seq 复用）。单事务 RMW。
const CHANNEL_SEQ: TableDefinition<&[u8], &[u8]> = TableDefinition::new("channel_seq");
/// 频道签名私钥（仅 home）：key = channel_id(32，= 公钥)，value = 私钥(32)。仅本地、绝不广播。
const CHANNEL_SECRETS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("channel_secrets");
/// 频道定序幂等表（仅 home）：key = channel_id(32)++sender(32)++nonce(≤32)，value = 已分配 seq(8BE)。
/// 让「持久先于广播」与「publisher 重传」都幂等：命中即重广播同一 seq，不重分配。按龄裁剪。
const CHANNEL_INTAKE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("channel_intake");
/// 频道发布待确认发件箱（publisher 侧）：key = channel_id(32)++sender(32)++nonce(≤32)，
/// val = first_ms(8)++retries(4)++next_due_ms(8)++pubreq_bytes。看到自己的 sequenced 消息/收 PubReject 即删。
const CHANNEL_OUTBOX: TableDefinition<&[u8], &[u8]> = TableDefinition::new("channel_outbox");
/// CHANNEL_OUTBOX 到期索引：key = next_due_ms(8BE)++CHANNEL_OUTBOX.key，value=空。
const CHANNEL_OUTBOX_DUE: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("channel_outbox_due");
/// P0-2 模式元信息：key = "version"(u32 BE) / "magic"(DB_MAGIC) / "last_backup_ms"(u64 BE)。
/// 打开时据此判定新库/旧库并跑迁移链；版本高于本二进制即拒绝启动（防降级静默丢数据）。
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
/// P0-1 blob 入库时间旁表：key = blob hash(32)，value = put_ms(8BE)。供 blob 按龄 GC（主表 BLOBS 本身无时间）。
const BLOB_META: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blob_meta");
/// P0-3 死信：投递达期限放弃时落档，key = sender(32)++receiver(32)++gram_id(8BE)++first_ms(8BE)，value = gram_bytes。
/// 供诊断/人工补投；按 first_ms 龄裁剪，不无界增长。
const DEAD_LETTER: TableDefinition<&[u8], &[u8]> = TableDefinition::new("dead_letter");
/// P1-6 多设备已读：key = account(32)++conversation(32)，value = last_read_ms(8BE，单调不减)。
/// conversation = 对端 EntityId(单聊) / group_id(群)，由客户端定义；本层只做按账号的单调 KV + 前缀扫。
const READ_STATE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("read_state");

/// 当前存储模式版本。每次对**手工字节键/值布局**或表语义做破坏性变更即 +1，并补一条迁移。
const SCHEMA_VERSION: u32 = 2;
/// 库身份魔数：防把别的 redb 文件误当 nmd 存储打开。
const DB_MAGIC: &[u8] = b"nmspace-nmd-store";

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

type GroupOutboxDecoded = (u64, u32, u64, Vec<Vec<u8>>, Vec<u8>);

/// 频道发布待确认发件箱一行（publisher 侧重传任务用）。`pubreq_bytes`=编码的 ChannelPubReq。
pub struct ChannelOutboxRow {
    pub channel_id: Vec<u8>,
    pub sender: Vec<u8>,
    pub nonce: Vec<u8>,
    pub first_ms: u64,
    pub retries: u32,
    pub next_due_ms: u64,
    pub pubreq_bytes: Vec<u8>,
}

fn db_err(e: impl std::fmt::Display) -> StoreError {
    StoreError::Db(e.to_string())
}

fn due_key(next_due_ms: u64, primary_key: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(8 + primary_key.len());
    k.extend_from_slice(&next_due_ms.to_be_bytes());
    k.extend_from_slice(primary_key);
    k
}

fn due_range(until_ms: u64) -> (Vec<u8>, Vec<u8>) {
    let lo = 0u64.to_be_bytes().to_vec();
    let hi = until_ms
        .checked_add(1)
        .map(|n| n.to_be_bytes().to_vec())
        .unwrap_or_else(|| vec![0xff; 9]);
    (lo, hi)
}

/// redb 后端。
pub struct RedbStore {
    db: Database,
    /// 数据库文件路径（P0-2 在线备份 `backup()` 据此拷贝一致快照）。
    path: std::path::PathBuf,
}

impl RedbStore {
    /// 打开/创建数据库文件，并确保表存在。随后做 P0-2 模式版本校验/迁移。
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let db = Database::create(&path).map_err(db_err)?;
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
            wtx.open_table(OPERATORS).map_err(db_err)?;
            wtx.open_table(DOMAINS).map_err(db_err)?;
            wtx.open_table(DEVICES).map_err(db_err)?;
            wtx.open_table(REVOKED).map_err(db_err)?;
            wtx.open_table(OUTBOX).map_err(db_err)?;
            wtx.open_table(OUTBOX_DUE).map_err(db_err)?;
            wtx.open_table(DEDUP_DIRECT).map_err(db_err)?;
            wtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
            wtx.open_table(GROUP_OUTBOX_DUE).map_err(db_err)?;
            wtx.open_table(CHANNELS).map_err(db_err)?;
            wtx.open_table(CHANNEL_SUBS).map_err(db_err)?;
            wtx.open_table(CHANNEL_LOG).map_err(db_err)?;
            wtx.open_table(CHANNEL_SEQ).map_err(db_err)?;
            wtx.open_table(CHANNEL_SECRETS).map_err(db_err)?;
            wtx.open_table(CHANNEL_INTAKE).map_err(db_err)?;
            wtx.open_table(CHANNEL_OUTBOX).map_err(db_err)?;
            wtx.open_table(CHANNEL_OUTBOX_DUE).map_err(db_err)?;
            wtx.open_table(META).map_err(db_err)?;
            wtx.open_table(BLOB_META).map_err(db_err)?;
            wtx.open_table(DEAD_LETTER).map_err(db_err)?;
            wtx.open_table(READ_STATE).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        let store = Self { db, path };
        store.ensure_schema()?;
        Ok(store)
    }

    // ── P0-2：模式版本 / 迁移 / 在线备份 ──

    /// 模式版本校验与迁移。META 无 version 时：代表性表全空→判为新库戳当前版本；否则判为旧库(v0)跑迁移链。
    /// version 高于本二进制 → **拒绝启动**（防旧版 nmd 打开新库后按旧布局静默丢行）。
    fn ensure_schema(&self) -> Result<()> {
        match self.meta_get_u32("version")? {
            Some(v) if v == SCHEMA_VERSION => self.verify_magic(),
            Some(v) if v > SCHEMA_VERSION => Err(StoreError::Db(format!(
                "db schema v{v} 比本 nmd(v{SCHEMA_VERSION})新：请升级 nmd 或恢复兼容备份，拒绝按旧布局打开"
            ))),
            Some(mut v) => {
                self.verify_magic()?;
                while v < SCHEMA_VERSION {
                    self.migrate(v, v + 1)?;
                    v += 1;
                }
                self.stamp_schema()
            }
            None => {
                let mut v = if self.looks_empty()? {
                    SCHEMA_VERSION
                } else {
                    0
                };
                while v < SCHEMA_VERSION {
                    self.migrate(v, v + 1)?;
                    v += 1;
                }
                self.stamp_schema()
            }
        }
    }

    fn meta_get_u32(&self, key: &str) -> Result<Option<u32>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(META).map_err(db_err)?;
        Ok(t.get(key)
            .map_err(db_err)?
            .and_then(|v| v.value().try_into().ok().map(u32::from_be_bytes)))
    }

    fn verify_magic(&self) -> Result<()> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(META).map_err(db_err)?;
        match t.get("magic").map_err(db_err)? {
            Some(m) if m.value() == DB_MAGIC => Ok(()),
            Some(_) => Err(StoreError::Db(
                "db magic 不符：这不是 nmspace nmd 存储文件".into(),
            )),
            None => Ok(()), // 迁移中途尚未写 magic，容忍
        }
    }

    fn stamp_schema(&self) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(META).map_err(db_err)?;
            t.insert("version", SCHEMA_VERSION.to_be_bytes().as_slice())
                .map_err(db_err)?;
            t.insert("magic", DB_MAGIC).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 代表性表全空 → 视为新库（无需迁移，直接戳当前版本）。
    fn looks_empty(&self) -> Result<bool> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        for td in [ENTITIES, INBOX, DEVICES, CHANNELS, GROUPS, NAMES] {
            if rtx.open_table(td).map_err(db_err)?.len().map_err(db_err)? > 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn migrate(&self, from: u32, to: u32) -> Result<()> {
        tracing::info!(from, to, "nm-store schema 迁移");
        match (from, to) {
            (0, 1) => self.migrate_v0_to_v1(),
            (1, 2) => self.migrate_v1_to_v2(),
            _ => Err(StoreError::Db(format!("无迁移路径 v{from}→v{to}"))),
        }
    }

    /// v0→v1：① INBOX value 由裸 Gram 改为 `recv_ms(8BE)++Gram`（按入库时间裁剪，不信发送方时钟）；
    /// ② CHANNEL_INTAKE value 由 `seq(8)` 改为 `seq(8)++ts(8)`（支持按龄裁剪）。一次性重写旧行。
    fn migrate_v0_to_v1(&self) -> Result<()> {
        let now = now_ms();
        // INBOX：裸 Gram → recv_ms ++ Gram。入库时间近似取发送时间(≤now)以保留原保留期意图。
        let rows: Vec<(Vec<u8>, Vec<u8>)> = {
            let rtx = self.db.begin_read().map_err(db_err)?;
            let t = rtx.open_table(INBOX).map_err(db_err)?;
            let mut v = Vec::new();
            for it in t.iter().map_err(db_err)? {
                let (k, val) = it.map_err(db_err)?;
                v.push((k.value().to_vec(), val.value().to_vec()));
            }
            v
        };
        if !rows.is_empty() {
            let wtx = self.db.begin_write().map_err(db_err)?;
            {
                let mut t = wtx.open_table(INBOX).map_err(db_err)?;
                for (k, raw) in &rows {
                    let recv = Gram::decode(raw.as_slice())
                        .map(|g| g.timestamp_ms.min(now))
                        .unwrap_or(now);
                    let mut nv = Vec::with_capacity(8 + raw.len());
                    nv.extend_from_slice(&recv.to_be_bytes());
                    nv.extend_from_slice(raw);
                    t.insert(k.as_slice(), nv.as_slice()).map_err(db_err)?;
                }
            }
            wtx.commit().map_err(db_err)?;
        }
        // CHANNEL_INTAKE：seq(8) → seq(8)++ts(8)（ts=now，自升级起计龄）。
        let irows: Vec<(Vec<u8>, Vec<u8>)> = {
            let rtx = self.db.begin_read().map_err(db_err)?;
            let t = rtx.open_table(CHANNEL_INTAKE).map_err(db_err)?;
            let mut v = Vec::new();
            for it in t.iter().map_err(db_err)? {
                let (k, val) = it.map_err(db_err)?;
                v.push((k.value().to_vec(), val.value().to_vec()));
            }
            v
        };
        if !irows.is_empty() {
            let wtx = self.db.begin_write().map_err(db_err)?;
            {
                let mut t = wtx.open_table(CHANNEL_INTAKE).map_err(db_err)?;
                for (k, val) in &irows {
                    if val.len() >= 16 {
                        continue;
                    }
                    let seq = u64::from_be_bytes(
                        val.get(..8)
                            .and_then(|s| s.try_into().ok())
                            .unwrap_or([0u8; 8]),
                    );
                    let mut nv = Vec::with_capacity(16);
                    nv.extend_from_slice(&seq.to_be_bytes());
                    nv.extend_from_slice(&now.to_be_bytes());
                    t.insert(k.as_slice(), nv.as_slice()).map_err(db_err)?;
                }
            }
            wtx.commit().map_err(db_err)?;
        }
        Ok(())
    }

    /// v1→v2：为三类 outbox 补 `next_due_ms` 索引。主表布局不变，迁移只从现有主表重建索引。
    fn migrate_v1_to_v2(&self) -> Result<()> {
        self.rebuild_outbox_due_indexes()
    }

    fn rebuild_outbox_due_indexes(&self) -> Result<()> {
        let outbox = self.outbox_all()?;
        let group = self.group_outbox_all()?;
        let channel = self.channel_outbox_all()?;
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut od = wtx.open_table(OUTBOX_DUE).map_err(db_err)?;
            for row in &outbox {
                let pk = Self::outbox_key(&row.sender, &row.receiver, row.gram_id);
                let dk = due_key(row.next_due_ms, &pk);
                od.insert(dk.as_slice(), [].as_slice()).map_err(db_err)?;
            }
            let mut gd = wtx.open_table(GROUP_OUTBOX_DUE).map_err(db_err)?;
            for row in &group {
                let pk = Self::group_outbox_key(&row.gid, &row.sender, row.gram_id);
                let dk = due_key(row.next_due_ms, &pk);
                gd.insert(dk.as_slice(), [].as_slice()).map_err(db_err)?;
            }
            let mut cd = wtx.open_table(CHANNEL_OUTBOX_DUE).map_err(db_err)?;
            for row in &channel {
                let pk = Self::chan_outbox_key(&row.channel_id, &row.sender, &row.nonce);
                let dk = due_key(row.next_due_ms, &pk);
                cd.insert(dk.as_slice(), [].as_slice()).map_err(db_err)?;
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 在线一致备份（P0-2）：先 Immediate 提交一次把 `Durability::None` 的批量写落盘，
    /// 再持写锁拷贝整个 db 文件 → 已提交态的一致快照（redb 崩溃安全，拷贝可直接打开）。返回拷贝字节数。
    pub fn backup(&self, dest: impl AsRef<std::path::Path>) -> Result<u64> {
        {
            let wtx = self.db.begin_write().map_err(db_err)?;
            {
                let mut t = wtx.open_table(META).map_err(db_err)?;
                t.insert("last_backup_ms", now_ms().to_be_bytes().as_slice())
                    .map_err(db_err)?;
            }
            wtx.commit().map_err(db_err)?;
        }
        let guard = self.db.begin_write().map_err(db_err)?; // 持写锁防拷贝期间并发提交
        let n = std::fs::copy(&self.path, dest.as_ref()).map_err(db_err)?;
        drop(guard); // 未 commit → abort，不改动数据
        Ok(n)
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
    /// value = `recv_ms(8BE)++Gram`：按**入库时间**裁剪，不信发送方可伪造的 `timestamp_ms`（P0-1）。
    pub fn push_inbox(&self, receiver: &[u8], gram: &Gram) -> Result<()> {
        let key = inbox_key(receiver, gram.gram_id);
        let val = inbox_encode(now_ms(), gram);
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
                let val = inbox_encode(now_ms(), gram);
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
            if let Some((_recv, g)) = inbox_decode(v.value()) {
                out.push(g);
            }
        }
        Ok(out)
    }

    /// 分页读取某实体的离线消息（按 gram_id 升序），不删除。
    ///
    /// `after_gram_id` 为上一页最后一条的 `gram_id`；`None` 表示从该收件人的第一条开始。
    /// 这是大离线信箱的冷数据 range/page 入口，避免重连补投时把整个 inbox 一次性解码到内存。
    pub fn peek_inbox_page(
        &self,
        receiver: &[u8],
        after_gram_id: Option<u64>,
        limit: usize,
    ) -> Result<Vec<Gram>> {
        if limit == 0 || after_gram_id == Some(u64::MAX) {
            return Ok(Vec::new());
        }
        let (lo, hi) = inbox_prefix_range(receiver);
        let start = match after_gram_id {
            Some(id) => inbox_key(receiver, id.saturating_add(1)),
            None => lo,
        };
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(INBOX).map_err(db_err)?;
        let mut out = Vec::with_capacity(limit.min(1024));
        for item in t.range(start.as_slice()..hi.as_slice()).map_err(db_err)? {
            let (_k, v) = item.map_err(db_err)?;
            if let Some((_recv, g)) = inbox_decode(v.value()) {
                out.push(g);
                if out.len() >= limit {
                    break;
                }
            }
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
                if let Some((_recv, g)) = inbox_decode(v.value()) {
                    out.push(g);
                }
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

    fn outbox_row_from(k: &[u8], v: &[u8]) -> Option<OutboxRow> {
        if k.len() != 72 || v.len() < 20 {
            return None;
        }
        Some(OutboxRow {
            sender: k[0..32].to_vec(),
            receiver: k[32..64].to_vec(),
            gram_id: u64::from_be_bytes(k[64..72].try_into().ok()?),
            first_ms: u64::from_be_bytes(v[0..8].try_into().ok()?),
            retries: u32::from_be_bytes(v[8..12].try_into().ok()?),
            next_due_ms: u64::from_be_bytes(v[12..20].try_into().ok()?),
            gram_bytes: v[20..].to_vec(),
        })
    }

    fn outbox_next_due(v: &[u8]) -> Option<u64> {
        v.get(12..20)
            .and_then(|s| s.try_into().ok())
            .map(u64::from_be_bytes)
    }

    /// 存/覆盖一条待确认条目（Immediate 落盘，崩溃不丢）。
    #[allow(clippy::too_many_arguments)]
    pub fn outbox_put(
        &self,
        sender: &[u8],
        receiver: &[u8],
        gram_id: u64,
        first_ms: u64,
        retries: u32,
        next_due_ms: u64,
        gram_bytes: &[u8],
    ) -> Result<()> {
        let key = Self::outbox_key(sender, receiver, gram_id);
        let mut val = Vec::with_capacity(20 + gram_bytes.len());
        val.extend_from_slice(&first_ms.to_be_bytes());
        val.extend_from_slice(&retries.to_be_bytes());
        val.extend_from_slice(&next_due_ms.to_be_bytes());
        val.extend_from_slice(gram_bytes);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let old_due = {
            let mut tb = wtx.open_table(OUTBOX).map_err(db_err)?;
            let old_due = tb
                .get(key.as_slice())
                .map_err(db_err)?
                .and_then(|old| Self::outbox_next_due(old.value()));
            tb.insert(key.as_slice(), val.as_slice()).map_err(db_err)?;
            old_due
        };
        {
            let mut due = wtx.open_table(OUTBOX_DUE).map_err(db_err)?;
            if let Some(old) = old_due {
                due.remove(due_key(old, &key).as_slice()).map_err(db_err)?;
            }
            due.insert(due_key(next_due_ms, &key).as_slice(), [].as_slice())
                .map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 删一条待确认条目（收到 DirectAck / 达期限放弃）。幂等。
    pub fn outbox_del(&self, sender: &[u8], receiver: &[u8], gram_id: u64) -> Result<bool> {
        let key = Self::outbox_key(sender, receiver, gram_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let old_due;
        let existed;
        {
            let mut tb = wtx.open_table(OUTBOX).map_err(db_err)?;
            let old = tb
                .get(key.as_slice())
                .map_err(db_err)?
                .map(|v| v.value().to_vec());
            old_due = old.as_deref().and_then(Self::outbox_next_due);
            existed = tb.remove(key.as_slice()).map_err(db_err)?.is_some();
        }
        if let Some(old) = old_due {
            let mut due = wtx.open_table(OUTBOX_DUE).map_err(db_err)?;
            due.remove(due_key(old, &key).as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(existed)
    }

    /// 读一条 DM 待确认条目的 first_ms（入发件箱时刻），用于 ack 到达时算 send→ack 时延（P1-9）。只读。
    pub fn outbox_first_ms(
        &self,
        sender: &[u8],
        receiver: &[u8],
        gram_id: u64,
    ) -> Result<Option<u64>> {
        let key = Self::outbox_key(sender, receiver, gram_id);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let tb = rtx.open_table(OUTBOX).map_err(db_err)?;
        Ok(tb.get(key.as_slice()).map_err(db_err)?.and_then(|v| {
            v.value()
                .get(..8)
                .and_then(|s| s.try_into().ok())
                .map(u64::from_be_bytes)
        }))
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
            if let Some(row) = Self::outbox_row_from(k, v) {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// 只读取 `next_due_ms <= until_ms` 的待确认 DM 条目。到期索引让重传循环避开大量未来条目。
    pub fn outbox_due(&self, until_ms: u64) -> Result<Vec<OutboxRow>> {
        let (lo, hi) = due_range(until_ms);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let due = rtx.open_table(OUTBOX_DUE).map_err(db_err)?;
        let tb = rtx.open_table(OUTBOX).map_err(db_err)?;
        let mut out = Vec::new();
        for item in due.range(lo.as_slice()..hi.as_slice()).map_err(db_err)? {
            let (dk, _v) = item.map_err(db_err)?;
            let dk = dk.value();
            if dk.len() < 8 {
                continue;
            }
            let pk = &dk[8..];
            let Some(v) = tb.get(pk).map_err(db_err)? else {
                continue;
            };
            if let Some(row) = Self::outbox_row_from(pk, v.value()) {
                out.push(row);
            }
        }
        Ok(out)
    }

    // ── P0-3 死信：投递达期限放弃的消息落档（诊断/人工补投），按 first_ms 龄裁剪，不无界增长 ──

    fn dead_key(sender: &[u8], receiver: &[u8], gram_id: u64, first_ms: u64) -> Vec<u8> {
        let mut k = Vec::with_capacity(sender.len() + receiver.len() + 16);
        k.extend_from_slice(sender);
        k.extend_from_slice(receiver);
        k.extend_from_slice(&gram_id.to_be_bytes());
        k.extend_from_slice(&first_ms.to_be_bytes());
        k
    }
    /// 落一条死信（放弃投递时）。
    pub fn dead_letter_put(
        &self,
        sender: &[u8],
        receiver: &[u8],
        gram_id: u64,
        first_ms: u64,
        gram_bytes: &[u8],
    ) -> Result<()> {
        let key = Self::dead_key(sender, receiver, gram_id, first_ms);
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(DEAD_LETTER).map_err(db_err)?;
            t.insert(key.as_slice(), gram_bytes).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    /// 死信当前条数（admin/指标）。
    pub fn dead_letter_count(&self) -> Result<usize> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        Ok(rtx
            .open_table(DEAD_LETTER)
            .map_err(db_err)?
            .len()
            .map_err(db_err)? as usize)
    }
    /// 裁剪 first_ms < before_ms 的死信（key 尾 8 字节为 first_ms），返回删除条数。
    pub fn dead_letter_prune(&self, before_ms: u64) -> Result<usize> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        let mut n = 0usize;
        {
            let mut t = wtx.open_table(DEAD_LETTER).map_err(db_err)?;
            for item in t
                .extract_if(|k, _| {
                    k.len() >= 80 && u64::from_be_bytes(k[72..80].try_into().unwrap()) < before_ms
                })
                .map_err(db_err)?
            {
                item.map_err(db_err)?;
                n += 1;
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok(n)
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
        {
            let mut tb = wtx.open_table(DEDUP_DIRECT).map_err(db_err)?;
            tb.insert(key.as_slice(), now_ms.to_be_bytes().as_slice())
                .map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 清理早于 before_ms 的去重记录（按龄，对齐重传上限）。返回删除条数。
    pub fn dedup_prune(&self, before_ms: u64) -> Result<usize> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        let mut n = 0usize;
        {
            let mut tb = wtx.open_table(DEDUP_DIRECT).map_err(db_err)?;
            for item in tb
                .extract_if(|_, v| {
                    v.len() >= 8 && u64::from_be_bytes(v[0..8].try_into().unwrap()) < before_ms
                })
                .map_err(db_err)?
            {
                item.map_err(db_err)?;
                n += 1;
            }
        }
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

    fn group_outbox_encode(
        first_ms: u64,
        retries: u32,
        next_due_ms: u64,
        pending: &[Vec<u8>],
        gram_bytes: &[u8],
    ) -> Vec<u8> {
        let mut v = Vec::with_capacity(22 + pending.len() * 32 + gram_bytes.len());
        v.extend_from_slice(&first_ms.to_be_bytes());
        v.extend_from_slice(&retries.to_be_bytes());
        v.extend_from_slice(&next_due_ms.to_be_bytes());
        v.extend_from_slice(&(pending.len() as u16).to_be_bytes());
        for n in pending {
            v.extend_from_slice(n);
        } // 每个节点 id 定长 32
        v.extend_from_slice(gram_bytes);
        v
    }

    /// 解出 (first_ms, retries, next_due_ms, pending_nodes, gram_bytes)；格式不符返回 None。
    fn group_outbox_decode(v: &[u8]) -> Option<GroupOutboxDecoded> {
        if v.len() < 22 {
            return None;
        }
        let first_ms = u64::from_be_bytes(v[0..8].try_into().ok()?);
        let retries = u32::from_be_bytes(v[8..12].try_into().ok()?);
        let next_due_ms = u64::from_be_bytes(v[12..20].try_into().ok()?);
        let n = u16::from_be_bytes(v[20..22].try_into().ok()?) as usize;
        let nodes_end = 22 + n * 32;
        if v.len() < nodes_end {
            return None;
        }
        let mut pending = Vec::with_capacity(n);
        for i in 0..n {
            pending.push(v[22 + i * 32..22 + (i + 1) * 32].to_vec());
        }
        let gram_bytes = v[nodes_end..].to_vec();
        Some((first_ms, retries, next_due_ms, pending, gram_bytes))
    }

    fn group_outbox_row_from(k: &[u8], v: &[u8]) -> Option<GroupOutboxRow> {
        if k.len() != 72 {
            return None;
        }
        let (first_ms, retries, next_due_ms, pending_nodes, gram_bytes) =
            Self::group_outbox_decode(v)?;
        Some(GroupOutboxRow {
            gid: k[0..32].to_vec(),
            sender: k[32..64].to_vec(),
            gram_id: u64::from_be_bytes(k[64..72].try_into().ok()?),
            first_ms,
            retries,
            next_due_ms,
            pending_nodes,
            gram_bytes,
        })
    }

    /// 存/覆盖一条群待确认条目（Immediate 落盘）。`pending_nodes`=期望回 ack 的成员 home 节点集。
    #[allow(clippy::too_many_arguments)]
    pub fn group_outbox_put(
        &self,
        gid: &[u8],
        sender: &[u8],
        gram_id: u64,
        first_ms: u64,
        retries: u32,
        next_due_ms: u64,
        pending_nodes: &[Vec<u8>],
        gram_bytes: &[u8],
    ) -> Result<()> {
        let key = Self::group_outbox_key(gid, sender, gram_id);
        let val =
            Self::group_outbox_encode(first_ms, retries, next_due_ms, pending_nodes, gram_bytes);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let old_due = {
            let mut tb = wtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
            let old_due = tb.get(key.as_slice()).map_err(db_err)?.and_then(|old| {
                Self::group_outbox_decode(old.value()).map(|(_, _, due, _, _)| due)
            });
            tb.insert(key.as_slice(), val.as_slice()).map_err(db_err)?;
            old_due
        };
        {
            let mut due = wtx.open_table(GROUP_OUTBOX_DUE).map_err(db_err)?;
            if let Some(old) = old_due {
                due.remove(due_key(old, &key).as_slice()).map_err(db_err)?;
            }
            due.insert(due_key(next_due_ms, &key).as_slice(), [].as_slice())
                .map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 收到某成员节点的 GroupAck：从待 ack 集移除该 node；集合空则删条目。
    /// 返回 (found=条目存在, cleared=移除后已全清并删除)。幂等（node 不在集中也不报错）。
    pub fn group_outbox_ack_node(
        &self,
        gid: &[u8],
        sender: &[u8],
        gram_id: u64,
        node: &[u8],
    ) -> Result<(bool, bool)> {
        let key = Self::group_outbox_key(gid, sender, gram_id);
        // 读预检：ack 经火管到达全联邦，绝大多数节点本地无此条目（非发送方）。先只读点查，
        // 未命中直接返回——省掉一个写事务 fsync（高并发群 ack 风暴的主要写放大来源）。
        {
            let rtx = self.db.begin_read().map_err(db_err)?;
            let tb = rtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
            if tb.get(key.as_slice()).map_err(db_err)?.is_none() {
                return Ok((false, false));
            }
        }
        let wtx = self.db.begin_write().map_err(db_err)?;
        let mut found = false;
        let mut cleared = false;
        {
            let mut tb = wtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
            let existing = tb
                .get(key.as_slice())
                .map_err(db_err)?
                .map(|g| g.value().to_vec());
            if let Some(bytes) = existing {
                found = true;
                if let Some((first_ms, retries, next_due_ms, mut pending, gram_bytes)) =
                    Self::group_outbox_decode(&bytes)
                {
                    pending.retain(|n| n.as_slice() != node);
                    if pending.is_empty() {
                        tb.remove(key.as_slice()).map_err(db_err)?;
                        let mut due = wtx.open_table(GROUP_OUTBOX_DUE).map_err(db_err)?;
                        due.remove(due_key(next_due_ms, &key).as_slice())
                            .map_err(db_err)?;
                        cleared = true;
                    } else {
                        let val = Self::group_outbox_encode(
                            first_ms,
                            retries,
                            next_due_ms,
                            &pending,
                            &gram_bytes,
                        );
                        tb.insert(key.as_slice(), val.as_slice()).map_err(db_err)?;
                    }
                }
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok((found, cleared))
    }

    /// 批量销账：一个成员 home（`node`）在一窗口内确认的多条群消息 `entries=(gid,sender,gram_id)`。
    /// 两阶段：(1) 只读点查收集本节点确有的候选键——全未命中则零写事务返回（非发送方快路径）；
    /// (2) 一个写事务内对每个候选**重新 get+decode**（读新值，防并发）→ 从待 ack 集移除 `node`，空则删条目。
    /// 返回 (acked=真的移除了 node 的条目数, cleared=因此清空删除的条目数)。幂等；node 不在集中不计 acked。
    pub fn group_outbox_ack_batch(
        &self,
        node: &[u8],
        entries: &[(Vec<u8>, Vec<u8>, u64)],
    ) -> Result<(usize, usize)> {
        // 候选键去重（防御：累加器已按 (gid,sender,gram_id) 去重，这里再兜一层）。
        let mut keys: Vec<Vec<u8>> = Vec::with_capacity(entries.len());
        for (gid, sender, gram_id) in entries {
            let k = Self::group_outbox_key(gid, sender, *gram_id);
            if !keys.iter().any(|e| e == &k) {
                keys.push(k);
            }
        }
        if keys.is_empty() {
            return Ok((0, 0));
        }
        // 读阶段：筛出本地确有的键。全无 → 零写事务返回。
        let mut candidates: Vec<Vec<u8>> = Vec::new();
        {
            let rtx = self.db.begin_read().map_err(db_err)?;
            let tb = rtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
            for k in &keys {
                if tb.get(k.as_slice()).map_err(db_err)?.is_some() {
                    candidates.push(k.clone());
                }
            }
        }
        if candidates.is_empty() {
            return Ok((0, 0));
        }
        // 写阶段：仅处理候选，事务内重新读值再改。
        let mut acked = 0usize;
        let mut cleared = 0usize;
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut tb = wtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
            for k in &candidates {
                let existing = tb
                    .get(k.as_slice())
                    .map_err(db_err)?
                    .map(|g| g.value().to_vec());
                let Some(bytes) = existing else { continue }; // 并发删 → 跳过
                let Some((first_ms, retries, next_due_ms, mut pending, gram_bytes)) =
                    Self::group_outbox_decode(&bytes)
                else {
                    continue;
                };
                let before = pending.len();
                pending.retain(|n| n.as_slice() != node);
                if pending.len() == before {
                    continue;
                } // node 不在集中 → 不计 acked（幂等）
                acked += 1;
                if pending.is_empty() {
                    tb.remove(k.as_slice()).map_err(db_err)?;
                    let mut due = wtx.open_table(GROUP_OUTBOX_DUE).map_err(db_err)?;
                    due.remove(due_key(next_due_ms, k).as_slice())
                        .map_err(db_err)?;
                    cleared += 1;
                } else {
                    let val = Self::group_outbox_encode(
                        first_ms,
                        retries,
                        next_due_ms,
                        &pending,
                        &gram_bytes,
                    );
                    tb.insert(k.as_slice(), val.as_slice()).map_err(db_err)?;
                }
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok((acked, cleared))
    }

    /// 读一条群待确认条目的 first_ms（建发件箱时刻），用于末个 GroupAck 清条目时算扇出 send→last-ack 时延（P1-9）。只读。
    pub fn group_outbox_first_ms(
        &self,
        gid: &[u8],
        sender: &[u8],
        gram_id: u64,
    ) -> Result<Option<u64>> {
        let key = Self::group_outbox_key(gid, sender, gram_id);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let tb = rtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
        Ok(tb
            .get(key.as_slice())
            .map_err(db_err)?
            .and_then(|v| Self::group_outbox_decode(v.value()).map(|(first_ms, ..)| first_ms)))
    }

    /// 删一条群待确认条目（达期限放弃）。幂等。
    pub fn group_outbox_del(&self, gid: &[u8], sender: &[u8], gram_id: u64) -> Result<bool> {
        let key = Self::group_outbox_key(gid, sender, gram_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let old_due;
        let existed;
        {
            let mut tb = wtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
            let old = tb
                .get(key.as_slice())
                .map_err(db_err)?
                .map(|v| v.value().to_vec());
            old_due = old
                .as_deref()
                .and_then(Self::group_outbox_decode)
                .map(|(_, _, due, _, _)| due);
            existed = tb.remove(key.as_slice()).map_err(db_err)?.is_some();
        }
        if let Some(old) = old_due {
            let mut due = wtx.open_table(GROUP_OUTBOX_DUE).map_err(db_err)?;
            due.remove(due_key(old, &key).as_slice()).map_err(db_err)?;
        }
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
            if let Some(row) = Self::group_outbox_row_from(k, v.value()) {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// 只读取 `next_due_ms <= until_ms` 的群待确认条目。
    pub fn group_outbox_due(&self, until_ms: u64) -> Result<Vec<GroupOutboxRow>> {
        let (lo, hi) = due_range(until_ms);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let due = rtx.open_table(GROUP_OUTBOX_DUE).map_err(db_err)?;
        let tb = rtx.open_table(GROUP_OUTBOX).map_err(db_err)?;
        let mut out = Vec::new();
        for item in due.range(lo.as_slice()..hi.as_slice()).map_err(db_err)? {
            let (dk, _v) = item.map_err(db_err)?;
            let dk = dk.value();
            if dk.len() < 8 {
                continue;
            }
            let pk = &dk[8..];
            let Some(v) = tb.get(pk).map_err(db_err)? else {
                continue;
            };
            if let Some(row) = Self::group_outbox_row_from(pk, v.value()) {
                out.push(row);
            }
        }
        Ok(out)
    }

    // ── 频道（P4）：元信息 / 账号订阅 / 全局日志 / seq 计数 / 私钥 / 定序幂等 / 发件箱 ──

    /// 存/覆盖频道元信息。
    pub fn put_channel(&self, c: &Channel) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(CHANNELS).map_err(db_err)?;
            t.insert(c.channel_id.as_slice(), c.encode_to_vec().as_slice())
                .map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    /// 读单个频道元信息。
    pub fn channel(&self, channel_id: &[u8]) -> Result<Option<Channel>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNELS).map_err(db_err)?;
        match t.get(channel_id).map_err(db_err)? {
            Some(v) => Ok(Some(
                Channel::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }
    /// 全部已知频道（启动回填目录 + rejoin 主题用）。
    pub fn all_channels(&self) -> Result<Vec<Channel>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNELS).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (_k, v) = item.map_err(db_err)?;
            if let Ok(c) = Channel::decode(v.value()) {
                out.push(c);
            }
        }
        Ok(out)
    }

    fn chan_sub_key(account: &[u8], channel_id: &[u8]) -> Vec<u8> {
        let mut k = Vec::with_capacity(account.len() + channel_id.len());
        k.extend_from_slice(account);
        k.extend_from_slice(channel_id);
        k
    }
    /// 记一条账号级订阅（account 订阅 channel_id）。幂等覆盖。
    pub fn channel_sub_put(&self, account: &[u8], channel_id: &[u8], ts_ms: u64) -> Result<()> {
        let key = Self::chan_sub_key(account, channel_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(CHANNEL_SUBS).map_err(db_err)?;
            t.insert(key.as_slice(), ts_ms.to_be_bytes().as_slice())
                .map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    /// 删一条账号级订阅（unsub）。幂等。
    pub fn channel_sub_del(&self, account: &[u8], channel_id: &[u8]) -> Result<bool> {
        let key = Self::chan_sub_key(account, channel_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let existed;
        {
            let mut t = wtx.open_table(CHANNEL_SUBS).map_err(db_err)?;
            existed = t.remove(key.as_slice()).map_err(db_err)?.is_some();
        }
        wtx.commit().map_err(db_err)?;
        Ok(existed)
    }
    /// 某账号订阅的全部频道 id（前缀区间扫）。
    pub fn channel_subs_of(&self, account: &[u8]) -> Result<Vec<Vec<u8>>> {
        let (lo, hi) = prefix_range(account);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNEL_SUBS).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.range(lo.as_slice()..hi.as_slice()).map_err(db_err)? {
            let (k, _v) = item.map_err(db_err)?;
            let k = k.value();
            if k.len() == account.len() + 32 {
                out.push(k[account.len()..].to_vec());
            }
        }
        Ok(out)
    }
    /// 全部订阅去重后的频道 id（启动时据此 rejoin 主题，无论哪个本地账号订阅）。
    pub fn channel_subs_all_ids(&self) -> Result<Vec<Vec<u8>>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNEL_SUBS).map_err(db_err)?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (k, _v) = item.map_err(db_err)?;
            let k = k.value();
            if k.len() >= 32 {
                let cid = k[k.len() - 32..].to_vec();
                if seen.insert(cid.clone()) {
                    out.push(cid);
                }
            }
        }
        Ok(out)
    }

    fn chan_log_key(channel_id: &[u8], seq: u64) -> Vec<u8> {
        let mut k = Vec::with_capacity(channel_id.len() + 8);
        k.extend_from_slice(channel_id);
        k.extend_from_slice(&seq.to_be_bytes());
        k
    }
    /// 写一条频道日志消息（key = cid++seq）。`(cid,seq)` 唯一 → 覆盖即幂等。
    pub fn channel_log_put(&self, m: &ChannelMsg) -> Result<()> {
        let key = Self::chan_log_key(&m.channel_id, m.seq);
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(CHANNEL_LOG).map_err(db_err)?;
            t.insert(key.as_slice(), m.encode_to_vec().as_slice())
                .map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    /// 该 (cid,seq) 是否已存（点查去重，O(log n)）。
    pub fn channel_log_has(&self, channel_id: &[u8], seq: u64) -> Result<bool> {
        let key = Self::chan_log_key(channel_id, seq);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNEL_LOG).map_err(db_err)?;
        Ok(t.get(key.as_slice()).map_err(db_err)?.is_some())
    }
    /// 回填：取 seq 在 (since_seq, ∞) 的消息，最多 limit 条（按 seq 升序）。
    pub fn channel_log_since(
        &self,
        channel_id: &[u8],
        since_seq: u64,
        limit: usize,
    ) -> Result<Vec<ChannelMsg>> {
        let lo = Self::chan_log_key(channel_id, since_seq.saturating_add(1));
        let (_p, hi) = prefix_range(channel_id);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNEL_LOG).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.range(lo.as_slice()..hi.as_slice()).map_err(db_err)? {
            let (_k, v) = item.map_err(db_err)?;
            if let Ok(m) = ChannelMsg::decode(v.value()) {
                out.push(m);
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }
    /// 该频道当前日志的最小/最大 seq（earliest/max 水位；日志空返回 None）。
    pub fn channel_log_bounds(&self, channel_id: &[u8]) -> Result<Option<(u64, u64)>> {
        let (lo, hi) = prefix_range(channel_id);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNEL_LOG).map_err(db_err)?;
        let mut rng = t.range(lo.as_slice()..hi.as_slice()).map_err(db_err)?;
        let first = match rng.next() {
            Some(it) => it.map_err(db_err)?.0.value().to_vec(),
            None => return Ok(None),
        };
        let last = match rng.next_back() {
            Some(it) => it.map_err(db_err)?.0.value().to_vec(),
            None => first.clone(),
        };
        let seq_of = |k: &[u8]| u64::from_be_bytes(k[k.len() - 8..].try_into().unwrap());
        Ok(Some((seq_of(&first), seq_of(&last))))
    }
    /// 从 floor+1 起连续无断层的最高 seq（回填游标）：扫 (floor, ∞)，遇第一个断层即止。
    /// 例：floor=0、日志有 {1,2,5,6} → 返回 2（缺 3 → 从 2 之后回填）。空/全缺 → floor。
    pub fn channel_log_contiguous_upto(&self, channel_id: &[u8], floor: u64) -> Result<u64> {
        let lo = Self::chan_log_key(channel_id, floor.saturating_add(1));
        let (_p, hi) = prefix_range(channel_id);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNEL_LOG).map_err(db_err)?;
        let mut contiguous = floor;
        for item in t.range(lo.as_slice()..hi.as_slice()).map_err(db_err)? {
            let (k, _v) = item.map_err(db_err)?;
            let k = k.value();
            let seq = u64::from_be_bytes(k[k.len() - 8..].try_into().unwrap());
            if seq == contiguous + 1 {
                contiguous = seq;
            } else {
                break; // 断层
            }
        }
        Ok(contiguous)
    }
    /// 裁剪频道日志：删 seq < floor_seq 的条目。返回删除条数（配合保留策略按龄/条数算 floor）。
    pub fn channel_log_prune(&self, channel_id: &[u8], floor_seq: u64) -> Result<usize> {
        let lo = Self::chan_log_key(channel_id, u64::MIN);
        let hi = Self::chan_log_key(channel_id, floor_seq); // 半开上界：删 [0, floor_seq)
        let wtx = self.db.begin_write().map_err(db_err)?;
        let mut n = 0usize;
        {
            let mut t = wtx.open_table(CHANNEL_LOG).map_err(db_err)?;
            for item in t
                .extract_from_if(lo.as_slice()..hi.as_slice(), |_, _| true)
                .map_err(db_err)?
            {
                item.map_err(db_err)?;
                n += 1;
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok(n)
    }

    /// home 分配下一个 seq（单事务 RMW：读 → +1 回写 → 返回分配值）。永不依赖日志推算，防裁剪后复用。
    pub fn channel_seq_next(&self, channel_id: &[u8]) -> Result<u64> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        let assigned;
        {
            let mut t = wtx.open_table(CHANNEL_SEQ).map_err(db_err)?;
            let cur = t
                .get(channel_id)
                .map_err(db_err)?
                .map(|v| u64::from_be_bytes(v.value().try_into().unwrap_or([0u8; 8])))
                .unwrap_or(1); // 从 1 起
            assigned = cur;
            t.insert(channel_id, (cur + 1).to_be_bytes().as_slice())
                .map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(assigned)
    }
    /// 读当前 seq 高水位（下一个待分配值；未初始化返回 1）。
    pub fn channel_seq_peek(&self, channel_id: &[u8]) -> Result<u64> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNEL_SEQ).map_err(db_err)?;
        Ok(t.get(channel_id)
            .map_err(db_err)?
            .map(|v| u64::from_be_bytes(v.value().try_into().unwrap_or([0u8; 8])))
            .unwrap_or(1))
    }

    /// 存频道私钥（仅 home）。
    pub fn put_channel_secret(&self, channel_id: &[u8], secret: &[u8]) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(CHANNEL_SECRETS).map_err(db_err)?;
            t.insert(channel_id, secret).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    /// 读频道私钥（home 判定：Some → 本节点是该频道定序器）。
    pub fn channel_secret(&self, channel_id: &[u8]) -> Result<Option<Vec<u8>>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNEL_SECRETS).map_err(db_err)?;
        Ok(t.get(channel_id)
            .map_err(db_err)?
            .map(|v| v.value().to_vec()))
    }

    fn chan_intake_key(channel_id: &[u8], sender: &[u8], nonce: &[u8]) -> Vec<u8> {
        let mut k = Vec::with_capacity(channel_id.len() + sender.len() + nonce.len());
        k.extend_from_slice(channel_id);
        k.extend_from_slice(sender);
        k.extend_from_slice(nonce);
        k
    }
    /// 定序幂等：若 (cid,sender,nonce) 已分配过 seq 返回 Some(seq)，否则记入 now_ms 占位并返回 None。
    /// 注意：真正的 seq 在 home 单事务里与 LOG/SEQ 一起写；此表仅记「nonce→seq」供重传命中重广播。
    pub fn channel_intake_get(
        &self,
        channel_id: &[u8],
        sender: &[u8],
        nonce: &[u8],
    ) -> Result<Option<u64>> {
        let key = Self::chan_intake_key(channel_id, sender, nonce);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNEL_INTAKE).map_err(db_err)?;
        Ok(t.get(key.as_slice())
            .map_err(db_err)?
            .map(|v| u64::from_be_bytes(v.value()[..8].try_into().unwrap_or([0u8; 8]))))
    }
    /// 定序提交（home 专用，单事务三写之一不便合并时的独立入口；正常由 channel_sequence 原子完成）。
    pub fn channel_intake_put(
        &self,
        channel_id: &[u8],
        sender: &[u8],
        nonce: &[u8],
        seq: u64,
    ) -> Result<()> {
        let key = Self::chan_intake_key(channel_id, sender, nonce);
        let mut iv = Vec::with_capacity(16);
        iv.extend_from_slice(&seq.to_be_bytes());
        iv.extend_from_slice(&now_ms().to_be_bytes());
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(CHANNEL_INTAKE).map_err(db_err)?;
            t.insert(key.as_slice(), iv.as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 裁剪定序幂等表（P0-1，防无界增长）：删 ts < before_ms 的行。返回删除条数。
    /// `before_ms` 须 ≤ now − 发布者 outbox horizon(24h)，否则可能裁掉仍会被重传命中的 nonce→seq 映射 → 重复定序。
    pub fn channel_intake_prune(&self, before_ms: u64) -> Result<usize> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        let mut n = 0usize;
        {
            let mut t = wtx.open_table(CHANNEL_INTAKE).map_err(db_err)?;
            for item in t
                .extract_if(|_, v| {
                    v.len() >= 16 && u64::from_be_bytes(v[8..16].try_into().unwrap()) < before_ms
                })
                .map_err(db_err)?
            {
                item.map_err(db_err)?;
                n += 1;
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok(n)
    }

    /// home 定序核心：**单事务原子**完成「分配 seq + 写日志 + 记 intake」。崩溃安全的关键。
    /// 若 (cid,sender,nonce) 已定序过（重传/重复 PubReq）→ 不重分配，返回已有 (seq, 已存的 ChannelMsg)。
    /// 否则分配新 seq，用 `make_msg(seq)` 构造并签名 ChannelMsg，持久化，返回。调用方 commit 后才广播。
    pub fn channel_sequence(
        &self,
        channel_id: &[u8],
        sender: &[u8],
        nonce: &[u8],
        make_msg: impl FnOnce(u64) -> ChannelMsg,
    ) -> Result<(u64, ChannelMsg, bool)> {
        let ik = Self::chan_intake_key(channel_id, sender, nonce);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let result;
        {
            let mut intake = wtx.open_table(CHANNEL_INTAKE).map_err(db_err)?;
            if let Some(v) = intake.get(ik.as_slice()).map_err(db_err)? {
                // 已定序 → 重传命中：取回已存日志条目重广播，不重分配。
                let seq = u64::from_be_bytes(v.value()[..8].try_into().unwrap_or([0u8; 8]));
                drop(v);
                let lk = Self::chan_log_key(channel_id, seq);
                let log = wtx.open_table(CHANNEL_LOG).map_err(db_err)?;
                let msg = match log.get(lk.as_slice()).map_err(db_err)? {
                    Some(mv) => ChannelMsg::decode(mv.value())
                        .map_err(|e| StoreError::Decode(e.to_string()))?,
                    None => return Err(StoreError::Db("intake without log entry".into())),
                };
                result = (seq, msg, false);
            } else {
                // 首见 → 分配 seq、构造签名消息、三写（SEQ+LOG+INTAKE）同一事务。
                let mut seqt = wtx.open_table(CHANNEL_SEQ).map_err(db_err)?;
                let seq = seqt
                    .get(channel_id)
                    .map_err(db_err)?
                    .map(|v| u64::from_be_bytes(v.value().try_into().unwrap_or([0u8; 8])))
                    .unwrap_or(1);
                seqt.insert(channel_id, (seq + 1).to_be_bytes().as_slice())
                    .map_err(db_err)?;
                let msg = make_msg(seq);
                let lk = Self::chan_log_key(channel_id, seq);
                {
                    let mut log = wtx.open_table(CHANNEL_LOG).map_err(db_err)?;
                    log.insert(lk.as_slice(), msg.encode_to_vec().as_slice())
                        .map_err(db_err)?;
                }
                let mut iv = Vec::with_capacity(16); // seq(8)++ts(8)：ts 供 channel_intake_prune 按龄裁剪
                iv.extend_from_slice(&seq.to_be_bytes());
                iv.extend_from_slice(&now_ms().to_be_bytes());
                intake
                    .insert(ik.as_slice(), iv.as_slice())
                    .map_err(db_err)?;
                result = (seq, msg, true);
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok(result)
    }

    // ── 频道发布发件箱（publisher 侧，仿 OUTBOX）──
    fn chan_outbox_key(channel_id: &[u8], sender: &[u8], nonce: &[u8]) -> Vec<u8> {
        Self::chan_intake_key(channel_id, sender, nonce)
    }

    fn channel_outbox_row_from(k: &[u8], v: &[u8]) -> Option<ChannelOutboxRow> {
        if k.len() < 64 || v.len() < 20 {
            return None;
        }
        Some(ChannelOutboxRow {
            channel_id: k[0..32].to_vec(),
            sender: k[32..64].to_vec(),
            nonce: k[64..].to_vec(),
            first_ms: u64::from_be_bytes(v[0..8].try_into().ok()?),
            retries: u32::from_be_bytes(v[8..12].try_into().ok()?),
            next_due_ms: u64::from_be_bytes(v[12..20].try_into().ok()?),
            pubreq_bytes: v[20..].to_vec(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn channel_outbox_put(
        &self,
        channel_id: &[u8],
        sender: &[u8],
        nonce: &[u8],
        first_ms: u64,
        retries: u32,
        next_due_ms: u64,
        pubreq_bytes: &[u8],
    ) -> Result<()> {
        let key = Self::chan_outbox_key(channel_id, sender, nonce);
        let mut val = Vec::with_capacity(20 + pubreq_bytes.len());
        val.extend_from_slice(&first_ms.to_be_bytes());
        val.extend_from_slice(&retries.to_be_bytes());
        val.extend_from_slice(&next_due_ms.to_be_bytes());
        val.extend_from_slice(pubreq_bytes);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let old_due = {
            let mut t = wtx.open_table(CHANNEL_OUTBOX).map_err(db_err)?;
            let old_due = t
                .get(key.as_slice())
                .map_err(db_err)?
                .and_then(|old| Self::outbox_next_due(old.value()));
            t.insert(key.as_slice(), val.as_slice()).map_err(db_err)?;
            old_due
        };
        {
            let mut due = wtx.open_table(CHANNEL_OUTBOX_DUE).map_err(db_err)?;
            if let Some(old) = old_due {
                due.remove(due_key(old, &key).as_slice()).map_err(db_err)?;
            }
            due.insert(due_key(next_due_ms, &key).as_slice(), [].as_slice())
                .map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    pub fn channel_outbox_del(
        &self,
        channel_id: &[u8],
        sender: &[u8],
        nonce: &[u8],
    ) -> Result<bool> {
        let key = Self::chan_outbox_key(channel_id, sender, nonce);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let old_due;
        let existed;
        {
            let mut t = wtx.open_table(CHANNEL_OUTBOX).map_err(db_err)?;
            let old = t
                .get(key.as_slice())
                .map_err(db_err)?
                .map(|v| v.value().to_vec());
            old_due = old.as_deref().and_then(Self::outbox_next_due);
            existed = t.remove(key.as_slice()).map_err(db_err)?.is_some();
        }
        if let Some(old) = old_due {
            let mut due = wtx.open_table(CHANNEL_OUTBOX_DUE).map_err(db_err)?;
            due.remove(due_key(old, &key).as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(existed)
    }
    pub fn channel_outbox_all(&self) -> Result<Vec<ChannelOutboxRow>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(CHANNEL_OUTBOX).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (k, v) = item.map_err(db_err)?;
            let k = k.value();
            let v = v.value();
            if let Some(row) = Self::channel_outbox_row_from(k, v) {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// 只读取 `next_due_ms <= until_ms` 的频道发布待确认条目。
    pub fn channel_outbox_due(&self, until_ms: u64) -> Result<Vec<ChannelOutboxRow>> {
        let (lo, hi) = due_range(until_ms);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let due = rtx.open_table(CHANNEL_OUTBOX_DUE).map_err(db_err)?;
        let tb = rtx.open_table(CHANNEL_OUTBOX).map_err(db_err)?;
        let mut out = Vec::new();
        for item in due.range(lo.as_slice()..hi.as_slice()).map_err(db_err)? {
            let (dk, _v) = item.map_err(db_err)?;
            let dk = dk.value();
            if dk.len() < 8 {
                continue;
            }
            let pk = &dk[8..];
            let Some(v) = tb.get(pk).map_err(db_err)? else {
                continue;
            };
            if let Some(row) = Self::channel_outbox_row_from(pk, v.value()) {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// 某实体离线消息条数。range 直接计数，不解码 value（避免 O(n) prost 解码）。
    pub fn inbox_len(&self, receiver: &[u8]) -> Result<usize> {
        let (lo, hi) = inbox_prefix_range(receiver);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(INBOX).map_err(db_err)?;
        Ok(t.range(lo.as_slice()..hi.as_slice())
            .map_err(db_err)?
            .count())
    }

    /// 删除所有收件人中**入库时间**（recv_ms）早于 `before_ms` 的离线消息（P0-1：不信发送方可伪造的时钟），返回删除条数。
    pub fn prune_inbox(&self, before_ms: u64) -> Result<usize> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        let mut n = 0usize;
        {
            let mut t = wtx.open_table(INBOX).map_err(db_err)?;
            for item in t
                .extract_if(|_, v| {
                    inbox_decode(v)
                        .map(|(recv, _)| recv < before_ms)
                        .unwrap_or(true)
                })
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
            t.insert(cert.device.as_slice(), val.as_slice())
                .map_err(db_err)?;
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
            t.insert(r.device.as_slice(), val.as_slice())
                .map_err(db_err)?;
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
            out.push(
                DeviceRevoke::decode(v.value()).map_err(|e| StoreError::Decode(e.to_string()))?,
            );
        }
        Ok(out)
    }

    // ---- 目录持久化 ----

    pub fn put_entity(&self, entity: &Entity) -> Result<()> {
        let val = entity.encode_to_vec();
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(ENTITIES).map_err(db_err)?;
            t.insert(entity.entity_id.as_slice(), val.as_slice())
                .map_err(db_err)?;
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
    /// 同时在 BLOB_META 记**首次**入库时间（内容寻址幂等，重复 put 不刷新龄）供按龄 GC。
    pub fn put_blob(&self, blob: &BlobData) -> Result<()> {
        let val = blob.encode_to_vec();
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(BLOBS).map_err(db_err)?;
            t.insert(blob.hash.as_slice(), val.as_slice())
                .map_err(db_err)?;
            let mut m = wtx.open_table(BLOB_META).map_err(db_err)?;
            if m.get(blob.hash.as_slice()).map_err(db_err)?.is_none() {
                m.insert(blob.hash.as_slice(), now_ms().to_be_bytes().as_slice())
                    .map_err(db_err)?;
            }
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

    /// 按入库龄裁剪内容寻址 blob（P0-1）：删 BLOB_META 中 put_ms < before_ms 的 hash 及其 BLOBS 主体。返回删除条数。
    /// blob 为内容寻址媒体缓存，误删仍被引用者可按 hash 重新上传/拉取，故按龄 GC 安全。
    pub fn prune_blobs(&self, before_ms: u64) -> Result<usize> {
        let expired: Vec<Vec<u8>> = {
            let rtx = self.db.begin_read().map_err(db_err)?;
            let m = rtx.open_table(BLOB_META).map_err(db_err)?;
            let mut v = Vec::new();
            for it in m.iter().map_err(db_err)? {
                let (k, val) = it.map_err(db_err)?;
                let ts = u64::from_be_bytes(val.value().try_into().unwrap_or([0u8; 8]));
                if ts < before_ms {
                    v.push(k.value().to_vec());
                }
            }
            v
        };
        if expired.is_empty() {
            return Ok(0);
        }
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut b = wtx.open_table(BLOBS).map_err(db_err)?;
            let mut m = wtx.open_table(BLOB_META).map_err(db_err)?;
            for h in &expired {
                b.remove(h.as_slice()).map_err(db_err)?;
                m.remove(h.as_slice()).map_err(db_err)?;
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok(expired.len())
    }

    // ---- 群组持久化 ----

    pub fn put_group(&self, g: &Group) -> Result<()> {
        let val = g.encode_to_vec();
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(GROUPS).map_err(db_err)?;
            t.insert(g.group_id.as_slice(), val.as_slice())
                .map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 删除一个群（解散）。同时清本节点为该群 home 时持有的签名私钥（P0-1：防 GROUP_SECRETS 泄漏）。
    pub fn del_group(&self, group_id: &[u8]) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(GROUPS).map_err(db_err)?;
            t.remove(group_id).map_err(db_err)?;
            let mut gs = wtx.open_table(GROUP_SECRETS).map_err(db_err)?;
            gs.remove(group_id).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// 单独删群签名私钥（幂等）。
    pub fn del_group_secret(&self, group_id: &[u8]) -> Result<bool> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        let existed;
        {
            let mut t = wtx.open_table(GROUP_SECRETS).map_err(db_err)?;
            existed = t.remove(group_id).map_err(db_err)?.is_some();
        }
        wtx.commit().map_err(db_err)?;
        Ok(existed)
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
    /// P1-8：存/覆盖一个管理操作员凭据（value=JSON 串，含 Argon2 PHC；镜像 [`Self::put_name_secret`]）。
    pub fn put_operator(&self, name: &str, rec_json: &str) -> Result<()> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(OPERATORS).map_err(db_err)?;
            t.insert(name.as_bytes(), rec_json.as_bytes()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }
    pub fn operator(&self, name: &str) -> Result<Option<String>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(OPERATORS).map_err(db_err)?;
        match t.get(name.as_bytes()).map_err(db_err)? {
            Some(v) => Ok(Some(String::from_utf8_lossy(v.value()).into_owned())),
            None => Ok(None),
        }
    }
    pub fn del_operator(&self, name: &str) -> Result<bool> {
        let wtx = self.db.begin_write().map_err(db_err)?;
        let existed;
        {
            let mut t = wtx.open_table(OPERATORS).map_err(db_err)?;
            existed = t.remove(name.as_bytes()).map_err(db_err)?.is_some();
        }
        wtx.commit().map_err(db_err)?;
        Ok(existed)
    }
    /// 全部操作员 (name, rec_json)。供启动回填/管理台列表（调用方剥离 phc 再外显）。
    pub fn all_operators(&self) -> Result<Vec<(String, String)>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(OPERATORS).map_err(db_err)?;
        let mut v = Vec::new();
        for it in t.iter().map_err(db_err)? {
            let (k, val) = it.map_err(db_err)?;
            v.push((
                String::from_utf8_lossy(k.value()).into_owned(),
                String::from_utf8_lossy(val.value()).into_owned(),
            ));
        }
        Ok(v)
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
            t.insert(domain.as_bytes(), b"1".as_slice())
                .map_err(db_err)?;
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
        key.extend_from_slice(account);
        key.extend_from_slice(entry_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(ROSTER).map_err(db_err)?;
            t.insert(key.as_slice(), json.as_bytes()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// Remove one roster entry (idempotent).
    pub fn del_roster(&self, account: &[u8], entry_id: &[u8]) -> Result<()> {
        let mut key = Vec::with_capacity(account.len() + entry_id.len());
        key.extend_from_slice(account);
        key.extend_from_slice(entry_id);
        let wtx = self.db.begin_write().map_err(db_err)?;
        {
            let mut t = wtx.open_table(ROSTER).map_err(db_err)?;
            t.remove(key.as_slice()).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(())
    }

    /// Return all roster entries for an account as JSON strings.
    pub fn get_roster_all(&self, account: &[u8]) -> Result<Vec<String>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(ROSTER).map_err(db_err)?;
        let lo = account.to_vec();
        let mut hi = lo.clone();
        for i in (0..hi.len()).rev() {
            if hi[i] < 0xFF {
                hi[i] += 1;
                break;
            } else {
                hi[i] = 0;
            }
        }
        let mut out = Vec::new();
        for item in t
            .range::<&[u8]>(lo.as_slice()..hi.as_slice())
            .map_err(db_err)?
        {
            let (_, v) = item.map_err(db_err)?;
            out.push(String::from_utf8_lossy(v.value()).into_owned());
        }
        Ok(out)
    }

    // ── P1-6 多设备已读状态（key = account(32)++conversation(32)，value = last_read_ms(8BE)，单调）──

    fn read_state_key(account: &[u8], conv: &[u8]) -> Vec<u8> {
        let mut k = Vec::with_capacity(account.len() + conv.len());
        k.extend_from_slice(account);
        k.extend_from_slice(conv);
        k
    }

    /// 单调推进某会话已读位（单事务 RMW）：仅当 `ms` > 现值才写。返回是否确有推进（上层据此决定是否扇出同步）。
    pub fn read_state_set(&self, account: &[u8], conv: &[u8], ms: u64) -> Result<bool> {
        let key = Self::read_state_key(account, conv);
        let wtx = self.db.begin_write().map_err(db_err)?;
        let advanced;
        {
            let mut t = wtx.open_table(READ_STATE).map_err(db_err)?;
            let cur = t
                .get(key.as_slice())
                .map_err(db_err)?
                .and_then(|v| {
                    v.value()
                        .get(..8)
                        .and_then(|s| s.try_into().ok())
                        .map(u64::from_be_bytes)
                })
                .unwrap_or(0);
            if ms > cur {
                t.insert(key.as_slice(), ms.to_be_bytes().as_slice())
                    .map_err(db_err)?;
                advanced = true;
            } else {
                advanced = false;
            }
        }
        wtx.commit().map_err(db_err)?;
        Ok(advanced)
    }

    /// 某账号全部已读位（前缀扫）：返回 (conversation(32), last_read_ms)。供客户端登录追平。
    pub fn read_state_all(&self, account: &[u8]) -> Result<Vec<(Vec<u8>, u64)>> {
        let (lo, hi) = prefix_range(account);
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(READ_STATE).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.range(lo.as_slice()..hi.as_slice()).map_err(db_err)? {
            let (k, v) = item.map_err(db_err)?;
            let k = k.value();
            if k.len() == account.len() + 32 {
                let conv = k[account.len()..].to_vec();
                let ms = u64::from_be_bytes(
                    v.value()
                        .get(..8)
                        .and_then(|s| s.try_into().ok())
                        .unwrap_or([0u8; 8]),
                );
                out.push((conv, ms));
            }
        }
        Ok(out)
    }
}

/// INBOX value 编解码（P0-1 入库时间前缀）：`recv_ms(8BE) ++ prost(Gram)`。
fn inbox_encode(recv_ms: u64, gram: &Gram) -> Vec<u8> {
    let raw = gram.encode_to_vec();
    let mut v = Vec::with_capacity(8 + raw.len());
    v.extend_from_slice(&recv_ms.to_be_bytes());
    v.extend_from_slice(&raw);
    v
}

/// 解 INBOX value → (入库时间 recv_ms, Gram)；长度不足/解码失败返回 None（损坏行按"可裁剪"处理）。
fn inbox_decode(v: &[u8]) -> Option<(u64, Gram)> {
    if v.len() < 8 {
        return None;
    }
    let recv = u64::from_be_bytes(v[..8].try_into().ok()?);
    let g = Gram::decode(&v[8..]).ok()?;
    Some((recv, g))
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

/// 任意前缀的半开区间 `[prefix, prefix+1)`：lo=prefix 本身，hi=把 prefix 当大整数 +1。
/// 供「prefix ++ 定长后缀」的前缀扫（频道订阅 account++cid、频道日志 cid++seq）。
fn prefix_range(prefix: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let lo = prefix.to_vec();
    let mut hi = prefix.to_vec();
    for i in (0..hi.len()).rev() {
        if hi[i] == 0xFF {
            hi[i] = 0;
        } else {
            hi[i] += 1;
            return (lo, hi);
        }
    }
    // 前缀全 0xFF：上界加一字节兜底。
    let mut hi = prefix.to_vec();
    hi.push(0xFF);
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nm_proto::{GramKind, now_ms};

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
    fn inbox_page_reads_by_receiver_and_gram_id() {
        let dir = tempfile::tempdir().unwrap();
        let s = RedbStore::open(dir.path().join("t.redb")).unwrap();
        let r = [7u8; 32];
        let other = [8u8; 32];
        s.push_inbox(&r, &msg(r, 2, "second")).unwrap();
        s.push_inbox(&r, &msg(r, 1, "first")).unwrap();
        s.push_inbox(&r, &msg(r, 3, "third")).unwrap();
        s.push_inbox(&other, &msg(other, 1, "other")).unwrap();

        let page1 = s.peek_inbox_page(&r, None, 2).unwrap();
        assert_eq!(
            page1.iter().map(|g| g.gram_id).collect::<Vec<_>>(),
            vec![1, 2]
        );

        let page2 = s
            .peek_inbox_page(&r, page1.last().map(|g| g.gram_id), 2)
            .unwrap();
        assert_eq!(page2.iter().map(|g| g.gram_id).collect::<Vec<_>>(), vec![3]);

        let page3 = s
            .peek_inbox_page(&r, page2.last().map(|g| g.gram_id), 2)
            .unwrap();
        assert!(page3.is_empty());
        assert_eq!(s.peek_inbox_page(&r, None, 0).unwrap().len(), 0);
        assert_eq!(s.inbox_len(&other).unwrap(), 1);
    }

    #[test]
    fn inbox_prune_by_recv_time() {
        let dir = tempfile::tempdir().unwrap();
        let s = RedbStore::open(dir.path().join("t.redb")).unwrap();
        let r = [7u8; 32];
        // 两条正常消息，入库时间=当前。
        s.push_inbox(&r, &msg(r, 1, "a")).unwrap();
        s.push_inbox(&r, &msg(r, 2, "b")).unwrap();
        // 一条伪造很旧 timestamp 的消息——按**入库时间**裁剪不应被提前删（P0-1）。
        let mut forged = msg(r, 3, "forged-old");
        forged.timestamp_ms = 1;
        s.push_inbox(&r, &forged).unwrap();
        // 阈值 0：无一早于 → 不删；伪造旧时间戳那条也安全。
        assert_eq!(s.prune_inbox(0).unwrap(), 0);
        assert_eq!(
            s.prune_inbox(10_000).unwrap(),
            0,
            "按入库时间裁剪，不被伪造的旧 timestamp 提前删除"
        );
        // 未来阈值：三条入库时间均早于它 → 全删。
        assert_eq!(s.prune_inbox(now_ms() + 60_000).unwrap(), 3);
        assert_eq!(s.inbox_len(&r).unwrap(), 0);
    }

    #[test]
    fn schema_version_stamped_and_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.redb");
        {
            let s = RedbStore::open(&path).unwrap();
            s.push_inbox(&[1u8; 32], &msg([1u8; 32], 1, "x")).unwrap();
            // 备份到另一个文件并能作为独立库打开。
            let bak = dir.path().join("t.bak.redb");
            let n = s.backup(&bak).unwrap();
            assert!(n > 0);
            let s2 = RedbStore::open(&bak).unwrap();
            assert_eq!(s2.peek_inbox(&[1u8; 32]).unwrap().len(), 1);
        }
        // 重开既有库不报错（版本已戳为当前）。
        let _ = RedbStore::open(&path).unwrap();
    }

    // P0-2：模拟"旧库"（裸 Gram 进 INBOX、8 字节 seq 进 CHANNEL_INTAKE、无 META version），
    // 打开触发 v0→v1 迁移，验证旧数据被正确重写、可读、可按龄裁剪（对标生产既有库升级）。
    #[test]
    fn migrate_v0_legacy_rows() {
        use redb::Database;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.redb");
        let r = [3u8; 32];
        let g = msg(r, 1, "legacy-mail");
        let raw = g.encode_to_vec();
        let ik = RedbStore::chan_intake_key(&[9u8; 32], &[8u8; 32], b"nonce");
        {
            let db = Database::create(&path).unwrap();
            let wtx = db.begin_write().unwrap();
            {
                let mut t = wtx.open_table(INBOX).unwrap();
                t.insert(inbox_key(&r, 1).as_slice(), raw.as_slice())
                    .unwrap(); // 裸 Gram（旧格式）
                let mut it = wtx.open_table(CHANNEL_INTAKE).unwrap();
                it.insert(ik.as_slice(), 42u64.to_be_bytes().as_slice())
                    .unwrap(); // 8 字节 seq（旧格式）
            }
            wtx.commit().unwrap();
            let _ = db; // drop → 释放文件锁
        }
        // 打开 → ensure_schema 判为旧库(无 version + 非空) → 跑 v0→v1 迁移。
        let s = RedbStore::open(&path).unwrap();
        // INBOX 行被包成 recv_ms++Gram，peek 正确解出原消息（旧邮件不丢）。
        let got = s.peek_inbox(&r).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].gram_id, 1);
        // CHANNEL_INTAKE 行扩成 16 字节：get 仍返回 seq，且现在可按龄裁剪。
        assert_eq!(
            s.channel_intake_get(&[9u8; 32], &[8u8; 32], b"nonce")
                .unwrap(),
            Some(42)
        );
        assert_eq!(s.channel_intake_prune(now_ms() + 60_000).unwrap(), 1);
        // 迁移后版本已戳为当前，二次打开不再迁移。
        drop(s);
        let _ = RedbStore::open(&path).unwrap();
    }

    #[test]
    fn read_state_monotonic_and_scan() {
        let dir = tempfile::tempdir().unwrap();
        let s = RedbStore::open(dir.path().join("t.redb")).unwrap();
        let acct = [1u8; 32];
        let conv_a = [2u8; 32];
        let conv_b = [3u8; 32];
        assert!(s.read_state_set(&acct, &conv_a, 100).unwrap(), "首写推进");
        assert!(
            !s.read_state_set(&acct, &conv_a, 50).unwrap(),
            "更小值不推进(单调)"
        );
        assert!(s.read_state_set(&acct, &conv_a, 200).unwrap(), "更大值推进");
        assert!(s.read_state_set(&acct, &conv_b, 77).unwrap());
        let m: std::collections::HashMap<Vec<u8>, u64> =
            s.read_state_all(&acct).unwrap().into_iter().collect();
        assert_eq!(m.len(), 2);
        assert_eq!(m[&conv_a.to_vec()], 200);
        assert_eq!(m[&conv_b.to_vec()], 77);
        assert!(
            s.read_state_all(&[9u8; 32]).unwrap().is_empty(),
            "别的账号不串"
        );
    }

    #[test]
    fn outbox_due_indexes_filter_update_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let s = RedbStore::open(dir.path().join("t.redb")).unwrap();
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = [3u8; 32];

        s.outbox_put(&a, &b, 1, 10, 0, 100, b"dm").unwrap();
        s.outbox_put(&a, &c, 2, 10, 0, 500, b"later").unwrap();
        assert_eq!(s.outbox_due(99).unwrap().len(), 0);
        assert_eq!(s.outbox_due(100).unwrap().len(), 1);
        s.outbox_put(&a, &b, 1, 10, 1, 700, b"dm2").unwrap();
        assert!(
            s.outbox_due(100).unwrap().is_empty(),
            "覆盖写应移走旧 due 索引"
        );
        let moved = s
            .outbox_due(700)
            .unwrap()
            .into_iter()
            .find(|row| row.gram_id == 1)
            .unwrap();
        assert_eq!(moved.retries, 1);
        assert!(s.outbox_del(&a, &b, 1).unwrap());
        assert_eq!(
            s.outbox_due(700).unwrap().len(),
            1,
            "删除后只剩另一条未来 DM"
        );

        s.group_outbox_put(&a, &b, 7, 10, 0, 100, &[c.to_vec()], b"group")
            .unwrap();
        assert_eq!(s.group_outbox_due(100).unwrap().len(), 1);
        assert_eq!(
            s.group_outbox_ack_node(&a, &b, 7, &c).unwrap(),
            (true, true)
        );
        assert!(
            s.group_outbox_due(100).unwrap().is_empty(),
            "GroupAck 清空时应删除 due 索引"
        );

        s.channel_outbox_put(&a, &b, b"nonce", 10, 0, 100, b"pub")
            .unwrap();
        assert_eq!(s.channel_outbox_due(100).unwrap().len(), 1);
        assert!(s.channel_outbox_del(&a, &b, b"nonce").unwrap());
        assert!(
            s.channel_outbox_due(100).unwrap().is_empty(),
            "频道 outbox 删除应清 due 索引"
        );
    }

    #[test]
    fn devices_and_revokes_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.redb");
        let cert = nm_proto::DeviceCert {
            account: vec![1; 32],
            device: vec![2; 32],
            role: "member".into(),
            ..Default::default()
        };
        {
            let s = RedbStore::open(&path).unwrap();
            s.put_device(&DeviceInfo {
                cert: Some(cert.clone()),
                label: "a".into(),
                ..Default::default()
            })
            .unwrap();
            s.put_device(&DeviceInfo {
                cert: Some(cert.clone()),
                label: "b".into(),
                ..Default::default()
            })
            .unwrap();
            s.put_revoke(&DeviceRevoke {
                account: vec![1; 32],
                device: vec![2; 32],
                ..Default::default()
            })
            .unwrap();
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
        assert_eq!(
            s.get_entity(&[9u8; 32]).unwrap().unwrap().kind,
            "agent.assistant"
        );
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
        let blob = BlobData {
            hash: hash.clone(),
            data: b"hello-avatar".to_vec(),
            mime: "image/jpeg".into(),
        };
        store.put_blob(&blob).unwrap();
        let got = store.get_blob(&hash).unwrap().expect("blob present");
        assert_eq!(got.data, b"hello-avatar");
        assert_eq!(got.mime, "image/jpeg");
        assert!(
            store.get_blob(&[9u8; 32]).unwrap().is_none(),
            "未知 hash 应 None"
        );
        let _ = std::fs::remove_file(&path);
    }
}

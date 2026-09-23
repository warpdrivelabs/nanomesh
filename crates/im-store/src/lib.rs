//! `im-store` — 存储抽象的 `redb` 实现：离线队列(inbox) + 实体目录持久化。
//! 见 `docs/PLAN_B` §8.6 与 `docs/PLAN_C` §6。
//!
//! 表设计：
//! - `INBOX`：key = `receiver(32) ++ gram_id(8, BE)`，value = prost 编码的 `Gram`。
//!   前缀 range 扫描即可取某实体的全部离线消息，`gram_id` 单调保证 FIFO。
//! - `ENTITIES`：key = `entity_id(32)`，value = prost 编码的 `Entity`（目录持久化）。

use im_proto::{Entity, Gram, Group};
use prost::Message;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

const INBOX: TableDefinition<&[u8], &[u8]> = TableDefinition::new("inbox");
const ENTITIES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("entities");
const GROUPS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("groups");
/// 黑名单：key = 被禁公钥(32)，value 置空（存在即被禁）。无白名单——默认放行，仅拒绝名单内公钥。
const BLACKLIST: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blacklist");

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("db error: {0}")]
    Db(String),
    #[error("decode error: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

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
            wtx.open_table(GROUPS).map_err(db_err)?;
            wtx.open_table(BLACKLIST).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(Self { db })
    }

    // ---- 离线队列 ----

    /// 把一条消息存入收件人的离线队列。
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

    pub fn inbox_len(&self, receiver: &[u8]) -> Result<usize> {
        Ok(self.peek_inbox(receiver)?.len())
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
    use im_proto::{now_ms, GramKind};

    fn msg(receiver: [u8; 32], id: u64, text: &str) -> Gram {
        Gram {
            version: 1,
            kind: GramKind::Message as i32,
            gram_id: id,
            ref_gram_id: None,
            sender: [1u8; 32].to_vec(),
            receiver: receiver.to_vec(),
            timestamp_ms: now_ms(),
            payload: Some(im_proto::Any {
                type_url: "imspace.v1/text".into(),
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

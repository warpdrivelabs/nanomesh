//! 跨节点单聊可靠投递——存储层语义（不依赖网络，稳定可跑）。
//!
//! 覆盖持久发件箱 OUTBOX + 持久去重 DEDUP_DIRECT 的核心不变量，即便跨节点 gossip 投递
//! 本身因 iroh Minimal-mode 连通性在 CI 里 flaky（见 per_topic_inbox_dm.rs 的 #[ignore]），
//! 这里验证的"重传续传 / 去重 / 退避 / horizon / 续传恢复"纯逻辑仍可确定性回归。
//!
//! 对应设计：docs 计划 jaunty-gliding-marble（E2E ack + 持久重传，24h horizon）。

use nm_store::RedbStore;

/// 复刻 federation_runtime 重传臂的退避推进（index=retries，封顶复用末项）。
const BACKOFF_MS: [u64; 8] = [2_000, 5_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000];
const HORIZON_MS: u64 = 24 * 3600 * 1000;

fn open() -> (tempfile::TempDir, RedbStore) {
    let dir = tempfile::tempdir().unwrap();
    let s = RedbStore::open(dir.path().join("t.redb")).unwrap();
    (dir, s)
}

/// 收到 DirectAck → outbox_del → 重传停止（条目消失）。
#[test]
fn acked_stops_retransmit() {
    let (_d, s) = open();
    let (a, b) = ([1u8; 32], [2u8; 32]);
    s.outbox_put(&a, &b, 100, 1_000, 0, 3_000, b"hi").unwrap();
    assert_eq!(s.outbox_all().unwrap().len(), 1, "入发件箱");
    // 模拟收到对端 home 的 DirectAck：
    assert!(s.outbox_del(&a, &b, 100).unwrap(), "ack 清条目");
    assert!(s.outbox_all().unwrap().is_empty(), "重传任务此后扫不到 → 停重传");
    // ack 重复到达（gossip 可能重复）→ 幂等不报错。
    assert!(!s.outbox_del(&a, &b, 100).unwrap());
}

/// 未确认 → 每轮重传推进 retries 与 next_due（指数退避），直到（这里）被确认。
#[test]
fn unacked_retransmit_backoff_advances() {
    let (_d, s) = open();
    let (a, b) = ([1u8; 32], [2u8; 32]);
    let first_ms = 1_000u64;
    s.outbox_put(&a, &b, 7, first_ms, 0, first_ms + BACKOFF_MS[0], b"payload").unwrap();

    // 模拟 5 轮"到期重传"：每轮推进到条目 next_due → retries+1 → next_due = now + backoff[min(r1,末项)]。
    let mut now;
    let mut prev_due = first_ms + BACKOFF_MS[0];
    for _round in 1..=5u32 {
        let row = &s.outbox_all().unwrap()[0];
        assert_eq!(row.next_due_ms, prev_due, "上轮写入的到期点应被读到");
        now = row.next_due_ms + 1; // 推进到期点之后 → 本轮应重传
        let r1 = row.retries.saturating_add(1);
        let backoff = BACKOFF_MS[(r1 as usize).min(BACKOFF_MS.len() - 1)];
        prev_due = now + backoff;
        s.outbox_put(&a, &b, 7, row.first_ms, r1, prev_due, &row.gram_bytes).unwrap();
    }
    let row = &s.outbox_all().unwrap()[0];
    assert_eq!(row.retries, 5, "retries 累进");
    assert_eq!(row.first_ms, first_ms, "first_ms 恒定（horizon 据此判定）");
    assert_eq!(row.gram_bytes, b"payload".to_vec(), "载荷原样保留供重发");
    assert_eq!(row.next_due_ms, prev_due, "退避已爬到第 5 档");
}

/// 超过 24h horizon → 放弃（outbox_del），不再无限重传。
#[test]
fn horizon_gives_up() {
    let (_d, s) = open();
    let (a, b) = ([1u8; 32], [2u8; 32]);
    let first_ms = 1_000u64;
    s.outbox_put(&a, &b, 9, first_ms, 20, first_ms + 1, b"x").unwrap();
    // 重传臂判定：now - first_ms > HORIZON → 放弃。
    let now = first_ms + HORIZON_MS + 1;
    let row = &s.outbox_all().unwrap()[0];
    let expired = now.saturating_sub(row.first_ms) > HORIZON_MS;
    assert!(expired, "应判超期");
    s.outbox_del(&a, &b, 9).unwrap();
    assert!(s.outbox_all().unwrap().is_empty(), "超期放弃后条目清除");
}

/// 续传恢复：node 重启 = 重新 open 同一 db → outbox_all 自动恢复未确认条目（无需单独 reload）。
#[test]
fn persist_reload_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.redb");
    let (a, b) = ([3u8; 32], [4u8; 32]);
    {
        let s = RedbStore::open(&path).unwrap();
        s.outbox_put(&a, &b, 42, 5_000, 2, 9_000, b"survive-restart").unwrap();
    } // drop = 关库（模拟进程退出）
    {
        let s = RedbStore::open(&path).unwrap(); // 重启重开
        let rows = s.outbox_all().unwrap();
        assert_eq!(rows.len(), 1, "重启后未确认条目仍在 → 首个 tick 自动续传");
        assert_eq!(rows[0].gram_id, 42);
        assert_eq!(rows[0].retries, 2, "retries 跨重启保留");
        assert_eq!(rows[0].gram_bytes, b"survive-restart".to_vec());
    }
}

/// client 重启致 gram_id 回绕：同 (sender,gram_id) 但不同 crc → 去重表视为两条不同消息，不误丢新消息。
#[test]
fn dedup_crc_distinguishes_restarted_client() {
    let (_d, s) = open();
    let a = [5u8; 32];
    // 旧消息 gram_id=1，crc=老载荷哈希。
    assert!(!s.dedup_seen(&a, 1, b"OLDcrc00").unwrap());
    s.dedup_mark(&a, 1, b"OLDcrc00", 1_000).unwrap();
    assert!(s.dedup_seen(&a, 1, b"OLDcrc00").unwrap(), "旧消息已见");
    // client 重启后 gram_id 从 1 重数，但新消息载荷不同 → crc 不同 → 不应被误判重复。
    assert!(!s.dedup_seen(&a, 1, b"NEWcrc99").unwrap(), "不同 crc 不得误去重（否则丢新消息）");
    s.dedup_mark(&a, 1, b"NEWcrc99", 2_000).unwrap();
    assert!(s.dedup_seen(&a, 1, b"NEWcrc99").unwrap());
}

/// 去重表按龄清理（对齐 24h horizon），防无界增长。
#[test]
fn dedup_prune_by_age() {
    let (_d, s) = open();
    let a = [6u8; 32];
    s.dedup_mark(&a, 1, b"crcAAAAA", 1_000).unwrap();
    s.dedup_mark(&a, 2, b"crcBBBBB", 50_000).unwrap();
    // 清理早于 10_000 的 → 只删第一条。
    assert_eq!(s.dedup_prune(10_000).unwrap(), 1);
    assert!(!s.dedup_seen(&a, 1, b"crcAAAAA").unwrap(), "老记录已清");
    assert!(s.dedup_seen(&a, 2, b"crcBBBBB").unwrap(), "新记录保留");
}

//! 跨节点群消息可靠扇出——存储层语义(不依赖网络,稳定可跑)。
//!
//! 覆盖 GROUP_OUTBOX「按待 ack 成员节点集跟踪」的核心不变量:逐节点 ack 递减→全清、
//! 重传退避推进、超期放弃、重启续传。对应第1层(可靠扇出)设计。
//! 跨节点实跑闭环见 #[ignore] 的 cross_group_ack_e2e.rs。

use nm_store::RedbStore;

const BACKOFF_MS: [u64; 8] = [2_000, 5_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000];
const HORIZON_MS: u64 = 24 * 3600 * 1000;

fn open() -> (tempfile::TempDir, RedbStore) {
    let d = tempfile::tempdir().unwrap();
    let s = RedbStore::open(d.path().join("t.redb")).unwrap();
    (d, s)
}

/// 逐成员节点 ack → 待 ack 集递减 → 最后一个 ack 全清删除;幂等。
#[test]
fn group_ack_shrinks_then_clears() {
    let (_d, s) = open();
    let (gid, snd) = ([9u8; 32], [1u8; 32]);
    let (n1, n2, n3) = (vec![0xA1u8; 32], vec![0xB2u8; 32], vec![0xC3u8; 32]);
    s.group_outbox_put(&gid, &snd, 5, 1000, 0, 1002, &[n1.clone(), n2.clone(), n3.clone()], b"grp").unwrap();
    assert_eq!(s.group_outbox_all().unwrap()[0].pending_nodes.len(), 3);

    let (found, cleared) = s.group_outbox_ack_node(&gid, &snd, 5, &n2).unwrap();
    assert!(found && !cleared, "ack 一个 → 剩 2,未清");
    assert_eq!(s.group_outbox_all().unwrap()[0].pending_nodes.len(), 2);

    // 重复 ack 同一节点 → 幂等(已不在集中),不误清
    let (found2, cleared2) = s.group_outbox_ack_node(&gid, &snd, 5, &n2).unwrap();
    assert!(found2 && !cleared2);
    assert_eq!(s.group_outbox_all().unwrap()[0].pending_nodes.len(), 2);

    s.group_outbox_ack_node(&gid, &snd, 5, &n1).unwrap();
    let (_f, cleared3) = s.group_outbox_ack_node(&gid, &snd, 5, &n3).unwrap();
    assert!(cleared3, "最后一个 ack 应全清");
    assert!(s.group_outbox_all().unwrap().is_empty());

    // ack 不存在的条目 → found=false
    let (found4, _) = s.group_outbox_ack_node(&gid, &snd, 5, &n1).unwrap();
    assert!(!found4);
}

/// 未全 ack → 每轮重传推进 retries/next_due,pending 集保持不变(等 ack 逐个销账)。
#[test]
fn group_retransmit_backoff_keeps_pending() {
    let (_d, s) = open();
    let (gid, snd) = ([7u8; 32], [2u8; 32]);
    let pending = vec![vec![1u8; 32], vec![2u8; 32]];
    let first = 1_000u64;
    s.group_outbox_put(&gid, &snd, 3, first, 0, first + BACKOFF_MS[0], &pending, b"payload").unwrap();

    let mut prev_due = first + BACKOFF_MS[0];
    for _ in 1..=4u32 {
        let row = &s.group_outbox_all().unwrap()[0];
        assert_eq!(row.next_due_ms, prev_due);
        assert_eq!(row.pending_nodes.len(), 2, "重传不改 pending 集");
        let now = row.next_due_ms + 1;
        let r1 = row.retries.saturating_add(1);
        let backoff = BACKOFF_MS[(r1 as usize).min(BACKOFF_MS.len() - 1)];
        prev_due = now + backoff;
        s.group_outbox_put(&gid, &snd, 3, row.first_ms, r1, prev_due, &row.pending_nodes, &row.gram_bytes).unwrap();
    }
    let row = &s.group_outbox_all().unwrap()[0];
    assert_eq!(row.retries, 4);
    assert_eq!(row.first_ms, first, "first_ms 恒定(horizon 判据)");
    assert_eq!(row.gram_bytes, b"payload".to_vec());
}

/// 超 24h horizon → 放弃(删除)。
#[test]
fn group_horizon_gives_up() {
    let (_d, s) = open();
    let (gid, snd) = ([1u8; 32], [2u8; 32]);
    let first = 1_000u64;
    s.group_outbox_put(&gid, &snd, 9, first, 20, first + 1, &[vec![3u8; 32]], b"x").unwrap();
    let now = first + HORIZON_MS + 1;
    let row = &s.group_outbox_all().unwrap()[0];
    assert!(now.saturating_sub(row.first_ms) > HORIZON_MS, "应判超期");
    s.group_outbox_del(&gid, &snd, 9).unwrap();
    assert!(s.group_outbox_all().unwrap().is_empty());
}

/// 重启续传:重开同一 db → group_outbox_all 自动恢复未全 ack 的条目(含 pending 集)。
#[test]
fn group_persist_reload_resumes() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("t.redb");
    let (gid, snd) = ([3u8; 32], [4u8; 32]);
    let pending = vec![vec![5u8; 32], vec![6u8; 32]];
    {
        let s = RedbStore::open(&path).unwrap();
        s.group_outbox_put(&gid, &snd, 42, 5000, 2, 9000, &pending, b"survive").unwrap();
    }
    {
        let s = RedbStore::open(&path).unwrap();
        let rows = s.group_outbox_all().unwrap();
        assert_eq!(rows.len(), 1, "重启后未全 ack 条目仍在 → 首 tick 续传");
        assert_eq!(rows[0].gram_id, 42);
        assert_eq!(rows[0].retries, 2, "retries 跨重启保留");
        assert_eq!(rows[0].pending_nodes.len(), 2, "待 ack 节点集跨重启保留");
        assert_eq!(rows[0].gram_bytes, b"survive".to_vec());
    }
}

// ── ack 聚合（批量销账 group_outbox_ack_batch）──

/// 一个批量覆盖多条条目：各自从待 ack 集移除同一 node（部分销账，未清空）。
#[test]
fn group_ack_batch_shrinks_multiple_entries() {
    let (_d, s) = open();
    let (gid, snd) = ([9u8; 32], [1u8; 32]);
    let (n1, n2) = (vec![0xA1u8; 32], vec![0xB2u8; 32]);
    for g in [10u64, 11, 12] {
        s.group_outbox_put(&gid, &snd, g, 1000, 0, 1002, &[n1.clone(), n2.clone()], b"grp").unwrap();
    }
    let entries: Vec<(Vec<u8>, Vec<u8>, u64)> =
        [10u64, 11, 12].iter().map(|g| (gid.to_vec(), snd.to_vec(), *g)).collect();
    let (acked, cleared) = s.group_outbox_ack_batch(&n1, &entries).unwrap();
    assert_eq!((acked, cleared), (3, 0), "三条各移除 n1,均剩 n2,无清空");
    for row in s.group_outbox_all().unwrap() {
        assert_eq!(row.pending_nodes, vec![n2.clone()]);
    }
}

/// 待 ack 集只剩该 node 时，批量 ack 全清删除条目。
#[test]
fn group_ack_batch_clears_singleton_pending() {
    let (_d, s) = open();
    let (gid, snd) = ([9u8; 32], [1u8; 32]);
    let n = vec![0xA1u8; 32];
    s.group_outbox_put(&gid, &snd, 5, 1000, 0, 1002, std::slice::from_ref(&n), b"x").unwrap();
    let (acked, cleared) = s.group_outbox_ack_batch(&n, &[(gid.to_vec(), snd.to_vec(), 5)]).unwrap();
    assert_eq!((acked, cleared), (1, 1));
    assert!(s.group_outbox_all().unwrap().is_empty());
}

/// 批量里无一命中本地（非发送方节点快路径）→ 返回 (0,0) 且不触碰任何条目（只读，不开写事务）。
#[test]
fn group_ack_batch_no_match_leaves_state() {
    let (_d, s) = open();
    let (gid, snd) = ([9u8; 32], [1u8; 32]);
    let x = vec![0x11u8; 32];
    s.group_outbox_put(&gid, &snd, 5, 1000, 0, 1002, std::slice::from_ref(&x), b"keep").unwrap(); // 哨兵
    let entries = vec![
        (gid.to_vec(), snd.to_vec(), 999u64), // 同群不同 gram_id
        (vec![0xEE; 32], snd.to_vec(), 5u64), // 不同 gid
    ];
    let (acked, cleared) = s.group_outbox_ack_batch(&x, &entries).unwrap();
    assert_eq!((acked, cleared), (0, 0), "无命中 → 零销账(只读快路径)");
    let rows = s.group_outbox_all().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pending_nodes, vec![x], "哨兵条目未被触碰");
    assert_eq!(rows[0].gram_bytes, b"keep".to_vec());
}

/// 批内重复键去重 + 跨批量重复 ack 同节点幂等（不虚增 acked、不误清）。
#[test]
fn group_ack_batch_idempotent_and_dedup() {
    let (_d, s) = open();
    let (gid, snd) = ([9u8; 32], [1u8; 32]);
    let (n1, n2) = (vec![0xA1u8; 32], vec![0xB2u8; 32]);
    s.group_outbox_put(&gid, &snd, 5, 1000, 0, 1002, &[n1.clone(), n2.clone()], b"x").unwrap();
    let dup = vec![(gid.to_vec(), snd.to_vec(), 5u64), (gid.to_vec(), snd.to_vec(), 5u64)];
    let (acked, cleared) = s.group_outbox_ack_batch(&n1, &dup).unwrap();
    assert_eq!((acked, cleared), (1, 0), "批内重复键去重 → 只移除一次");
    assert_eq!(s.group_outbox_all().unwrap()[0].pending_nodes, vec![n2.clone()]);
    let (acked2, _) = s.group_outbox_ack_batch(&n1, &[(gid.to_vec(), snd.to_vec(), 5)]).unwrap();
    assert_eq!(acked2, 0, "重复 ack 同节点幂等,不计 acked");
    assert_eq!(s.group_outbox_all().unwrap()[0].pending_nodes, vec![n2]);
}

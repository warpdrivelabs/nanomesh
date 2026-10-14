use nm_core::Directory;
use nm_proto::{now_ms, Gram, Group, GroupGossip};
use nm_transport::{write_gram, IrohConnection};
use prost::Message;

use crate::retire::inbox_retireable;
use crate::s2s::fed_relay;
use crate::{Ctx, Devices, Principals, Sessions, LIVE_TTL_MS, PENDING_FWD_CAP};

/// 某群主题的 bootstrap 对端：群归属节点(锚点) + 本节点已知对等 + F2 pkarr 解析到的 seed（无锚/兜底）。
pub(crate) fn group_bootstrap(group: &Group, ctx: &Ctx) -> Vec<[u8; 32]> {
    let mut v: Vec<[u8; 32]> = Vec::new();
    if let Ok(h) = <[u8; 32]>::try_from(group.home_node.as_slice()) {
        v.push(h);
    }
    for e in ctx.peers.iter() {
        if let Ok(id) = <[u8; 32]>::try_from(e.key().as_slice()) {
            if !v.contains(&id) {
                v.push(id);
            }
        }
    }
    // F2：并入 pkarr 解析到的 seed（home 不可达 / 无锚发现时的入网点）。
    if let Some(s) = ctx.pkarr_seeds.get(&group.group_id) {
        for id in s.value() {
            if !v.contains(id) {
                v.push(*id);
            }
        }
    }
    v
}

/// F3：收件人收件箱主题的 bootstrap——收件人 home 节点(锚点) + 本节点已知对等。
/// 发送方据此接入 `nmspace-inbox:<to>` 叠加网（收件人 home 已订阅在其上）。
pub(crate) fn inbox_bootstrap(account: &[u8], ctx: &Ctx) -> Vec<[u8; 32]> {
    let mut v: Vec<[u8; 32]> = Vec::new();
    if let Some(e) = ctx.dir.entities.get(account) {
        if let Ok(h) = <[u8; 32]>::try_from(e.home_node.as_slice()) {
            v.push(h);
        }
    }
    for e in ctx.peers.iter() {
        if let Ok(id) = <[u8; 32]>::try_from(e.key().as_slice()) {
            if !v.contains(&id) {
                v.push(id);
            }
        }
    }
    v
}

/// F3：本节点为 home 的账号集合（目录 home_node==本节点 ∪ 本地登记设备的账号）。
/// 这些账号的收件箱主题需由本节点订阅，以收取发给它们的私聊单播（在线直投 / 离线入库）。
pub(crate) fn local_accounts(ctx: &Ctx) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    for e in ctx.dir.entities.iter() {
        if e.value().home_node == ctx.node_id.as_slice() && !out.contains(e.key()) {
            out.push(e.key().clone());
        }
    }
    for d in ctx.devices.iter() {
        if let Some(c) = d.cert.as_ref() {
            if !out.contains(&c.account) {
                out.push(c.account.clone());
            }
        }
    }
    out
}

/// F4：某命名域主题的 bootstrap——该域已知记录的 home 节点(锚点) + 本节点已知对等。
pub(crate) fn names_bootstrap(domain: &str, ctx: &Ctx) -> Vec<[u8; 32]> {
    let mut v: Vec<[u8; 32]> = Vec::new();
    for r in ctx.names.iter() {
        if r.domain == domain {
            if let Ok(h) = <[u8; 32]>::try_from(r.home_node.as_slice()) {
                if !v.contains(&h) {
                    v.push(h);
                }
            }
        }
    }
    for e in ctx.peers.iter() {
        if let Ok(id) = <[u8; 32]>::try_from(e.key().as_slice()) {
            if !v.contains(&id) {
                v.push(id);
            }
        }
    }
    v
}

/// 把群消息投递给「本节点负责的成员」：在线设备直投；本节点是其 home 则离线设备入库。
/// 发送方自己的其它设备也会收到（已发同步），只跳过发出这条消息的连接 `skip_rid`。
pub(crate) async fn deliver_group_here(gram: &Gram, members: &[Vec<u8>], skip_rid: &[u8], ctx: &Ctx) {
    for m in members {
        let home_here = is_home_here(ctx, m);
        deliver_account(m, gram, ctx, home_here, Some(skip_rid)).await;
    }
}

/// 本节点是否为该账号的 home（目录登记 home_node 为本节点，或有设备在本节点登记）。
pub(crate) fn is_home_here(ctx: &Ctx, account: &[u8]) -> bool {
    // entity.home_node 是账号的**单一权威** home——有它就以它为准，不再 OR 设备判定。
    // 否则：账号的旧设备 cert 一旦残留在**非 home** 节点（该账号曾在连本节点的设备登录过），
    // 下面的 devices.any() 会让本节点也误判 "home 在我"，deliver_direct 遂投本地离线库并 return、
    // 不往真正的 home 节点转发 → 跨节点消息困死在本节点（表现为"对方收不到"，且因 entity 同步时序
    // 时有时无 → 忽通忽不通）。account 的全部设备必连同一 home node，故 home_node 唯一即正确。
    if let Some(e) = ctx.dir.entities.get(account) {
        if !e.home_node.is_empty() {
            return e.home_node == ctx.node_id.as_slice();
        }
    }
    // 仅当 directory 无该账号的 home_node 记录时（老数据/entity 尚未同步），才回退到
    // "本节点有其设备" —— 此时无更好的单一权威可依。
    ctx.devices.iter().any(|d| d.cert.as_ref().is_some_and(|c| c.account == account))
}

/// 账号当前在本节点的在线连接：账号公钥直连（老客户端）+ hello 过的设备连接
/// （principals 映射）+ **devices 表登记的设备中仍有活连接者**。
///
/// 最后一项是对 churn 的容错：连接在 `session up`→`device.hello`→秒断→`session closed`
/// （清 principals）反复翻搅时，principals 映射会被 `session closed` 清掉，仅凭它会把
/// 「连接其实还在、只是映射刚被清」的设备误判为不在线 → 实时消息不直投、落离线库 →
/// 用户在线却收不到、需重进才 drain。devices 表的 cert.account 是**持久**的设备归属，
/// 只要该设备 rid 此刻在 sessions 里有活连接，就应算作该账号的在线连接、可直投。
pub(crate) fn online_rids(
    sessions: &Sessions,
    principals: &Principals,
    devices: &Devices,
    account: &[u8],
) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = principals
        .iter()
        .filter(|e| e.value().as_slice() == account && sessions.contains_key(e.key()))
        .map(|e| e.key().clone())
        .collect();
    // devices 表里属于该账号、且仍有活连接的设备 rid（principals 映射可能已被 churn 清掉）。
    for d in devices.iter() {
        if d.cert.as_ref().is_some_and(|c| c.account == account)
            && sessions.contains_key(d.key())
            && !v.contains(d.key())
        {
            v.push(d.key().clone());
        }
    }
    if sessions.contains_key(account) && !v.contains(&account.to_vec()) {
        v.push(account.to_vec());
    }
    v
}

pub(crate) fn account_online(
    sessions: &Sessions,
    principals: &Principals,
    devices: &Devices,
    account: &[u8],
) -> bool {
    sessions.contains_key(account)
        || principals.iter().any(|e| e.value().as_slice() == account && sessions.contains_key(e.key()))
        // churn 容错：principals 映射可能已被 session-closed 清掉，但连接仍在（见 online_rids）。
        || devices
            .iter()
            .any(|d| d.cert.as_ref().is_some_and(|c| c.account == account) && sessions.contains_key(d.key()))
}

/// 账号离线消息的存放对象：有效（未吊销）设备各一份；从未登记设备的老账号存在账号公钥下。
fn inbox_targets(ctx: &Ctx, account: &[u8]) -> Vec<Vec<u8>> {
    let devs: Vec<Vec<u8>> = ctx
        .devices
        .iter()
        .filter(|d| d.cert.as_ref().is_some_and(|c| c.account == account) && !ctx.revoked.contains_key(d.key()))
        .map(|d| d.key().clone())
        .collect();
    if devs.is_empty() {
        vec![account.to_vec()]
    } else {
        devs
    }
}

/// 投递给账号：所有在线连接直推；`store_offline` 时给没收到的有效设备各存一份。
/// `skip` 为不需要回送的连接（发送方自己）。返回是否至少推到一台在线设备。
pub(crate) async fn deliver_account(
    account: &[u8],
    gram: &Gram,
    ctx: &Ctx,
    store_offline: bool,
    skip: Option<&[u8]>,
) -> bool {
    let mut reached: Vec<Vec<u8>> = Vec::new();
    for r in online_rids(&ctx.sessions, &ctx.principals, &ctx.devices, account) {
        if skip == Some(r.as_slice()) {
            continue;
        }
        if try_push(&r, gram, &ctx.sessions).await {
            reached.push(r);
        }
    }
    if store_offline {
        if let Some(s) = &ctx.store {
            for d in inbox_targets(ctx, account) {
                if skip == Some(d.as_slice()) || reached.contains(&d) {
                    continue;
                }
                if let Err(e) = s.push_inbox(&d, gram) {
                    tracing::warn!("push_inbox failed: {e}");
                }
            }
        }
    }
    !reached.is_empty()
}

/// 取走某连接名下的离线消息并推送给它。
pub(crate) fn drain_inbox_to(ctx: &Ctx, rid: &[u8], conn: &IrohConnection) {
    let Some(store) = &ctx.store else { return };
    let Ok(pending) = store.drain_inbox(rid) else { return };
    if pending.is_empty() {
        return;
    }
    let c = conn.clone();
    tracing::info!(count = pending.len(), "delivering offline inbox");
    ctx.tracker.spawn(async move {
        for g in pending {
            if let Ok(mut s) = c.open_uni().await {
                let _ = write_gram(&mut s, &g).await;
                let _ = s.finish();
            }
        }
    });
}

/// 私聊单播投递（不依赖 s2s 中继）：
/// 1) 本地在线 → 直投；2) 已知且 home==本节点(离线) → 本地离线库；
/// 3) 已知远端 → 仅联邦 gossip 广播（对端节点投递/入库）；
/// 4) 完全未知 → 本地离线库兜底(可能连来本节点) + 联邦 gossip 广播(可能在远端)。
pub(crate) async fn deliver_direct(gram: &Gram, ctx: &Ctx) {
    let to = &gram.receiver;
    let home_here = is_home_here(ctx, to);
    let known_remote = !home_here && ctx.dir.entities.contains_key(to.as_slice());
    // 1) 本地在线设备直投；2) home 在本节点 → 离线设备入库；4) 完全未知 → 本地兜底入库（可能连来本节点）。
    let online = deliver_account(to, gram, ctx, !known_remote, None).await;
    if online || home_here {
        return;
    }
    // 远端 / 未知：经联邦 gossip 广播；对端节点收到后投递（取代 s2s 中继）。
    let gg = GroupGossip {
        origin: ctx.node_id.to_vec(),
        body: Some(nm_proto::pb::group_gossip::Body::Direct(gram.clone())),
    };
    let bytes = gg.encode_to_vec();
    let per_topic = ctx.per_topic.load(std::sync::atomic::Ordering::Relaxed);
    let retire = per_topic
        && inbox_retireable(to, &ctx.node_id, &ctx.dir.entities, &ctx.per_group_live, now_ms(), LIVE_TTL_MS);
    let mut inbox_ok = false;
    if per_topic {
        let _ = ctx.fed.join_inbox(to, inbox_bootstrap(to, ctx)).await;
        inbox_ok = ctx.fed.publish_inbox(to, bytes.clone()).await.is_ok();
    }
    let inbox_has_neighbors = per_topic && ctx.fed.inbox_neighbors(to).await > 0;
    if retire && inbox_ok && inbox_has_neighbors {
        tracing::debug!("firehose Direct suppressed (recipient home live on inbox topic)");
    } else {
        let _ = ctx.group_pub.send(bytes.clone());
    }
    if per_topic && !inbox_has_neighbors {
        let mut q = ctx.pending_fwd.lock().unwrap();
        if q.len() < PENDING_FWD_CAP {
            q.push_back((to.clone(), bytes, crate::now_secs()));
            tracing::debug!(qlen = q.len(), "pending-forward enqueued (cold overlay)");
        }
    }
}

/// 尝试把 gram 经 uni 流推送给某在线会话；返回是否投递成功。
/// 推送失败（连接已断但会话表尚未清理）则移除陈旧会话，交由上层落离线队列。
pub(crate) async fn try_push(to: &[u8], gram: &Gram, sessions: &Sessions) -> bool {
    let Some(target) = sessions.get(to).map(|r| r.clone()) else {
        return false;
    };
    if target.close_reason().is_some() {
        sessions.remove(to);
        return false;
    }
    let ok = async {
        let mut s = target.open_uni().await.ok()?;
        write_gram(&mut s, gram).await.ok()?;
        let _ = s.finish();
        Some(())
    }
    .await
    .is_some();
    if !ok {
        sessions.remove(to); // 陈旧会话，清理
        tracing::info!("stale session removed on push failure");
    }
    ok
}

/// 投递一份 gram 给目标实体：在线且推送成功→直达；否则落离线队列(若有 store)。
pub(crate) async fn deliver_or_store(to: &[u8], gram: &Gram, ctx: &Ctx) {
    // 1) 本地在线设备直投（同时给离线的有效设备各存一份）。
    if account_online(&ctx.sessions, &ctx.principals, &ctx.devices, to) {
        deliver_account(to, gram, ctx, true, None).await;
        return;
    }
    // 2) 跨节点：目标实体归属其它节点 → Relay 转发给其归属节点。
    if let Ok(Some(target)) = ctx.dir.get(to).await {
        if !target.home_node.is_empty() && target.home_node != ctx.node_id.as_slice() {
            if let Some(addr) = ctx.peers.get(&target.home_node).map(|a| a.clone()) {
                if fed_relay(&ctx.ep, addr, gram).await.is_ok() {
                    tracing::info!("relayed to home node");
                    return;
                }
                tracing::warn!("relay to home node failed");
            }
        }
    }
    // 3) 离线：入库，重连补投。
    if ctx.store.is_some() {
        deliver_account(to, gram, ctx, true, None).await;
        tracing::info!("target offline; stored to inbox");
    } else {
        tracing::warn!("target offline; no store; dropping");
    }
}


#[cfg(test)]
mod home_here_tests {
    use super::*;
    use nm_proto::pb::{DeviceCert, DeviceInfo, Entity};
    use crate::Node;

    /// 回归：账号的 home 在**别的**节点，但其旧设备 cert 残留在本节点 devices 表时，
    /// is_home_here 必须返回 false——否则 deliver_direct 会把跨节点消息误投本地离线库并 return、
    /// 不往真正 home 节点转发（线上现象：对方收不到、忽通忽不通）。
    #[tokio::test]
    async fn stale_device_cert_does_not_claim_home() {
        let node = Node::bind_local([7u8; 32]).await.unwrap();
        let ctx = node.ctx();
        let me = ctx.node_id;
        let other_node = [9u8; 32]; // 账号真正的 home
        let account = vec![42u8; 32];
        let dev_key = vec![43u8; 32]; // 该账号在本节点登录过的旧设备公钥

        // directory：账号的 entity.home_node 指向**另一个**节点。
        ctx.dir.merge_lww(Entity {
            entity_id: account.clone(),
            home_node: other_node.to_vec(),
            updated_at: 100,
            ..Default::default()
        });
        // 本节点 devices 表残留该账号的设备 cert（曾在连本节点的设备上登录过）。
        ctx.devices.insert(
            dev_key.clone(),
            DeviceInfo {
                cert: Some(DeviceCert { account: account.clone(), device: dev_key.clone(), ..Default::default() }),
                ..Default::default()
            },
        );

        // 修复前：devices.any() 命中 → 误判 true（消息困死本节点）。修复后：以 home_node 为准 → false。
        assert!(!is_home_here(&ctx, &account),
            "home_node 指向他节点时，本地残留设备 cert 不应让本节点误判为 home");

        // 对照：账号 home_node 正是本节点 → true（正常本地投递）。
        let local_acct = vec![50u8; 32];
        ctx.dir.merge_lww(Entity {
            entity_id: local_acct.clone(),
            home_node: me.to_vec(),
            updated_at: 100,
            ..Default::default()
        });
        assert!(is_home_here(&ctx, &local_acct), "home_node==本节点应为 home");

        // 对照：directory 无 home_node 记录（老数据）但本地有设备 → 回退设备判定 true。
        let legacy_acct = vec![60u8; 32];
        let legacy_dev = vec![61u8; 32];
        ctx.devices.insert(
            legacy_dev.clone(),
            DeviceInfo {
                cert: Some(DeviceCert { account: legacy_acct.clone(), device: legacy_dev, ..Default::default() }),
                ..Default::default()
            },
        );
        assert!(is_home_here(&ctx, &legacy_acct), "无 entity 记录时回退到设备判定");
    }

    /// 回归（C）：device.list 的 caller 退化为「设备 rid」（hello 未完成/首连时）时，
    /// 必须仍能解析出真账号并列出其设备——否则新注册用户设备列表显示为空。
    #[tokio::test]
    async fn device_list_resolves_account_when_caller_is_device_rid() {
        use crate::devices::device_list;
        use nm_proto::DeviceList;
        use prost::Message;

        let node = Node::bind_local([8u8; 32]).await.unwrap();
        let ctx = node.ctx();
        let account = vec![70u8; 32];
        let dev_rid = vec![71u8; 32]; // 该账号的设备（= 本连接 rid）

        // devices 表已登记该设备（hello 曾成功持久化），但本连接的 caller 仍退化为设备 rid。
        ctx.devices.insert(
            dev_rid.clone(),
            DeviceInfo {
                cert: Some(DeviceCert { account: account.clone(), device: dev_rid.clone(), ..Default::default() }),
                ..Default::default()
            },
        );

        // caller == 设备 rid（hello 未把 caller 升格为账号的时序）。修复前用设备 rid 查 cert.account 为空。
        let (ok, result, _) = device_list(&ctx, &dev_rid, &dev_rid);
        assert!(ok);
        let list = DeviceList::decode(result.unwrap().value.as_slice()).unwrap();
        assert_eq!(list.devices.len(), 1, "caller 退化为设备 rid 时也应解析出账号并列出其设备");
        assert_eq!(
            list.devices[0].cert.as_ref().unwrap().account, account,
            "列出的应是该设备所属账号的设备"
        );

        // 对照：caller 已是账号（正常 hello 后）——照常列出。
        let (ok2, result2, _) = device_list(&ctx, &account, &dev_rid);
        assert!(ok2);
        let list2 = DeviceList::decode(result2.unwrap().value.as_slice()).unwrap();
        assert_eq!(list2.devices.len(), 1, "caller 为账号时正常列出");
    }
}

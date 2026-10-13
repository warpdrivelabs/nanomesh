use crate::delivery::{drain_inbox_to, online_rids, try_push};
use crate::naming::{account_params, acct_fail, acct_text, parse_acct, verify_account_password};
use crate::records::{hex_decode_n, hex_encode};
use crate::{Ctx, DEVICE_EVENT_TYPE};

use nm_proto::{
    now_ms, Any, Command, DeviceCert, DeviceInfo, DeviceList, DeviceRevoke, Gram, GramKind, GroupGossip,
    PROTOCOL_VERSION,
};
use prost::Message;

/// 账号公钥直连（老客户端或持有账号私钥的连接）或 admin 设备连接。
pub(crate) fn is_admin_conn(ctx: &Ctx, caller: &[u8], rid: &[u8]) -> bool {
    rid == caller
        || ctx.devices.get(rid).is_some_and(|d| {
            d.cert.as_ref().is_some_and(|c| c.account == caller && c.role == nm_crypto::DEVICE_ROLE_ADMIN)
        })
}

/// 设备是否已被其账号吊销。`account` 未知时以本节点登记的证书账号为准。
pub(crate) fn is_revoked(ctx: &Ctx, device: &[u8], account: Option<&[u8]>) -> bool {
    let Some(r) = ctx.revoked.get(device) else {
        return false;
    };
    match account {
        Some(a) => r.account == a,
        None => ctx
            .devices
            .get(device)
            .and_then(|d| d.cert.as_ref().map(|c| c.account == r.account))
            .unwrap_or(false),
    }
}

/// 设备连接出示账号签发的证书，登记后该连接以账号身份收发。
pub(crate) fn device_hello(ctx: &Ctx, rid: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    // 失败分支一律记 warn（含 rid 前缀）：device.hello 失败会让该连接始终以设备密钥身份存在、
    // principals 无账号映射 → 账号被判离线、收不到消息、设备列表为空。此前失败静默无日志，
    // 线上无从排查（grep invalid_cert/bad_cert 皆空）。故每个失败点都留痕。
    let rid4 = hex_encode(&rid[..rid.len().min(4)]);
    let Some(cert) = cmd.params.as_ref().and_then(|p| DeviceCert::decode(p.value.as_slice()).ok()) else {
        tracing::warn!(device = %rid4, "device.hello rejected: invalid_cert (decode failed)");
        return acct_fail("invalid_cert");
    };
    if nm_crypto::verify_device_cert(&cert).is_err() {
        tracing::warn!(device = %rid4, "device.hello rejected: bad_cert (signature verify failed)");
        return acct_fail("bad_cert");
    }
    if cert.device != rid {
        tracing::warn!(device = %rid4, cert_device = %hex_encode(&cert.device[..cert.device.len().min(4)]),
            "device.hello rejected: device_mismatch (cert.device != connection rid)");
        return acct_fail("device_mismatch");
    }
    if is_revoked(ctx, rid, Some(&cert.account)) {
        tracing::warn!(device = %rid4, "device.hello rejected: device_revoked");
        if let Some(conn) = ctx.sessions.get(rid).map(|c| c.clone()) {
            conn.close(0u32.into(), b"device_revoked");
        }
        return acct_fail("device_revoked");
    }
    if <[u8; 32]>::try_from(cert.account.as_slice()).is_ok_and(|a| ctx.blacklist.contains(&a)) {
        tracing::warn!(device = %rid4, "device.hello rejected: banned (account blacklisted)");
        return acct_fail("banned");
    }
    let mut info = ctx.devices.get(rid).map(|d| d.clone()).unwrap_or_default();
    let keep_old = info
        .cert
        .as_ref()
        .is_some_and(|old| old.account == cert.account && old.issued_at > cert.issued_at);
    if !keep_old {
        if info.cert.as_ref().is_some_and(|old| old.account != cert.account) {
            info.label = String::new();
        }
        info.cert = Some(cert.clone());
    }
    if info.label.is_empty() {
        info.label = cert.label.clone();
    }
    info.last_seen = now_ms() as i64;
    info.online = false;
    info.revoked = None;
    if let Some(s) = &ctx.store {
        if let Err(e) = s.put_device(&info) {
            tracing::warn!("persist device failed: {e}");
        }
    }
    let account = info.cert.as_ref().map(|c| c.account.clone()).unwrap_or_default();
    ctx.devices.insert(rid.to_vec(), info.clone());
    ctx.principals.insert(rid.to_vec(), account.clone());
    tracing::info!(device = %hex_encode(&rid[..4]), account = %hex_encode(&account[..4]), "device hello");
    // 补投 hello 之前按设备存下的消息，以及登记设备前按账号存下的旧消息。
    if let Some(conn) = ctx.sessions.get(rid).map(|c| c.clone()) {
        drain_inbox_to(ctx, rid, &conn);
        drain_inbox_to(ctx, &account, &conn);
    }
    info.online = true;
    (true, Some(Any { type_url: "nmspace.v1.DeviceInfo".into(), value: info.encode_to_vec() }), String::new())
}

pub(crate) fn device_list(ctx: &Ctx, caller: &[u8], rid: &[u8]) -> (bool, Option<Any>, String) {
    // 规整出「真账号」：device.hello 成功后 caller 已是账号；但首连/hello 未完成时 caller 退化为
    // 设备 rid（= 连接公钥），此时用 rid 的证书账号兜底解析——否则下面用设备 rid 去匹配
    // cert.account 永远为空 → 新注册用户设备列表显示为空（即便本机设备已登记）。
    let account: Vec<u8> = if ctx.devices.iter().any(|d| d.cert.as_ref().is_some_and(|c| c.account == caller)) {
        caller.to_vec() // caller 已是账号（名下有设备）
    } else {
        // caller 可能是设备 rid：优先按本连接 rid 的证书账号，其次 principals 映射，再退回 caller。
        ctx.devices
            .get(rid)
            .and_then(|d| d.cert.as_ref().map(|c| c.account.clone()))
            .or_else(|| ctx.principals.get(rid).map(|p| p.clone()))
            .unwrap_or_else(|| caller.to_vec())
    };
    let mut devices: Vec<DeviceInfo> = ctx
        .devices
        .iter()
        .filter(|d| d.cert.as_ref().is_some_and(|c| c.account == account))
        .map(|d| {
            let mut i = d.clone();
            i.online = ctx.sessions.contains_key(d.key())
                && ctx.principals.get(d.key()).is_some_and(|p| p.as_slice() == account);
            i.revoked = ctx.revoked.get(d.key()).filter(|r| r.account == account).map(|r| r.clone());
            i
        })
        .collect();
    devices.sort_by_key(|d| d.cert.as_ref().map(|c| c.issued_at).unwrap_or(0));
    (
        true,
        Some(Any { type_url: "nmspace.v1.DeviceList".into(), value: DeviceList { devices }.encode_to_vec() }),
        String::new(),
    )
}

pub(crate) fn device_rename(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let v = account_params(cmd);
    let Some(dev) = v.get("device").and_then(|x| x.as_str()).and_then(hex_decode_n::<32>) else {
        return acct_fail("invalid_device");
    };
    let label = v.get("label").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    if label.is_empty() || label.chars().count() > 64 {
        return acct_fail("invalid_label");
    }
    let Some(mut info) = ctx.devices.get(&dev[..]).map(|d| d.clone()) else {
        return acct_fail("no_such_device");
    };
    if !info.cert.as_ref().is_some_and(|c| c.account == caller) {
        return acct_fail("not_owner");
    }
    info.label = label;
    if let Some(s) = &ctx.store {
        if s.put_device(&info).is_err() {
            return acct_fail("no_store");
        }
    }
    ctx.devices.insert(dev.to_vec(), info);
    acct_text("ok")
}

/// 紧急冻结：凭账号口令由家节点代签吊销（无管理设备在手时用）。
pub(crate) fn device_freeze(ctx: &Ctx, cmd: &Command) -> (bool, Option<Any>, String) {
    let req = match parse_acct(cmd) {
        Ok(r) => r,
        Err(code) => return acct_fail(code),
    };
    let v = account_params(cmd);
    let dev_hex = v.get("device").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let only = if dev_hex.is_empty() {
        None
    } else {
        match hex_decode_n::<32>(&dev_hex) {
            Some(d) => Some(d),
            None => return acct_fail("invalid_device"),
        }
    };
    let account = match ctx.names.get(&req.full) {
        Some(r) if r.home_node == ctx.node_id.as_slice() => r.client_pubkey.clone(),
        Some(_) => return acct_fail("not_home_node"),
        None => return acct_fail("no_such_user"),
    };
    let Some(store) = ctx.store.as_ref() else {
        return acct_fail("no_store");
    };
    match store.name_secret(&req.full) {
        Ok(Some(phc)) if verify_account_password(&phc, &req.password) => {}
        Ok(Some(_)) => return acct_fail("bad_password"),
        Ok(None) => return acct_fail("no_password"),
        Err(_) => return acct_fail("no_store"),
    }
    let Ok(account_arr) = <[u8; 32]>::try_from(account.as_slice()) else {
        return acct_fail("no_such_user");
    };
    // 指定设备只冻结它；不指定则冻结该账号全部有效设备（找不到设备公钥时的兜底）。
    let targets: Vec<[u8; 32]> = ctx
        .devices
        .iter()
        .filter(|d| d.cert.as_ref().is_some_and(|c| c.account == account))
        .filter(|d| !is_revoked(ctx, d.key(), Some(&account)))
        .filter_map(|d| <[u8; 32]>::try_from(d.key().as_slice()).ok())
        .filter(|d| only.is_none_or(|o| o == *d))
        .collect();
    if targets.is_empty() {
        return acct_fail("no_such_device");
    }
    let now = now_ms() as i64;
    for dev in &targets {
        let r = nm_crypto::sign_device_revoke(ctx.ep.secret_key(), account_arr, *dev, now, "freeze", true);
        if let Err(e) = apply_revoke(ctx, r, true) {
            return acct_fail(e);
        }
    }
    acct_text(&targets.len().to_string())
}

/// 节点能否代该账号冻结设备：须是该账号的家节点（目录或命名记录登记）。
fn node_may_freeze(ctx: &Ctx, account: &[u8], node: &[u8]) -> bool {
    node == ctx.node_id.as_slice()
        || ctx.dir.entities.get(account).is_some_and(|e| e.home_node == node)
        || ctx.names.iter().any(|n| n.client_pubkey == account && n.home_node == node)
}

/// 校验并落地一条吊销：断开该设备连接、丢弃其信箱、通知同账号在线设备，可选联邦广播。
/// 返回是否为新吊销（重复的返回 false）。
pub(crate) fn apply_revoke(ctx: &Ctx, r: DeviceRevoke, broadcast: bool) -> Result<bool, &'static str> {
    nm_crypto::verify_device_revoke(&r).map_err(|_| "bad_signature")?;
    if !r.by_node.is_empty() && !node_may_freeze(ctx, &r.account, &r.by_node) {
        return Err("node_not_home");
    }
    let registered_account = ctx.devices.get(&r.device).and_then(|d| d.cert.as_ref().map(|c| c.account.clone()));
    if registered_account.as_ref().is_some_and(|a| *a != r.account) {
        return Err("not_owner");
    }
    if let Some(old) = ctx.revoked.get(&r.device) {
        // 同账号已吊销则幂等；他人抢占的记录仅在本条属于登记账号时覆盖。
        if old.account == r.account || registered_account.is_none() {
            return Ok(false);
        }
    }
    if let Some(s) = &ctx.store {
        s.put_revoke(&r).map_err(|_| "no_store")?;
    }
    ctx.revoked.insert(r.device.clone(), r.clone());
    if let Some((_, conn)) = ctx.sessions.remove(&r.device) {
        ctx.sessions_meta.remove(&r.device);
        conn.close(0u32.into(), b"device_revoked");
    }
    ctx.principals.remove(&r.device);
    if let Some(s) = &ctx.store {
        let _ = s.drain_inbox(&r.device);
    }
    tracing::info!(device = %hex_encode(&r.device[..4]), by_node = !r.by_node.is_empty(), "device revoked");
    notify_account(
        ctx,
        &r.account,
        serde_json::json!({
            "event": "revoked",
            "device": hex_encode(&r.device),
            "reason": r.reason,
            "by_node": !r.by_node.is_empty(),
        }),
    );
    if broadcast {
        let gg = GroupGossip {
            origin: ctx.node_id.to_vec(),
            body: Some(nm_proto::pb::group_gossip::Body::Revoke(r)),
        };
        let _ = ctx.group_pub.send(gg.encode_to_vec());
    }
    Ok(true)
}

/// 向账号所有在线设备推一条设备事件（客户端按 type_url 拦截，不进聊天）。
fn notify_account(ctx: &Ctx, account: &[u8], event: serde_json::Value) {
    let rids = online_rids(&ctx.sessions, &ctx.principals, &ctx.devices, account);
    if rids.is_empty() {
        return;
    }
    let now = now_ms();
    let gram = Gram {
        version: PROTOCOL_VERSION,
        kind: GramKind::Message as i32,
        gram_id: now,
        ref_gram_id: None,
        sender: ctx.node_id.to_vec(),
        receiver: account.to_vec(),
        timestamp_ms: now,
        payload: Some(Any { type_url: DEVICE_EVENT_TYPE.into(), value: event.to_string().into_bytes() }),
        crc: Vec::new(),
    };
    let sessions = ctx.sessions.clone();
    tokio::spawn(async move {
        for r in rids {
            try_push(&r, &gram, &sessions).await;
        }
    });
}

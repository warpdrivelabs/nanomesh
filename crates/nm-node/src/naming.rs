use crate::records::hex_encode;
use crate::Ctx;

use nm_proto::{now_ms, Any, Command, GroupGossip, NameRecord};
use prost::Message;

/// 命名记录规范字节（清空 home_sig 后编码，用于签名/验签的确定性输入）。
pub(crate) fn name_canonical(rec: &NameRecord) -> Vec<u8> {
    let mut r = rec.clone();
    r.home_sig = Vec::new();
    r.encode_to_vec()
}

/// 校验命名记录：home_sig 必须由 home_node 私钥对规范字节签名（自证明委派）。
pub(crate) fn verify_name(rec: &NameRecord) -> bool {
    let Ok(home) = <[u8; 32]>::try_from(rec.home_node.as_slice()) else {
        return false;
    };
    let Ok(sig) = <[u8; 64]>::try_from(rec.home_sig.as_slice()) else {
        return false;
    };
    nm_crypto::verify_bytes(&home, &name_canonical(rec), &sig).is_ok()
}

pub(crate) fn hash_account_password(pw: &str) -> Result<String, String> {
    use argon2::password_hash::PasswordHasher;
    use argon2::Argon2;
    Argon2::default()
        .hash_password(pw.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub(crate) fn verify_account_password(phc: &str, pw: &str) -> bool {
    use argon2::password_hash::phc::PasswordHash;
    use argon2::password_hash::PasswordVerifier;
    use argon2::Argon2;
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default().verify_password(pw.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

pub(crate) fn account_params(cmd: &Command) -> serde_json::Value {
    let text = cmd
        .params
        .as_ref()
        .map(|p| String::from_utf8_lossy(&p.value).into_owned())
        .unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_default()
}

pub(crate) struct AcctReq {
    pub(crate) domain: String,
    pub(crate) full: String,
    pub(crate) password: String,
    pub(crate) old: String,
}

pub(crate) fn parse_acct(cmd: &Command) -> Result<AcctReq, &'static str> {
    let v = account_params(cmd);
    let local = v.get("local").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    let domain = v.get("domain").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    if !valid_local_part(&local) || domain.is_empty() || domain.len() > 253 {
        return Err("invalid_name");
    }
    Ok(AcctReq {
        full: format!("{local}@{domain}"),
        domain,
        password: v.get("password").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        old: v.get("old").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
}

pub(crate) fn acct_fail(code: &'static str) -> (bool, Option<Any>, String) {
    (false, None, code.into())
}

pub(crate) fn acct_text(text: &str) -> (bool, Option<Any>, String) {
    (true, Some(Any { type_url: "text/plain".into(), value: text.as_bytes().to_vec() }), String::new())
}

fn owns_domain(ctx: &Ctx, domain: &str) -> bool {
    ctx.domains.read().unwrap().iter().any(|d| d == domain)
}

/// 在本节点登记 local@domain。口令只存 Argon2 PHC，明文不落盘。
pub(crate) fn register_name_account(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let v = account_params(cmd);
    let local = v.get("local").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    let domain = v.get("domain").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    let password = v.get("password").and_then(|x| x.as_str()).unwrap_or("");
    if !owns_domain(ctx, &domain) {
        return acct_fail("domain_not_owned");
    }
    if !valid_local_part(&local) {
        return acct_fail("invalid_name");
    }
    if password.chars().count() < 8 {
        return acct_fail("password_short");
    }
    let full = format!("{local}@{domain}");
    if let Some(r) = ctx.names.get(&full) {
        if r.client_pubkey.as_slice() != caller {
            return acct_fail("name_taken");
        }
        return acct_fail("name_taken");
    }
    let phc = match hash_account_password(password) {
        Ok(h) => h,
        Err(e) => return (false, None, format!("口令无法保存: {e}")),
    };
    let Some(store) = ctx.store.as_ref() else {
        return (false, None, "节点未打开数据库，无法保存口令".into());
    };
    if let Err(e) = store.put_name_secret(&full, &phc) {
        return (false, None, format!("口令无法保存: {e}"));
    }
    let mut rec = NameRecord {
        local_part: local,
        domain,
        client_pubkey: caller.to_vec(),
        serial: 1,
        issued_at: now_ms() as i64,
        ttl: 3600,
        home_node: ctx.node_id.to_vec(),
        home_sig: Vec::new(),
    };
    rec.home_sig = nm_crypto::sign_bytes(ctx.ep.secret_key(), &name_canonical(&rec)).to_vec();
    if let Err(e) = store.put_name(&rec) {
        return (false, None, format!("名字无法保存: {e}"));
    }
    ctx.names.insert(full, rec.clone());
    let gg = GroupGossip {
        origin: ctx.node_id.to_vec(),
        body: Some(nm_proto::pb::group_gossip::Body::Name(rec.clone())),
    };
    let _ = ctx.group_pub.send(gg.encode_to_vec());
    (
        true,
        Some(Any {
            type_url: "text/plain".into(),
            value: hex_encode(caller).into_bytes(),
        }),
        String::new(),
    )
}

/// 在家节点比对口令。成功时返回该名字登记的客户端公钥 hex。
pub(crate) fn login_name_account(ctx: &Ctx, _caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let v = account_params(cmd);
    let local = v.get("local").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    let domain = v.get("domain").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
    let password = v.get("password").and_then(|x| x.as_str()).unwrap_or("");
    if !valid_local_part(&local) || domain.is_empty() {
        return acct_fail("invalid_name");
    }
    let full = format!("{local}@{domain}");
    let Some(rec) = ctx.names.get(&full) else {
        return acct_fail("no_such_user");
    };
    let Some(store) = ctx.store.as_ref() else {
        return acct_fail("no_store");
    };
    let phc = match store.name_secret(&full) {
        Ok(Some(h)) => h,
        Ok(None) => return acct_fail("no_password"),
        Err(_) => return acct_fail("no_store"),
    };
    if !verify_account_password(&phc, password) {
        return acct_fail("bad_password");
    }
    (
        true,
        Some(Any {
            type_url: "text/plain".into(),
            value: hex_encode(&rec.client_pubkey).into_bytes(),
        }),
        String::new(),
    )
}

/// 注册前询问名字是否已存在。不比对口令，也不返回私钥。
pub(crate) fn lookup_name_account(ctx: &Ctx, cmd: &Command) -> (bool, Option<Any>, String) {
    let req = match parse_acct(cmd) {
        Ok(r) => r,
        Err(code) => return acct_fail(code),
    };
    if !owns_domain(ctx, &req.domain) {
        return acct_fail("domain_not_owned");
    }
    let exists = if ctx.names.contains_key(&req.full) { "1" } else { "0" };
    acct_text(exists)
}

/// 已登录且公钥匹配时，用旧密码换成新的 Argon2 哈希。
pub(crate) fn passwd_name_account(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let req = match parse_acct(cmd) {
        Ok(r) => r,
        Err(code) => return acct_fail(code),
    };
    if req.password.chars().count() < 8 {
        return acct_fail("password_short");
    }
    if req.password == req.old {
        return acct_fail("same_password");
    }
    let owner = match ctx.names.get(&req.full) {
        Some(rec) => rec.client_pubkey.clone(),
        None => return acct_fail("no_such_user"),
    };
    if owner.as_slice() != caller {
        return acct_fail("not_key_owner");
    }
    let Some(store) = ctx.store.as_ref() else {
        return acct_fail("no_store");
    };
    let phc = match store.name_secret(&req.full) {
        Ok(Some(h)) => h,
        Ok(None) => return acct_fail("no_password"),
        Err(_) => return acct_fail("no_store"),
    };
    if !verify_account_password(&phc, &req.old) {
        return acct_fail("bad_password");
    }
    match hash_account_password(&req.password) {
        Ok(h) => match store.put_name_secret(&req.full, &h) {
            Ok(()) => acct_text("ok"),
            Err(_) => acct_fail("no_store"),
        },
        Err(_) => acct_fail("no_store"),
    }
}

/// 本机私钥与登记公钥一致时，不需要旧密码即可设置新密码。
pub(crate) fn reset_name_account(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let req = match parse_acct(cmd) {
        Ok(r) => r,
        Err(code) => return acct_fail(code),
    };
    if req.password.chars().count() < 8 {
        return acct_fail("password_short");
    }
    let owner = match ctx.names.get(&req.full) {
        Some(rec) => rec.client_pubkey.clone(),
        None => return acct_fail("no_such_user"),
    };
    if owner.as_slice() != caller {
        return acct_fail("not_key_owner");
    }
    let Some(store) = ctx.store.as_ref() else {
        return acct_fail("no_store");
    };
    match hash_account_password(&req.password) {
        Ok(h) => match store.put_name_secret(&req.full, &h) {
            Ok(()) => acct_text("ok"),
            Err(_) => acct_fail("no_store"),
        },
        Err(_) => acct_fail("no_store"),
    }
}

/// local-part 合法性：非空、≤63、仅 [a-z0-9._-]。
pub(crate) fn valid_local_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_')
}

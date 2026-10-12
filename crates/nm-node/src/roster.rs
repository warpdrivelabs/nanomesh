use crate::records::hex_decode_n;
use crate::{now_secs, Ctx};

use nm_proto::{Any, Command};

pub(crate) fn roster_list(ctx: &Ctx, caller: &[u8]) -> (bool, Option<Any>, String) {
    match &ctx.store {
        Some(s) => {
            let entries: Vec<serde_json::Value> = s
                .get_roster_all(caller)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|j| serde_json::from_str(&j).ok())
                .collect();
            let body = serde_json::to_string(&entries).unwrap_or_default();
            (true, Some(Any { type_url: "text/plain".into(), value: body.into_bytes() }), String::new())
        }
        None => (false, None, "no store".into()),
    }
}

pub(crate) fn roster_add(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let params: Option<serde_json::Value> = cmd.params.as_ref().and_then(|p| serde_json::from_slice(&p.value).ok());
    match params {
        None => (false, None, "invalid params".into()),
        Some(p) => {
            let id_hex = p.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            match (id_hex.len() == 64).then(|| hex_decode_n::<32>(&id_hex)).flatten() {
                None => (false, None, "bad id".into()),
                Some(entry_id) => {
                    let kind = p.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if kind.is_empty() || name.is_empty() {
                        (false, None, "kind and name required".into())
                    } else {
                        let handle = {
                            let s = p.get("handle").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            if s.is_empty() {
                                ctx.dir
                                    .entities
                                    .get(&entry_id[..].to_vec())
                                    .and_then(|e| e.attributes.get("name").cloned())
                                    .unwrap_or_default()
                            } else {
                                s
                            }
                        };
                        let remark = p.get("remark").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let now = now_secs();
                        let entry = serde_json::json!({
                            "id": id_hex, "kind": kind, "name": name,
                            "handle": handle, "remark": remark,
                            "status": "active", "added_at": now, "updated_at": now
                        });
                        let json = entry.to_string();
                        match &ctx.store {
                            Some(s) => {
                                let _ = s.put_roster(caller, &entry_id, &json);
                                (
                                    true,
                                    Some(Any { type_url: "text/plain".into(), value: json.into_bytes() }),
                                    String::new(),
                                )
                            }
                            None => (false, None, "no store".into()),
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn roster_remove(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let id_hex = cmd
        .params
        .as_ref()
        .and_then(|p| serde_json::from_slice::<serde_json::Value>(&p.value).ok())
        .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    match hex_decode_n::<32>(&id_hex) {
        Some(entry_id) => {
            if let Some(s) = &ctx.store {
                let _ = s.del_roster(caller, &entry_id);
            }
            (true, None, String::new())
        }
        None => (false, None, "bad id".into()),
    }
}

pub(crate) fn roster_update(ctx: &Ctx, caller: &[u8], cmd: &Command) -> (bool, Option<Any>, String) {
    let params: Option<serde_json::Value> = cmd.params.as_ref().and_then(|p| serde_json::from_slice(&p.value).ok());
    match params {
        None => (false, None, "invalid params".into()),
        Some(p) => {
            let id_hex = p.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            match hex_decode_n::<32>(&id_hex) {
                None => (false, None, "bad id".into()),
                Some(entry_id) => match &ctx.store {
                    None => (false, None, "no store".into()),
                    Some(store) => {
                        let existing = store.get_roster_all(caller).unwrap_or_default().into_iter().find_map(|j| {
                            let v: serde_json::Value = serde_json::from_str(&j).ok()?;
                            if v.get("id").and_then(|x| x.as_str()) == Some(&id_hex) {
                                Some(v)
                            } else {
                                None
                            }
                        });
                        let mut entry = existing.unwrap_or_else(|| {
                            serde_json::json!({
                                "id": id_hex, "kind": "", "name": "", "handle": "", "remark": "",
                                "status": "active", "added_at": now_secs(), "updated_at": now_secs()
                            })
                        });
                        if let Some(n) = p.get("name").and_then(|v| v.as_str()) {
                            if !n.is_empty() {
                                entry["name"] = serde_json::Value::String(n.to_string());
                            }
                        }
                        if let Some(h) = p.get("handle").and_then(|v| v.as_str()) {
                            if !h.is_empty() {
                                entry["handle"] = serde_json::Value::String(h.to_string());
                            }
                        }
                        if let Some(r) = p.get("remark").and_then(|v| v.as_str()) {
                            entry["remark"] = serde_json::Value::String(r.to_string());
                        }
                        entry["updated_at"] = serde_json::Value::Number(now_secs().into());
                        let json = entry.to_string();
                        let _ = store.put_roster(caller, &entry_id, &json);
                        (
                            true,
                            Some(Any { type_url: "text/plain".into(), value: json.into_bytes() }),
                            String::new(),
                        )
                    }
                },
            }
        }
    }
}

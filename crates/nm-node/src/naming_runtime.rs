use std::sync::Arc;

use dashmap::DashMap;
use nm_proto::{now_ms, Any, Command, Gram, GramKind, GroupGossip, NameRecord, PROTOCOL_VERSION};
use nm_store::RedbStore;
use prost::Message;

use crate::naming::{name_canonical, valid_local_part, verify_name};
use crate::Node;

impl Node {
    /// 从配置设置本节点域名（逗号分隔多个）；持久化并去重。nmd 启动时调用。
    pub fn set_domain(&self, csv: String) {
        let list: Vec<String> = csv.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
        if list.is_empty() {
            return;
        }
        let mut g = self.domains.write().unwrap();
        for d in list {
            if !g.contains(&d) {
                g.push(d.clone());
            }
            if let Some(s) = &self.store {
                let _ = s.put_domain(&d);
            }
        }
        tracing::info!(domains = ?*g, "node domains set");
    }

    /// 把域名审批结果发给目标节点，并在目标就是自己时立即入账。
    pub fn publish_domain_decision(&self, node_id: [u8; 32], domain: &str, approved: bool) {
        let text = serde_json::json!({ "domain": domain, "approved": approved }).to_string();
        let cmd = Command {
            method: "domain.decision".into(),
            params: Some(Any { type_url: "text/plain".into(), value: text.into_bytes() }),
            correlation_id: 0,
            timeout_ms: 0,
            grant: None,
        };
        let gram = Gram {
            version: PROTOCOL_VERSION,
            kind: GramKind::Command as i32,
            gram_id: now_ms(),
            ref_gram_id: None,
            sender: self.ep.id_bytes().to_vec(),
            receiver: node_id.to_vec(),
            timestamp_ms: now_ms(),
            payload: Some(Any {
                type_url: "nmspace.v1.Command".into(),
                value: cmd.encode_to_vec(),
            }),
            crc: Vec::new(),
        };
        if node_id == self.ep.id_bytes() {
            apply_domain_decision(&gram, &self.domains, &self.store, &self.domain_notices);
        }
        let gg = GroupGossip {
            origin: self.ep.id_bytes().to_vec(),
            body: Some(nm_proto::pb::group_gossip::Body::Direct(gram)),
        };
        let _ = self.group_pub.send(gg.encode_to_vec());
    }

    pub fn domain_notices(&self) -> Vec<(String, bool, u64)> {
        self.domain_notices
            .iter()
            .map(|e| (e.key().clone(), e.value().0, e.value().1))
            .collect()
    }

    /// 本节点拥有的域名列表（去中心命名 N1）。
    pub fn owned_domains(&self) -> Vec<String> {
        self.domains.read().unwrap().clone()
    }

    /// 申请（TOFU 认领）一个域名到本节点：校验语法、去重、持久化；返回是否新增。
    /// N1 无全局唯一约束——若命名缓存里已见其它节点为该域签发过记录，软性拒绝（全局唯一见 N2）。
    pub fn add_domain(&self, domain: &str) -> Result<bool, String> {
        let d = domain.trim().to_lowercase();
        if d.is_empty() || d.len() > 253 || !d.contains('.')
            || !d.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        {
            return Err("非法域名（形如 example.nm）".into());
        }
        if self.domains.read().unwrap().contains(&d) {
            return Ok(false);
        }
        let me = self.ep.id_bytes();
        if self.names.iter().any(|r| r.domain == d && r.home_node.as_slice() != me.as_slice()) {
            return Err(format!("域名 {d} 似乎已被联邦中其它节点占用（N1=TOFU；全局唯一见 N2）"));
        }
        self.domains.write().unwrap().push(d.clone());
        if let Some(s) = &self.store {
            let _ = s.put_domain(&d);
        }
        tracing::info!(domain = %d, "domain claimed (TOFU)");
        Ok(true)
    }

    /// 本节点签发的命名记录（可按域名过滤；不含墓碑）。
    pub fn names_owned(&self, domain: Option<&str>) -> Vec<NameRecord> {
        let me = self.ep.id_bytes();
        self.names
            .iter()
            .filter(|r| r.home_node.as_slice() == me.as_slice() && !r.client_pubkey.is_empty())
            .filter(|r| domain.map(|d| r.domain == d).unwrap_or(true))
            .map(|r| r.clone())
            .collect()
    }

    /// 管理端：在本节点某域名下绑定 local_part → pubkey（注册商权威）。签名 + 持久 + 广播。
    pub fn admin_set_name(&self, domain: &str, local_part: &str, pubkey: [u8; 32]) -> Result<NameRecord, String> {
        let d = domain.trim().to_lowercase();
        let lp = local_part.trim().to_lowercase();
        if !self.domains.read().unwrap().contains(&d) {
            return Err(format!("本节点未拥有域名 {d}"));
        }
        if !valid_local_part(&lp) {
            return Err("非法 local-part（仅 a-z 0-9 . - _，≤63）".into());
        }
        let full = format!("{lp}@{d}");
        let serial = self.names.get(&full).map(|r| r.serial + 1).unwrap_or(1);
        let mut rec = NameRecord {
            local_part: lp, domain: d, client_pubkey: pubkey.to_vec(), serial,
            issued_at: now_ms() as i64, ttl: 3600, home_node: self.ep.id_bytes().to_vec(), home_sig: Vec::new(),
        };
        rec.home_sig = nm_crypto::sign_bytes(self.ep.secret_key(), &name_canonical(&rec)).to_vec();
        if let Some(s) = &self.store {
            let _ = s.put_name(&rec);
        }
        self.names.insert(full, rec.clone());
        self.broadcast_name(&rec);
        Ok(rec)
    }

    /// 管理端：删除某域名下的 local_part（墓碑：空公钥 + 更高 serial，经 gossip 扩散移除）。
    pub fn admin_del_name(&self, domain: &str, local_part: &str) -> Result<(), String> {
        let d = domain.trim().to_lowercase();
        let lp = local_part.trim().to_lowercase();
        if !self.domains.read().unwrap().contains(&d) {
            return Err(format!("本节点未拥有域名 {d}"));
        }
        let full = format!("{lp}@{d}");
        let serial = self.names.get(&full).map(|r| r.serial + 1).ok_or("该名字不存在")?;
        let mut rec = NameRecord {
            local_part: lp, domain: d, client_pubkey: Vec::new(), serial, // 空公钥 = 墓碑
            issued_at: now_ms() as i64, ttl: 0, home_node: self.ep.id_bytes().to_vec(), home_sig: Vec::new(),
        };
        rec.home_sig = nm_crypto::sign_bytes(self.ep.secret_key(), &name_canonical(&rec)).to_vec();
        if let Some(s) = &self.store {
            let _ = s.del_name(&full);
        }
        self.names.remove(&full);
        self.broadcast_name(&rec);
        Ok(())
    }

    fn broadcast_name(&self, rec: &NameRecord) {
        let gg = GroupGossip {
            origin: self.ep.id_bytes().to_vec(),
            body: Some(nm_proto::pb::group_gossip::Body::Name(rec.clone())),
        };
        let _ = self.group_pub.send(gg.encode_to_vec());
    }

    /// 合并命名记录到缓存（gossip 学到的他域记录仅入内存；验签通过 + serial 更新才收）。
    pub(crate) fn merge_name_lww(&self, rec: NameRecord) {
        if !verify_name(&rec) {
            tracing::debug!("name: bad signature, dropped");
            return;
        }
        let key = format!("{}@{}", rec.local_part, rec.domain);
        // 墓碑（空公钥）：serial 不更旧则移除缓存/持久。
        if rec.client_pubkey.is_empty() {
            if let Some(e) = self.names.get(&key) {
                if rec.serial < e.serial {
                    return;
                }
            }
            self.names.remove(&key);
            if let Some(s) = &self.store {
                let _ = s.del_name(&key);
            }
            return;
        }
        use dashmap::mapref::entry::Entry;
        match self.names.entry(key) {
            Entry::Occupied(mut o) => {
                if rec.serial >= o.get().serial {
                    o.insert(rec);
                }
            }
            Entry::Vacant(v) => {
                v.insert(rec);
            }
        }
    }
}

/// 收件人是本节点时，把注册中心的通过/驳回写入本地域名或通知列表。
pub(crate) fn apply_domain_decision(
    gram: &Gram,
    domains: &std::sync::RwLock<Vec<String>>,
    store: &Option<Arc<RedbStore>>,
    notices: &DashMap<String, (bool, u64)>,
) {
    if gram.kind() != GramKind::Command {
        return;
    }
    let Some(payload) = gram.payload.as_ref() else { return };
    let Ok(cmd) = Command::decode(payload.value.as_slice()) else { return };
    if cmd.method != "domain.decision" {
        return;
    }
    let text = cmd
        .params
        .as_ref()
        .map(|p| String::from_utf8_lossy(&p.value).into_owned())
        .unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    let Some(domain) = v.get("domain").and_then(|d| d.as_str()) else { return };
    let approved = v.get("approved").and_then(|d| d.as_bool()).unwrap_or(false);
    let d = domain.trim().to_lowercase();
    if d.is_empty() {
        return;
    }
    if approved && !domains.read().unwrap().iter().any(|x| x == &d) {
        domains.write().unwrap().push(d.clone());
        if let Some(s) = store {
            let _ = s.put_domain(&d);
        }
    }
    notices.insert(d, (approved, now_ms()));
}

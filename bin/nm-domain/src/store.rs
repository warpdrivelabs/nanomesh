//! 域名注册表。存储用 redb，和 `nmd` 的 `nm-store` 同一类数据库。
//! 表 `domains`：key = 归一化域名，value = JSON（公钥、邮箱、是否停用、登记时间、更新时间）。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

const DOMAINS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("domains");
const APPLICATIONS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("applications");

const RESERVED: &[&str] = &[
    "localhost", "invalid", "example", "test", "local", "nmspace", "nmd",
];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("db error: {0}")]
    Db(String),
    #[error("{0}")]
    Invalid(String),
    #[error("域名已被 {0} 登记")]
    Taken(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

fn db_err(e: impl std::fmt::Display) -> StoreError {
    StoreError::Db(e.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 归一化域名：去空白和末尾点、转小写。至少两个标签，只允许字母、数字、连字符。
pub fn normalize_domain(raw: &str) -> Result<String> {
    let s = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if s.is_empty() || s.len() > 253 || s.starts_with('.') || s.contains("..") {
        return Err(StoreError::Invalid("域名不合法".into()));
    }
    let labels: Vec<&str> = s.split('.').collect();
    if labels.len() < 2 {
        return Err(StoreError::Invalid("域名至少要有两段，如 mesh.example".into()));
    }
    for label in &labels {
        if label.is_empty() || label.len() > 63 {
            return Err(StoreError::Invalid("域名标签长度不合法".into()));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(StoreError::Invalid("域名标签不能以连字符开头或结尾".into()));
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(StoreError::Invalid("域名只允许字母、数字和连字符".into()));
        }
    }
    if RESERVED.iter().any(|r| s == *r || labels.iter().any(|l| l == r)) {
        return Err(StoreError::Invalid("该域名在保留名单中".into()));
    }
    Ok(s)
}

/// 登记邮箱：去空白、转小写，须包含一个 @ 和带点的域名。
pub fn normalize_email(raw: &str) -> Result<String> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() || s.len() > 254 || s.chars().any(|c| c.is_whitespace()) {
        return Err(StoreError::Invalid("邮箱不合法".into()));
    }
    let Some((local, host)) = s.split_once('@') else {
        return Err(StoreError::Invalid("邮箱不合法".into()));
    };
    if local.is_empty()
        || host.is_empty()
        || local.contains('@')
        || !host.contains('.')
        || host.starts_with('.')
        || host.ends_with('.')
        || host.contains("..")
    {
        return Err(StoreError::Invalid("邮箱不合法".into()));
    }
    let local_ok = local
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-'));
    let host_ok = host
        .split('.')
        .all(|label| !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    if !local_ok || !host_ok {
        return Err(StoreError::Invalid("邮箱不合法".into()));
    }
    Ok(s)
}

/// 节点公钥：64 位十六进制，存小写。
pub fn normalize_pubkey(raw: &str) -> Result<String> {
    let s = raw.trim().to_ascii_lowercase();
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(StoreError::Invalid("公钥须为 64 位十六进制".into()));
    }
    Ok(s)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainRec {
    pub domain: String,
    pub pubkey: String,
    #[serde(default)]
    pub email: String,
    /// 停用后记录仍在，公开解析不再返回公钥。
    #[serde(default)]
    pub disabled: bool,
    pub created_ms: u64,
    pub updated_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterOutcome {
    Created,
    Unchanged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Application {
    pub domain: String,
    pub node_id: String,
    #[serde(default)]
    pub email: String,
    /// pending / approved / rejected
    pub status: String,
    pub created_ms: u64,
    pub decided_ms: u64,
}

pub struct Registry {
    db: Database,
}

impl Registry {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path).map_err(db_err)?;
        let wtx = db.begin_write().map_err(db_err)?;
        {
            wtx.open_table(DOMAINS).map_err(db_err)?;
            wtx.open_table(APPLICATIONS).map_err(db_err)?;
        }
        wtx.commit().map_err(db_err)?;
        Ok(Self { db })
    }

    /// 登记域名。同一把公钥重复登记视为成功，并更新邮箱；已属于其他公钥则拒绝。
    pub fn register(&self, domain: &str, pubkey: &str, email: &str) -> Result<RegisterOutcome> {
        self.register_inner(domain, pubkey, email.to_string(), true)
    }

    /// 审批通过时登记：公钥是申请节点，不要求邮箱。
    pub fn claim(&self, domain: &str, pubkey: &str, email: &str) -> Result<RegisterOutcome> {
        let email = if email.is_empty() {
            String::new()
        } else {
            normalize_email(email)?
        };
        self.register_inner(domain, pubkey, email, false)
    }

    fn register_inner(
        &self,
        domain: &str,
        pubkey: &str,
        email: String,
        check_email: bool,
    ) -> Result<RegisterOutcome> {
        let domain = normalize_domain(domain)?;
        let pubkey = normalize_pubkey(pubkey)?;
        let email = if check_email { normalize_email(&email)? } else { email };
        let now = now_ms();
        let wtx = self.db.begin_write().map_err(db_err)?;
        let outcome = {
            let mut t = wtx.open_table(DOMAINS).map_err(db_err)?;
            let existing = match t.get(domain.as_bytes()).map_err(db_err)? {
                Some(v) => {
                    let rec: DomainRec = serde_json::from_slice(v.value())
                        .map_err(|e| StoreError::Db(e.to_string()))?;
                    Some(rec)
                }
                None => None,
            };
            if let Some(rec) = existing {
                if rec.pubkey != pubkey {
                    return Err(StoreError::Taken(rec.pubkey));
                }
                let mut next = rec;
                if check_email {
                    next.email = email;
                }
                next.disabled = false;
                next.updated_ms = now;
                let val = serde_json::to_vec(&next).map_err(|e| StoreError::Db(e.to_string()))?;
                t.insert(domain.as_bytes(), val.as_slice()).map_err(db_err)?;
                RegisterOutcome::Unchanged
            } else {
                let rec = DomainRec {
                    domain: domain.clone(),
                    pubkey,
                    email,
                    disabled: false,
                    created_ms: now,
                    updated_ms: now,
                };
                let val = serde_json::to_vec(&rec).map_err(|e| StoreError::Db(e.to_string()))?;
                t.insert(domain.as_bytes(), val.as_slice()).map_err(db_err)?;
                RegisterOutcome::Created
            }
        };
        wtx.commit().map_err(db_err)?;
        Ok(outcome)
    }

    /// 提交域名申请。必须带合法邮箱。同一域名的待审申请不改节点；已被其他节点正式持有则拒绝。
    pub fn apply(&self, domain: &str, node_id: &str, email: &str) -> Result<Application> {
        let domain = normalize_domain(domain)?;
        let node_id = normalize_pubkey(node_id)?;
        let email = normalize_email(email)?;
        if let Some(rec) = self.resolve(&domain)? {
            if rec.pubkey != node_id {
                return Err(StoreError::Taken(rec.pubkey));
            }
        }
        let now = now_ms();
        let wtx = self.db.begin_write().map_err(db_err)?;
        let app = {
            let mut t = wtx.open_table(APPLICATIONS).map_err(db_err)?;
            let existing = match t.get(domain.as_bytes()).map_err(db_err)? {
                Some(v) => Some(
                    serde_json::from_slice::<Application>(v.value())
                        .map_err(|e| StoreError::Db(e.to_string()))?,
                ),
                None => None,
            };
            if let Some(cur) = existing {
                if cur.status == "pending" {
                    if cur.node_id != node_id {
                        return Err(StoreError::Taken(cur.node_id));
                    }
                    let mut cur = cur;
                    cur.email = email;
                    let val = serde_json::to_vec(&cur).map_err(|e| StoreError::Db(e.to_string()))?;
                    t.insert(domain.as_bytes(), val.as_slice()).map_err(db_err)?;
                    cur
                } else if cur.status == "approved" && cur.node_id == node_id {
                    cur
                } else {
                    let app = Application {
                        domain: domain.clone(),
                        node_id,
                        email,
                        status: "pending".into(),
                        created_ms: now,
                        decided_ms: 0,
                    };
                    let val = serde_json::to_vec(&app).map_err(|e| StoreError::Db(e.to_string()))?;
                    t.insert(domain.as_bytes(), val.as_slice()).map_err(db_err)?;
                    app
                }
            } else {
                let app = Application {
                    domain: domain.clone(),
                    node_id,
                    email,
                    status: "pending".into(),
                    created_ms: now,
                    decided_ms: 0,
                };
                let val = serde_json::to_vec(&app).map_err(|e| StoreError::Db(e.to_string()))?;
                t.insert(domain.as_bytes(), val.as_slice()).map_err(db_err)?;
                app
            }
        };
        wtx.commit().map_err(db_err)?;
        Ok(app)
    }

    pub fn list_for_node(&self, node_id: &str) -> Result<Vec<Application>> {
        let node_id = normalize_pubkey(node_id)?;
        Ok(self
            .list_applications()?
            .into_iter()
            .filter(|a| a.node_id == node_id)
            .collect())
    }

    pub fn list_applications(&self) -> Result<Vec<Application>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(APPLICATIONS).map_err(db_err)?;
        let mut out: Vec<Application> = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (_k, v) = item.map_err(db_err)?;
            out.push(
                serde_json::from_slice(v.value()).map_err(|e| StoreError::Db(e.to_string()))?,
            );
        }
        out.sort_by(|a, b| {
            let rank = |s: &str| match s {
                "pending" => 0,
                "approved" => 1,
                _ => 2,
            };
            rank(&a.status)
                .cmp(&rank(&b.status))
                .then(b.created_ms.cmp(&a.created_ms))
        });
        Ok(out)
    }

    /// 通过或驳回待审申请。通过时写入正式域名表。
    pub fn decide(&self, domain: &str, approved: bool) -> Result<Application> {
        let domain = normalize_domain(domain)?;
        if approved {
            let (node_id, email) = {
                let rtx = self.db.begin_read().map_err(db_err)?;
                let t = rtx.open_table(APPLICATIONS).map_err(db_err)?;
                let v = t
                    .get(domain.as_bytes())
                    .map_err(db_err)?
                    .ok_or_else(|| StoreError::Invalid("没有这条申请".into()))?;
                let app: Application = serde_json::from_slice(v.value())
                    .map_err(|e| StoreError::Db(e.to_string()))?;
                if app.status != "pending" {
                    return Err(StoreError::Invalid("申请已处理".into()));
                }
                (app.node_id, app.email)
            };
            self.claim(&domain, &node_id, &email)?;
        }
        let now = now_ms();
        let wtx = self.db.begin_write().map_err(db_err)?;
        let app = {
            let mut t = wtx.open_table(APPLICATIONS).map_err(db_err)?;
            let existing = match t.get(domain.as_bytes()).map_err(db_err)? {
                Some(v) => serde_json::from_slice::<Application>(v.value())
                    .map_err(|e| StoreError::Db(e.to_string()))?,
                None => return Err(StoreError::Invalid("没有这条申请".into())),
            };
            let mut app = existing;
            if app.status != "pending" {
                return Err(StoreError::Invalid("申请已处理".into()));
            }
            app.status = if approved { "approved" } else { "rejected" }.into();
            app.decided_ms = now;
            let val = serde_json::to_vec(&app).map_err(|e| StoreError::Db(e.to_string()))?;
            t.insert(domain.as_bytes(), val.as_slice()).map_err(db_err)?;
            app
        };
        wtx.commit().map_err(db_err)?;
        Ok(app)
    }

    /// 停用或重新启用。不删除记录。域名不存在时返回 false。
    pub fn set_disabled(&self, domain: &str, disabled: bool) -> Result<bool> {
        let domain = normalize_domain(domain)?;
        let now = now_ms();
        let wtx = self.db.begin_write().map_err(db_err)?;
        let found = {
            let mut t = wtx.open_table(DOMAINS).map_err(db_err)?;
            let existing = match t.get(domain.as_bytes()).map_err(db_err)? {
                Some(v) => {
                    let rec: DomainRec = serde_json::from_slice(v.value())
                        .map_err(|e| StoreError::Db(e.to_string()))?;
                    Some(rec)
                }
                None => None,
            };
            if let Some(mut rec) = existing {
                rec.disabled = disabled;
                rec.updated_ms = now;
                let val = serde_json::to_vec(&rec).map_err(|e| StoreError::Db(e.to_string()))?;
                t.insert(domain.as_bytes(), val.as_slice()).map_err(db_err)?;
                true
            } else {
                false
            }
        };
        wtx.commit().map_err(db_err)?;
        Ok(found)
    }

    pub fn resolve(&self, domain: &str) -> Result<Option<DomainRec>> {
        let domain = normalize_domain(domain)?;
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(DOMAINS).map_err(db_err)?;
        match t.get(domain.as_bytes()).map_err(db_err)? {
            Some(v) => {
                let rec = serde_json::from_slice(v.value()).map_err(|e| StoreError::Db(e.to_string()))?;
                Ok(Some(rec))
            }
            None => Ok(None),
        }
    }

    pub fn list(&self) -> Result<Vec<DomainRec>> {
        let rtx = self.db.begin_read().map_err(db_err)?;
        let t = rtx.open_table(DOMAINS).map_err(db_err)?;
        let mut out = Vec::new();
        for item in t.iter().map_err(db_err)? {
            let (_k, v) = item.map_err(db_err)?;
            let rec: DomainRec =
                serde_json::from_slice(v.value()).map_err(|e| StoreError::Db(e.to_string()))?;
            out.push(rec);
        }
        out.sort_by(|a, b| a.domain.cmp(&b.domain));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "nm-domain-{}-{}.redb",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn register_resolve_conflict_disable() {
        let path = tmp();
        let reg = Registry::open(&path).unwrap();
        let pk = "ab".repeat(32);
        let other = "cd".repeat(32);
        assert_eq!(
            reg.register("Acme.Mesh", &pk, "Ops@Acme.Mesh").unwrap(),
            RegisterOutcome::Created
        );
        assert_eq!(
            reg.register("acme.mesh.", &pk, "ops@acme.mesh").unwrap(),
            RegisterOutcome::Unchanged
        );
        let got = reg.resolve("ACME.mesh").unwrap().unwrap();
        assert_eq!(got.domain, "acme.mesh");
        assert_eq!(got.pubkey, pk);
        assert_eq!(got.email, "ops@acme.mesh");
        assert_eq!(
            reg.register("acme.mesh", &pk, "admin@acme.mesh").unwrap(),
            RegisterOutcome::Unchanged
        );
        assert_eq!(reg.resolve("acme.mesh").unwrap().unwrap().email, "admin@acme.mesh");
        assert!(matches!(
            reg.register("acme.mesh", &other, "other@acme.mesh"),
            Err(StoreError::Taken(_))
        ));
        assert!(reg.set_disabled("acme.mesh", true).unwrap());
        let parked = reg.resolve("acme.mesh").unwrap().unwrap();
        assert!(parked.disabled);
        assert_eq!(parked.pubkey, pk);
        assert!(matches!(
            reg.register("acme.mesh", &other, "other@acme.mesh"),
            Err(StoreError::Taken(_))
        ));
        assert_eq!(
            reg.register("acme.mesh", &pk, "ops@acme.mesh").unwrap(),
            RegisterOutcome::Unchanged
        );
        assert!(!reg.resolve("acme.mesh").unwrap().unwrap().disabled);
        assert!(reg.set_disabled("acme.mesh", true).unwrap());
        assert!(!reg.set_disabled("missing.mesh", true).unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_bad_names() {
        assert!(normalize_domain("jeff").is_err());
        assert!(normalize_domain("localhost.local").is_err());
        assert!(normalize_pubkey("abcd").is_err());
        assert!(normalize_pubkey(&"ab".repeat(32)).is_ok());
        assert!(normalize_email("ops@acme.mesh").is_ok());
        assert!(normalize_email("not-an-email").is_err());
    }
}

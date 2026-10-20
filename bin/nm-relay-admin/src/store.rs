//! 持久化：拉黑名单 + 观测到的对等节点（来自 iroh-relay access.http 回调）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeerSighting {
    /// 节点公钥（iroh EndpointId 的 Display 字符串，通常为 z-base-32）。
    pub id: String,
    /// 首次见到（unix 秒）。
    pub first_seen: u64,
    /// 最近一次 access 回调（unix 秒）。
    pub last_seen: u64,
    /// 累计 access 检查次数（≈ 连接尝试次数）。
    pub connects: u64,
    /// 最近一次是否被放行。
    pub last_allowed: bool,
    /// 可选备注。
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Persist {
    /// 被拉黑的 endpoint id（与 iroh Display 字符串一致；也接受 64 hex）。
    blacklist: Vec<String>,
    peers: HashMap<String, PeerSighting>,
}

pub struct Store {
    path: PathBuf,
    inner: Mutex<Persist>,
}

impl Store {
    pub fn load_or_init(path: PathBuf) -> anyhow::Result<Self> {
        let persist = if let Ok(bytes) = std::fs::read(&path) {
            serde_json::from_slice(&bytes).unwrap_or_default()
        } else {
            Persist::default()
        };
        let s = Self {
            path,
            inner: Mutex::new(persist),
        };
        s.persist()?;
        Ok(s)
    }

    fn persist(&self) -> anyhow::Result<()> {
        let data = self.inner.lock().unwrap().clone();
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, serde_json::to_vec_pretty(&data)?)?;
        Ok(())
    }

    /// 规范化 id：去空白；若是 64 hex 则小写。
    pub fn normalize_id(id: &str) -> String {
        let t = id.trim();
        if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
            t.to_ascii_lowercase()
        } else {
            t.to_string()
        }
    }

    pub fn is_banned(&self, id: &str) -> bool {
        let id = Self::normalize_id(id);
        let g = self.inner.lock().unwrap();
        g.blacklist.iter().any(|b| Self::normalize_id(b) == id || ids_equivalent(b, &id))
    }

    pub fn ban(&self, id: &str, note: Option<&str>) -> anyhow::Result<()> {
        let id = Self::normalize_id(id);
        if id.is_empty() {
            anyhow::bail!("空 endpoint id");
        }
        {
            let mut g = self.inner.lock().unwrap();
            if !g.blacklist.iter().any(|b| ids_equivalent(b, &id)) {
                g.blacklist.push(id.clone());
            }
            let now = now_secs();
            let e = g.peers.entry(id.clone()).or_insert_with(|| PeerSighting {
                id: id.clone(),
                first_seen: now,
                last_seen: now,
                connects: 0,
                last_allowed: false,
                note: String::new(),
            });
            e.last_allowed = false;
            if let Some(n) = note {
                if !n.is_empty() {
                    e.note = n.to_string();
                }
            }
        }
        self.persist()
    }

    pub fn unban(&self, id: &str) -> anyhow::Result<()> {
        let id = Self::normalize_id(id);
        {
            let mut g = self.inner.lock().unwrap();
            g.blacklist.retain(|b| !ids_equivalent(b, &id));
        }
        self.persist()
    }

    pub fn blacklist(&self) -> Vec<String> {
        self.inner.lock().unwrap().blacklist.clone()
    }

    /// access.http 回调：记录观测并返回是否放行。
    pub fn on_access_check(&self, id: &str) -> bool {
        let id = Self::normalize_id(id);
        if id.is_empty() {
            return false;
        }
        let now = now_secs();
        let (allowed, should_persist) = {
            let mut g = self.inner.lock().unwrap();
            let allowed = !g
                .blacklist
                .iter()
                .any(|b| ids_equivalent(b, &id));
            let e = g.peers.entry(id.clone()).or_insert_with(|| PeerSighting {
                id: id.clone(),
                first_seen: now,
                last_seen: now,
                connects: 0,
                last_allowed: allowed,
                note: String::new(),
            });
            e.last_seen = now;
            e.connects = e.connects.saturating_add(1);
            e.last_allowed = allowed;
            // 每 10 次连接落盘一次，降低回调热路径 IO。
            (allowed, e.connects == 1 || e.connects % 10 == 0)
        };
        if should_persist {
            let _ = self.persist();
        }
        allowed
    }

    pub fn peers_snapshot(&self) -> Vec<PeerSighting> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<_> = g.peers.values().cloned().collect();
        v.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        // 标注 ban 状态到 note 字段旁：调用方用 blacklist 集合判断。
        v
    }

    pub fn peer_count(&self) -> usize {
        self.inner.lock().unwrap().peers.len()
    }
}

fn ids_equivalent(a: &str, b: &str) -> bool {
    Store::normalize_id(a) == Store::normalize_id(b)
}

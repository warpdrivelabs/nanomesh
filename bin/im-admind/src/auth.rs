//! im-admind 登录鉴权：固定用户 `admin` + 初始密码（首登强制改密），argon2 哈希持久化，内存会话。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use serde::{Deserialize, Serialize};

/// 初始密码：首次运行内置；`must_change=true`，首次登录后强制修改。
pub const DEFAULT_PASSWORD: &str = "imspace-admin";
const SESSION_TTL: Duration = Duration::from_secs(8 * 3600);

#[derive(Serialize, Deserialize, Clone)]
struct Cred {
    username: String,
    phc: String, // argon2 PHC 串
    must_change: bool,
}

pub struct Auth {
    path: PathBuf,
    cred: Mutex<Cred>,
    sessions: Mutex<HashMap<String, Instant>>, // token -> 过期时刻
}

impl Auth {
    /// 载入凭据；不存在则用默认初始密码创建(`must_change=true`)。返回 (Auth, 是否用默认密码新建)。
    pub fn load_or_init(path: PathBuf) -> anyhow::Result<(Self, bool)> {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(cred) = serde_json::from_slice::<Cred>(&bytes) {
                return Ok((
                    Self {
                        path,
                        cred: Mutex::new(cred),
                        sessions: Mutex::new(HashMap::new()),
                    },
                    false,
                ));
            }
        }
        let cred = Cred {
            username: "admin".into(),
            phc: hash_password(DEFAULT_PASSWORD)?,
            must_change: true,
        };
        let auth = Self {
            path,
            cred: Mutex::new(cred),
            sessions: Mutex::new(HashMap::new()),
        };
        auth.persist()?;
        Ok((auth, true))
    }

    fn persist(&self) -> anyhow::Result<()> {
        let cred = self.cred.lock().unwrap().clone();
        std::fs::write(&self.path, serde_json::to_vec_pretty(&cred)?)?;
        Ok(())
    }

    /// 登录：校验通过则发一个会话 token，返回 (token, must_change)。
    pub fn login(&self, username: &str, password: &str) -> Option<(String, bool)> {
        let cred = self.cred.lock().unwrap().clone();
        if username != cred.username || !verify_password(&cred.phc, password) {
            return None;
        }
        let token = gen_token();
        self.sessions
            .lock()
            .unwrap()
            .insert(token.clone(), Instant::now() + SESSION_TTL);
        Some((token, cred.must_change))
    }

    /// 会话是否有效（未过期）。
    pub fn valid(&self, token: &str) -> bool {
        let mut s = self.sessions.lock().unwrap();
        match s.get(token) {
            Some(exp) if *exp > Instant::now() => true,
            Some(_) => {
                s.remove(token);
                false
            }
            None => false,
        }
    }

    /// 当前是否处于「首登需改密」。
    pub fn must_change(&self) -> bool {
        self.cred.lock().unwrap().must_change
    }

    /// 改密：验证旧密码后设置新密码、清 `must_change`。
    pub fn change_password(&self, old: &str, new: &str) -> anyhow::Result<()> {
        if new.chars().count() < 6 {
            anyhow::bail!("新密码至少 6 位");
        }
        {
            let mut cred = self.cred.lock().unwrap();
            if !verify_password(&cred.phc, old) {
                anyhow::bail!("原密码不正确");
            }
            cred.phc = hash_password(new)?;
            cred.must_change = false;
        }
        self.persist()?;
        Ok(())
    }

    pub fn logout(&self, token: &str) {
        self.sessions.lock().unwrap().remove(token);
    }
}

fn hash_password(pw: &str) -> anyhow::Result<String> {
    // argon2 0.6：hash_password 自动生成盐（getrandom，默认特性）。
    Ok(Argon2::default()
        .hash_password(pw.as_bytes())
        .map_err(|e| anyhow::anyhow!("hash: {e}"))?
        .to_string())
}

fn verify_password(phc: &str, pw: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(pw.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

fn gen_token() -> String {
    // 128 位随机会话 token（复用 argon2/password-hash 的 getrandom 盐生成，避免额外依赖）。
    argon2::password_hash::generate_salt()
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

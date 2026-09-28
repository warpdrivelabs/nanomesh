//! 登录鉴权，和 nm-admind 同一套：固定用户 `admin`、argon2、首登强制改密、内存会话。
//! Cookie 名用 `nmdomain`，避免和同机的管理台 `admind` 串会话。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use serde::{Deserialize, Serialize};

pub const DEFAULT_PASSWORD: &str = "nmspace-domain";
const SESSION_TTL: Duration = Duration::from_secs(8 * 3600);

#[derive(Serialize, Deserialize, Clone)]
struct Cred {
    username: String,
    phc: String,
    must_change: bool,
}

pub struct Auth {
    path: PathBuf,
    cred: Mutex<Cred>,
    sessions: Mutex<HashMap<String, Instant>>,
}

impl Auth {
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

    pub fn must_change(&self) -> bool {
        self.cred.lock().unwrap().must_change
    }

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
    Ok(Argon2::default()
        .hash_password(pw.as_bytes())
        .map_err(|e| anyhow::anyhow!("hash: {e}"))?
        .to_string())
}

fn verify_password(phc: &str, pw: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default().verify_password(pw.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

fn gen_token() -> String {
    argon2::password_hash::generate_salt()
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

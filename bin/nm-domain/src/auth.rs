//! 登录鉴权，和 nm-admind 同一套：固定用户 `admin`、随机初始密码、argon2、首登强制改密、内存会话。
//! Cookie 名用 `nmdomain`，避免和同机的管理台 `admind` 串会话。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use serde::{Deserialize, Serialize};

const SESSION_TTL: Duration = Duration::from_secs(8 * 3600);
const LOGIN_FAIL_WINDOW: Duration = Duration::from_secs(300);
const LOGIN_MAX_FAILS: u32 = 5;

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
    login_fails: Mutex<(Instant, u32)>,
}

impl Auth {
    pub fn load_or_init(path: PathBuf) -> anyhow::Result<(Self, Option<String>)> {
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(cred) = serde_json::from_slice::<Cred>(&bytes) {
            return Ok((
                Self {
                    path,
                    cred: Mutex::new(cred),
                    sessions: Mutex::new(HashMap::new()),
                    login_fails: Mutex::new((Instant::now(), 0)),
                },
                None,
            ));
        }
        let initial_password = gen_initial_password();
        let cred = Cred {
            username: "admin".into(),
            phc: hash_password(&initial_password)?,
            must_change: true,
        };
        let auth = Self {
            path,
            cred: Mutex::new(cred),
            sessions: Mutex::new(HashMap::new()),
            login_fails: Mutex::new((Instant::now(), 0)),
        };
        auth.persist()?;
        Ok((auth, Some(initial_password)))
    }

    fn persist(&self) -> anyhow::Result<()> {
        let cred = self.cred.lock().unwrap().clone();
        std::fs::write(&self.path, serde_json::to_vec_pretty(&cred)?)?;
        Ok(())
    }

    pub fn login(&self, username: &str, password: &str, src: &str) -> Option<(String, bool)> {
        {
            let mut lf = self.login_fails.lock().unwrap();
            if lf.0.elapsed() >= LOGIN_FAIL_WINDOW {
                *lf = (Instant::now(), 0);
            }
            if lf.1 >= LOGIN_MAX_FAILS {
                tracing::warn!(audit = true, event = "login", ok = false, reason = "rate_limited",
                    user = %username, src = %src, "DOMAIN ADMIN LOGIN 限流锁定");
                return None;
            }
        }
        let cred = self.cred.lock().unwrap().clone();
        if username != cred.username || !verify_password(&cred.phc, password) {
            let mut lf = self.login_fails.lock().unwrap();
            lf.1 += 1;
            let fails = lf.1;
            drop(lf);
            tracing::warn!(audit = true, event = "login", ok = false, reason = "bad_credentials",
                user = %username, src = %src, fails, "DOMAIN ADMIN LOGIN 失败");
            return None;
        }
        let token = gen_token();
        self.sessions
            .lock()
            .unwrap()
            .insert(token.clone(), Instant::now() + SESSION_TTL);
        *self.login_fails.lock().unwrap() = (Instant::now(), 0);
        tracing::info!(audit = true, event = "login", ok = true, user = %username, src = %src, "DOMAIN ADMIN LOGIN 成功");
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
                tracing::warn!(audit = true, event = "change_password", ok = false, reason = "bad_old_password", "DOMAIN ADMIN 改密失败");
                anyhow::bail!("原密码不正确");
            }
            cred.phc = hash_password(new)?;
            cred.must_change = false;
        }
        self.persist()?;
        tracing::info!(audit = true, event = "change_password", ok = true, "DOMAIN ADMIN 改密成功");
        Ok(())
    }

    pub fn logout(&self, token: &str) {
        if self.sessions.lock().unwrap().remove(token).is_some() {
            tracing::info!(audit = true, event = "logout", ok = true, "DOMAIN ADMIN 登出");
        }
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

fn gen_initial_password() -> String {
    let token = gen_token();
    format!("nm-{}", &token[..24.min(token.len())])
}

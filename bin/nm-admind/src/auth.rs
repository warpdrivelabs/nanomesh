//! nm-admind 登录鉴权：固定用户 `admin` + 随机初始密码（首登强制改密），argon2 哈希持久化，内存会话。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use serde::{Deserialize, Serialize};

const SESSION_TTL: Duration = Duration::from_secs(8 * 3600);
// P1-8 登录限流：滑动窗口内失败次数达上限则锁定该窗口，挡暴力破解。成功登录清零。
const LOGIN_FAIL_WINDOW: Duration = Duration::from_secs(300); // 5 分钟窗口
const LOGIN_MAX_FAILS: u32 = 5; // 窗口内最多 5 次失败，超出锁定至窗口结束

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
    // P1-8 登录失败滑动窗口：(窗口起点, 窗口内失败数)。单用户 admin → 实际是全局暴力破解闸。
    login_fails: Mutex<(Instant, u32)>,
}

impl Auth {
    /// 载入凭据；不存在则生成随机初始密码并创建(`must_change=true`)。
    /// 返回的 `Some(password)` 仅用于首次启动打印一次。
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

    /// 登录：校验通过则发一个会话 token，返回 (token, must_change)。
    /// P1-8：失败滑动窗口限流(挡暴力破解) + 成功/失败/锁定均记审计日志。`src` 为来源标识(IP/直连)。
    pub fn login(&self, username: &str, password: &str, src: &str) -> Option<(String, bool)> {
        // 限流闸：窗口内失败数达上限 → 直接拒绝（不验密码），记审计。
        {
            let mut lf = self.login_fails.lock().unwrap();
            if lf.0.elapsed() >= LOGIN_FAIL_WINDOW {
                *lf = (Instant::now(), 0); // 窗口过期重置
            }
            if lf.1 >= LOGIN_MAX_FAILS {
                tracing::warn!(audit = true, event = "login", ok = false, reason = "rate_limited",
                    user = %username, src = %src, "ADMIN LOGIN 限流锁定（疑似暴力破解）");
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
                user = %username, src = %src, fails, "ADMIN LOGIN 失败");
            return None;
        }
        let token = gen_token();
        self.sessions
            .lock()
            .unwrap()
            .insert(token.clone(), Instant::now() + SESSION_TTL);
        *self.login_fails.lock().unwrap() = (Instant::now(), 0); // 成功 → 清失败计数
        tracing::info!(audit = true, event = "login", ok = true, user = %username, src = %src, "ADMIN LOGIN 成功");
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
                tracing::warn!(audit = true, event = "change_password", ok = false, reason = "bad_old_password", "ADMIN 改密失败");
                anyhow::bail!("原密码不正确");
            }
            cred.phc = hash_password(new)?;
            cred.must_change = false;
        }
        self.persist()?;
        tracing::info!(audit = true, event = "change_password", ok = true, "ADMIN 改密成功");
        Ok(())
    }

    pub fn logout(&self, token: &str) {
        if self.sessions.lock().unwrap().remove(token).is_some() {
            tracing::info!(audit = true, event = "logout", ok = true, "ADMIN 登出");
        }
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

fn gen_initial_password() -> String {
    let token = gen_token();
    format!("nm-{}", &token[..24.min(token.len())])
}

// ── 本地访问认证前端（P1）：首启设主口令 / 解锁 / 上锁 + 启动路由。
//    未解锁不加载任何身份数据（后端命令门禁 + 前端锁屏）。

const PWD_SPECIAL = "!@#$%^&*()_+-=[]{}|;':\",./<>?`~";
function pwdPolicyError(p) {
  if (p.length < 8) return "主口令至少 8 位";
  if (!/[A-Z]/.test(p)) return "需包含大写字母";
  if (!/[a-z]/.test(p)) return "需包含小写字母";
  if (!/[0-9]/.test(p)) return "需包含数字";
  if (![...p].some((c) => PWD_SPECIAL.includes(c))) return "需包含特殊字符（如 !@#$%^&*）";
  return "";
}

function showLockView() {
  document.getElementById("main-view").style.display = "none";
  document.getElementById("login-view").style.display = "none";
  document.getElementById("lock-view").style.display = "flex";
}

function renderLock(mode) {
  const box = document.getElementById("lock-box");
  if (mode === "setup") {
    box.innerHTML = `
      <div class="lk-title">设置访问口令</div>
      <div class="lk-sub">首次使用：设一个<b>主口令</b>保护本机的身份私钥。去中心化<b>无法找回</b>，请牢记并做好备份。</div>
      <input id="lk-p1" type="password" class="lk-input" placeholder="主口令（≥8 位，含大小写 + 数字 + 特殊字符）">
      <input id="lk-p2" type="password" class="lk-input" placeholder="再次输入">
      <div class="lk-err" id="lk-err"></div>
      <button class="lk-btn" id="lk-go">设置并进入</button>`;
    box.querySelector("#lk-go").addEventListener("click", doSetup);
    box.querySelector("#lk-p2").addEventListener("keydown", (e) => { if (e.key === "Enter") doSetup(); });
  } else {
    box.innerHTML = `
      <div class="lk-title">解锁</div>
      <div class="lk-sub">输入主口令，解锁本机身份。</div>
      <input id="lk-p1" type="password" class="lk-input" placeholder="主口令">
      <div class="lk-err" id="lk-err"></div>
      <button class="lk-btn" id="lk-go">解锁</button>`;
    box.querySelector("#lk-go").addEventListener("click", doUnlock);
    box.querySelector("#lk-p1").addEventListener("keydown", (e) => { if (e.key === "Enter") doUnlock(); });
  }
  setTimeout(() => { const i = document.getElementById("lk-p1"); if (i) i.focus(); }, 40);
}

async function doSetup() {
  const p1 = document.getElementById("lk-p1").value, p2 = document.getElementById("lk-p2").value;
  const err = document.getElementById("lk-err");
  const pe = pwdPolicyError(p1); if (pe) { err.textContent = pe; return; }
  if (p1 !== p2) { err.textContent = "两次输入不一致"; return; }
  const btn = document.getElementById("lk-go"); btn.disabled = true; btn.textContent = "设置中…";
  try { await NM.inv("setup_master", { password: p1 }); afterUnlock(); }
  catch (e) { err.textContent = "设置失败：" + (e && e.message ? e.message : e); btn.disabled = false; btn.textContent = "设置并进入"; }
}
async function doUnlock() {
  const p1 = document.getElementById("lk-p1").value;
  const err = document.getElementById("lk-err");
  if (!p1) { err.textContent = "请输入主口令"; return; }
  const btn = document.getElementById("lk-go"); btn.disabled = true; btn.textContent = "解锁中…";
  try { await NM.inv("unlock", { password: p1 }); afterUnlock(); }
  catch (e) {
    err.textContent = e && e.message ? e.message : String(e);
    btn.disabled = false; btn.textContent = "解锁";
    const i = document.getElementById("lk-p1"); if (i) { i.value = ""; i.focus(); }
  }
}
function afterUnlock() {
  document.getElementById("lock-view").style.display = "none";
  if (typeof showLoginView === "function") showLoginView();
}

// 手动上锁（标题栏 🔒）：后端清零 VK + 断连，前端回锁屏。
async function lockApp() {
  try { await NM.inv("lock"); } catch (_) {}
  window.CURRENT_SVC = null;
  renderLock("unlock");
  showLockView();
}
window.lockApp = lockApp;
window.routeStart = routeStart;

// ── 启动路由：未设主口令→设置向导；已设未解锁→解锁；已解锁→登录页 ──
async function routeStart() {
  let st;
  try { st = await NM.inv("auth_status"); } catch (_) { st = { masterSet: false, unlocked: false }; }
  if (!st.masterSet) { renderLock("setup"); showLockView(); }
  else if (!st.unlocked) { renderLock("unlock"); showLockView(); }
  else if (typeof showLoginView === "function") showLoginView();
}
routeStart(); // defer 脚本：DOM 已就绪

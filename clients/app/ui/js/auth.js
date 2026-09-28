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
  seedLockSky();
}

function seedLockSky() {
  const far = document.getElementById("lock-stars-far");
  if (!far || far.childElementCount || typeof seedSpaceStars !== "function") return;
  seedSpaceStars(far, 78, 1, 1.5);
  seedSpaceStars(document.getElementById("lock-stars-mid"), 34, 1.5, 2.6);
  seedSpaceStars(document.getElementById("lock-band-stars"), 110, 0.7, 1.7);
  const host = document.getElementById("lock-shooters");
  for (let i = 0; host && i < 5; i++) {
    const el = document.createElement("span");
    el.className = "space-shooter";
    el.style.setProperty("--shoot-angle", (-32 + Math.random() * 18) + "deg");
    el.style.top = (4 + Math.random() * 84) + "%";
    el.style.left = "-6%";
    el.style.animationDuration = (5.2 + Math.random() * 3.4) + "s";
    el.style.animationDelay = (-Math.random() * 14) + "s";
    host.appendChild(el);
  }
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
  } else if (mode === "recover") {
    box.innerHTML = `
      <div class="lk-title">用恢复码重置</div>
      <div class="lk-sub">输入当初生成的 <b>24 词恢复码</b>（空格分隔）与新的主口令。</div>
      <textarea id="lk-mn" class="lk-input lk-ta" placeholder="24 词助记词，用空格分隔"></textarea>
      <input id="lk-p1" type="password" class="lk-input" placeholder="新主口令（≥8 + 大小写 + 数字 + 特殊字符）">
      <input id="lk-p2" type="password" class="lk-input" placeholder="确认新主口令">
      <div class="lk-err" id="lk-err"></div>
      <button class="lk-btn" id="lk-go">重置并解锁</button>
      <a class="lk-link" id="lk-back">← 返回</a>`;
    box.querySelector("#lk-go").addEventListener("click", doRecover);
    box.querySelector("#lk-back").addEventListener("click", () => renderLock("unlock"));
  } else {
    box.innerHTML = `
      <div class="lk-title">解锁</div>
      <div class="lk-sub">输入主口令，解锁本机身份。</div>
      <input id="lk-p1" type="password" class="lk-input" placeholder="主口令">
      <div class="lk-err" id="lk-err"></div>
      <button class="lk-btn" id="lk-go">解锁</button>
      <a class="lk-link" id="lk-forgot">忘记主口令？用恢复码重置</a>`;
    box.querySelector("#lk-go").addEventListener("click", doUnlock);
    box.querySelector("#lk-p1").addEventListener("keydown", (e) => { if (e.key === "Enter") doUnlock(); });
    box.querySelector("#lk-forgot").addEventListener("click", () => renderLock("recover"));
    applyCooldownUI(); // 若在冷却中，禁用并显示倒计时
  }
  setTimeout(() => { const i = document.getElementById("lk-p1") || document.getElementById("lk-mn"); if (i) i.focus(); }, 40);
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
  const err = document.getElementById("lk-err");
  const left = cooldownLeft();
  if (left > 0) { err.textContent = `尝试过多，请等待 ${Math.ceil(left / 1000)} 秒`; return; }
  const p1 = document.getElementById("lk-p1").value;
  if (!p1) { err.textContent = "请输入主口令"; return; }
  const btn = document.getElementById("lk-go"); btn.disabled = true; btn.textContent = "解锁中…";
  try { await NM.inv("unlock", { password: p1 }); clearFail(); afterUnlock(); }
  catch (e) {
    recordFail();
    err.textContent = e && e.message ? e.message : String(e);
    btn.disabled = false; btn.textContent = "解锁";
    const i = document.getElementById("lk-p1"); if (i) { i.value = ""; i.focus(); }
    applyCooldownUI();
  }
}

// 用恢复码重置主口令（P3）
async function doRecover() {
  const err = document.getElementById("lk-err");
  const mn = document.getElementById("lk-mn").value.trim();
  const p1 = document.getElementById("lk-p1").value, p2 = document.getElementById("lk-p2").value;
  if (!mn) { err.textContent = "请输入恢复码"; return; }
  const pe = pwdPolicyError(p1); if (pe) { err.textContent = pe; return; }
  if (p1 !== p2) { err.textContent = "两次新口令不一致"; return; }
  const btn = document.getElementById("lk-go"); btn.disabled = true; btn.textContent = "重置中…";
  try {
    await NM.inv("recover", { mnemonic: mn, newPassword: p1 });
    clearFail();
    if (window.toast) toast("已用恢复码重置主口令，请用新口令解锁");
    renderLock("unlock");
  } catch (e) {
    err.textContent = e && e.message ? e.message : String(e);
    btn.disabled = false; btn.textContent = "重置并解锁";
  }
}

// ── 解锁失败冷却（P3）：连续失败 ≥5 次指数退避，最长 5 分钟。 ──
function failState() { try { return JSON.parse(localStorage.getItem("nmspace-unlock-fails") || '{"count":0,"until":0}'); } catch (_) { return { count: 0, until: 0 }; } }
function saveFailState(s) { try { localStorage.setItem("nmspace-unlock-fails", JSON.stringify(s)); } catch (_) {} }
function cooldownLeft() { return Math.max(0, failState().until - Date.now()); }
function clearFail() { saveFailState({ count: 0, until: 0 }); }
function recordFail() {
  const s = failState(); s.count = (s.count || 0) + 1;
  if (s.count >= 5) s.until = Date.now() + Math.min(5 * 60000, 5000 * Math.pow(2, s.count - 5));
  saveFailState(s);
}
let _cooldownTimer = null;
function applyCooldownUI() {
  clearInterval(_cooldownTimer);
  const btn = () => document.getElementById("lk-go");
  const tick = () => {
    const left = cooldownLeft(), b = btn();
    if (left <= 0) { clearInterval(_cooldownTimer); if (b) { b.disabled = false; b.textContent = "解锁"; } return; }
    if (b) { b.disabled = true; b.textContent = `请等待 ${Math.ceil(left / 1000)}s`; }
  };
  if (cooldownLeft() > 0) { tick(); _cooldownTimer = setInterval(tick, 1000); }
}
function afterUnlock() {
  document.getElementById("lock-view").style.display = "none";
  if (typeof showLoginView === "function") showLoginView();
  resetIdle();
}

// 手动上锁（标题栏 🔒）：后端清零 VK + 断连，前端回锁屏。
async function lockApp() {
  clearTimeout(_idleTimer);
  try { await NM.inv("lock"); } catch (_) {}
  window.CURRENT_SVC = null;
  renderLock("unlock");
  showLockView();
}
window.lockApp = lockApp;
window.routeStart = routeStart;

// ── 自动锁定（P2）：空闲超时 + 可选失焦即锁。仅在已解锁视图内计时。 ──
let _idleTimer = null;
function autolockCfg() {
  try { return JSON.parse(localStorage.getItem("nmspace-autolock") || '{"minutes":10,"onBlur":false}'); }
  catch (_) { return { minutes: 10, onBlur: false }; }
}
function saveAutolockCfg(c) { try { localStorage.setItem("nmspace-autolock", JSON.stringify(c)); } catch (_) {} }
function isUnlockedView() {
  return document.getElementById("lock-view").style.display === "none" &&
    (document.getElementById("login-view").style.display !== "none" || document.getElementById("main-view").style.display !== "none");
}
function resetIdle() {
  clearTimeout(_idleTimer);
  const c = autolockCfg();
  const mins = Math.max(1, Number(c.minutes) || 10);
  _idleTimer = setTimeout(() => { if (isUnlockedView()) lockApp(); }, mins * 60000);
}
["mousemove", "keydown", "mousedown", "touchstart", "wheel"].forEach((ev) =>
  document.addEventListener(ev, () => { if (isUnlockedView()) resetIdle(); }, { passive: true }));
document.addEventListener("visibilitychange", () => {
  if (document.hidden && autolockCfg().onBlur && isUnlockedView()) lockApp();
});

// ── 安全设置弹窗（P2）：自动锁定 / 改主口令 / 加密备份导出导入 ──
function openSecModal() {
  const c = autolockCfg();
  document.getElementById("sec-al-min").value = c.minutes;
  document.getElementById("sec-al-blur").checked = !!c.onBlur;
  ["sec-old", "sec-new", "sec-new2", "sec-exp-pw", "sec-exp-out", "sec-imp-in", "sec-imp-pw", "sec-rec-out"].forEach((id) => {
    const el = document.getElementById(id); if (el) el.value = "";
  });
  document.getElementById("sec-audit-out").innerHTML = "";
  secMsg("");
  // 恢复码状态
  NM.inv("auth_status").then((st) => {
    const el = document.getElementById("sec-rec-status");
    if (el) el.textContent = st && st.hasRecovery ? "本机已设置恢复码。" : "本机尚未设置恢复码。";
  }).catch(() => {});
  document.getElementById("sec-modal").classList.add("on");
}
function closeSecModal() { document.getElementById("sec-modal").classList.remove("on"); }
function secMsg(t, ok) { const el = document.getElementById("sec-msg"); if (el) { el.textContent = t || ""; el.className = "sec-msg" + (ok ? " ok" : ""); } }
window.openSecModal = openSecModal;

function saveAutolock() {
  const minutes = Math.max(1, parseInt(document.getElementById("sec-al-min").value, 10) || 10);
  const onBlur = document.getElementById("sec-al-blur").checked;
  saveAutolockCfg({ minutes, onBlur });
  resetIdle();
  secMsg("自动锁定已保存", true);
}
async function doChangeMaster() {
  const o = document.getElementById("sec-old").value, n = document.getElementById("sec-new").value, n2 = document.getElementById("sec-new2").value;
  if (!o || !n) { secMsg("请输入旧口令与新口令"); return; }
  const pe = pwdPolicyError(n); if (pe) { secMsg(pe); return; }
  if (n !== n2) { secMsg("两次新口令不一致"); return; }
  try { await NM.inv("change_master", { old: o, new: n }); secMsg("主口令已修改", true); ["sec-old", "sec-new", "sec-new2"].forEach((id) => (document.getElementById(id).value = "")); }
  catch (e) { secMsg("修改失败：" + (e && e.message ? e.message : e)); }
}
async function doExport() {
  const pw = document.getElementById("sec-exp-pw").value;
  if (pw.length < 6) { secMsg("请设置 ≥6 位的备份口令"); return; }
  try { const blob = await NM.inv("export_backup", { password: pw }); document.getElementById("sec-exp-out").value = blob; secMsg("已导出，请复制并妥善保存这段备份串", true); }
  catch (e) { secMsg("导出失败：" + (e && e.message ? e.message : e)); }
}
async function doImport() {
  const blob = document.getElementById("sec-imp-in").value.trim(), pw = document.getElementById("sec-imp-pw").value;
  if (!blob || !pw) { secMsg("请粘贴备份串并输入其口令"); return; }
  try { const n = await NM.inv("import_backup", { blob, password: pw }); secMsg("已导入 " + n + " 个身份", true); if (typeof renderUserOptions === "function") renderUserOptions(); }
  catch (e) { secMsg("导入失败：" + (e && e.message ? e.message : e)); }
}
async function doGenerateRecovery() {
  try {
    const m = await NM.inv("generate_recovery");
    document.getElementById("sec-rec-out").value = m;
    document.getElementById("sec-rec-status").textContent = "已生成，请立即离线抄写；离开后不再显示。";
    secMsg("恢复码已生成，务必抄写保存", true);
  } catch (e) { secMsg("生成失败：" + (e && e.message ? e.message : e)); }
}
async function doViewAudit() {
  try {
    const lines = await NM.inv("read_audit");
    const box = document.getElementById("sec-audit-out");
    if (!lines.length) { box.innerHTML = '<div class="au-empty">暂无记录</div>'; return; }
    box.innerHTML = lines.map((ln) => {
      const sp = ln.indexOf(" ");
      const ts = Number(ln.slice(0, sp)) * 1000, ev = ln.slice(sp + 1);
      const t = isNaN(ts) ? "" : new Date(ts).toLocaleString();
      return `<div class="au-row"><span class="au-ev">${escapeHtml(ev)}</span><span class="au-ts">${escapeHtml(t)}</span></div>`;
    }).join("");
  } catch (e) { secMsg("读取失败：" + (e && e.message ? e.message : e)); }
}
function initSec() {
  const on = (id, fn) => { const el = document.getElementById(id); if (el) el.addEventListener("click", fn); };
  on("sec-close", closeSecModal);
  on("sec-al-save", saveAutolock);
  on("sec-cm-save", doChangeMaster);
  on("sec-exp-go", doExport);
  on("sec-imp-go", doImport);
  on("sec-rec-go", doGenerateRecovery);
  on("sec-audit-go", doViewAudit);
  on("sec-exp-copy", async () => {
    const v = document.getElementById("sec-exp-out").value;
    if (!v) return;
    try { await navigator.clipboard.writeText(v); secMsg("备份串已复制到剪贴板", true); } catch (_) { secMsg("复制失败，请手动选中复制"); }
  });
  const m = document.getElementById("sec-modal");
  if (m) m.addEventListener("click", (e) => { if (e.target === m) closeSecModal(); });
}
initSec();
async function routeStart() {
  if (window.UIStore) await window.UIStore.hydrate(); // 先从后端回灌 UI 状态（节点服务/身份名等）
  if (typeof window.renderNodeSvcList === "function") window.renderNodeSvcList(); // hydrate 后刷新节点服务面板
  let st;
  try { st = await NM.inv("auth_status"); } catch (_) { st = { masterSet: false, unlocked: false }; }
  if (!st.masterSet) { renderLock("setup"); showLockView(); }
  else if (!st.unlocked) { renderLock("unlock"); showLockView(); }
  else if (typeof showLoginView === "function") showLoginView();
}
routeStart(); // defer 脚本：DOM 已就绪

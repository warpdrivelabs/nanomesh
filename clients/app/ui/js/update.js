// ── 自动更新（与 cmx-agent 同款方案的前端闭环）──
// 壳命令 download_and_install 已自持：外环 4 轮退避重试 + 断点续传（.part）+ 回环流安装
// 全在壳内（update_common.rs）。本层只负责：确认窗 / 重试窗 / agreed 标记 / 30 分钟自动
// 再试的闭环；入口 = 标题栏「更新」按钮 + 用户菜单「检查更新」+ 启动静默检查。
// 服务端契约同 cmx-agent 门户（/agent-updates/latest.json + /report），更新源未部署时
// 壳 check 恒回无更新，本层零打扰。
let UPDATE_INFO = null;   // {version, notes, force} 或 null（无可用更新）
let APP_VERSION = "";     // 懒取自壳 get_app_version（tauri.conf.json version 真源）
let _updating = false;    // 防重入：下载安装进行中不再触发（壳内 INSTALLING 双保险）
let _lastAutoTry = 0;     // 上次自动再试时刻（回前台再试的 5 分钟门槛）
let _autoTimer = null;    // 30 分钟自动再试 interval 句柄（只挂一次）
let _desktop = null;      // null=未知；mobile 端 get_app_version 未注册 → false，功能整体关闭

// 桌面判定：探测一次即缓存（mobile 上命令不存在 → 永久 false，不再产生无效 IPC）。
async function updIsDesktop() {
  if (_desktop !== null) return _desktop;
  if (!(window.__TAURI__ && window.__TAURI__.core)) { _desktop = false; return false; }
  try { await window.__TAURI__.core.invoke("get_app_version"); _desktop = true; }
  catch (e) { _desktop = false; }
  return _desktop;
}

// ── 「已同意」标记（R14）：点「立即更新」即写入；成功/撤回清除、失败保留（自动再试依据）。
// 存 localStorage：SPA 刷新 / 视图切换不丢；壳命令自持化，仅凭标记即可恢复重试。
function setUpdateAgreed(v) {
  try { v ? localStorage.setItem("nmspace-update-agreed", JSON.stringify({ version: v, ts: Date.now() }))
          : localStorage.removeItem("nmspace-update-agreed"); } catch (e) {}
}
function getUpdateAgreed() {
  try { return JSON.parse(localStorage.getItem("nmspace-update-agreed") || "null"); } catch (e) { return null; }
}

function renderUpdateBtn() {
  const btn = document.getElementById("tb-update-btn");
  if (btn) {
    if (UPDATE_INFO) { btn.hidden = false; document.getElementById("tb-update-label").textContent = "更新 " + UPDATE_INFO.version; }
    else { btn.hidden = true; }
  }
  // 设置面板「应用更新」状态行（#tp-update-btn 所在 pane）：随检查结果刷新
  const st = document.getElementById("tp-update-status");
  if (st) {
    if (UPDATE_INFO) st.textContent = "新版本 " + UPDATE_INFO.version + " 可用，可立即安装。";
    else if (APP_VERSION) st.textContent = "当前已是最新版本 v" + APP_VERSION + "。";
  }
}

// 标题栏「更新」按钮：不直接下载，先弹更新确认窗，用户点「立即更新」才装。
function updateBtnClick() {
  if (!UPDATE_INFO) return;
  showUpdateDialog(UPDATE_INFO);
}

// 下载 + 安装（壳内 4 轮重试 + 断点续传 + 回环安装，这里只等结果）。
// 进度：壳 emit 的 update_progress {received, total, attempt}（received 已含续传偏移）→ 按钮内联百分比；
// attempt>1 时文案标「重试N」（新一轮断点续传，百分比从上次断点起跳）。
// auto=true（自动再试）：失败只 toast 不弹窗（防打扰循环）；手动路径失败弹重试窗。
// 结果闭环（R14）：invoke Ok = 撤回（安装成功壳直接 restart，前端收不到返回）→ 清标记+按钮；
// Err = 4 轮全败 → 标记保留（自动再试兜底）。
async function applyUpdate(auto) {
  if (_updating) return;
  if (!auto && !UPDATE_INFO) return;
  _updating = true;
  const label = document.getElementById("tb-update-label");
  if (label) label.textContent = auto ? "自动重试中…" : "下载中…";
  (async () => {
    let unlisten = null;
    if (window.__TAURI__ && window.__TAURI__.event) {
      try { unlisten = await window.__TAURI__.event.listen("update_progress", (e) => {
        const p = e.payload || {};
        const att = p.attempt || 1;
        const n = p.total ? Math.round(p.received / p.total * 100) + "%"
                          : Math.round(p.received / 1024) + "KB";
        if (label) label.textContent = att > 1 ? "重试" + att + " " + n : "下载 " + n;
      }); } catch (err) {}
    }
    try {
      await window.__TAURI__.core.invoke("download_and_install");
      // Ok = 版本撤回（204）：清 agreed + 按钮（update_withdrawn 事件监听会 toast，这里静默）。
      setUpdateAgreed(null); UPDATE_INFO = null; _updating = false; if (unlisten) unlisten(); renderUpdateBtn();
    } catch (err) {
      // 4 轮重试全败：agreed 保留（30 分钟自动再试兜底）；手动弹重试窗，自动只 toast。
      _updating = false; if (unlisten) unlisten(); renderUpdateBtn();
      if (auto) { if (window.toast) toast("自动更新重试失败：" + err); }
      else { showRetryDialog(String(err)); }
    }
  })();
}

// 轻量弹窗（复用 sec-overlay 样式，自包含；浏览器原生 confirm/alert 在 WebView 里带
// 「网页说」前缀且样式出戏，禁用）。lock=true（强制更新）：无「稍后」、点遮罩/Esc 均不可关。
function updModal(opts) {
  const m = document.createElement("div"); m.className = "sec-overlay on";
  const x = opts.lock ? "" : `<button class="sec-x upd-x">${nmIcon("close")}</button>`;
  const actions = (opts.actions || []).map((a, i) =>
    `<button class="ns-btn${a.kind === "primary" ? " ns-primary" : ""}" data-i="${i}">${escapeHtml(a.text)}</button>`).join("");
  m.innerHTML = `<div class="sec-box"><div class="sec-head">${escapeHtml(opts.title)}${x}</div>
    <div class="sec-body"><div class="sec-sec"><div class="upd-notes">${opts.body || ""}</div>
    <div class="sec-actions">${actions}</div></div></div></div>`;
  document.body.appendChild(m);
  const done = (fn) => { m.remove(); document.removeEventListener("keydown", onKey); if (fn) fn(); };
  const onKey = (e) => { if (e.key === "Escape" && !opts.lock) done(null); };
  if (!opts.lock) {
    document.addEventListener("keydown", onKey);
    m.querySelector(".upd-x").onclick = () => done(null);
    m.addEventListener("click", (e) => { if (e.target === m) done(null); });
  }
  m.querySelectorAll(".sec-actions .ns-btn").forEach((b) => {
    b.onclick = () => done(opts.actions[+b.dataset.i].onClick);
  });
  return m;
}

// 更新提示弹窗：force 无「稍后」且不可关（数据通路已通，服务端 force 随版本走）。
function showUpdateDialog(info) {
  // 弹出即记「已提示」：30 分钟内静默检查不重复弹同一版本（不依赖用户点「稍后」；
  // 手动菜单检查不受限；force 不受限）
  try { localStorage.setItem("nmspace-update-snooze", JSON.stringify({ version: info.version, ts: Date.now() })); } catch (e) {}
  updModal({
    title: "发现新版本 " + (info.version || ""),
    body: escapeHtml(info.notes || "暂无更新说明"),
    lock: !!info.force,
    actions: (info.force ? [] : [{ text: "稍后" }]).concat([
      { text: "立即更新", kind: "primary", onClick: () => {
        setUpdateAgreed(info.version);   // R14：用户已同意安装——之后失败由 30 分钟自动再试兜底
        applyUpdate(false);
      } },
    ]),
  });
}

// 更新失败重试窗：重试=再调壳命令（外环重新走）；稍后=关窗（agreed 已保留，
// 30 分钟自动再试兜底；标题栏按钮常驻可手动重试）。
function showRetryDialog(reason) {
  updModal({
    title: "更新下载失败",
    body: escapeHtml("已自动重试 4 次仍未成功：\n" + reason),
    actions: [
      { text: "稍后" },
      { text: "重试", kind: "primary", onClick: () => applyUpdate(false) },
    ],
  });
}

// 「已提示」标记查询（写入口：showUpdateDialog，弹出即写）
function updateSnoozed(v) {
  try {
    const s = JSON.parse(localStorage.getItem("nmspace-update-snooze") || "null");
    return !!(s && s.version === v && Date.now() - s.ts < 30 * 60 * 1000);
  } catch (e) { return false; }
}

// 检查更新（用户菜单项 / 启动自动检查）：有新版 → 确认弹窗（用户确认才装）+ 标题栏按钮。
// silent=true：启动自动检查——失败无感（fail-closed），有新版同样弹窗；无新版零打扰。
// 30 分钟内已提示过的同版本，静默检查只出标题栏按钮不再弹（手动检查 = 用户点名，照弹）。
async function checkUpdate(silent) {
  if (!(await updIsDesktop())) { if (!silent && window.toast) toast("当前环境不支持应用内更新"); return; }
  if (!silent) document.getElementById("user-menu")?.classList.remove("on");
  try {
    const r = JSON.parse(await window.__TAURI__.core.invoke("check_update"));
    if (r && r.ok && r.data && r.data.update_available) {
      UPDATE_INFO = { version: r.data.version, notes: r.data.notes, force: r.data.force };
      renderUpdateBtn();
      if (UPDATE_INFO.force || !silent || !updateSnoozed(UPDATE_INFO.version)) showUpdateDialog(UPDATE_INFO);
    } else {
      UPDATE_INFO = null; renderUpdateBtn();
      if (!silent) {
        if (!APP_VERSION) { try { APP_VERSION = await window.__TAURI__.core.invoke("get_app_version"); } catch (e) {} }
        if (window.toast) toast("当前已是最新版本" + (APP_VERSION ? " v" + APP_VERSION : ""));
      }
    }
  } catch (err) {
    UPDATE_INFO = null; renderUpdateBtn();
    if (!silent && window.toast) {
      const msg = String(err);
      if (msg.includes("fallback platforms")) toast("更新服务暂未配置当前平台的安装包");
      else toast("检查更新失败：" + msg);
    }
  }
}
window.checkUpdate = checkUpdate;

// 启动自动检查（5s 后例行一次；登录/首屏完成后可见时兜底再查）。
// 窗口不可见时不查不弹——弹窗挂在不可见界面里会被错过。
let _updChecked = false;
function autoUpdateCheck() {
  if (_updChecked) return;
  if (document.visibilityState === "hidden") return;
  _updChecked = true;
  checkUpdate(true);
}
setTimeout(() => { updIsDesktop().then((d) => { if (d) autoUpdateCheck(); }); }, 5000);
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible") return;
  updIsDesktop().then((d) => { if (d) autoUpdateCheck(); });
});

// 版本撤回事件（壳外环 check 拿 204 时 emit）：清 agreed + 按钮 + toast（R13/R14 闭环）。
(async () => {
  if (window.__TAURI__ && window.__TAURI__.event) {
    try {
      await window.__TAURI__.event.listen("update_withdrawn", () => {
        setUpdateAgreed(null); UPDATE_INFO = null; renderUpdateBtn();
        if (window.toast) toast("更新已撤回");
      });
    } catch (e) {}
  }
})();

// 已同意版本的自动再试（P1）：30 分钟 interval + 回前台（距上次尝试 >5 分钟）。
// 只认 agreed 标记——壳命令自持化（自己 check），无需 UPDATE_INFO；刷新后标记仍在即恢复重试。
// 再次最终失败只 toast（applyUpdate(auto=true)），标题栏按钮常驻可手动重试。
function tryAutoRetry() {
  const a = getUpdateAgreed();
  if (!a || _updating) return;
  _lastAutoTry = Date.now();
  applyUpdate(true);
}
function ensureAutoRetryLoop() {
  if (_autoTimer) return;
  _autoTimer = setInterval(() => { if (getUpdateAgreed() && !_updating) tryAutoRetry(); }, 30 * 60 * 1000);
}
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible") return;
  if (getUpdateAgreed() && !_updating && Date.now() - _lastAutoTry > 5 * 60 * 1000) tryAutoRetry();
});
ensureAutoRetryLoop();

(function initUpdateUi() {
  const btn = document.getElementById("tb-update-btn");
  if (btn) btn.addEventListener("click", (e) => { e.stopPropagation(); updateBtnClick(); });
  // 设置面板「检查更新」按钮（他版 UI 面板，接同一手动检查入口）
  const tpBtn = document.getElementById("tp-update-btn");
  if (tpBtn) tpBtn.addEventListener("click", () => {
    if (_updating) return;
    tpBtn.disabled = true;
    Promise.resolve(checkUpdate(false)).finally(() => { tpBtn.disabled = false; });
  });
})();

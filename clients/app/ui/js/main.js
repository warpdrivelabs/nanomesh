// ── 外壳交互内核（抽自 cmx-agent main.js，仅留即时通讯所需）：
//    data-act 事件委托 + 主题/侧栏/splitter + toast。窗口三键在 platform.js（全局 win* 函数）。

function toggleSidebar() {
  document.body.classList.toggle("sidebar-collapsed");
  const on = document.body.classList.contains("sidebar-collapsed");
  const ico = document.getElementById("tb-side-ico");
  if (ico) ico.textContent = on ? "⇥" : "⇤";
}
function applyTheme(t) {
  document.documentElement.setAttribute("data-theme", t);
  const ico = document.getElementById("tb-theme-ico");
  if (ico) ico.textContent = t === "light" ? "☾" : "☀";
  try { localStorage.setItem("nm-theme", t); } catch (e) {}
}
function toggleTheme() {
  const cur = document.documentElement.getAttribute("data-theme") === "light" ? "light" : "dark";
  applyTheme(cur === "light" ? "dark" : "light");
}
function showPanel(name) {
  document.querySelectorAll(".activitybar .act").forEach((a) => a.classList.toggle("active", a.dataset.panel === name));
  document.querySelectorAll("aside .panel").forEach((p) => p.classList.toggle("on", p.id === "panel-" + name));
}

// ── Toast ──
function showToast(msg) {
  let t = document.getElementById("nm-toast");
  if (!t) { t = document.createElement("div"); t.id = "nm-toast"; t.className = "nm-toast"; document.body.appendChild(t); }
  t.textContent = msg; t.classList.add("on");
  clearTimeout(showToast._t); showToast._t = setTimeout(() => t.classList.remove("on"), 1800);
}
window.toast = showToast;

// ── data-act 事件委托（一个监听处理所有点击动作）──
const ACTIONS = {
  toggleSidebar, toggleTheme, showPanel,
  imRefresh: () => window.imRefresh && window.imRefresh(),
  disconnect: () => window.imDisconnect && window.imDisconnect(),
  winMinimize: () => window.winMinimize && window.winMinimize(),
  winToggleMaximize: () => window.winToggleMaximize && window.winToggleMaximize(),
  winClose: () => window.winClose && window.winClose(),
};
document.addEventListener("click", (e) => {
  const el = e.target.closest("[data-act]");
  if (!el) return;
  const act = el.dataset.act;
  if (act === "showPanel") { showPanel(el.dataset.panel); return; }
  if (ACTIONS[act]) ACTIONS[act](el);
});

// ── 初始化主题（读上次选择，缺省深色）──
applyTheme((() => { try { return localStorage.getItem("nm-theme") || "dark"; } catch (e) { return "dark"; } })());

// ── 左侧栏宽度可调：拖动 .splitter，宽度存 localStorage，clamp [200,480]（抄自 cmx-agent）──
(function initSplitter() {
  const sp = document.getElementById("splitter"), root = document.documentElement;
  if (!sp) return;
  try { const w = localStorage.getItem("nm-aside-w"); if (w) root.style.setProperty("--aside-w", w + "px"); } catch (e) {}
  let startX = 0, startW = 0, dragging = false;
  const curW = () => parseInt(getComputedStyle(root).getPropertyValue("--aside-w")) || 270;
  sp.addEventListener("pointerdown", (e) => {
    dragging = true; startX = e.clientX; startW = curW();
    sp.classList.add("dragging"); sp.setPointerCapture(e.pointerId);
    document.body.style.userSelect = "none";
  });
  sp.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    const w = Math.max(200, Math.min(480, startW + (e.clientX - startX)));
    root.style.setProperty("--aside-w", w + "px");
  });
  const end = () => { if (!dragging) return; dragging = false; sp.classList.remove("dragging"); document.body.style.userSelect = "";
    try { localStorage.setItem("nm-aside-w", curW()); } catch (e) {} };
  sp.addEventListener("pointerup", end); sp.addEventListener("pointercancel", end);
})();

// 窗口最大化态图标（win-max-btn）：platform.js 里已按 toggleMaximize 更新，这里不重复。

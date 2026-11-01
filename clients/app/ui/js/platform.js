// ── 平台窗口控制（Windows/Linux 自绘三键 + 缩放热区 + 无边框装饰）──
// ── Windows/Linux 窗口控件（最小化/最大化/关闭）+ 无边框缩放 ──
// macOS 用系统红绿灯（Overlay 标题栏），不建这些；仅非 macOS 且在 Tauri 壳内才生效（Web 壳无 __TAURI__.window）。
function _tauriWin(){
  return (window.__TAURI__ && window.__TAURI__.window) ? window.__TAURI__.window.getCurrentWindow() : null;
}
async function winMinimize(){ const w=_tauriWin(); if(w) await w.minimize(); }
async function winToggleMaximize(){
  const w=_tauriWin(); if(!w) return;
  await w.toggleMaximize();
  try{
    const maxed = await w.isMaximized();
    const ico = document.getElementById("win-max-btn")?.querySelector(".ico");
    if (ico) ico.innerHTML = nmIcon(maxed ? "restore" : "maximize");
  }catch(e){}
}
async function winClose(){ const w=_tauriWin(); if(w) await w.close(); }
// 无边框缩放热区：pointerdown 触发 Tauri 原生 startResizeDragging（比手搓 CSS resize 在各平台更可靠）。
// macOS 用系统边框缩放（decorations=Overlay 保留系统 resize），resize-handle 在 mac 上通过 CSS 隐藏，不干扰。
async function _startWinDrag() {
  const w = _tauriWin();
  if (!w || !w.startDragging) return;
  try { await w.startDragging(); } catch (_) {}
}
/** 明确不可拖的控件（点按要生效）。 */
function _winDragBlocked(t) {
  return !!t.closest(
    "button, a, input, select, textarea, .tab-x, .tab-more, .tb-ctrls, .win-ctrls, .tb-btn, .tl, .act, .act-user"
  );
}
// 标题栏 / 左侧活动栏空白处：立即拖窗。
document.querySelectorAll(".titlebar, .activitybar").forEach((el) => {
  el.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    if (_winDragBlocked(e.target)) return;
    // 顶栏标签：走阈值拖动（点按仍切换），不在这里立刻 startDragging。
    if (e.target.closest(".titlebar .tab")) return;
    _startWinDrag();
  });
});
// 顶栏标签：按下后移动超过阈值则拖窗；单击/短按仍由 tabs.js 切换或关闭。
const _TAB_DRAG_PX = 5;
document.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  const tab = e.target.closest(".titlebar .tab");
  if (!tab || e.target.closest(".tab-x")) return;
  const x0 = e.clientX, y0 = e.clientY;
  let started = false;
  const onMove = (ev) => {
    if (started) return;
    if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < _TAB_DRAG_PX) return;
    started = true;
    cleanup();
    _startWinDrag();
  };
  const cleanup = () => {
    window.removeEventListener("pointermove", onMove, true);
    window.removeEventListener("pointerup", cleanup, true);
    window.removeEventListener("pointercancel", cleanup, true);
  };
  window.addEventListener("pointermove", onMove, true);
  window.addEventListener("pointerup", cleanup, true);
  window.addEventListener("pointercancel", cleanup, true);
}, true);
document.querySelectorAll(".resize-handle").forEach(h=>{
  h.addEventListener("pointerdown", async e=>{
    e.preventDefault();
    const w=_tauriWin(); if(!w) return;
    try{ await w.startResizeDragging(h.dataset.resize); }catch(e){}
  });
});
// 平台探测：非 macOS 才显示自绘窗口控件 + 缩放热区（decorations:false 仅在 tauri.windows/linux.conf.json 生效）。
// 用自定义 `platform` 命令（= std::env::consts::OS）而非 tauri-plugin-os：省一份插件依赖/首次联网编译。
(async function initPlatformChrome(){
  if(!(window.__TAURI__ && window.__TAURI__.core)) return; // Web 壳：系统浏览器窗口，不处理
  try{
    const platform = await window.__TAURI__.core.invoke("platform"); // "macos" | "windows" | "linux"
    document.body.classList.add("no-native-decor");
    const w = _tauriWin();
    if (w) { try { await w.setDecorations(false); } catch (_) {} }
    const wc = document.getElementById("win-ctrls");
    if (wc) wc.style.display = "flex";
    const lwc = document.getElementById("login-win-ctrls");
    if (lwc) lwc.style.display = "flex";
    const lds = document.querySelector(".login-drag-strip");
    if (lds) lds.style.display = "block";
    if (platform === "macos") document.body.classList.add("is-macos");
  }catch(e){}
})();

// 用户菜单

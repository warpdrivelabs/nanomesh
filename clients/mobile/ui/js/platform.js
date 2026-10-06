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
document.querySelectorAll(".tb-brand").forEach((el) => {
  el.addEventListener("mousedown", async (e) => {
    if (e.button !== 0) return;
    const w = _tauriWin();
    if (!w || !w.startDragging) return;
    try { await w.startDragging(); } catch (_) {}
  });
});
document.querySelectorAll(".resize-handle").forEach(h=>{
  h.addEventListener("pointerdown", async e=>{
    e.preventDefault();
    const w=_tauriWin(); if(!w) return;
    try{ await w.startResizeDragging(h.dataset.resize); }catch(e){}
  });
});
// 平台探测：区分 桌面(macOS/Windows/Linux) 与 移动(Android/iOS)。
// 移动端 → 加 body.mobile(/.tablet)，跳过桌面窗口装饰；桌面端 → 自绘三键 + 缩放热区(非 macOS)。
// 用自定义 `platform` 命令（= std::env::consts::OS）而非 tauri-plugin-os：省一份插件依赖/首次联网编译。
// 开发期可在桌面/浏览器用 ?mobile / ?mobile=phone / ?mobile=tablet 强制移动布局预览（无需模拟器）。
// 开发期强制移动布局的来源：?mobile[=phone|tablet] 或 localStorage['nm-force-mobile']=phone|tablet|1
function mbForce() {
  var m = /[?&]mobile(?:=([a-z]+))?(?:&|$)/.exec(location.search);
  if (m) return m[1] || "1";
  try { return localStorage.getItem("nm-force-mobile") || ""; } catch (e) { return ""; }
}
function mbApplyTablet() {
  var f = mbForce(), tablet;
  if (f === "phone") tablet = false;
  else if (f === "tablet") tablet = true;
  else tablet = Math.min(window.innerWidth, window.innerHeight) >= 600; // ~600dp 以上作平板双栏
  document.body.classList.toggle("tablet", tablet);
}
function mbEnterMobile(kind) {
  document.body.classList.add("mobile");
  document.body.classList.add(kind === "ios" ? "is-ios" : "is-android");
  mbApplyTablet();
  window.addEventListener("resize", mbApplyTablet);
}
(async function initPlatformChrome(){
  var forced = !!mbForce();
  if(!(window.__TAURI__ && window.__TAURI__.core)){ // Web/浏览器：无原生壳
    if (forced) mbEnterMobile("android");            // 仅用于布局预览
    return;
  }
  try{
    const platform = await window.__TAURI__.core.invoke("platform"); // macos|windows|linux|android|ios
    if (forced || platform === "android" || platform === "ios") {
      mbEnterMobile(platform === "ios" ? "ios" : "android");
      return; // 移动端：不画桌面窗口装饰
    }
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

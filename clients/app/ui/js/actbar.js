// ── 活动栏动态注册(VSCode 式)──
// 标签两级注册:① 内置表 ui/actbar.json(随 app 打包,改它+重构建=新增内置标签);
// ② 用户表 app_data/activitybar.json(运行时动态注册,落盘)。按 id 合并,用户表优先,order 升序。
// API 注册:window.Actbar.register(item) / tauri invoke("actbar_register",{item});
// 注册即落盘并 emit core://event{type:"actbar"} → 活动栏即刻重绘(动态安装,免重启)。
// item 类型: panel(左侧面板) | workbench(智能体工作台) | iframe(全区内嵌页,url 必填)。
(function () {
  // 内置兜底:fetch actbar.json 失败(极端)时保证活动栏不空。与 actbar.json 保持一致。
  const FALLBACK = [
    { id: "im", title: "即时通讯", icon: "chat", type: "panel", panel: "im", order: 10 },
    { id: "cmx", title: "智能体工作台", icon: "robot", type: "workbench", order: 20 },
    { id: "entities", title: "实体目录", icon: "contacts", type: "panel", panel: "entities", order: 30 },
    { id: "models", title: "大模型", icon: "llm", type: "panel", panel: "models", order: 40 },
    { id: "channels", title: "频道", icon: "channels", type: "panel", panel: "channels", order: 50 },
  ];
  let MERGED = [];

  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }

  async function loadBuiltin() {
    try {
      const r = await fetch("actbar.json");
      const j = await r.json();
      const items = Array.isArray(j) ? j : j.items;
      if (Array.isArray(items) && items.length) return items;
    } catch (_) {}
    return FALLBACK;
  }
  async function loadUser() {
    if (!window.NM || !NM.hasTauri || !NM.hasTauri()) return [];
    try { return JSON.parse((await NM.inv("actbar_load")) || "[]") || []; } catch (_) { return []; }
  }
  function mergeTables(builtin, user) {
    const map = {};
    builtin.forEach((it) => { if (it && it.id) map[it.id] = { ...it }; });
    user.forEach((it) => { if (it && it.id) map[it.id] = { ...(map[it.id] || {}), ...it }; });
    return Object.values(map)
      .filter((it) => !it.hidden)
      .sort((a, b) => (Number(a.order) || 0) - (Number(b.order) || 0));
  }

  // ── iframe 全区舞台(泛化自智能体工作台):每个 iframe 标签一个常驻子层,显隐不重建 ──
  function stageHost() { return document.getElementById("ext-stage"); }
  function showExt(item) {
    const host = stageHost();
    if (!host) return;
    let sub = host.querySelector(`[data-ext="${CSS.escape(item.id)}"]`);
    if (!sub) {
      sub = document.createElement("div");
      sub.className = "ext-sub";
      sub.dataset.ext = item.id;
      sub.innerHTML = `<iframe class="ext-frame" src="${esc(item.url || "about:blank")}"></iframe>`;
      host.appendChild(sub);
    }
    host.querySelectorAll(".ext-sub").forEach((el) => { el.hidden = el !== sub; });
    host.hidden = false;
    document.body.classList.add("cmx-full"); // 复用"全区接管隐藏侧栏"
    document.querySelectorAll(".activitybar .act").forEach((a) => a.classList.toggle("active", a.dataset.panel === "ext:" + item.id));
  }
  function hideExt() {
    const host = stageHost();
    if (host) host.hidden = true;
  }
  function openExt(item) {
    const draw = () => showExt(item);
    if (window.Tabs) {
      Tabs.open({ key: "ext:" + item.id, kind: "ext", title: item.title || item.id, ico: item.icon || "link", render: draw });
    } else draw();
  }
  // 舞台显隐随活跃 tab(cmx_agent.js 同款协议;两个监听互不冲突:各管各的 stage)。
  document.addEventListener("nm-tab-changed", (e) => {
    const key = e.detail && e.detail.key;
    if (typeof key === "string" && key.startsWith("ext:")) {
      const item = MERGED.find((x) => "ext:" + x.id === key);
      if (item) showExt(item);
    } else hideExt();
  });

  function render() {
    const box = document.getElementById("actbar-items");
    if (!box) return;
    box.innerHTML = MERGED.map((it) => {
      const ico = typeof nmIcon === "function" ? nmIcon(it.icon || "file") : "";
      if (it.type === "iframe") {
        return `<div class="act" data-actbar-id="${esc(it.id)}" data-panel="ext:${esc(it.id)}" title="${esc(it.title || it.id)}">${ico}</div>`;
      }
      const panel = it.type === "workbench" ? "cmx" : (it.panel || it.id);
      return `<div class="act${panel === "im" ? " active" : ""}" id="act-${esc(it.id)}" data-act="showPanel" data-panel="${esc(panel)}" title="${esc(it.title || it.id)}">${ico}</div>`;
    }).join("");
    // iframe 型自绑点击(panel/workbench 型走 main.js 的 data-act 事件委托)。
    box.querySelectorAll("[data-actbar-id]").forEach((el) => {
      el.addEventListener("click", () => {
        const item = MERGED.find((x) => x.id === el.dataset.actbarId);
        if (item) openExt(item);
      });
    });
  }

  async function reload() {
    const [b, u] = await Promise.all([loadBuiltin(), loadUser()]);
    MERGED = mergeTables(b, u);
    render();
    // 重绘会丢掉挂在 act-im 上的未读角标,补画一次。
    if (typeof paintActUnread === "function") paintActUnread();
  }

  // 公开 API:动态安装/卸载标签(落盘+全端即刻生效)。
  window.Actbar = {
    list: () => MERGED.slice(),
    register: async (item) => { await NM.inv("actbar_register", { item }); },
    remove: async (id) => { await NM.inv("actbar_remove", { id }); },
    reload,
  };

  if (window.NM && NM.on) {
    NM.on("core://event", (ev) => { if (ev && ev.type === "actbar") reload(); });
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", reload);
  else reload();
})();

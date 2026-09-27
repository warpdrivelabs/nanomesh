// ── 主区多 tab 工作区（参考 cmx-agent 样式）──
// 每个打开的项目（会话/实体详情/群详情/节点服务详情）是一个 tab。
// 本实现共用单个 #conv 内容区：切 tab 时调用该 tab 的 render() 重绘（数据从各自缓存重建）。
(function () {
  let TABS = [];   // {key, kind, title, ico, render}
  let ACTIVE = null;

  const strip = () => document.getElementById("tabstrip");
  const bar = () => document.getElementById("tabbar");
  const find = (k) => TABS.find((t) => t.key === k);
  const esc = (s) => (typeof escapeHtml === "function" ? escapeHtml(s) : String(s == null ? "" : s));

  function renderStrip() {
    const s = strip();
    if (!s) return;
    s.innerHTML = TABS.map((t) => `
      <div class="tab${t.key === ACTIVE ? " active" : ""}" data-k="${encodeURIComponent(t.key)}" title="${esc(t.title)}">
        <span class="tab-ico">${t.ico || "📄"}</span>
        <span class="tab-t">${esc(t.title)}</span>
        <span class="tab-x" title="关闭">✕</span>
      </div>`).join("");
    s.querySelectorAll(".tab").forEach((el) => {
      const k = decodeURIComponent(el.dataset.k);
      el.addEventListener("click", (e) => { if (e.target.classList.contains("tab-x")) close(k); else activate(k); });
      el.addEventListener("auxclick", (e) => { if (e.button === 1) { e.preventDefault(); close(k); } }); // 中键关闭
    });
    if (bar()) bar().style.display = TABS.length ? "" : "none";
  }

  // 打开或聚焦一个 tab。render：把内容画进 #conv 的函数（可被重复调用）。
  function open(spec) {
    const t = find(spec.key);
    if (t) {
      if (spec.title != null) t.title = spec.title;
      if (spec.ico != null) t.ico = spec.ico;
      if (spec.render) t.render = spec.render;
      activate(spec.key);
      return;
    }
    TABS.push({ key: spec.key, kind: spec.kind, title: spec.title, ico: spec.ico, render: spec.render });
    activate(spec.key);
  }

  function activate(key) {
    const t = find(key);
    if (!t) return;
    ACTIVE = key;
    renderStrip();
    try { t.render && t.render(); } catch (e) { console.error("tab render", e); }
  }

  function close(key) {
    const idx = TABS.findIndex((t) => t.key === key);
    if (idx < 0) return;
    TABS.splice(idx, 1);
    if (ACTIVE === key) {
      const next = TABS[idx] || TABS[idx - 1];
      if (next) activate(next.key);
      else {
        ACTIVE = null;
        renderStrip();
        const conv = document.getElementById("conv");
        if (conv) conv.innerHTML = '<div class="im-center"><div class="ico">💬</div><div class="txt">选择左侧的会话 / 实体 / 群 / 节点服务</div><div class="sub">打开的项目会以标签页停靠在顶部</div></div>';
      }
    } else renderStrip();
  }

  function closeAll() { TABS = []; ACTIVE = null; renderStrip(); const conv = document.getElementById("conv"); if (conv) conv.innerHTML = ""; }
  // 某 tab 对应的目标被删除时移除该 tab（不重绘其它）。
  function remove(key) { const wasActive = ACTIVE === key; if (wasActive) return close(key); const i = TABS.findIndex((t) => t.key === key); if (i >= 0) { TABS.splice(i, 1); renderStrip(); } }
  function retitle(key, title, ico) { const t = find(key); if (t) { if (title != null) t.title = title; if (ico != null) t.ico = ico; renderStrip(); } }

  window.Tabs = { open, activate, close, closeAll, remove, retitle, active: () => ACTIVE, has: (k) => !!find(k) };
})();

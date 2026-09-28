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
    bindBar();
    s.innerHTML = TABS.map((t) => `
      <div class="tab${t.key === ACTIVE ? " active" : ""}" data-k="${encodeURIComponent(t.key)}" title="${esc(t.title)}">
        <span class="tab-ico">${window.nmIcon ? nmIcon(t.ico || "file") : ""}</span>
        <span class="tab-t">${esc(t.title)}</span>
        <span class="tab-x" title="关闭">${window.nmIcon ? nmIcon("close") : ""}</span>
      </div>`).join("");
    s.querySelectorAll(".tab").forEach((el) => {
      const k = decodeURIComponent(el.dataset.k);
      el.addEventListener("click", (e) => { if (e.target.closest(".tab-x")) close(k); else activate(k); });
      el.addEventListener("auxclick", (e) => { if (e.button === 1) { e.preventDefault(); close(k); } });
      el.addEventListener("contextmenu", (e) => { e.preventDefault(); e.stopPropagation(); openCtx(e.clientX, e.clientY, k); });
    });
    const active = s.querySelector(".tab.active");
    if (active) reveal(active);
    if (bar()) bar().style.display = TABS.length ? "" : "none";
    hideMenus();
    updateOverflow();
  }

  function bindBar() {
    const b = bar();
    if (!b || b.dataset.bound) return;
    b.dataset.bound = "1";
    const more = document.getElementById("tab-more");
    if (more) {
      more.innerHTML = window.nmIcon ? nmIcon("chevron") : "";
      more.addEventListener("click", (e) => { e.stopPropagation(); toggleOverflow(); });
    }
    const s = strip();
    if (s) s.addEventListener("scroll", () => { updateOverflow(); if (overOpen()) fillOverflow(); });
    document.addEventListener("pointerdown", (e) => {
      if (e.target.closest(".tab-ctx, .tab-over, .tab-more")) return;
      hideMenus();
    });
    document.addEventListener("keydown", (e) => { if (e.key === "Escape") hideMenus(); });
    if (window.ResizeObserver) new ResizeObserver(() => updateOverflow()).observe(b);
  }

  function reveal(el) {
    const s = strip();
    if (!s || !el) return;
    const er = el.getBoundingClientRect();
    const sr = s.getBoundingClientRect();
    if (er.left < sr.left) s.scrollLeft -= sr.left - er.left;
    else if (er.right > sr.right) s.scrollLeft += er.right - sr.right;
  }

  function hiddenTabs() {
    const s = strip();
    if (!s) return [];
    const sr = s.getBoundingClientRect();
    const out = [];
    s.querySelectorAll(".tab").forEach((el) => {
      const r = el.getBoundingClientRect();
      if (r.right > sr.right + 1 || r.left < sr.left - 1) {
        const t = find(decodeURIComponent(el.dataset.k));
        if (t) out.push(t);
      }
    });
    return out;
  }

  function updateOverflow() {
    const s = strip();
    const btn = document.getElementById("tab-more");
    if (!s || !btn) return;
    const overflow = TABS.length > 0 && s.scrollWidth > s.clientWidth + 1;
    btn.classList.toggle("on", overflow);
    btn.title = overflow ? "未显示的标签" : "";
    if (!overflow) hideOver();
  }

  function ctxEl() {
    let m = document.getElementById("tab-ctx");
    if (!m) {
      m = document.createElement("div");
      m.id = "tab-ctx";
      m.className = "tab-ctx";
      m.hidden = true;
      document.body.appendChild(m);
    }
    return m;
  }
  function overEl() { return document.getElementById("tab-over"); }
  function overOpen() { const m = overEl(); return !!(m && !m.hidden); }

  function hideMenus() { hideCtx(); hideOver(); }
  function hideCtx() { const m = document.getElementById("tab-ctx"); if (m) m.hidden = true; }
  function hideOver() {
    const m = overEl();
    if (m) m.hidden = true;
    const btn = document.getElementById("tab-more");
    if (btn) btn.classList.remove("open");
  }

  function openCtx(x, y, key) {
    hideOver();
    const idx = TABS.findIndex((t) => t.key === key);
    if (idx < 0) return;
    const m = ctxEl();
    const item = (act, label, disabled) => `<button type="button" class="tab-mi" data-act="${act}"${disabled ? " disabled" : ""}>${esc(label)}</button>`;
    m.innerHTML = [
      item("current", "关闭当前", false),
      item("left", "关闭左侧", idx === 0),
      item("right", "关闭右侧", idx === TABS.length - 1),
      '<div class="tab-sep"></div>',
      item("all", "关闭全部", false),
    ].join("");
    m.hidden = false;
    m.style.left = "0px";
    m.style.top = "0px";
    const r = m.getBoundingClientRect();
    m.style.left = Math.max(8, Math.min(x, window.innerWidth - r.width - 8)) + "px";
    m.style.top = Math.max(8, Math.min(y, window.innerHeight - r.height - 8)) + "px";
    m.querySelectorAll(".tab-mi").forEach((btn) => btn.addEventListener("click", () => {
      const act = btn.dataset.act;
      hideCtx();
      if (act === "current") close(key);
      else if (act === "left") closeKeys(TABS.slice(0, idx).map((t) => t.key), key);
      else if (act === "right") closeKeys(TABS.slice(idx + 1).map((t) => t.key), key);
      else if (act === "all") closeShown();
    }));
  }

  function toggleOverflow() {
    if (overOpen()) { hideOver(); return; }
    hideCtx();
    fillOverflow();
  }
  function fillOverflow() {
    const m = overEl();
    const btn = document.getElementById("tab-more");
    if (!m) return;
    const hidden = hiddenTabs();
    if (!hidden.length) { hideOver(); return; }
    m.innerHTML = hidden.map((t) => `
      <button type="button" class="tab-mi${t.key === ACTIVE ? " on" : ""}" data-k="${encodeURIComponent(t.key)}">
        <span class="tab-ico">${window.nmIcon ? nmIcon(t.ico || "file") : ""}</span>
        <span class="tab-mi-t">${esc(t.title)}</span>
      </button>`).join("");
    m.hidden = false;
    if (btn) btn.classList.add("open");
    m.querySelectorAll(".tab-mi").forEach((el) => el.addEventListener("click", () => {
      const k = decodeURIComponent(el.dataset.k);
      hideOver();
      activate(k);
    }));
  }

  function closeKeys(keys, keep) {
    if (!keys.length) return;
    const drop = new Set(keys);
    const activeGone = drop.has(ACTIVE);
    TABS = TABS.filter((t) => !drop.has(t.key));
    if (!TABS.length) { closeShown(); return; }
    if (activeGone && find(keep)) activate(keep);
    else renderStrip();
  }
  function closeShown() {
    TABS = [];
    ACTIVE = null;
    renderStrip();
    showEmpty();
    syncSideLists();
  }
  function showEmpty() {
    const conv = document.getElementById("conv");
    const tpl = document.getElementById("conv-home-tpl");
    if (!conv || !tpl) return;
    conv.replaceChildren(tpl.content.cloneNode(true));
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
    syncSideLists();
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
        showEmpty();
        syncSideLists();
      }
    } else renderStrip();
  }

  function closeAll() { TABS = []; ACTIVE = null; renderStrip(); showEmpty(); syncSideLists(); }
  // 某 tab 对应的目标被删除时移除该 tab（不重绘其它）。
  function remove(key) { const wasActive = ACTIVE === key; if (wasActive) return close(key); const i = TABS.findIndex((t) => t.key === key); if (i >= 0) { TABS.splice(i, 1); renderStrip(); } }
  function retitle(key, title, ico) { const t = find(key); if (t) { if (title != null) t.title = title; if (ico != null) t.ico = ico; renderStrip(); } }

  function syncSideLists() {
    if (window.Groups && Groups.syncSelection) Groups.syncSelection();
    if (window.Channels && Channels.syncSelection) Channels.syncSelection();
  }

  showEmpty();
  window.Tabs = { open, activate, close, closeAll, remove, retitle, showEmpty, active: () => ACTIVE, has: (k) => !!find(k) };
})();

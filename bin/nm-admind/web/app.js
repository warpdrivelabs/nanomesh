// nm-admind 前端：纯 Web Components 标准（自定义元素 + 原生 DOM，无框架、无构建）。

/* ---------------- helpers ---------------- */
const TOKEN_KEY = "admind-token";
function authHeaders(extra) {
  const headers = { ...(extra || {}) };
  const token = sessionStorage.getItem(TOKEN_KEY);
  if (token) headers.Authorization = "Bearer " + token;
  return headers;
}
async function api(path, opts = {}) {
  const res = await fetch(path, {
    method: opts.method || "GET",
    headers: authHeaders(opts.body ? { "Content-Type": "application/json" } : {}),
    body: opts.body ? JSON.stringify(opts.body) : undefined,
    credentials: "same-origin",
  });
  if (res.status === 401) {
    sessionStorage.removeItem(TOKEN_KEY);
    window.dispatchEvent(new CustomEvent("admind:logout"));
    throw new Error("会话已失效，请重新登录");
  }
  const text = await res.text();
  let data;
  try { data = text ? JSON.parse(text) : {}; } catch { data = { raw: text }; }
  if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
  return data;
}
const fmtBytes = (n) => {
  n = Number(n) || 0; const u = ["B", "KB", "MB", "GB", "TB"]; let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return `${n.toFixed(i ? 1 : 0)} ${u[i]}`;
};
const fmtDur = (s) => {
  s = Math.floor(Number(s) || 0);
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d) return `${d} 天 ${h} 时`; if (h) return `${h} 时 ${m} 分`; if (m) return `${m} 分 ${s % 60} 秒`;
  return `${s} 秒`;
};
const sinceMs = (ms) => (ms ? fmtDur((Date.now() - ms) / 1000) : "—");
const fmtTime = (ms) => {
  if (!ms) return "—";
  const d = new Date(ms);
  return Number.isNaN(d.getTime()) ? "—" : d.toLocaleString();
};
const fmtTimeSec = (sec) => (sec ? fmtTime(Number(sec) * 1000) : "—");
const fmtRtt = (ms) => {
  if (ms == null || ms === "") return "—";
  const n = Number(ms);
  if (!Number.isFinite(n)) return "—";
  return n < 10 ? n.toFixed(1) + " ms" : Math.round(n) + " ms";
};
const fmtRate = (bps) => {
  if (bps == null || !Number.isFinite(bps) || bps < 0) return "—";
  if (bps < 1024) return Math.round(bps) + " B/s";
  return fmtBytes(bps) + "/s";
};
const lossPct = (lost, dgrams) => {
  lost = Number(lost) || 0;
  dgrams = Number(dgrams) || 0;
  const base = dgrams + lost;
  if (!base) return null;
  return (lost / base) * 100;
};
const fmtLoss = (lost, dgrams) => {
  const p = lossPct(lost, dgrams);
  if (p == null) return "—";
  return (p < 0.1 && p > 0 ? "<0.1" : p.toFixed(p < 1 ? 2 : 1)) + "%";
};
const pathKindLabel = (k) => (k === "relay" ? "中继" : k === "direct" ? "直连" : "—");
/** 按 peer/connection id 采样瞬时吞吐（近几次刷新差分）。 */
const LINK_TRAF = new Map();
function sampleRate(id, rx, tx) {
  const now = Date.now();
  const key = String(id || "");
  if (!key) return { down: null, up: null };
  const arr = LINK_TRAF.get(key) || [];
  arr.push({ t: now, rx: Number(rx) || 0, tx: Number(tx) || 0 });
  while (arr.length > 12) arr.shift();
  LINK_TRAF.set(key, arr);
  if (arr.length < 2) return { down: null, up: null };
  const a = arr[0], b = arr[arr.length - 1];
  const dt = (b.t - a.t) / 1000;
  if (dt < 0.8) return { down: null, up: null };
  return { down: Math.max(0, (b.rx - a.rx) / dt), up: Math.max(0, (b.tx - a.tx) / dt) };
}
const shortId = (h) => (h && h.length > 16 ? `${h.slice(0, 10)}…${h.slice(-4)}` : h || "");
const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
async function copyToClipboard(text) {
  const value = String(text ?? "");
  if (!value) throw new Error("没有可复制的内容");
  if (navigator.clipboard && window.isSecureContext) {
    await navigator.clipboard.writeText(value);
    return;
  }
  const ta = document.createElement("textarea");
  ta.value = value;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.top = "0";
  ta.style.left = "0";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.focus();
  ta.select();
  const ok = document.execCommand("copy");
  ta.remove();
  if (!ok) throw new Error("浏览器拒绝写入剪贴板");
}
const COPY_ICON = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>';
function copyIconBtn(attr) {
  return `<button class="btn btn--icon" type="button" title="复制" aria-label="复制" ${attr}>${COPY_ICON}</button>`;
}
function bindCopy(button, text) {
  button.onclick = async () => {
    try {
      await copyToClipboard(text);
      button.classList.add("is-copied");
      button.title = "已复制";
      setTimeout(() => {
        if (!button.isConnected) return;
        button.classList.remove("is-copied");
        button.title = "复制";
      }, 1200);
    } catch (e) { alert("复制失败：" + (e && e.message ? e.message : e)); }
  };
}

const ICONS = {
  names: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c3 3 3 15 0 18M12 3c-3 3-3 15 0 18"/></svg>',
  overview: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="5" rx="1.5"/><rect x="14" y="12" width="7" height="9" rx="1.5"/><rect x="3" y="16" width="7" height="5" rx="1.5"/></svg>',
  connections: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M9 7 4 12l5 5M15 7l5 5-5 5"/></svg>',
  users: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="12" cy="8" r="3.2"/><path d="M5 20a7 7 0 0 1 14 0" stroke-linecap="round"/></svg>',
  storage: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/></svg>',
  traffic: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 17l5-6 4 3 4-7 5 5"/></svg>',
  system: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="4" y="4" width="16" height="12" rx="2"/><path d="M8 20h8M12 16v4" stroke-linecap="round"/></svg>',
  peers: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="6" cy="6" r="2.4"/><circle cx="18" cy="6" r="2.4"/><circle cx="12" cy="18" r="2.4"/><path d="M7.6 7.6 12 15.6 16.4 7.6M8.4 6h7.2" stroke-linecap="round"/></svg>',
  identity: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="5" width="18" height="14" rx="2"/><circle cx="8.5" cy="11" r="2"/><path d="M5.5 16c.6-1.6 4.2-1.6 6 0M14 9h4M14 12h4M14 15h2" stroke-linecap="round"/></svg>',
  password: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="4" y="10" width="16" height="10" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3M12 14v2" stroke-linecap="round"/></svg>',
};
const TABS = [
  { k: "overview", label: "总览" },
  { k: "connections", label: "连接监控" },
  { k: "users", label: "用户管理" },
  { k: "peers", label: "对等节点" },
  { k: "names", label: "域名管理" },
  { k: "storage", label: "存储管理" },
  { k: "traffic", label: "流量监控" },
  { k: "system", label: "系统资源" },
  { k: "identity", label: "服务标识" },
  { k: "password", label: "修改密码" },
];
// 无需轮询的静态面板（表单/稳定信息）——渲染一次即可，避免定时重渲染打断输入。
const STATIC_TABS = new Set(["identity", "password", "names"]);

/* ---------------- 根：鉴权路由 ---------------- */
class AdminApp extends HTMLElement {
  async connectedCallback() {
    this._onLogout = () => this.show("login");
    window.addEventListener("admind:logout", this._onLogout);
    try {
      const s = await fetch("/api/session", { credentials: "same-origin", headers: authHeaders() }).then((r) => r.json());
      this.show(!s.authed ? "login" : s.mustChange ? "change" : "shell");
    } catch { this.show("login"); }
  }
  disconnectedCallback() { window.removeEventListener("admind:logout", this._onLogout); }
  show(view) {
    this.innerHTML = "";
    const tag = { login: "admin-login", change: "admin-change", shell: "admin-shell" }[view];
    const el = document.createElement(tag);
    el.addEventListener("admind:navigate", (e) => this.show(e.detail));
    this.appendChild(el);
  }
}

/* ---------------- 登录 ---------------- */
class AdminLogin extends HTMLElement {
  connectedCallback() {
    this.innerHTML = `
    <div class="screen"><div class="card auth">
      <div class="brand"><img class="logo" src="/nanomesh-logo.png" alt="NANO MESH" /><div><h1>nmd 管理控制台</h1><p>请登录以继续</p></div></div>
      <form>
        <label class="field"><span>用户名</span><input name="u" value="admin" autocomplete="username"></label>
        <label class="field"><span>密码</span><input name="p" type="password" autocomplete="current-password"></label>
        <div class="err" hidden></div>
        <button class="btn btn--primary btn--block" type="submit">登录</button>
      </form>
    </div></div>`;
    const form = this.querySelector("form"), err = this.querySelector(".err");
    form.addEventListener("submit", async (e) => {
      e.preventDefault(); err.hidden = true;
      try {
        const r = await api("/api/login", { method: "POST", body: { username: form.u.value.trim(), password: form.p.value } });
        if (r.token) sessionStorage.setItem(TOKEN_KEY, r.token);
        this.dispatchEvent(new CustomEvent("admind:navigate", { detail: r.mustChange ? "change" : "shell", bubbles: true }));
      } catch (ex) { err.textContent = ex.message; err.hidden = false; }
    });
  }
}

/* ---------------- 首登强制改密 ---------------- */
class AdminChange extends HTMLElement {
  connectedCallback() {
    this.innerHTML = `
    <div class="screen"><div class="card auth">
      <div class="brand"><img class="logo" src="/nanomesh-logo.png" alt="NANO MESH" /><div><h1>修改初始密码</h1><p>首次登录，请设置新密码后进入</p></div></div>
      <form>
        <label class="field"><span>原密码</span><input name="o" type="password"></label>
        <label class="field"><span>新密码（至少 6 位）</span><input name="n" type="password"></label>
        <label class="field"><span>确认新密码</span><input name="c" type="password"></label>
        <div class="err" hidden></div>
        <button class="btn btn--primary btn--block" type="submit">保存并进入</button>
      </form>
    </div></div>`;
    const form = this.querySelector("form"), err = this.querySelector(".err");
    form.addEventListener("submit", async (e) => {
      e.preventDefault(); err.hidden = true;
      if (form.n.value !== form.c.value) { err.textContent = "两次新密码不一致"; err.hidden = false; return; }
      try {
        await api("/api/change-password", { method: "POST", body: { oldPassword: form.o.value, newPassword: form.n.value } });
        this.dispatchEvent(new CustomEvent("admind:navigate", { detail: "shell", bubbles: true }));
      } catch (ex) { err.textContent = ex.message; err.hidden = false; }
    });
  }
}

/* ---------------- 控制台外壳 ---------------- */
class AdminShell extends HTMLElement {
  connectedCallback() {
    this.innerHTML = `
    <div class="admin">
      <aside class="side">
        <div class="side__brand"><img class="logo" src="/nanomesh-logo.png" alt="NANO MESH" /><span>nmd 控制台</span></div>
        <nav class="side__nav"></nav>
        <div class="side__foot"><button class="btn btn--ghost btn--block side__logout">退出登录</button></div>
      </aside>
      <main class="main">
        <header class="top"><h2 class="top__title"></h2><span class="top__meta muted"></span></header>
        <section class="host"></section>
      </main>
    </div>`;
    const nav = this.querySelector(".side__nav");
    TABS.forEach((t) => {
      const b = document.createElement("button");
      b.className = "navitem"; b.dataset.k = t.k;
      b.innerHTML = `${ICONS[t.k] || ""}<span>${t.label}</span>`;
      b.onclick = () => this.select(t.k);
      nav.appendChild(b);
    });
    this.querySelector(".side__logout").onclick = async () => {
      try { await api("/api/logout", { method: "POST" }); } catch {}
      sessionStorage.removeItem(TOKEN_KEY);
      this.dispatchEvent(new CustomEvent("admind:navigate", { detail: "login", bubbles: true }));
    };
    this.select("overview");
  }
  disconnectedCallback() { clearInterval(this.timer); }
  select(k) {
    this.tab = k;
    this.querySelectorAll(".navitem").forEach((b) => b.classList.toggle("is-active", b.dataset.k === k));
    this.querySelector(".top__title").textContent = TABS.find((t) => t.k === k).label;
    clearInterval(this.timer);
    this.refresh();
    // 自动刷新（3s）会整块重渲染面板，若用户正在「对等节点」表单里输入/编辑会被刷掉。
    // 因此定时刷新前先判断是否正在编辑：正在编辑则跳过这次自动刷新（显式刷新不受影响）。
    if (!STATIC_TABS.has(k)) this.timer = setInterval(() => { if (!this._isEditing()) this.refresh(); }, 3000);
  }
  /// 用户是否正在当前面板里编辑（有则跳过自动刷新，避免刷掉输入内容）。
  _isEditing() {
    const host = this.querySelector(".host");
    if (!host) return false;
    // ① 焦点在本面板的输入框里（正在输入）——通用保护。
    const ae = document.activeElement;
    if (ae && host.contains(ae) && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA")) return true;
    // ② 对等节点表单已填了内容但尚未提交（即便已失焦也别刷掉；点「编辑」也会填充字段）。
    const form = host.querySelector(".peerform");
    if (form && [...form.querySelectorAll("input")].some((el) => el.value.trim() !== "")) return true;
    return false;
  }
  async refresh() {
    const host = this.querySelector(".host"), meta = this.querySelector(".top__meta");
    if (this.tab === "password") {
      meta.textContent = "";
      renderPassword(host);
      return;
    }
    if (this.tab === "names") {
      try {
        const [dom, names, pending] = await Promise.all([
          api("/api/names/domains"),
          api("/api/names/list"),
          api("/api/names/pending").catch((ex) => ({ items: [], error: ex.message })),
        ]);
        meta.textContent = "刷新于 " + new Date().toLocaleTimeString();
        renderNames(host, dom, names, pending, () => this.refresh());
      } catch (ex) {
        host.innerHTML = `<div class="err">${esc(ex.message)}</div>`;
      }
      return;
    }
    try {
      const data = await api("/api/" + this.tab);
      meta.textContent = "刷新于 " + new Date().toLocaleTimeString();
      renderPanel(this.tab, host, data, () => this.refresh());
    } catch (ex) {
      host.innerHTML = `<div class="err">${esc(ex.message)}</div>`;
    }
  }
}

/* ---------------- 面板渲染 ---------------- */
const metric = (label, val, sub) =>
  `<div class="metric"><div class="metric__label">${label}</div><div class="metric__val">${val ?? "—"}</div>${sub ? `<div class="metric__sub">${sub}</div>` : ""}</div>`;

let TRAF = []; // 流量历史采样
let USER_PAGE = 1;
const USER_PAGE_SIZE = 20;

function renderPanel(tab, host, d, refresh) {
  if (tab === "overview") {
    host.innerHTML = `<div class="grid">
      ${metric("在线连接", d.online)}
      ${metric("联邦对等", d.peers)}
      ${metric("实体 / 用户", d.entities)}
      ${metric("群组", d.groups)}
      ${metric("黑名单", d.banned)}
      ${metric("运行时长", fmtDur(d.uptime_s))}
      ${metric("内存 RSS", fmtBytes(d.rss_bytes))}
      ${metric("CPU", (Number(d.cpu_pct) || 0).toFixed(1) + "%")}
      ${metric("存储占用", fmtBytes(d.db_bytes))}
      ${metric("累计接收", fmtBytes(d.traffic?.bytes_in))}
      ${metric("累计发送", fmtBytes(d.traffic?.bytes_out))}
      ${metric("节点", `<code class="mono">${shortId(d.id)}</code>`)}
    </div>
    <div class="card restart-card">
      <div>
        <b>nmd 服务</b>
        <div class="muted">重启本机节点。进行中的连接会短暂断开。</div>
        <div class="restart-msg" hidden></div>
      </div>
      <button class="btn btn--danger" type="button" id="nmd-restart">重启 nmd</button>
    </div>`;
    const btn = host.querySelector("#nmd-restart");
    const msg = host.querySelector(".restart-msg");
    btn.onclick = async () => {
      if (!confirm("重启 nmd？当前连接会短暂断开。")) return;
      btn.disabled = true;
      btn.textContent = "正在重启…";
      msg.hidden = true;
      try {
        const r = await api("/api/nmd/restart", { method: "POST" });
        msg.className = "restart-msg " + (r.back ? "ok" : "err");
        msg.textContent = r.back ? "nmd 已重启并恢复。" : "已发出重启，节点尚未恢复，请稍后刷新。";
        msg.hidden = false;
        if (r.back) setTimeout(refresh, 800);
      } catch (e) {
        msg.className = "restart-msg err";
        msg.textContent = e.message;
        msg.hidden = false;
      } finally {
        btn.disabled = false;
        btn.textContent = "重启 nmd";
      }
    };
  } else if (tab === "connections") {
    const rows = (d.connections || []).map((c) => {
      const rate = sampleRate(c.id, c.bytes_rx, c.bytes_tx);
      const loss = fmtLoss(c.lost_packets, c.datagrams_tx);
      return `<tr>
      <td class="nowrap"><span class="copyline"><code class="mono">${shortId(c.id)}</code>${copyIconBtn(`data-copy="${esc(c.id)}"`)}</span></td>
      <td class="stack"><span>${fmtTime(c.since_unix_ms)}</span><span class="muted">${sinceMs(c.since_unix_ms)}</span></td>
      <td class="stack"><span>↓ ${fmtBytes(c.bytes_rx)}</span><span>↑ ${fmtBytes(c.bytes_tx)}</span>
        <span class="muted">${fmtRate(rate.down)} / ${fmtRate(rate.up)}</span></td>
      <td class="stack"><span>${fmtRtt(c.rtt_ms)}</span><span class="muted">丢包 ${loss}</span>
        <span class="muted">${pathKindLabel(c.path_kind)}${c.mtu ? ` · MTU ${c.mtu}` : ""}</span></td>
      <td class="mono muted small" title="${esc(c.remote_addr || "")}">${esc(c.remote_addr) || "—"}</td>
      <td><span class="badge">${esc(c.alpn)}</span></td>
      <td class="right"><button class="btn btn--sm btn--danger" data-kick="${c.id}">踢下线</button></td>
    </tr>`;
    }).join("");
    host.innerHTML = `<div class="card"><table class="tbl">
      <thead><tr><th>公钥</th><th>上线 / 在线</th><th>流量 / 带宽</th><th>链路质量</th><th>远端地址</th><th>协议</th><th></th></tr></thead>
      <tbody>${rows || '<tr><td colspan="7" class="muted center">暂无活动连接</td></tr>'}</tbody></table></div>`;
    host.querySelectorAll("[data-copy]").forEach((b) => bindCopy(b, b.dataset.copy));
    host.querySelectorAll("[data-kick]").forEach((b) => (b.onclick = async () => {
      b.disabled = true;
      try { await api("/api/kick", { method: "POST", body: { id: b.dataset.kick } }); refresh(); } catch (e) { alert(e.message); b.disabled = false; }
    }));
  } else if (tab === "users") {
    const all = d.users || [];
    const pages = Math.max(1, Math.ceil(all.length / USER_PAGE_SIZE));
    if (USER_PAGE > pages) USER_PAGE = pages;
    if (USER_PAGE < 1) USER_PAGE = 1;
    const start = (USER_PAGE - 1) * USER_PAGE_SIZE;
    const rows = all.slice(start, start + USER_PAGE_SIZE).map((u) => `<tr>
      <td><b>${esc(u.name) || "(无名)"}</b></td>
      <td><span class="badge">${esc(u.kind)}</span></td>
      <td class="nowrap"><span class="copyline"><code class="mono">${shortId(u.id)}</code>${copyIconBtn(`data-copy="${esc(u.id)}"`)}</span></td>
      <td>${u.banned ? '<span class="badge badge--danger">已封禁</span>' : '<span class="badge badge--ok">正常</span>'}</td>
      <td class="right">${u.banned
        ? `<button class="btn btn--sm" data-unban="${u.id}">解封</button>`
        : `<button class="btn btn--sm btn--danger" data-ban="${u.id}">拉黑</button>`}</td>
    </tr>`).join("");
    host.innerHTML = `<div class="card"><table class="tbl">
      <thead><tr><th>名称</th><th>类型</th><th>公钥</th><th>状态</th><th></th></tr></thead>
      <tbody>${rows || '<tr><td colspan="5" class="muted center">暂无实体，点连接后刷新</td></tr>'}</tbody></table>
      <div class="pager">
        <span class="muted">共 ${all.length} 条 · 每页 ${USER_PAGE_SIZE} 条 · 第 ${USER_PAGE} / ${pages} 页</span>
        <span>
          <button class="btn btn--sm" type="button" data-page="prev" ${USER_PAGE <= 1 ? "disabled" : ""}>上一页</button>
          <button class="btn btn--sm" type="button" data-page="next" ${USER_PAGE >= pages ? "disabled" : ""}>下一页</button>
        </span>
      </div></div>`;
    const act = (sel, url) => host.querySelectorAll(sel).forEach((b) => (b.onclick = async () => {
      b.disabled = true;
      try { await api(url, { method: "POST", body: { id: b.dataset[url.includes("unban") ? "unban" : "ban"] } }); refresh(); } catch (e) { alert(e.message); b.disabled = false; }
    }));
    act("[data-ban]", "/api/ban");
    act("[data-unban]", "/api/unban");
    host.querySelectorAll("[data-copy]").forEach((b) => bindCopy(b, b.dataset.copy));
    const go = (delta) => { const n = USER_PAGE + delta; if (n < 1 || n > pages) return; USER_PAGE = n; refresh(); };
    host.querySelector("[data-page=prev]").onclick = () => go(-1);
    host.querySelector("[data-page=next]").onclick = () => go(1);
  } else if (tab === "storage") {
    host.innerHTML = `<div class="grid">
      ${metric("实体", d.entities)}
      ${metric("群组", d.groups)}
      ${metric("数据库大小", fmtBytes(d.db_bytes))}
    </div>
    <div class="card"><div class="kv"><span class="muted">数据库路径</span><code class="mono">${esc(d.db_path)}</code></div></div>`;
  } else if (tab === "traffic") {
    TRAF.push({ t: Date.now(), i: Number(d.bytes_in) || 0, o: Number(d.bytes_out) || 0 });
    if (TRAF.length > 80) TRAF.shift();
    host.innerHTML = `<div class="grid">
      ${metric("累计接收", fmtBytes(d.bytes_in))}
      ${metric("累计发送", fmtBytes(d.bytes_out))}
      ${metric("接收条数", d.grams_in)}
      ${metric("发送条数", d.grams_out)}
    </div>
    <div class="card"><div class="card__title">吞吐（字节 / 秒）
      <span class="legend"><i class="dot dot--in"></i>接收 <i class="dot dot--out"></i>发送</span></div>
      <canvas class="chart"></canvas></div>`;
    drawChart(host.querySelector(".chart"));
  } else if (tab === "system") {
    host.innerHTML = `<div class="grid">
      ${metric("进程 PID", d.pid)}
      ${metric("内存 RSS", fmtBytes(d.rss_bytes))}
      ${metric("CPU 占用", (Number(d.cpu_pct) || 0).toFixed(1) + "%")}
      ${metric("运行时长", fmtDur(d.uptime_s))}
      ${metric("在线连接", d.online)}
    </div>
    <div class="card"><p class="muted">CPU / 内存为进程实时监控。真正的 CPU / 内存<strong>硬配额</strong>属部署级（systemd / cgroup），此处仅展示用量，不做强制限制。</p></div>`;
  } else if (tab === "identity") {
    host.innerHTML = `
    <div class="idcards">
      <div class="card idcard">
        <div class="idcard__head"><span class="badge badge--node">Node ID · 节点公钥</span></div>
        <img class="qr" src="/api/qr?kind=node&t=${Date.now()}" alt="node id QR" />
        <div class="idcard__val copyline"><code class="mono wrap">${esc(d.node_id)}</code>${copyIconBtn('data-copy="node"')}</div>
      </div>
      <div class="card idcard">
        <div class="idcard__head"><span class="badge badge--addr">完整地址 · NM_NODE_ADDR</span></div>
        <img class="qr" src="/api/qr?kind=addr&t=${Date.now()}" alt="addr QR" />
        <div class="idcard__val copyline"><code class="mono wrap">${esc(d.addr)}</code>${copyIconBtn('data-copy="addr"')}</div>
      </div>
    </div>
    <p class="muted">二维码按类型着色（<b style="color:var(--accent)">靛蓝=Node ID</b> / <b style="color:var(--sky)">天蓝=完整地址</b>），载荷带类型前缀 <code>nmspace:node:</code> / <code>nmspace:addr:</code> 以区分类型。</p>`;
    const copyNode = host.querySelector('[data-copy="node"]');
    const copyAddr = host.querySelector('[data-copy="addr"]');
    if (copyNode) bindCopy(copyNode, d.node_id || "");
    if (copyAddr) bindCopy(copyAddr, d.addr || "");
  } else if (tab === "peers") {
    renderPeersPanel(host, d, refresh);
  }
}

/** 对等节点面板：拓扑图跨刷新保活，表格/指标增量更新。 */
function renderPeersPanel(host, d, refresh) {
  const peers = d.peers || [];
  const fed = d.federation || "nmspace";
  const selfId = d.self_id || "";
  const onlineN = d.online ?? peers.filter((p) => p.connected).length;
  const offlineN = d.offline ?? Math.max(0, peers.length - onlineN);
  const ago = (s) => {
    if (!s) return "";
    const dlt = Math.max(0, Math.floor(Date.now() / 1000) - s);
    if (dlt < 60) return dlt + "s前";
    if (dlt < 3600) return Math.floor(dlt / 60) + "分前";
    if (dlt < 86400) return Math.floor(dlt / 3600) + "时前";
    return Math.floor(dlt / 86400) + "天前";
  };
  const rows = peers.map((p, i) => {
    const disc = p.source === "discovered";
    const badge = disc
      ? `<span class="badge badge--discovered">自动发现</span>`
      : `<span class="badge badge--manual">手工</span>`;
    const connBadge = p.connected
      ? `<span class="badge badge--ok">在线</span>`
      : `<span class="badge badge--off">离线</span>`;
    const rate = p.connected ? sampleRate(p.id, p.bytes_rx, p.bytes_tx) : { down: null, up: null };
    const loss = p.connected ? fmtLoss(p.lost_packets, p.datagrams_tx) : "—";
    const seen = p.last_seen
      ? `<span title="${esc(fmtTimeSec(p.last_seen))}">${ago(p.last_seen)}</span>`
      : `<span class="muted">—</span>`;
    const upTime = p.connected
      ? `<span class="stack"><span>${fmtTime(p.since_unix_ms)}</span><span class="muted">${sinceMs(p.since_unix_ms)}</span></span>`
      : `<span class="muted">—</span>`;
    const link = p.connected
      ? `<span class="stack">
          <span>${fmtRtt(p.rtt_ms)} · 丢包 ${loss}</span>
          <span class="muted">${pathKindLabel(p.path_kind)}${p.mtu ? ` · MTU ${p.mtu}` : ""}${p.cwnd != null ? ` · cwnd ${fmtBytes(p.cwnd)}` : ""}</span>
          ${p.remote_addr ? `<span class="muted mono small" title="${esc(p.remote_addr)}">${esc(p.remote_addr)}</span>` : ""}
        </span>`
      : `<span class="muted">无会话</span>`;
    const traf = p.connected
      ? `<span class="stack">
          <span>↓ ${fmtBytes(p.bytes_rx)} · ↑ ${fmtBytes(p.bytes_tx)}</span>
          <span class="muted">${fmtRate(rate.down)} / ${fmtRate(rate.up)}</span>
        </span>`
      : `<span class="muted">—</span>`;
    const metaBits = [p.address, p.email, p.mobile, p.gps].filter(Boolean).map(esc);
    const nameCell = `<td>
        <b>${esc(p.name) || '<span class="muted">—</span>'}</b>
        ${metaBits.length ? `<div class="muted small peer-meta">${metaBits.join(" · ")}</div>` : ""}
      </td>`;
    const actions = disc
      ? `<button class="btn btn--sm btn--danger" data-ban="${p.id}" title="永久排除：断开连接且不再被自动发现学回">封禁</button>
         <button class="btn btn--sm" data-del="${p.id}" title="瞬时移除；对方仍在广播会被再次发现">移除</button>`
      : `<button class="btn btn--sm" data-edit="${i}">编辑</button>
         <button class="btn btn--sm btn--danger" data-del="${p.id}">删除</button>`;
    return `<tr class="${p.connected ? "peer-online" : "peer-offline"}">
    ${nameCell}
    <td class="nowrap">${connBadge}</td>
    <td class="nowrap">${badge}</td>
    <td class="nowrap"><span class="badge badge--fed">${esc(p.federation || fed)}</span></td>
    <td class="nowrap"><span class="copyline"><code class="mono">${shortId(p.id)}</code>${copyIconBtn(`data-copy="${esc(p.id)}"`)}</span></td>
    <td class="nowrap">${upTime}</td>
    <td class="nowrap muted">${seen}</td>
    <td>${link}</td>
    <td>${traf}</td>
    <td class="right nowrap">${actions}</td>
  </tr>`;
  }).join("");

  const metricsHtml = `
    ${metric("对等总数", d.total ?? peers.length)}
    ${metric("在线", onlineN)}
    ${metric("离线", offlineN)}
    ${metric("本节点联邦", `<span class="badge badge--fed">${esc(fed)}</span>`)}`;

  const first = !host.querySelector(".peer-panel");
  if (first) {
    host.innerHTML = `
    <div class="peer-panel">
      <div class="grid peer-metrics">${metricsHtml}</div>
      <div class="card peer-map">
        <div class="card__title peer-map__title">节点连接传输图
          <span class="peer-map__legend">
            <span class="lg"><i class="lg--self"></i>本节点</span>
            <span class="lg"><i class="lg--on"></i>在线</span>
            <span class="lg"><i class="lg--off"></i>离线</span>
            <span class="lg"><i class="lg--relay"></i>中继路径</span>
            <span class="muted">线宽/粒子 ≈ 带宽 · 色调 ≈ RTT</span>
          </span>
        </div>
        <div class="peer-map__canvas-wrap">
          <canvas class="peer-map__canvas"></canvas>
          <div class="peer-map__tip"></div>
        </div>
      </div>
      <div class="card">
        <div class="card__title" id="pf-title">添加对等节点（手工/种子 · 运行时生效 · 无需重启 · 连接按 Node ID 发现）</div>
        <p class="muted" style="margin:-4px 0 12px">手工添加的即「种子」并作为 gossip 引导，将加入此联邦；其余成员经成员频道<b>自动发现</b>。连通性来自当前 QUIC 会话；链路 RTT / 丢包 / MTU / 带宽为实时采样。</p>
        <form class="peerform">
          <label class="field"><span>名称</span><input name="name" placeholder="如：北京机房 nmd"></label>
          <label class="field"><span>Node ID（64 位 hex，必填）</span><input name="id" spellcheck="false" placeholder="对端 node 公钥"></label>
          <label class="field"><span>物理地址（街道门牌等）</span><input name="address" placeholder="如：北京市海淀区 XX 路 8 号"></label>
          <div class="peergrid">
            <label class="field"><span>Email</span><input name="email" placeholder="ops@example.com"></label>
            <label class="field"><span>手机</span><input name="mobile" placeholder="+86 …"></label>
            <label class="field"><span>GPS</span><input name="gps" placeholder="39.90,116.40"></label>
          </div>
          <div class="err" hidden></div>
          <div class="peerbtns"><button class="btn btn--primary" type="submit">保存对等</button>
            <button class="btn btn--ghost" type="reset">清空</button></div>
        </form>
      </div>
      <div class="card card--scroll"><table class="tbl tbl--peers">
        <thead><tr>
          <th>名称</th><th>连通</th><th>来源</th><th>联邦</th><th>Node ID</th>
          <th>上线 / 在线</th><th>最近存活</th><th>链路质量</th><th>流量 / 带宽</th><th></th>
        </tr></thead>
        <tbody class="peer-tbody">${rows || '<tr><td colspan="10" class="muted center">暂无对等节点</td></tr>'}</tbody></table></div>
    </div>`;
    const form = host.querySelector(".peerform"), err = host.querySelector(".err");
    const setf = (k, v) => { const el = form.querySelector(`[name="${k}"]`); if (el) el.value = v || ""; };
    form.addEventListener("submit", async (e) => {
      e.preventDefault(); err.hidden = true;
      const fd = new FormData(form); const g = (k) => (fd.get(k) || "").toString().trim();
      const body = { id: g("id"), name: g("name"), address: g("address"), email: g("email"), mobile: g("mobile"), gps: g("gps") };
      if (!/^[0-9a-fA-F]{64}$/.test(body.id)) { err.textContent = "请填写 64 位十六进制 Node ID"; err.hidden = false; return; }
      try { await api("/api/add-peer", { method: "POST", body }); form.reset(); host.querySelector("#pf-title").textContent = "添加对等节点（手工/种子 · 运行时生效 · 无需重启 · 连接按 Node ID 发现）"; refresh(); }
      catch (ex) { err.textContent = ex.message; err.hidden = false; }
    });
    host._peerBindActions = () => {
      host.querySelectorAll("[data-edit]").forEach((b) => (b.onclick = () => {
        const p = host._peers[+b.dataset.edit];
        if (!p) return;
        setf("name", p.name); setf("id", p.id); setf("address", p.address);
        setf("email", p.email); setf("mobile", p.mobile); setf("gps", p.gps);
        host.querySelector("#pf-title").textContent = "编辑对等节点（Node ID 相同即覆盖保存）";
        form.scrollIntoView({ behavior: "smooth", block: "start" });
      }));
      host.querySelectorAll("[data-del]").forEach((b) => (b.onclick = async () => {
        if (!confirm("删除该对等节点？")) return;
        b.disabled = true;
        try { await api("/api/remove-peer", { method: "POST", body: { id: b.dataset.del } }); refresh(); }
        catch (ex) { alert(ex.message); b.disabled = false; }
      }));
      host.querySelectorAll("[data-copy]").forEach((b) => bindCopy(b, b.dataset.copy));
      host.querySelectorAll("[data-ban]").forEach((b) => (b.onclick = async () => {
        if (!confirm("封禁该节点？将断开其连接，并不再被自动发现学回（永久排除）。")) return;
        b.disabled = true;
        try {
          await api("/api/ban", { method: "POST", body: { id: b.dataset.ban } });
          await api("/api/remove-peer", { method: "POST", body: { id: b.dataset.ban } });
          refresh();
        } catch (ex) { alert(ex.message); b.disabled = false; }
      }));
    };
    const wrap = host.querySelector(".peer-map__canvas-wrap");
    host._peerGraph = new PeerGraph(wrap);
  } else {
    host.querySelector(".peer-metrics").innerHTML = metricsHtml;
    host.querySelector(".peer-tbody").innerHTML = rows || '<tr><td colspan="10" class="muted center">暂无对等节点</td></tr>';
  }
  host._peers = peers;
  host._peerBindActions();
  if (host._peerGraph) host._peerGraph.setData({ selfId, federation: fed, peers });
}

/**
 * 对等拓扑实时图：本节点居中，对等环绕；边表示会话，粒子表示传输，色调表示 RTT。
 */
class PeerGraph {
  constructor(wrap) {
    this.wrap = wrap;
    this.cv = wrap.querySelector("canvas");
    this.tip = wrap.querySelector(".peer-map__tip");
    this.ctx = this.cv.getContext("2d");
    this.selfId = "";
    this.federation = "";
    this.peers = [];
    this.nodes = new Map(); // id -> {x,y,vx,vy,...}
    this.particles = [];
    this.hoverId = null;
    this.t0 = performance.now();
    this._ro = new ResizeObserver(() => this._resize());
    this._ro.observe(wrap);
    this._onMove = (e) => this._pointer(e);
    this._onLeave = () => { this.hoverId = null; this.tip.classList.remove("is-on"); };
    this.cv.addEventListener("pointermove", this._onMove);
    this.cv.addEventListener("pointerleave", this._onLeave);
    this._resize();
    this._raf = requestAnimationFrame((t) => this._frame(t));
  }
  setData({ selfId, federation, peers }) {
    this.selfId = selfId || "";
    this.federation = federation || "";
    // 刷新时采样一次吞吐，动画帧只读缓存，避免污染差分窗口。
    this.peers = (peers || []).map((p) => {
      const rate = p.connected ? sampleRate(p.id, p.bytes_rx, p.bytes_tx) : { down: null, up: null };
      return { ...p, _rate: rate };
    });
    const ids = new Set(this.peers.map((p) => p.id));
    ids.add("__self__");
    for (const id of [...this.nodes.keys()]) {
      if (!ids.has(id)) this.nodes.delete(id);
    }
    if (!this.nodes.has("__self__")) {
      this.nodes.set("__self__", { x: 0, y: 0, vx: 0, vy: 0, r: 22 });
    }
    const n = this.peers.length || 1;
    this.peers.forEach((p, i) => {
      if (!this.nodes.has(p.id)) {
        const ang = (i / n) * Math.PI * 2 - Math.PI / 2;
        const rad = 0.62;
        this.nodes.set(p.id, {
          x: Math.cos(ang) * rad,
          y: Math.sin(ang) * rad,
          vx: 0, vy: 0, r: 16,
        });
      }
    });
  }
  _resize() {
    const dpr = window.devicePixelRatio || 1;
    const w = this.wrap.clientWidth || 600;
    const h = this.wrap.clientHeight || 360;
    this.w = w; this.h = h;
    this.cv.width = Math.floor(w * dpr);
    this.cv.height = Math.floor(h * dpr);
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  _xy(n) {
    return { x: this.w / 2 + n.x * Math.min(this.w, this.h) * 0.42, y: this.h / 2 + n.y * Math.min(this.w, this.h) * 0.42 };
  }
  _pointer(e) {
    const rect = this.cv.getBoundingClientRect();
    const x = e.clientX - rect.left, y = e.clientY - rect.top;
    let hit = null, best = 1e9;
    for (const [id, n] of this.nodes) {
      const p = this._xy(n);
      const d = Math.hypot(p.x - x, p.y - y);
      if (d < n.r + 8 && d < best) { best = d; hit = id; }
    }
    this.hoverId = hit;
    if (!hit || hit === "__self__") {
      if (hit === "__self__") {
        this.tip.innerHTML = `<b>本节点</b><div class="mono">${esc(shortId(this.selfId))}</div>
          <div class="row"><span>联邦</span><span>${esc(this.federation)}</span></div>
          <div class="row"><span>对等</span><span>${this.peers.length}</span></div>`;
        this._placeTip(e.clientX - rect.left, e.clientY - rect.top);
      } else this.tip.classList.remove("is-on");
      return;
    }
    const peer = this.peers.find((p) => p.id === hit);
    if (!peer) { this.tip.classList.remove("is-on"); return; }
    const rate = peer._rate || { down: null, up: null };
    this.tip.innerHTML = `<b>${esc(peer.name) || "未命名节点"}</b>
      <div class="mono">${esc(shortId(peer.id))}</div>
      <div class="row"><span>状态</span><span>${peer.connected ? "在线" : "离线"}</span></div>
      <div class="row"><span>来源</span><span>${peer.source === "discovered" ? "自动发现" : "手工"}</span></div>
      <div class="row"><span>路径</span><span>${peer.connected ? pathKindLabel(peer.path_kind) : "—"}</span></div>
      <div class="row"><span>RTT</span><span>${peer.connected ? fmtRtt(peer.rtt_ms) : "—"}</span></div>
      <div class="row"><span>丢包</span><span>${peer.connected ? fmtLoss(peer.lost_packets, peer.datagrams_tx) : "—"}</span></div>
      <div class="row"><span>带宽</span><span>${peer.connected ? `${fmtRate(rate.down)} ↓ / ${fmtRate(rate.up)} ↑` : "—"}</span></div>
      <div class="row"><span>流量</span><span>${peer.connected ? `↓${fmtBytes(peer.bytes_rx)} ↑${fmtBytes(peer.bytes_tx)}` : "—"}</span></div>`;
    this._placeTip(x, y);
  }
  _placeTip(x, y) {
    this.tip.classList.add("is-on");
    const tw = this.tip.offsetWidth || 200, th = this.tip.offsetHeight || 120;
    let left = x + 14, top = y + 14;
    if (left + tw > this.w - 8) left = x - tw - 10;
    if (top + th > this.h - 8) top = y - th - 10;
    this.tip.style.left = Math.max(6, left) + "px";
    this.tip.style.top = Math.max(6, top) + "px";
  }
  _linkQuality(p) {
    if (!p.connected) return { color: "rgba(138,148,166,.35)", width: 1.2, speed: 0, glow: 0, rate: { down: 0, up: 0 } };
    const rtt = Number(p.rtt_ms);
    let color = "rgba(18,160,106,.85)"; // good
    if (Number.isFinite(rtt)) {
      if (rtt > 180) color = "rgba(229,72,77,.9)";
      else if (rtt > 80) color = "rgba(245,158,11,.9)";
    }
    if (p.path_kind === "relay") color = "rgba(14,165,233,.9)";
    const rate = p._rate || { down: 0, up: 0 };
    const bps = Math.max(rate.down || 0, rate.up || 0);
    const width = 1.5 + Math.min(7, Math.log10(1 + bps / 512) * 3.2);
    const speed = 0.15 + Math.min(1.6, bps / (80 * 1024));
    return { color, width, speed, glow: Math.min(1, bps / (200 * 1024)), rate };
  }
  _stepPhysics(dt) {
    const self = this.nodes.get("__self__");
    if (!self) return;
    self.x *= 0.85; self.y *= 0.85;
    const list = this.peers.map((p) => this.nodes.get(p.id)).filter(Boolean);
    // soft circular target + repulsion
    const n = Math.max(list.length, 1);
    list.forEach((a, i) => {
      const ang = (i / n) * Math.PI * 2 - Math.PI / 2 + (this.t0 % 100000) * 0.00002;
      const tx = Math.cos(ang) * 0.68, ty = Math.sin(ang) * 0.68;
      a.vx += (tx - a.x) * 1.8 * dt;
      a.vy += (ty - a.y) * 1.8 * dt;
    });
    for (let i = 0; i < list.length; i++) {
      for (let j = i + 1; j < list.length; j++) {
        const a = list[i], b = list[j];
        let dx = a.x - b.x, dy = a.y - b.y;
        let d2 = dx * dx + dy * dy || 0.0001;
        if (d2 < 0.09) {
          const f = (0.09 - d2) * 2.2;
          const inv = 1 / Math.sqrt(d2);
          dx *= inv; dy *= inv;
          a.vx += dx * f * dt; a.vy += dy * f * dt;
          b.vx -= dx * f * dt; b.vy -= dy * f * dt;
        }
      }
    }
    for (const a of list) {
      a.vx *= 0.86; a.vy *= 0.86;
      a.x += a.vx * dt; a.y += a.vy * dt;
      const lim = 0.92;
      const m = Math.hypot(a.x, a.y);
      if (m > lim) { a.x *= lim / m; a.y *= lim / m; }
    }
  }
  _spawnParticles(dt) {
    const self = this.nodes.get("__self__");
    if (!self) return;
    const sxy = this._xy(self);
    for (const p of this.peers) {
      if (!p.connected) continue;
      const n = this.nodes.get(p.id);
      if (!n) continue;
      const q = this._linkQuality(p);
      if (q.speed <= 0) continue;
      // spawn rate ~ bandwidth
      const chance = q.speed * dt * 2.2;
      if (Math.random() > chance) continue;
      const rate = q.rate || { down: 0, up: 0 };
      const toPeer = (rate.up || 0) >= (rate.down || 0);
      const pxy = this._xy(n);
      this.particles.push({
        x0: toPeer ? sxy.x : pxy.x,
        y0: toPeer ? sxy.y : pxy.y,
        x1: toPeer ? pxy.x : sxy.x,
        y1: toPeer ? pxy.y : sxy.y,
        t: 0,
        speed: 0.55 + q.speed * 0.7,
        color: q.color,
        size: 2 + Math.min(3.5, q.width * 0.35),
      });
    }
    if (this.particles.length > 220) this.particles.splice(0, this.particles.length - 220);
  }
  _frame(now) {
    if (!this.cv.isConnected) {
      this._ro.disconnect();
      this.cv.removeEventListener("pointermove", this._onMove);
      this.cv.removeEventListener("pointerleave", this._onLeave);
      return;
    }
    const dt = Math.min(0.05, (now - (this._last || now)) / 1000) || 0.016;
    this._last = now;
    this._stepPhysics(dt);
    this._spawnParticles(dt);
    const ctx = this.ctx;
    ctx.clearRect(0, 0, this.w, this.h);
    // soft grid
    ctx.save();
    ctx.strokeStyle = getComputedStyle(document.body).getPropertyValue("--border").trim() || "#e6e9f0";
    ctx.globalAlpha = 0.45;
    ctx.lineWidth = 1;
    const step = 36;
    for (let x = step; x < this.w; x += step) { ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, this.h); ctx.stroke(); }
    for (let y = step; y < this.h; y += step) { ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(this.w, y); ctx.stroke(); }
    ctx.restore();

    const self = this.nodes.get("__self__");
    const sxy = self ? this._xy(self) : { x: this.w / 2, y: this.h / 2 };

    // links
    for (const p of this.peers) {
      const n = this.nodes.get(p.id);
      if (!n) continue;
      const pxy = this._xy(n);
      const q = this._linkQuality(p);
      ctx.save();
      ctx.beginPath();
      ctx.moveTo(sxy.x, sxy.y);
      ctx.lineTo(pxy.x, pxy.y);
      ctx.strokeStyle = q.color;
      ctx.lineWidth = q.width;
      if (!p.connected) ctx.setLineDash([5, 6]);
      ctx.globalAlpha = p.connected ? 0.85 : 0.55;
      if (q.glow > 0.05) {
        ctx.shadowColor = q.color;
        ctx.shadowBlur = 8 + q.glow * 14;
      }
      ctx.stroke();
      ctx.restore();
      // edge label
      if (p.connected) {
        const mx = (sxy.x + pxy.x) / 2, my = (sxy.y + pxy.y) / 2;
        const label = `${fmtRtt(p.rtt_ms)}`;
        ctx.save();
        ctx.font = "600 10px ui-sans-serif, system-ui, sans-serif";
        ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--muted").trim() || "#8a94a6";
        ctx.textAlign = "center";
        ctx.fillText(label, mx, my - 6);
        ctx.restore();
      }
    }

    // particles
    const alive = [];
    for (const pt of this.particles) {
      pt.t += dt * pt.speed;
      if (pt.t > 1) continue;
      const x = pt.x0 + (pt.x1 - pt.x0) * pt.t;
      const y = pt.y0 + (pt.y1 - pt.y0) * pt.t;
      ctx.beginPath();
      ctx.fillStyle = pt.color;
      ctx.globalAlpha = 0.35 + 0.65 * Math.sin(pt.t * Math.PI);
      ctx.arc(x, y, pt.size, 0, Math.PI * 2);
      ctx.fill();
      ctx.globalAlpha = 1;
      alive.push(pt);
    }
    this.particles = alive;

    // peer nodes
    for (const p of this.peers) {
      const n = this.nodes.get(p.id);
      if (!n) continue;
      const pxy = this._xy(n);
      const on = !!p.connected;
      const pulse = on ? 1 + 0.08 * Math.sin(now / 280) : 1;
      const r = n.r * pulse;
      ctx.beginPath();
      ctx.fillStyle = on
        ? (p.path_kind === "relay" ? "#0ea5e9" : "#12a06a")
        : "#8a94a6";
      ctx.globalAlpha = on ? 1 : 0.55;
      ctx.arc(pxy.x, pxy.y, r, 0, Math.PI * 2);
      ctx.fill();
      ctx.globalAlpha = 1;
      ctx.lineWidth = this.hoverId === p.id ? 3 : 1.5;
      ctx.strokeStyle = getComputedStyle(document.body).getPropertyValue("--surface").trim() || "#fff";
      ctx.stroke();
      // label
      const name = p.name || shortId(p.id);
      ctx.font = "600 11px ui-sans-serif, system-ui, sans-serif";
      ctx.textAlign = "center";
      ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--text").trim() || "#101828";
      ctx.fillText(name.length > 12 ? name.slice(0, 11) + "…" : name, pxy.x, pxy.y + r + 14);
      if (on) {
        const rate = p._rate || { down: null, up: null };
        const bps = Math.max(rate.down || 0, rate.up || 0);
        if (bps > 0) {
          ctx.font = "500 10px ui-sans-serif, system-ui, sans-serif";
          ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--muted").trim() || "#8a94a6";
          ctx.fillText(fmtRate(bps), pxy.x, pxy.y + r + 26);
        }
      }
    }

    // self node
    if (self) {
      const pulse = 1 + 0.06 * Math.sin(now / 320);
      const r = self.r * pulse;
      const g = ctx.createRadialGradient(sxy.x, sxy.y, 2, sxy.x, sxy.y, r * 2.2);
      g.addColorStop(0, "rgba(91,91,240,.55)");
      g.addColorStop(1, "rgba(91,91,240,0)");
      ctx.beginPath();
      ctx.fillStyle = g;
      ctx.arc(sxy.x, sxy.y, r * 2.2, 0, Math.PI * 2);
      ctx.fill();
      ctx.beginPath();
      ctx.fillStyle = "#5b5bf0";
      ctx.arc(sxy.x, sxy.y, r, 0, Math.PI * 2);
      ctx.fill();
      ctx.lineWidth = this.hoverId === "__self__" ? 3 : 2;
      ctx.strokeStyle = "#fff";
      ctx.stroke();
      ctx.font = "700 12px ui-sans-serif, system-ui, sans-serif";
      ctx.textAlign = "center";
      ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--text").trim() || "#101828";
      ctx.fillText("本节点", sxy.x, sxy.y + r + 16);
      ctx.font = "500 10px ui-monospace, Menlo, monospace";
      ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--muted").trim() || "#8a94a6";
      ctx.fillText(shortId(this.selfId) || "—", sxy.x, sxy.y + r + 30);
    }

    if (!this.peers.length) {
      ctx.font = "500 13px ui-sans-serif, system-ui, sans-serif";
      ctx.textAlign = "center";
      ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--muted").trim() || "#8a94a6";
      ctx.fillText("暂无对等节点 — 添加种子或等待自动发现", this.w / 2, this.h / 2 + 56);
    }

    this._raf = requestAnimationFrame((t) => this._frame(t));
  }
}

function renderPassword(host) {
  host.innerHTML = `
  <div class="card auth-inline">
    <div class="card__title">修改管理员密码</div>
    <form class="pwform">
      <label class="field"><span>原密码</span><input name="o" type="password" autocomplete="current-password"></label>
      <label class="field"><span>新密码（至少 6 位）</span><input name="n" type="password" autocomplete="new-password"></label>
      <label class="field"><span>确认新密码</span><input name="c" type="password" autocomplete="new-password"></label>
      <div class="err" hidden></div>
      <div class="okmsg" hidden>✓ 密码已修改</div>
      <button class="btn btn--primary" type="submit">保存</button>
    </form>
  </div>`;
  const form = host.querySelector(".pwform"), err = host.querySelector(".err"), ok = host.querySelector(".okmsg");
  form.addEventListener("submit", async (e) => {
    e.preventDefault(); err.hidden = true; ok.hidden = true;
    if (form.n.value !== form.c.value) { err.textContent = "两次新密码不一致"; err.hidden = false; return; }
    try {
      await api("/api/change-password", { method: "POST", body: { oldPassword: form.o.value, newPassword: form.n.value } });
      form.reset(); ok.hidden = false;
    } catch (ex) { err.textContent = ex.message; err.hidden = false; }
  });
}

/* ---------------- 域名管理面板 ---------------- */
function renderNames(host, dom, names, pending, refresh) {
  const domains = (dom && dom.domains) || [];
  const list = (names && names.names) || [];
  const waiting = ((pending && pending.items) || []).filter((a) => a.status === "pending");
  const pendingErr = pending && pending.error;
  const approved = ((pending && pending.items) || []).filter((a) => a.status === "approved");
  const approvedByDomain = new Map(approved.map((a) => [String(a.domain).toLowerCase(), a]));
  const ownedRows = domains.length
    ? domains.map((d) => {
        const a = approvedByDomain.get(String(d).toLowerCase());
        return `<tr><td><b>${esc(d)}</b></td><td>${esc(a && a.email || "—")}</td><td>${esc(fmtTime(a && a.created_ms))}</td><td>${esc(fmtTime(a && a.decided_ms))}</td></tr>`;
      }).join("")
    : '<tr><td colspan="4" class="muted center">本节点还没有已注册域名</td></tr>';
  const waitRows = waiting.length
    ? waiting.map((a) => `<tr><td><b>${esc(a.domain)}</b></td><td>${esc(a.email || "—")}</td><td>等待审核</td><td>${esc(fmtTime(a.created_ms))}</td></tr>`).join("")
    : '<tr><td colspan="4" class="muted center">没有正在等待审核的域名</td></tr>';
  const domOpts = domains.map((d) => `<option value="${esc(d)}">${esc(d)}</option>`).join("");
  const rows = list.length
    ? list.map((r) => `<tr>
        <td><b>${esc(r.local_part)}</b></td>
        <td>${esc(r.domain)}</td>
        <td style="font-family:monospace;font-size:11.5px;word-break:break-all">${esc(r.pubkey)}</td>
        <td><button class="btn btn--ghost nm-del" data-d="${esc(r.domain)}" data-l="${esc(r.local_part)}">删除</button></td>
      </tr>`).join("")
    : '<tr><td colspan="4" class="muted center">该节点尚未登记任何命名</td></tr>';
  host.innerHTML = `
    <div class="card">
      <div class="card__title">本节点域名</div>
      <div class="muted" style="margin:8px 0 4px">已注册</div>
      <table class="tbl"><thead><tr><th>域名</th><th>邮箱</th><th>申请时间</th><th>审核通过时间</th></tr></thead><tbody>${ownedRows}</tbody></table>
      <div class="muted" style="margin:14px 0 4px">等待审核</div>
      ${pendingErr ? `<div class="err">${esc(pendingErr)}</div>` : ""}
      <table class="tbl"><thead><tr><th>域名</th><th>邮箱</th><th>状态</th><th>申请时间</th></tr></thead><tbody>${waitRows}</tbody></table>
      <form class="domform" style="margin-top:12px;display:flex;gap:8px;flex-wrap:wrap">
        <input name="domain" placeholder="申请域名，如 example.nm" style="flex:1;min-width:180px" autocomplete="off">
        <input name="email" type="email" placeholder="邮箱，如 ops@example.nm" style="flex:1;min-width:180px" autocomplete="off" required>
        <button class="btn btn--primary" type="submit">申请域名</button>
      </form>
      <div class="err" hidden style="margin-top:8px"></div>
      <div class="wait" hidden style="margin-top:8px"></div>
      <div class="muted" style="margin-top:6px">申请提交到注册中心，通过或驳回后由网格消息写回本节点。一个节点可持有多个域名。</div>
    </div>
    <div class="card" style="margin-top:16px">
      <div class="card__title">域内命名登记（local-part → 公钥）</div>
      <form class="nameform" style="display:flex;gap:8px;flex-wrap:wrap;align-items:center;margin-bottom:12px">
        <input name="local_part" placeholder="local-part（@ 前）" autocomplete="off">
        <span class="muted">@</span>
        <select name="domain" ${domains.length ? "" : "disabled"}>${domOpts}</select>
        <span class="muted" aria-hidden="true">→</span>
        <input name="pubkey" placeholder="公钥 64 位 hex" style="flex:1;min-width:300px" autocomplete="off">
        <button class="btn btn--primary" type="submit" ${domains.length ? "" : "disabled"}>登记 / 更新</button>
      </form>
      <div class="err2" hidden style="margin-bottom:8px"></div>
      <table class="tbl"><thead><tr><th>local-part</th><th>域名</th><th>公钥</th><th></th></tr></thead>
      <tbody>${rows}</tbody></table>
      <div class="muted" style="margin-top:8px">一个公钥可登记多个名字；一个名字只指向一个公钥。登记/删除即经联邦 gossip 广播，各节点收敛。</div>
    </div>`;
  const err = host.querySelector(".err"), err2 = host.querySelector(".err2");
  const wait = host.querySelector(".wait");
  host.querySelector(".domform").addEventListener("submit", async (e) => {
    e.preventDefault(); err.hidden = true;
    const domain = e.target.domain.value.trim();
    const email = e.target.email.value.trim();
    if (!domain) return;
    if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
      err.textContent = "请填写正确的邮箱地址";
      err.hidden = false;
      return;
    }
    try {
      const r = await api("/api/names/signup", { method: "POST", body: { domain, email } });
      if (r && r.ok === false) throw new Error(r.error || "申请失败");
      const want = (r.domain || domain).toLowerCase();
      wait.hidden = false;
      wait.textContent = `已提交 ${want}，等待注册中心审批…`;
      const deadline = Date.now() + 10 * 60 * 1000;
      const timer = setInterval(async () => {
        if (!wait.isConnected) { clearInterval(timer); return; }
        if (Date.now() > deadline) {
          clearInterval(timer);
          wait.textContent = "仍在等待审批。通过或驳回后会经网格送到本节点。";
          return;
        }
        try {
          const n = await api("/api/names/domain-notices");
          const hit = (n.notices || []).find((x) => String(x.domain).toLowerCase() === want);
          if (!hit) return;
          clearInterval(timer);
          if (hit.approved) {
            wait.textContent = `${want} 已通过，已写入本节点域名列表。`;
          } else {
            wait.textContent = `${want} 已被驳回。`;
          }
          refresh();
        } catch (ex) { wait.textContent = ex.message; }
      }, 3000);
    } catch (ex) { err.textContent = ex.message; err.hidden = false; }
  });
  host.querySelector(".nameform").addEventListener("submit", async (e) => {
    e.preventDefault(); err2.hidden = true;
    const domain = e.target.domain.value;
    const local_part = e.target.local_part.value.trim();
    const pubkey = e.target.pubkey.value.trim();
    if (!local_part || !pubkey) { err2.textContent = "请填写 local-part 与公钥"; err2.hidden = false; return; }
    try {
      const r = await api("/api/names/set", { method: "POST", body: { domain, local_part, pubkey } });
      if (r && r.ok === false) throw new Error(r.error || "登记失败");
      e.target.local_part.value = ""; e.target.pubkey.value = "";
      refresh();
    } catch (ex) { err2.textContent = ex.message; err2.hidden = false; }
  });
  host.querySelectorAll(".nm-del").forEach((b) => b.addEventListener("click", async () => {
    if (!confirm(`删除命名 ${b.dataset.l}@${b.dataset.d}？此操作将广播给联邦。`)) return;
    try {
      const r = await api("/api/names/del", { method: "POST", body: { domain: b.dataset.d, local_part: b.dataset.l } });
      if (r && r.ok === false) throw new Error(r.error || "删除失败");
      refresh();
    } catch (ex) { err2.textContent = ex.message; err2.hidden = false; }
  }));
}

function drawChart(cv) {
  if (!cv) return;
  const dpr = window.devicePixelRatio || 1;
  const w = (cv.width = cv.clientWidth * dpr);
  const h = (cv.height = 170 * dpr);
  const ctx = cv.getContext("2d");
  ctx.clearRect(0, 0, w, h);
  if (TRAF.length < 2) return;
  const rates = [];
  for (let k = 1; k < TRAF.length; k++) {
    const dt = (TRAF[k].t - TRAF[k - 1].t) / 1000 || 1;
    rates.push({ i: Math.max(0, (TRAF[k].i - TRAF[k - 1].i) / dt), o: Math.max(0, (TRAF[k].o - TRAF[k - 1].o) / dt) });
  }
  const max = Math.max(1, ...rates.map((r) => Math.max(r.i, r.o)));
  const css = (v, f) => (getComputedStyle(document.documentElement).getPropertyValue(v).trim() || f);
  const plot = (key, color, fill) => {
    ctx.beginPath();
    rates.forEach((r, i) => {
      const x = (i / (rates.length - 1)) * w;
      const y = h - (r[key] / max) * (h - 12 * dpr) - 6 * dpr;
      i ? ctx.lineTo(x, y) : ctx.moveTo(x, y);
    });
    ctx.strokeStyle = color; ctx.lineWidth = 2 * dpr; ctx.lineJoin = "round"; ctx.stroke();
    ctx.lineTo(w, h); ctx.lineTo(0, h); ctx.closePath(); ctx.fillStyle = fill; ctx.fill();
  };
  plot("i", css("--sky", "#0ea5e9"), "rgba(14,165,233,.10)");
  plot("o", css("--accent", "#5b5bf0"), "rgba(91,91,240,.10)");
}

customElements.define("admin-app", AdminApp);
customElements.define("admin-login", AdminLogin);
customElements.define("admin-change", AdminChange);
customElements.define("admin-shell", AdminShell);

// im-admind 前端：纯 Web Components 标准（自定义元素 + 原生 DOM，无框架、无构建）。

/* ---------------- helpers ---------------- */
async function api(path, opts = {}) {
  const res = await fetch(path, {
    method: opts.method || "GET",
    headers: opts.body ? { "Content-Type": "application/json" } : {},
    body: opts.body ? JSON.stringify(opts.body) : undefined,
    credentials: "same-origin",
  });
  if (res.status === 401) {
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
const shortId = (h) => (h && h.length > 16 ? `${h.slice(0, 10)}…${h.slice(-4)}` : h || "");
const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

const ICONS = {
  overview: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="5" rx="1.5"/><rect x="14" y="12" width="7" height="9" rx="1.5"/><rect x="3" y="16" width="7" height="5" rx="1.5"/></svg>',
  connections: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M9 7 4 12l5 5M15 7l5 5-5 5"/></svg>',
  users: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="12" cy="8" r="3.2"/><path d="M5 20a7 7 0 0 1 14 0" stroke-linecap="round"/></svg>',
  storage: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/></svg>',
  traffic: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 17l5-6 4 3 4-7 5 5"/></svg>',
  system: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="4" y="4" width="16" height="12" rx="2"/><path d="M8 20h8M12 16v4" stroke-linecap="round"/></svg>',
};
const TABS = [
  { k: "overview", label: "总览" },
  { k: "connections", label: "连接监控" },
  { k: "users", label: "用户管理" },
  { k: "storage", label: "存储管理" },
  { k: "traffic", label: "流量监控" },
  { k: "system", label: "系统资源" },
];

/* ---------------- 根：鉴权路由 ---------------- */
class AdminApp extends HTMLElement {
  async connectedCallback() {
    this._onLogout = () => this.show("login");
    window.addEventListener("admind:logout", this._onLogout);
    try {
      const s = await fetch("/api/session", { credentials: "same-origin" }).then((r) => r.json());
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
      <div class="brand"><div class="logo">im</div><div><h1>imd 管理控制台</h1><p>请登录以继续</p></div></div>
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
      <div class="brand"><div class="logo">im</div><div><h1>修改初始密码</h1><p>首次登录，请设置新密码后进入</p></div></div>
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
        <div class="side__brand"><div class="logo">im</div><span>imd 控制台</span></div>
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
    this.timer = setInterval(() => this.refresh(), 3000);
  }
  async refresh() {
    const host = this.querySelector(".host"), meta = this.querySelector(".top__meta");
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
    </div>`;
  } else if (tab === "connections") {
    const rows = (d.connections || []).map((c) => `<tr>
      <td><code class="mono">${shortId(c.id)}</code></td>
      <td>${sinceMs(c.since_unix_ms)}</td>
      <td>${fmtBytes(c.bytes_rx)}</td>
      <td>${fmtBytes(c.bytes_tx)}</td>
      <td><span class="badge">${esc(c.alpn)}</span></td>
      <td class="right"><button class="btn btn--sm btn--danger" data-kick="${c.id}">踢下线</button></td>
    </tr>`).join("");
    host.innerHTML = `<div class="card"><table class="tbl">
      <thead><tr><th>公钥</th><th>接入时长</th><th>接收</th><th>发送</th><th>协议</th><th></th></tr></thead>
      <tbody>${rows || '<tr><td colspan="6" class="muted center">暂无活动连接</td></tr>'}</tbody></table></div>`;
    host.querySelectorAll("[data-kick]").forEach((b) => (b.onclick = async () => {
      b.disabled = true;
      try { await api("/api/kick", { method: "POST", body: { id: b.dataset.kick } }); refresh(); } catch (e) { alert(e.message); b.disabled = false; }
    }));
  } else if (tab === "users") {
    const rows = (d.users || []).map((u) => `<tr>
      <td><b>${esc(u.name) || "(无名)"}</b></td>
      <td><span class="badge">${esc(u.kind)}</span></td>
      <td><code class="mono">${shortId(u.id)}</code></td>
      <td>${u.banned ? '<span class="badge badge--danger">已封禁</span>' : '<span class="badge badge--ok">正常</span>'}</td>
      <td class="right">${u.banned
        ? `<button class="btn btn--sm" data-unban="${u.id}">解封</button>`
        : `<button class="btn btn--sm btn--danger" data-ban="${u.id}">拉黑</button>`}</td>
    </tr>`).join("");
    host.innerHTML = `<div class="card"><table class="tbl">
      <thead><tr><th>名称</th><th>类型</th><th>公钥</th><th>状态</th><th></th></tr></thead>
      <tbody>${rows || '<tr><td colspan="5" class="muted center">暂无实体，点连接后刷新</td></tr>'}</tbody></table></div>`;
    const act = (sel, url) => host.querySelectorAll(sel).forEach((b) => (b.onclick = async () => {
      b.disabled = true;
      try { await api(url, { method: "POST", body: { id: b.dataset[url.includes("unban") ? "unban" : "ban"] } }); refresh(); } catch (e) { alert(e.message); b.disabled = false; }
    }));
    act("[data-ban]", "/api/ban");
    act("[data-unban]", "/api/unban");
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
  }
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

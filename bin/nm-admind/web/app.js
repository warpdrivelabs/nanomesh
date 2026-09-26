// nm-admind 前端：纯 Web Components 标准（自定义元素 + 原生 DOM，无框架、无构建）。

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
  peers: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="6" cy="6" r="2.4"/><circle cx="18" cy="6" r="2.4"/><circle cx="12" cy="18" r="2.4"/><path d="M7.6 7.6 12 15.6 16.4 7.6M8.4 6h7.2" stroke-linecap="round"/></svg>',
  identity: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="5" width="18" height="14" rx="2"/><circle cx="8.5" cy="11" r="2"/><path d="M5.5 16c.6-1.6 4.2-1.6 6 0M14 9h4M14 12h4M14 15h2" stroke-linecap="round"/></svg>',
  password: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="4" y="10" width="16" height="10" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3M12 14v2" stroke-linecap="round"/></svg>',
};
const TABS = [
  { k: "overview", label: "总览" },
  { k: "connections", label: "连接监控" },
  { k: "users", label: "用户管理" },
  { k: "peers", label: "对等节点" },
  { k: "storage", label: "存储管理" },
  { k: "traffic", label: "流量监控" },
  { k: "system", label: "系统资源" },
  { k: "identity", label: "服务标识" },
  { k: "password", label: "修改密码" },
];
// 无需轮询的静态面板（表单/稳定信息）——渲染一次即可，避免定时重渲染打断输入。
const STATIC_TABS = new Set(["identity", "password"]);

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
      <div class="brand"><div class="logo">im</div><div><h1>nmd 管理控制台</h1><p>请登录以继续</p></div></div>
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
        <div class="side__brand"><div class="logo">im</div><span>nmd 控制台</span></div>
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
  } else if (tab === "identity") {
    host.innerHTML = `
    <div class="idcards">
      <div class="card idcard">
        <div class="idcard__head"><span class="badge badge--node">Node ID · 节点公钥</span></div>
        <img class="qr" src="/api/qr?kind=node&t=${Date.now()}" alt="node id QR" />
        <div class="idcard__val"><code class="mono wrap">${esc(d.node_id)}</code></div>
        <button class="btn btn--sm btn--block" data-copy="${esc(d.node_id)}">复制 Node ID</button>
      </div>
      <div class="card idcard">
        <div class="idcard__head"><span class="badge badge--addr">完整地址 · NM_NODE_ADDR</span></div>
        <img class="qr" src="/api/qr?kind=addr&t=${Date.now()}" alt="addr QR" />
        <div class="idcard__val"><code class="mono wrap">${esc(d.addr)}</code></div>
        <button class="btn btn--sm btn--block" data-copy="${esc(d.addr)}">复制完整地址</button>
      </div>
    </div>
    <p class="muted">二维码按类型着色（<b style="color:var(--accent)">靛蓝=Node ID</b> / <b style="color:var(--sky)">天蓝=完整地址</b>），载荷带类型前缀 <code>nmspace:node:</code> / <code>nmspace:addr:</code> 以区分类型。</p>`;
    host.querySelectorAll("[data-copy]").forEach((b) => (b.onclick = () => {
      navigator.clipboard?.writeText(b.dataset.copy);
      const t = b.textContent; b.textContent = "已复制 ✓"; setTimeout(() => (b.textContent = t), 1200);
    }));
  } else if (tab === "peers") {
    const peers = d.peers || [];
    const fed = d.federation || "nmspace";
    const ago = (s) => {
      if (!s) return "";
      const d = Math.max(0, Math.floor(Date.now() / 1000) - s);
      if (d < 60) return d + "s前";
      if (d < 3600) return Math.floor(d / 60) + "分前";
      if (d < 86400) return Math.floor(d / 3600) + "时前";
      return Math.floor(d / 86400) + "天前";
    };
    const rows = peers.map((p, i) => {
      const disc = p.source === "discovered";
      const badge = disc
        ? `<span class="badge badge--discovered">自动发现</span>${p.last_seen ? ` <span class="muted">${ago(p.last_seen)}</span>` : ""}`
        : `<span class="badge badge--manual">手工</span>`;
      const actions = disc
        ? `<button class="btn btn--sm btn--danger" data-ban="${p.id}" title="永久排除：断开连接且不再被自动发现学回">封禁</button>
           <button class="btn btn--sm" data-del="${p.id}" title="瞬时移除；对方仍在广播会被再次发现">移除</button>`
        : `<button class="btn btn--sm" data-edit="${i}">编辑</button>
           <button class="btn btn--sm btn--danger" data-del="${p.id}">删除</button>`;
      return `<tr>
      <td><b>${esc(p.name) || '<span class="muted">—</span>'}</b></td>
      <td class="nowrap">${badge}</td>
      <td class="nowrap"><span class="badge badge--fed">${esc(p.federation || fed)}</span></td>
      <td><code class="mono">${shortId(p.id)}</code></td>
      <td>${esc(p.address) || '<span class="muted">—</span>'}</td>
      <td>${esc(p.email) || '<span class="muted">—</span>'}</td>
      <td>${esc(p.mobile) || '<span class="muted">—</span>'}</td>
      <td>${esc(p.gps) || '<span class="muted">—</span>'}</td>
      <td class="right nowrap">${actions}</td>
    </tr>`;
    }).join("");
    host.innerHTML = `
    <div class="card">
      <div class="card__title" id="pf-title">添加对等节点（手工/种子 · 运行时生效 · 无需重启 · 连接按 Node ID 发现）</div>
      <p class="muted" style="margin:-4px 0 12px">本节点联邦：<span class="badge badge--fed">${esc(fed)}</span> · 手工添加的即「种子」并作为 gossip 引导，将加入此联邦；其余成员经成员频道<b>自动发现</b>，每台只需配少量种子。</p>
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
    <div class="card"><table class="tbl">
      <thead><tr><th>名称</th><th>来源</th><th>联邦</th><th>Node ID</th><th>物理地址</th><th>Email</th><th>手机</th><th>GPS</th><th></th></tr></thead>
      <tbody>${rows || '<tr><td colspan="9" class="muted center">暂无对等节点</td></tr>'}</tbody></table></div>`;
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
    host.querySelectorAll("[data-edit]").forEach((b) => (b.onclick = () => {
      const p = peers[+b.dataset.edit];
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
    host.querySelectorAll("[data-ban]").forEach((b) => (b.onclick = async () => {
      if (!confirm("封禁该节点？将断开其连接，并不再被自动发现学回（永久排除）。")) return;
      b.disabled = true;
      try {
        await api("/api/ban", { method: "POST", body: { id: b.dataset.ban } });
        await api("/api/remove-peer", { method: "POST", body: { id: b.dataset.ban } });
        refresh();
      } catch (ex) { alert(ex.message); b.disabled = false; }
    }));
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

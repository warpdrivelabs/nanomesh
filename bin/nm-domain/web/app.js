const I18N = {
  "zh-CN": {
    "login.title": "域名注册中心", "login.lead": "登记 acme.mesh 这类域名，解析为节点公钥。用户名 name@domain 不在这里。",
    "login.user": "用户名", "login.pass": "密码", "login.submit": "登录",
    "change.title": "修改初始密码", "change.lead": "首次登录，请设置新密码后进入。",
    "change.old": "原密码", "change.new": "新密码（至少 6 位）", "change.again": "确认新密码", "change.save": "保存并进入",
    "change.mismatch": "两次新密码不一致",
    "shell.title": "全球域名注册", "shell.logout": "退出", "lang": "English",
    "col.domain": "域名", "col.email": "邮箱", "col.node": "节点", "col.status": "状态", "col.time": "时间",
    "status.pending": "待审", "status.approved": "已通过", "status.rejected": "已驳回",
    "act.approve": "通过", "act.reject": "驳回",
  },
  en: {
    "login.title": "Domain registry", "login.lead": "Register names like acme.mesh and resolve them to a node key. User names such as name@domain are not stored here.",
    "login.user": "Username", "login.pass": "Password", "login.submit": "Sign in",
    "change.title": "Change the initial password", "change.lead": "First sign-in. Set a new password to continue.",
    "change.old": "Current password", "change.new": "New password (at least 6 characters)", "change.again": "Confirm new password", "change.save": "Save and continue",
    "change.mismatch": "The two new passwords do not match",
    "shell.title": "Domain registry", "shell.logout": "Sign out", "lang": "中文",
    "col.domain": "Domain", "col.email": "Email", "col.node": "Node", "col.status": "Status", "col.time": "Time",
    "status.pending": "Pending", "status.approved": "Approved", "status.rejected": "Rejected",
    "act.approve": "Approve", "act.reject": "Reject",
  },
};
function domLocale() {
  const saved = localStorage.getItem("nmdomain-locale") || "";
  if (I18N[saved]) return saved;
  return (navigator.language || "").toLowerCase().startsWith("en") ? "en" : "zh-CN";
}
let DOM_LOCALE = domLocale();
function t(key) { return (I18N[DOM_LOCALE] && I18N[DOM_LOCALE][key]) || I18N["zh-CN"][key] || key; }
function toggleDomLocale() {
  DOM_LOCALE = DOM_LOCALE === "en" ? "zh-CN" : "en";
  localStorage.setItem("nmdomain-locale", DOM_LOCALE);
  location.reload();
}
document.documentElement.lang = DOM_LOCALE === "en" ? "en" : "zh-CN";

const COPY_ICON = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>';

const TOKEN_KEY = "nmdomain-token";
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
    window.dispatchEvent(new CustomEvent("domain:logout"));
    throw new Error("会话已失效，请重新登录");
  }
  const text = await res.text();
  let data = {};
  try { data = text ? JSON.parse(text) : {}; } catch { data = { raw: text }; }
  if (!res.ok || data.ok === false) throw new Error(data.error || `HTTP ${res.status}`);
  return data;
}
const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const shortId = (h) => (h && h.length > 16 ? `${h.slice(0, 10)}…${h.slice(-4)}` : h || "");
function fmtTime(ms) {
  if (!ms) return "—";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "—";
  return d.toLocaleString();
}
async function copyText(text) {
  if (navigator.clipboard && window.isSecureContext) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.focus();
  ta.select();
  const ok = document.execCommand("copy");
  ta.remove();
  if (!ok) throw new Error("浏览器拒绝写入剪贴板");
}
function bindCopy(button, text) {
  button.onclick = async () => {
    try {
      await copyText(text);
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

class DomainApp extends HTMLElement {
  async connectedCallback() {
    this._onLogout = () => this.show("login");
    window.addEventListener("domain:logout", this._onLogout);
    try {
      const s = await api("/api/session");
      this.show(!s.authed ? "login" : s.mustChange ? "change" : "shell");
    } catch { this.show("login"); }
  }
  disconnectedCallback() { window.removeEventListener("domain:logout", this._onLogout); }
  show(view) {
    this.innerHTML = "";
    const tag = { login: "domain-login", change: "domain-change", shell: "domain-shell" }[view];
    const el = document.createElement(tag);
    el.addEventListener("domain:navigate", (e) => this.show(e.detail));
    this.appendChild(el);
  }
}

class DomainLogin extends HTMLElement {
  connectedCallback() {
    this.innerHTML = `
    <div class="screen"><div class="card auth">
      <h1>${t("login.title")}</h1>
      <p class="lead">${t("login.lead")}</p>
      <form>
        <label>${t("login.user")}<input name="u" value="admin" autocomplete="username"></label>
        <label>${t("login.pass")}<input name="p" type="password" autocomplete="current-password"></label>
        <div class="err" hidden></div>
        <button class="btn btn--primary btn--block" type="submit">${t("login.submit")}</button>
      </form>
      <button class="btn" type="button" id="dom-lang">${t("lang")}</button>
    </div></div>`;
    const form = this.querySelector("form"), err = this.querySelector(".err");
    this.querySelector("#dom-lang").onclick = () => toggleDomLocale();
    form.addEventListener("submit", async (e) => {
      e.preventDefault(); err.hidden = true;
      try {
        const r = await api("/api/login", { method: "POST", body: { username: form.u.value.trim(), password: form.p.value } });
        if (r.token) sessionStorage.setItem(TOKEN_KEY, r.token);
        this.dispatchEvent(new CustomEvent("domain:navigate", { detail: r.mustChange ? "change" : "shell", bubbles: true }));
      } catch (ex) { err.textContent = ex.message; err.hidden = false; }
    });
  }
}

class DomainChange extends HTMLElement {
  connectedCallback() {
    this.innerHTML = `
    <div class="screen"><div class="card auth">
      <h1>${t("change.title")}</h1>
      <p class="lead">${t("change.lead")}</p>
      <form>
        <label>${t("change.old")}<input name="o" type="password"></label>
        <label>${t("change.new")}<input name="n" type="password"></label>
        <label>${t("change.again")}<input name="c" type="password"></label>
        <div class="err" hidden></div>
        <button class="btn btn--primary btn--block" type="submit">${t("change.save")}</button>
      </form>
    </div></div>`;
    const form = this.querySelector("form"), err = this.querySelector(".err");
    form.addEventListener("submit", async (e) => {
      e.preventDefault(); err.hidden = true;
      if (form.n.value !== form.c.value) { err.textContent = t("change.mismatch"); err.hidden = false; return; }
      try {
        await api("/api/change-password", { method: "POST", body: { oldPassword: form.o.value, newPassword: form.n.value } });
        this.dispatchEvent(new CustomEvent("domain:navigate", { detail: "shell", bubbles: true }));
      } catch (ex) { err.textContent = ex.message; err.hidden = false; }
    });
  }
}

class DomainShell extends HTMLElement {
  connectedCallback() {
    this.page = 1;
    this.q = "";
    this.innerHTML = `
    <div class="shell">
      <header class="top"><h1>${t("shell.title")}</h1><span><button class="btn" type="button" id="dom-lang">${t("lang")}</button> <button class="btn" type="button" id="logout">${t("shell.logout")}</button></span></header>
      <section class="host"></section>
    </div>`;
    this.querySelector("#dom-lang").onclick = () => toggleDomLocale();
    this.querySelector("#logout").onclick = async () => {
      try { await api("/api/logout", { method: "POST" }); } catch {}
      sessionStorage.removeItem(TOKEN_KEY);
      this.dispatchEvent(new CustomEvent("domain:navigate", { detail: "login", bubbles: true }));
    };
    this.refresh();
  }
  async refresh() {
    const host = this.querySelector(".host");
    const q = new URLSearchParams({ page: String(this.page), q: this.q });
    let data, apps;
    try {
      [data, apps] = await Promise.all([
        api("/api/domains?" + q.toString()),
        api("/api/applications"),
      ]);
    }
    catch (ex) { host.innerHTML = `<div class="err">${esc(ex.message)}</div>`; return; }
    const appRows = (apps.items || []).map((a) => `<tr>
      <td><b>${esc(a.domain)}</b></td>
      <td>${esc(a.email || "—")}</td>
      <td><code class="mono">${shortId(a.node_id)}</code></td>
      <td>${a.status === "pending" ? t("status.pending") : a.status === "approved" ? t("status.approved") : t("status.rejected")}</td>
      <td>${esc(fmtTime(a.created_ms))}</td>
      <td class="right">${a.status === "pending"
        ? `<button class="btn btn--primary" type="button" data-approve="${esc(a.domain)}">${t("act.approve")}</button>
           <button class="btn btn--danger" type="button" data-reject="${esc(a.domain)}">${t("act.reject")}</button>`
        : ""}</td>
    </tr>`).join("");
    const rows = (data.items || []).map((r) => `<tr>
      <td><b>${esc(r.domain)}</b></td>
      <td>${esc(r.email)}</td>
      <td><span class="copyline"><code class="mono">${shortId(r.pubkey)}</code>
        <button class="btn btn--icon" type="button" title="复制" aria-label="复制" data-copy="${esc(r.pubkey)}">${COPY_ICON}</button></span></td>
      <td>${esc(fmtTime(r.created_ms))}</td>
      <td>${r.disabled ? '<span class="tag tag--off">已停用</span>' : '<span class="tag">使用中</span>'}</td>
      <td class="right">${r.disabled
        ? `<button class="btn" type="button" data-enable="${esc(r.domain)}">启用</button>`
        : `<button class="btn btn--danger" type="button" data-disable="${esc(r.domain)}">停用</button>`}</td>
    </tr>`).join("");
    host.innerHTML = `
    <div class="card">
      <form class="reg">
        <div class="row">
          <label>域名<input name="domain" placeholder="acme.mesh" autocomplete="off"></label>
          <label>邮箱<input name="email" type="email" placeholder="ops@acme.mesh" autocomplete="off"></label>
          <label>节点公钥<input name="pubkey" placeholder="64 位 hex" autocomplete="off" spellcheck="false"></label>
          <button class="btn btn--primary" type="submit">注册</button>
        </div>
        <div class="err" hidden></div>
      </form>
      <p class="hint">公开解析：<code>/api/resolve?domain=acme.mesh</code>，无需登录。停用后不再返回公钥，登记记录仍保留。这里不登记 name@domain。</p>
    </div>
    <div class="card" style="margin-top:14px">
      <b>域名申请</b>
      <p class="hint">节点通过 <code>/api/signup?nodeid=&amp;domain=</code> 提交。通过或驳回都会经本机 nmd 通知该节点。</p>
      <table class="tbl">
        <thead><tr><th>${t("col.domain")}</th><th>${t("col.email")}</th><th>${t("col.node")}</th><th>${t("col.status")}</th><th>${t("col.time")}</th><th></th></tr></thead>
        <tbody>${appRows || '<tr><td colspan="6" style="color:var(--muted)">没有申请</td></tr>'}</tbody>
      </table>
    </div>
    <div class="card" style="margin-top:14px">
      <form class="find row">
        <label>查询<input name="q" value="${esc(this.q)}" placeholder="域名、邮箱或公钥"></label>
        <button class="btn" type="submit">搜索</button>
      </form>
      <table class="tbl">
        <thead><tr><th>${t("col.domain")}</th><th>${t("col.email")}</th><th>${t("col.node")}</th><th>${t("col.time")}</th><th>${t("col.status")}</th><th></th></tr></thead>
        <tbody>${rows || '<tr><td colspan="6" style="color:var(--muted)">还没有域名</td></tr>'}</tbody>
      </table>
      <div class="pager">
        <span>共 ${data.total} 条 · 每页 ${data.pageSize} 条 · 第 ${data.page} / ${data.pages} 页</span>
        <span>
          <button class="btn" type="button" data-page="prev" ${data.page <= 1 ? "disabled" : ""}>上一页</button>
          <button class="btn" type="button" data-page="next" ${data.page >= data.pages ? "disabled" : ""}>下一页</button>
        </span>
      </div>
    </div>`;
    const form = host.querySelector(".reg"), err = host.querySelector(".err");
    form.addEventListener("submit", async (e) => {
      e.preventDefault(); err.hidden = true;
      try {
        await api("/api/domains", { method: "POST", body: { domain: form.domain.value.trim(), email: form.email.value.trim(), pubkey: form.pubkey.value.trim() } });
        form.reset();
        this.page = 1;
        this.refresh();
      } catch (ex) { err.textContent = ex.message; err.hidden = false; }
    });
    host.querySelector(".find").addEventListener("submit", (e) => {
      e.preventDefault();
      this.q = e.target.q.value.trim();
      this.page = 1;
      this.refresh();
    });
    host.querySelectorAll("[data-copy]").forEach((b) => bindCopy(b, b.dataset.copy));
    host.querySelectorAll("[data-disable]").forEach((b) => (b.onclick = async () => {
      if (!confirm(`停用域名 ${b.dataset.disable}？停用后不再解析出公钥，登记记录仍保留。`)) return;
      try { await api("/api/domains/disable", { method: "POST", body: { domain: b.dataset.disable } }); this.refresh(); }
      catch (ex) { alert(ex.message); }
    }));
    const decide = (sel, path) => host.querySelectorAll(sel).forEach((b) => (b.onclick = async () => {
      b.disabled = true;
      try { await api(path, { method: "POST", body: { domain: b.dataset.approve || b.dataset.reject } }); this.refresh(); }
      catch (ex) { alert(ex.message); b.disabled = false; }
    }));
    decide("[data-approve]", "/api/applications/approve");
    decide("[data-reject]", "/api/applications/reject");
    host.querySelectorAll("[data-enable]").forEach((b) => (b.onclick = async () => {
      try { await api("/api/domains/enable", { method: "POST", body: { domain: b.dataset.enable } }); this.refresh(); }
      catch (ex) { alert(ex.message); }
    }));
    host.querySelector("[data-page=prev]").onclick = () => { if (this.page > 1) { this.page--; this.refresh(); } };
    host.querySelector("[data-page=next]").onclick = () => { if (data.page < data.pages) { this.page++; this.refresh(); } };
  }
}

customElements.define("domain-app", DomainApp);
customElements.define("domain-login", DomainLogin);
customElements.define("domain-change", DomainChange);
customElements.define("domain-shell", DomainShell);

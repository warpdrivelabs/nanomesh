const COPY_ICON = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>';

async function api(path, opts = {}) {
  const res = await fetch(path, {
    method: opts.method || "GET",
    headers: opts.body ? { "Content-Type": "application/json" } : {},
    body: opts.body ? JSON.stringify(opts.body) : undefined,
    credentials: "same-origin",
  });
  if (res.status === 401) {
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
      const s = await fetch("/api/session", { credentials: "same-origin" }).then((r) => r.json());
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
      <h1>域名注册中心</h1>
      <p class="lead">登记 acme.mesh 这类域名，解析为节点公钥。用户名 name@domain 不在这里。</p>
      <form>
        <label>用户名<input name="u" value="admin" autocomplete="username"></label>
        <label>密码<input name="p" type="password" autocomplete="current-password"></label>
        <div class="err" hidden></div>
        <button class="btn btn--primary btn--block" type="submit">登录</button>
      </form>
    </div></div>`;
    const form = this.querySelector("form"), err = this.querySelector(".err");
    form.addEventListener("submit", async (e) => {
      e.preventDefault(); err.hidden = true;
      try {
        const r = await api("/api/login", { method: "POST", body: { username: form.u.value.trim(), password: form.p.value } });
        this.dispatchEvent(new CustomEvent("domain:navigate", { detail: r.mustChange ? "change" : "shell", bubbles: true }));
      } catch (ex) { err.textContent = ex.message; err.hidden = false; }
    });
  }
}

class DomainChange extends HTMLElement {
  connectedCallback() {
    this.innerHTML = `
    <div class="screen"><div class="card auth">
      <h1>修改初始密码</h1>
      <p class="lead">首次登录，请设置新密码后进入。</p>
      <form>
        <label>原密码<input name="o" type="password"></label>
        <label>新密码（至少 6 位）<input name="n" type="password"></label>
        <label>确认新密码<input name="c" type="password"></label>
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
      <header class="top"><h1>全球域名注册</h1><span><button class="btn" type="button" id="logout">退出</button></span></header>
      <section class="host"></section>
    </div>`;
    this.querySelector("#logout").onclick = async () => {
      try { await api("/api/logout", { method: "POST" }); } catch {}
      this.dispatchEvent(new CustomEvent("domain:navigate", { detail: "login", bubbles: true }));
    };
    this.refresh();
  }
  async refresh() {
    const host = this.querySelector(".host");
    const q = new URLSearchParams({ page: String(this.page), q: this.q });
    let data;
    try { data = await api("/api/domains?" + q.toString()); }
    catch (ex) { host.innerHTML = `<div class="err">${esc(ex.message)}</div>`; return; }
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
      <form class="find row">
        <label>查询<input name="q" value="${esc(this.q)}" placeholder="域名、邮箱或公钥"></label>
        <button class="btn" type="submit">搜索</button>
      </form>
      <table class="tbl">
        <thead><tr><th>域名</th><th>邮箱</th><th>节点公钥</th><th>登记时间</th><th>状态</th><th></th></tr></thead>
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

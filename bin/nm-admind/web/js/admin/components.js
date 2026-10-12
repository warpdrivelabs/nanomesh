import { api, authHeaders, TOKEN_KEY } from "./api.js";
import { esc, ICONS, STATIC_TABS, TABS } from "./helpers.js";
import { t, toggleAdmLocale } from "./i18n.js";
import { renderNames, renderPanel, renderPassword } from "./panels.js";

/* ---------------- 根：鉴权路由 ---------------- */
export class AdminApp extends HTMLElement {
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
export class AdminLogin extends HTMLElement {
  connectedCallback() {
    this.innerHTML = `
    <div class="screen"><div class="card auth">
      <div class="brand"><img class="logo" src="/nanomesh-logo.png" alt="NANO MESH" /><div><h1>${t("login.title")}</h1><p>${t("login.lead")}</p></div></div>
      <form>
        <label class="field"><span>${t("login.user")}</span><input name="u" value="admin" autocomplete="username"></label>
        <label class="field"><span>${t("login.pass")}</span><input name="p" type="password" autocomplete="current-password"></label>
        <div class="err" hidden></div>
        <button class="btn btn--primary btn--block" type="submit">${t("login.submit")}</button>
      </form>
      <button class="btn btn--ghost btn--block" type="button" id="adm-lang">${t("lang")}</button>
    </div></div>`;
    const form = this.querySelector("form"), err = this.querySelector(".err");
    this.querySelector("#adm-lang").onclick = () => toggleAdmLocale();
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
export class AdminChange extends HTMLElement {
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
export class AdminShell extends HTMLElement {
  connectedCallback() {
    this.innerHTML = `
    <div class="admin">
      <aside class="side">
        <div class="side__brand"><img class="logo" src="/nanomesh-logo.png" alt="NANO MESH" /><span>${t("shell.brand")}</span></div>
        <nav class="side__nav"></nav>
        <div class="side__foot">
          <button class="btn btn--ghost btn--block" type="button" id="adm-lang">${t("lang")}</button>
          <button class="btn btn--ghost btn--block side__logout">${t("shell.logout")}</button>
        </div>
      </aside>
      <main class="main">
        <header class="top"><h2 class="top__title"></h2><span class="top__meta muted"></span></header>
        <section class="host"></section>
      </main>
    </div>`;
    const nav = this.querySelector(".side__nav");
    TABS.forEach((tab) => {
      const b = document.createElement("button");
      b.className = "navitem"; b.dataset.k = tab.k;
      b.innerHTML = `${ICONS[tab.k] || ""}<span>${t(tab.key)}</span>`;
      b.onclick = () => this.select(tab.k);
      nav.appendChild(b);
    });
    this.querySelector("#adm-lang").onclick = () => toggleAdmLocale();
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
    const tab = TABS.find((item) => item.k === k);
    this.querySelector(".top__title").textContent = tab ? t(tab.key) : k;
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
        meta.textContent = t("shell.refreshed") + new Date().toLocaleTimeString();
        renderNames(host, dom, names, pending, () => this.refresh());
      } catch (ex) {
        host.innerHTML = `<div class="err">${esc(ex.message)}</div>`;
      }
      return;
    }
    try {
      const data = await api("/api/" + this.tab);
      meta.textContent = t("shell.refreshed") + new Date().toLocaleTimeString();
      renderPanel(this.tab, host, data, () => this.refresh());
    } catch (ex) {
      host.innerHTML = `<div class="err">${esc(ex.message)}</div>`;
    }
  }
}

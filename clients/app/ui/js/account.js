// 账号登录 / 注册 / 重置密码。口令只送到家节点做 Argon2 校验，本机只记住「名字 → 公钥」。
(function () {
  const KEYS = "nmspace-account-keys";
  const LAST = "nmspace-account-last";
  let mode = "login";
  let suggestTimer = null;

  const MSG = {
    domain_not_owned: "域名已登记，家节点尚未同步，请稍后再试。",
    invalid_name: "用户名或域名不合法",
    password_short: "密码至少 8 位",
    name_taken: "这个用户名已经注册。",
    no_such_user: "家节点上没有这个用户。",
    bad_password: "密码不正确",
    same_password: "新密码不能与旧密码相同",
    not_key_owner: "本机没有该账号的私钥。若在其他设备注册过，请先导入那台设备导出的备份串。",
    no_password: "该账号未设置密码",
    no_store: "家节点暂时无法保存口令",
  };

  function map() {
    try { return JSON.parse(localStorage.getItem(KEYS) || "{}"); } catch (_) { return {}; }
  }
  function saveKey(name, pk) {
    const m = map();
    m[name] = pk;
    try { localStorage.setItem(KEYS, JSON.stringify(m)); } catch (_) {}
  }
  function remember(local, domain) {
    try { localStorage.setItem(LAST, JSON.stringify({ local, domain })); } catch (_) {}
  }
  function lastAccount() {
    try { return JSON.parse(localStorage.getItem(LAST) || "null"); } catch (_) { return null; }
  }
  function fullName() {
    const local = document.getElementById("acct-local").value.trim().toLowerCase();
    const domain = (document.getElementById("acct-domain").value || "").trim().toLowerCase();
    return local && domain ? local + "@" + domain : "";
  }
  function errBox() { return document.getElementById("acct-error"); }
  function rawErr(err) {
    return (err && (err.message || err)) ? String(err.message || err) : "失败";
  }
  function codeOf(err) {
    const msg = rawErr(err);
    if (msg === "no_such_user" || msg.indexOf("账号不存在") >= 0) return "no_such_user";
    if (msg === "not_key_owner" || msg.indexOf("其他设备") >= 0) return "not_key_owner";
    if (msg === "name_taken" || msg.indexOf("已被占用") >= 0 || msg.indexOf("该账号已注册") >= 0) return "name_taken";
    if (msg === "bad_password" || msg.indexOf("密码不正确") >= 0) return "bad_password";
    if (msg === "domain_not_owned" || msg.indexOf("不拥有该域名") >= 0) return "domain_not_owned";
    if (msg.indexOf("域名未登记") >= 0) return "domain_unknown";
    if (msg.indexOf("域名已停用") >= 0) return "domain_disabled";
    return msg;
  }
  function explain(err) {
    const code = codeOf(err);
    if (code === "domain_unknown") return "这个域名还没有在注册中心登记，不能用来注册或登录。";
    if (code === "domain_disabled") return "这个域名已停用。";
    return MSG[code] || code;
  }
  function hideAsk() {
    const box = document.getElementById("acct-ask");
    if (box) box.hidden = true;
  }
  function ask(text, yesLabel) {
    const box = document.getElementById("acct-ask");
    document.getElementById("acct-ask-text").textContent = text;
    document.getElementById("acct-ask-yes").textContent = yesLabel;
    box.hidden = false;
    return new Promise((resolve) => {
      document.getElementById("acct-ask-yes").onclick = () => { box.hidden = true; resolve(true); };
      document.getElementById("acct-ask-no").onclick = () => { box.hidden = true; resolve(false); };
    });
  }

  let pairRid = "";

  function showTab(name) {
    document.querySelectorAll("#acct-import .acct-tabs button").forEach((b) => b.classList.toggle("on", b.dataset.tab === name));
    document.querySelectorAll("#acct-import .acct-pane").forEach((p) => { p.hidden = p.dataset.pane !== name; });
  }
  function showImport(on, tab) {
    const box = document.getElementById("acct-import");
    if (!box) return;
    box.hidden = !on;
    if (on && tab) showTab(tab);
    if (!on) stopPair();
  }
  function pairStatus(text, sas) {
    const box = document.getElementById("acct-pair-status");
    box.hidden = !text && !sas;
    document.getElementById("acct-pair-text").textContent = text || "";
    document.getElementById("acct-pair-sas").textContent = sas || "";
  }
  function stopPair() {
    if (pairRid) NM.inv("pair_cancel").catch(() => {});
    pairRid = "";
    pairStatus("", "");
  }
  function accountFields() {
    const local = document.getElementById("acct-local").value.trim();
    const domain = document.getElementById("acct-domain").value.trim();
    return { local, domain, password: document.getElementById("acct-password").value };
  }

  async function startPair(kind) {
    const { local, domain, password } = accountFields();
    const box = errBox();
    box.textContent = "";
    if (!local || !domain) { box.textContent = "请先填写用户名和域名"; return; }
    if (kind === "push" && !password) { box.textContent = "请先填写密码"; return; }
    const btn = document.getElementById(kind === "push" ? "acct-pair-go" : "acct-ticket-go");
    btn.disabled = true;
    stopPair();
    try {
      if (kind === "push") {
        const r = await NM.inv("pair_request", { local, domain, password, registry: "", device: "" });
        if (r && r.have) {
          showImport(false);
          await submit();
          return;
        }
        pairRid = r.rid;
        pairStatus("请求已发送，等待旧设备响应…", "");
      } else {
        const r = await NM.inv("pair_ticket", { local, domain, registry: "", device: "" });
        pairRid = r.rid;
        document.getElementById("acct-ticket-out").value = r.ticket;
        pairStatus("迁移串已生成，请在 5 分钟内到旧设备粘贴。", "");
      }
    } catch (err) {
      box.textContent = explain(err);
    } finally {
      btn.disabled = false;
    }
  }

  async function onPairEvent(ev) {
    if (!ev || !pairRid || ev.rid !== pairRid) return;
    if (ev.type === "sas") {
      pairStatus("请在旧设备的确认框中输入下面的核对码：", ev.sas);
    } else if (ev.type === "done") {
      pairRid = "";
      document.getElementById("acct-ticket-out").value = "";
      saveKey(ev.name, ev.user);
      if (document.getElementById("acct-password").value && mode === "login") {
        pairStatus("私钥已迁移到本机，正在登录…", "");
        showImport(false);
        await submit();
      } else {
        pairStatus("私钥已迁移到本机。请输入密码登录；忘记密码可点「忘记密码」重新设置。", "");
      }
    } else if (ev.type === "denied") {
      pairRid = "";
      pairStatus("旧设备拒绝了这次迁移。", "");
    } else if (ev.type === "expired") {
      pairRid = "";
      pairStatus("迁移请求已过期，请重新发起。", "");
    } else if (ev.type === "failed") {
      pairRid = "";
      pairStatus("迁移失败：" + (ev.error || "未知错误"), "");
    }
  }

  async function doImport() {
    const blob = document.getElementById("acct-import-blob").value.trim();
    const password = document.getElementById("acct-import-pw").value;
    const btn = document.getElementById("acct-import-go");
    const box = errBox();
    box.textContent = "";
    if (!blob || !password) { box.textContent = "请粘贴备份串并输入备份口令"; return; }
    btn.disabled = true;
    btn.textContent = "导入中…";
    try {
      await NM.inv("ensure_device");
      const n = await NM.inv("import_backup", { blob, password });
      document.getElementById("acct-import-blob").value = "";
      document.getElementById("acct-import-pw").value = "";
      showImport(false);
      if (typeof renderUserOptions === "function") renderUserOptions();
      const ready = fullName() && document.getElementById("acct-password").value;
      box.textContent = "已导入 " + n + " 个身份" + (ready ? "，正在登录…" : "，请输入账号和密码登录。");
      if (ready && mode === "login") await submit();
    } catch (err) {
      box.textContent = explain(err);
    } finally {
      btn.disabled = false;
      btn.textContent = "导入并登录";
    }
  }

  function setMode(next) {
    mode = next;
    const reg = next === "register";
    const reset = next === "reset";
    document.getElementById("acct-title").textContent = reg ? "注册" : reset ? "重置密码" : "登录";
    document.getElementById("acct-sub").textContent = reg
      ? "在域名对应的家节点上登记名字。口令只以哈希保存在该节点。"
      : reset
        ? "本机持有该账号私钥时，可以设置新密码。"
        : "使用用户名@域名和密码登录家节点";
    document.getElementById("acct-nick-row").style.display = reg ? "" : "none";
    document.getElementById("acct-pass2-row").style.display = (reg || reset) ? "" : "none";
    document.getElementById("acct-submit").textContent = reg ? "注册" : reset ? "设置新密码" : "登录";
    document.getElementById("acct-switch").textContent = reg || reset ? "已有账号？去登录" : "没有账号？去注册";
    document.getElementById("acct-forgot").style.display = next === "login" ? "" : "none";
    document.getElementById("acct-import-open").style.display = next === "register" ? "none" : "";
    if (next === "register") showImport(false);
    document.getElementById("acct-password").autocomplete = reg || reset ? "new-password" : "current-password";
    document.getElementById("acct-password").placeholder = reset ? "新密码，至少 8 位" : "至少 8 位";
    const box = errBox();
    if (box) box.textContent = "";
    hideAsk();
  }

  function hideSuggest() {
    const el = document.getElementById("acct-suggest");
    if (el) { el.hidden = true; el.innerHTML = ""; }
  }

  async function suggestDomains() {
    const input = document.getElementById("acct-domain");
    const hint = document.getElementById("acct-domain-hint");
    const box = document.getElementById("acct-suggest");
    const q = input.value.trim().toLowerCase();
    hint.classList.remove("ok");
    if (!q.includes(".")) { hideSuggest(); hint.textContent = ""; return; }
    try {
      const r = await NM.inv("account_suggest", { q, registry: "" });
      const items = (r && r.items) || [];
      const status = (r && r.status) || "";
      if (document.getElementById("acct-domain").value.trim().toLowerCase() !== q) return;
      if (items.length) {
        const exact = status === "exact" || items.some((it) => String(it.domain).toLowerCase() === q);
        hint.textContent = exact ? "已登记" : "";
        hint.classList.toggle("ok", exact);
        box.innerHTML = items.map((it) =>
          `<button type="button" data-domain="${escapeHtml(it.domain)}">${escapeHtml(it.domain)}</button>`).join("");
        box.hidden = false;
        box.querySelectorAll("button").forEach((btn) => btn.addEventListener("click", () => {
          input.value = btn.getAttribute("data-domain") || "";
          hint.textContent = "已登记";
          hint.classList.add("ok");
          hideSuggest();
        }));
        return;
      }
      hideSuggest();
      if (status === "disabled") {
        hint.textContent = "这个域名已停用。";
        return;
      }
      hint.textContent = "这个域名还没有在注册中心登记，不能用来注册或登录。";
    } catch (_) {
      hideSuggest();
      hint.textContent = q.includes(".") ? "暂时无法查询域名" : "";
    }
  }

  async function enter(r, nickname) {
    Identity.setCurrent(r.user);
    if (nickname) Identity.setName(r.user, nickname);
    else if (!Identity.nameOf(r.user)) Identity.setName(r.user, r.name);
    window.CURRENT_SVC = {
      id: "account", mode: "nat", node: r.node, name: r.name,
      relayUrls: [], pkarrUrl: "", dnsOrigin: "",
    };
    showMainView();
    if (typeof updateUserChip === "function") updateUserChip();
    if (typeof imStart === "function") await imStart(r.user);
  }

  async function submit(e) {
    if (e) e.preventDefault();
    const local = document.getElementById("acct-local").value.trim();
    const domain = document.getElementById("acct-domain").value.trim();
    const password = document.getElementById("acct-password").value;
    const pass2 = document.getElementById("acct-pass2").value;
    const nickname = document.getElementById("acct-nick").value.trim();
    const box = errBox();
    const btn = document.getElementById("acct-submit");
    box.textContent = "";
    hideAsk();
    if (!local || !domain || !password) { box.textContent = "请填写用户名、域名和密码"; return; }
    if (password.length < 8) { box.textContent = "密码至少 8 位"; return; }
    if ((mode === "register" || mode === "reset") && password !== pass2) {
      box.textContent = "两次密码不一致";
      return;
    }
    btn.disabled = true;
    btn.textContent = mode === "register" ? "注册中…" : mode === "reset" ? "设置中…" : "登录中…";
    try {
      if (mode === "reset") {
        const user = map()[fullName()] || "";
        const owner = await NM.inv("account_reset", { local, domain, password, user, registry: "" });
        if (owner) saveKey(fullName(), owner);
        document.getElementById("acct-password").value = "";
        document.getElementById("acct-pass2").value = "";
        setMode("login");
        box.textContent = "新密码已写入家节点，请登录。";
        return;
      }
      let r;
      if (mode === "register") {
        r = await NM.inv("account_register", { local, domain, nickname, password, registry: "" });
      } else {
        const user = map()[fullName()] || "";
        r = await NM.inv("account_login", { local, domain, password, user, nickname, registry: "" });
      }
      saveKey(r.name, r.user);
      remember(local.toLowerCase(), domain.toLowerCase());
      document.getElementById("acct-password").value = "";
      document.getElementById("acct-pass2").value = "";
      await enter(r, nickname);
    } catch (err) {
      const code = codeOf(err);
      if (mode === "login" && code === "no_such_user") {
        const name = fullName();
        const yes = await ask("家节点上没有用户「" + name + "」。是否现在注册？", "注册");
        if (yes) {
          document.getElementById("acct-password").value = "";
          document.getElementById("acct-pass2").value = "";
          setMode("register");
        }
        return;
      }
      if (mode === "register" && code === "name_taken") {
        const yes = await ask("用户「" + fullName() + "」已经存在。是否改为登录？", "登录");
        if (yes) setMode("login");
        return;
      }
      if (code === "not_key_owner") {
        box.textContent = MSG.not_key_owner;
        showImport(true);
        return;
      }
      box.textContent = explain(err);
    } finally {
      btn.disabled = false;
      btn.textContent = mode === "register" ? "注册" : mode === "reset" ? "设置新密码" : "登录";
    }
  }

  function openAccountPassword() {
    const last = lastAccount();
    if (!last || !last.local || !last.domain) {
      if (window.toast) toast("当前账号不是从登录页进入的，无法修改登录密码");
      return;
    }
    const old = document.createElement("div");
    old.className = "sec-overlay on";
    old.innerHTML = `
      <div class="sec-box">
        <div class="sec-head">修改登录密码<button class="sec-x" type="button">${typeof nmIcon === "function" ? nmIcon("close") : "×"}</button></div>
        <div class="sec-body">
          <p class="acct-hint" style="color:var(--ink2)">账号 ${escapeHtml(last.local)}@${escapeHtml(last.domain)}。新密码只以哈希写在家节点。</p>
          <div class="login-form__item"><label>旧密码</label><input id="ap-old" type="password" /></div>
          <div class="login-form__item"><label>新密码</label><input id="ap-new" type="password" placeholder="至少 8 位" /></div>
          <div class="login-form__item"><label>确认新密码</label><input id="ap-new2" type="password" /></div>
          <p class="error" id="ap-err"></p>
          <button class="login-submit" id="ap-save" type="button">保存</button>
        </div>
      </div>`;
    document.body.appendChild(old);
    const close = () => old.remove();
    old.querySelector(".sec-x").addEventListener("click", close);
    old.addEventListener("click", (ev) => { if (ev.target === old) close(); });
    old.querySelector("#ap-save").addEventListener("click", async () => {
      const a = old.querySelector("#ap-old").value;
      const b = old.querySelector("#ap-new").value;
      const c = old.querySelector("#ap-new2").value;
      const err = old.querySelector("#ap-err");
      err.textContent = "";
      if (!a || !b) { err.textContent = "请填写旧密码和新密码"; return; }
      if (b.length < 8) { err.textContent = "密码至少 8 位"; return; }
      if (b !== c) { err.textContent = "两次新密码不一致"; return; }
      try {
        await NM.inv("account_passwd", { local: last.local, domain: last.domain, oldPassword: a, password: b });
        close();
        if (window.toast) toast("登录密码已更新");
      } catch (e) {
        err.textContent = explain(e);
      }
    });
  }
  window.openAccountPassword = openAccountPassword;

  function init() {
    const form = document.getElementById("account-form");
    if (!form) return;
    const last = lastAccount();
    if (last) {
      if (last.local) document.getElementById("acct-local").value = last.local;
      if (last.domain) document.getElementById("acct-domain").value = last.domain;
    }
    form.addEventListener("submit", submit);
    document.getElementById("acct-import-open").addEventListener("click", () => {
      showImport(document.getElementById("acct-import").hidden);
    });
    document.getElementById("acct-import-cancel").addEventListener("click", () => showImport(false));
    document.getElementById("acct-import-go").addEventListener("click", doImport);
    document.querySelectorAll("#acct-import .acct-tabs button").forEach((b) => b.addEventListener("click", () => showTab(b.dataset.tab)));
    document.getElementById("acct-pair-go").addEventListener("click", () => startPair("push"));
    document.getElementById("acct-ticket-go").addEventListener("click", () => startPair("ticket"));
    document.getElementById("acct-ticket-copy").addEventListener("click", async () => {
      const v = document.getElementById("acct-ticket-out").value;
      if (!v) return;
      try { await navigator.clipboard.writeText(v); if (window.toast) toast("迁移串已复制"); } catch (_) {}
    });
    NM.onEvent("pair://event", onPairEvent);
    document.getElementById("acct-switch").addEventListener("click", () => {
      setMode(mode === "login" ? "register" : "login");
    });
    document.getElementById("acct-forgot").addEventListener("click", () => {
      if (!document.getElementById("acct-local").value.trim() || !document.getElementById("acct-domain").value.trim()) {
        errBox().textContent = "请先填写用户名和域名";
        return;
      }
      document.getElementById("acct-password").value = "";
      document.getElementById("acct-pass2").value = "";
      setMode("reset");
    });
    document.getElementById("acct-domain").addEventListener("input", () => {
      clearTimeout(suggestTimer);
      suggestTimer = setTimeout(suggestDomains, 300);
    });
    document.addEventListener("click", (ev) => {
      if (!ev.target.closest(".acct-id-wrap")) hideSuggest();
    });
    document.querySelectorAll(".pwd-eye").forEach((btn) => btn.addEventListener("click", () => {
      const input = document.getElementById(btn.getAttribute("data-eye"));
      if (!input) return;
      const show = input.type === "password";
      input.type = show ? "text" : "password";
      btn.textContent = show ? "隐藏" : "显示";
    }));
  }
  init();
})();

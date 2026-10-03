// 我的设备：列表、改名、吊销（管理设备）、紧急冻结（登录密码），以及本机被吊销时的处理。
(function () {
  let self = null;
  let kicked = false;

  function el(id) { return document.getElementById(id); }
  function errText(e) { return String((e && e.message) || e || "失败"); }
  function say(t, ok) {
    const m = el("dev-msg");
    if (m) { m.textContent = t || ""; m.className = "sec-msg" + (ok ? " ok" : ""); }
  }
  function open() {
    if (typeof closeSecModal === "function") closeSecModal();
    el("sec-frz-pw").value = "";
    el("sec-pair-in").value = "";
    say("");
    if (window.dlgShowPane) dlgShowPane(el("dev-modal").querySelector(".dlg"), "list");
    el("dev-modal").classList.add("on");
    refresh();
  }
  function close() { el("dev-modal").classList.remove("on"); }
  function fmtTime(ms) { return ms ? new Date(ms).toLocaleString() : "—"; }
  function accountName() {
    const name = (window.CURRENT_SVC && window.CURRENT_SVC.name) || "";
    const at = name.lastIndexOf("@");
    return at > 0 ? { local: name.slice(0, at), domain: name.slice(at + 1) } : null;
  }

  const REASON = { lost: "设备丢失", stolen: "设备被盗", retired: "不再使用", frozen: "紧急冻结" };

  function rowHtml(d) {
    const tags = [];
    if (d.current) tags.push('<span class="dev-tag cur">本机</span>');
    tags.push(d.role === "admin" ? '<span class="dev-tag adm">管理</span>' : '<span class="dev-tag">普通</span>');
    if (d.revoked) tags.push('<span class="dev-tag off">已吊销</span>');
    else if (d.online) tags.push('<span class="dev-tag on">在线</span>');
    const sub = d.revoked
      ? "吊销于 " + fmtTime(d.revoked.at) + (d.revoked.byNode ? "（家节点冻结）" : "") +
        (d.revoked.reason ? " · " + escapeHtml(REASON[d.revoked.reason] || d.revoked.reason) : "")
      : "授权于 " + fmtTime(d.issuedAt) + " · 最近在线 " + (d.online ? "现在" : fmtTime(d.lastSeen));
    const ops = [];
    if (!d.revoked) {
      ops.push(`<button class="acct-link" data-op="rename">改名</button>`);
      if (!d.current) {
        ops.push(self && self.admin
          ? `<button class="acct-link dev-danger" data-op="revoke">吊销</button>`
          : `<button class="acct-link dev-danger" data-op="freeze">冻结</button>`);
      }
    }
    const ico = typeof nmIcon === "function" ? nmIcon("device") : "";
    const cls = d.revoked ? " off" : d.current ? " cur" : "";
    return `<div class="dev-row" data-dev="${escapeHtml(d.device)}" data-label="${escapeHtml(d.label || "")}">
      <span class="dev-ico${cls}">${ico}</span>
      <div class="dev-main">
        <div class="dev-name">${escapeHtml(d.label || "未命名设备")} ${tags.join("")}</div>
        <div class="dev-sub">${sub}</div>
        <div class="dev-id">${escapeHtml(d.device.slice(0, 16))}…</div>
      </div>
      <div class="dev-ops">${ops.join("")}</div>
    </div>`;
  }

  async function refresh() {
    const box = el("sec-dev-list");
    if (!box) return;
    box.innerHTML = '<div class="au-empty">加载中…</div>';
    try {
      self = await NM.inv("device_self");
      const hint = el("sec-dev-hint");
      if (hint) {
        hint.textContent = self.admin
          ? "本机是管理设备（持有账号私钥），可以吊销其它设备。丢了一台只需吊销它，账号和其它设备不受影响。"
          : "本机是普通设备，只持有自己的设备密钥。吊销其它设备需在管理设备上操作；紧急时可用下方登录密码冻结。";
      }
      if (!self.multiDevice) {
        box.innerHTML = '<div class="au-empty">家节点不支持多设备，当前直接使用账号私钥连接。</div>';
        return;
      }
      const list = await NM.inv("device_list");
      list.sort((a, b) => (b.current - a.current) || (!!a.revoked - !!b.revoked) || (b.issuedAt - a.issuedAt));
      box.innerHTML = list.length ? list.map(rowHtml).join("") : '<div class="au-empty">暂无登记的设备</div>';
    } catch (e) {
      box.innerHTML = '<div class="au-empty">' + escapeHtml(errText(e)) + "</div>";
    }
  }

  async function onRowClick(ev) {
    const btn = ev.target.closest("button[data-op]");
    if (!btn) return;
    const row = btn.closest(".dev-row");
    const device = row && row.dataset.dev;
    if (!device) return;
    const op = btn.dataset.op;
    const label = row.dataset.label || "未命名设备";
    try {
      if (op === "rename") {
        startRename(row, device);
        return;
      } else if (op === "revoke") {
        if (!confirm("吊销「" + label + "」？吊销后它会立即掉线且不能再登录，此操作不可撤销。")) return;
        await NM.inv("device_revoke", { device, reason: "lost" });
        say("已吊销「" + label + "」", true);
      } else if (op === "freeze") {
        await freeze(device, label);
        return;
      }
      await refresh();
    } catch (e) {
      say(errText(e) === "not_admin_device" ? "本机不是管理设备，不能吊销" : errText(e));
    }
  }

  function startRename(row, device) {
    const name = row.querySelector(".dev-name");
    const input = document.createElement("input");
    input.className = "sec-input dev-rename";
    input.maxLength = 64;
    input.value = row.dataset.label || "";
    name.replaceWith(input);
    input.focus();
    input.select();
    let done = false;
    const finish = async (save) => {
      if (done) return;
      done = true;
      const label = input.value.trim();
      if (save && label && label !== row.dataset.label) {
        try { await NM.inv("device_rename", { device, label }); say("已改名", true); }
        catch (e) { say(errText(e)); }
      }
      refresh();
    };
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") finish(true);
      else if (e.key === "Escape") finish(false);
    });
    input.addEventListener("blur", () => finish(true));
  }

  async function freeze(device, label) {
    const acct = accountName();
    const password = el("sec-frz-pw").value;
    if (!acct) { say("当前账号不是从登录页进入的，无法冻结"); return; }
    if (!password) {
      if (window.dlgShowPane) dlgShowPane(el("dev-modal").querySelector(".dlg"), "freeze");
      say("请先输入登录密码，再回到设备列表点「冻结」");
      el("sec-frz-pw").focus();
      return;
    }
    const what = device ? "「" + label + "」" : "本账号全部设备（包括本机）";
    if (!confirm("冻结" + what + "？冻结后不能恢复，只能用持有账号私钥的设备或备份重新登录。")) return;
    try {
      const n = await NM.inv("device_freeze", { local: acct.local, domain: acct.domain, password, device: device || "", registry: "" });
      el("sec-frz-pw").value = "";
      say("已冻结 " + n + " 台设备", true);
      await refresh();
    } catch (e) {
      say(errText(e) === "bad_password" ? "密码不正确" : errText(e));
    }
  }

  async function onSelfRevoked() {
    if (kicked) return;
    kicked = true;
    if (typeof closeSecModal === "function") closeSecModal();
    close();
    if (typeof window.imDisconnect === "function") await window.imDisconnect();
    alert("本设备已被吊销，已退出登录。\n若这是你自己的设备，请在登录页用「换了设备？」重新授权本机。");
    kicked = false;
  }

  function onEvent(ev) {
    if (!ev) return;
    if (ev.event === "self_revoked" || (ev.event === "revoked" && self && ev.device === self.device)) {
      onSelfRevoked();
      return;
    }
    if (ev.event === "revoked") {
      if (window.toast) toast(ev.by_node ? "家节点已冻结一台设备" : "一台设备已被吊销");
      if (el("dev-modal").classList.contains("on")) refresh();
    }
  }

  function init() {
    if (!el("sec-dev-list")) return;
    el("sec-dev-list").addEventListener("click", onRowClick);
    el("sec-dev-refresh").addEventListener("click", refresh);
    el("sec-frz-all").addEventListener("click", () => freeze("", ""));
    el("dev-close").addEventListener("click", close);
    el("dev-modal").addEventListener("click", (e) => { if (e.target === el("dev-modal")) close(); });
    const go = el("sec-dev-open");
    if (go) go.addEventListener("click", open);
    NM.onEvent("device://event", onEvent);
  }

  window.Devices = { open, close, refresh, say };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();

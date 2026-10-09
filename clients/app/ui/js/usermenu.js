// ── 标题栏用户切换（多账号）：显示当前身份，点开下拉切换其他身份或回启动页新建/换号。
//    切换 = 断开当前会话 → 用目标身份重连到「当前节点服务」（节点服务是公用的）。
//    复用 connect.js 的 connectAs / escapeHtml / shortNode 与 im.js 的 imDisconnect。

function uColor(id) {
  const p = ["#3987e5", "#1fb182", "#9085e9", "#d55181", "#eb6834", "#22c55e", "#e5b95f"];
  let h = 0; for (let i = 0; i < (id || "").length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return p[h % p.length];
}

function updateUserChip() {
  const cur = Identity.current();
  const nameEl = document.getElementById("tb-user-name");
  const avEl = document.getElementById("tb-user-av");
  if (nameEl) nameEl.textContent = cur ? Identity.label(cur) : "未登录";
  const btn = document.getElementById("tb-user");
  if (btn) btn.title = cur ? Identity.label(cur) + " · 点击切换身份 / 编辑资料" : "未登录（点击）"; // 昵称经悬浮提示可见
  if (avEl) {
    const av = (window.Profile && cur) ? Profile.ownAvatar(cur) : "";
    if (window.Profile && Profile.isImg(av)) {
      avEl.style.background = "transparent";
      avEl.innerHTML = `<img src="${av}" alt="" style="width:100%;height:100%;object-fit:cover;border-radius:var(--avatar-radius)">`;
    } else {
      avEl.innerHTML = cur ? escapeHtml(Identity.label(cur).slice(0, 1)) : "?";
      avEl.style.background = cur ? uColor(cur) : "var(--muted)";
    }
    if (window.Profile && cur) { // 自己的在线状态圆点
      avEl.insertAdjacentHTML("beforeend", Profile.presenceDot(Profile.get(cur).status || "online", 10));
    }
  }
}
window.updateUserChip = updateUserChip;

async function toggleUserMenu() {
  const m = document.getElementById("user-menu");
  if (!m) return;
  if (m.classList.contains("on")) { m.classList.remove("on"); return; }
  const cur = Identity.current();
  const ids = await Identity.list();
  const chev = `<span class="um-chev">${nmIcon("chevron")}</span>`;
  const row = (sub, ico, title, hint) => `<div class="um-item um-subrow" data-sub="${sub}"><span class="um-av um-plus">${nmIcon(ico)}</span><span class="um-meta"><b>${title}</b>${hint ? `<small>${hint}</small>` : ""}</span>${chev}</div>`;
  const st = cur && window.Profile ? (Profile.get(cur).status || "online") : "";
  const light = document.documentElement.getAttribute("data-theme") === "light";
  const langName = window.nmLocale && nmLocale() === "en" ? "English" : "中文";
  const head = cur ? (() => {
    const av = window.Profile ? Profile.ownAvatar(cur) : "";
    const face = (window.Profile && Profile.isImg(av))
      ? `<span class="um-av" style="padding:0;overflow:hidden"><img src="${av}" alt="" style="width:100%;height:100%;object-fit:cover"></span>`
      : `<span class="um-av" style="background:${uColor(cur)}">${escapeHtml(Identity.label(cur).slice(0, 1))}</span>`;
    return `<div class="um-item um-head">${face}<span class="um-meta"><b>${escapeHtml(Identity.label(cur))}</b><small class="mono">${shortNode(cur)}</small></span></div><div class="um-sep"></div>`;
  })() : "";
  m.innerHTML = head +
    (cur && window.Profile ? row("status", "person", "状态", Profile.presenceLabel(st)) : "") +
    (cur ? row("account", "contacts", "账号", ids.length > 1 ? ids.length + " 个身份" : "资料与设备") : "") +
    row("look", light ? "moon" : "sun", "外观", (light ? "浅色" : "深色") + " · " + langName) +
    row("app", "shield", "通知与安全", window.Tray && Tray.available() ? "托盘、锁定、备份" : "锁定与备份") +
    `<div class="um-fly" id="um-fly" hidden></div>`;
  m.classList.add("on");
  const closeAll = () => m.classList.remove("on");
  const identRow = (pk) => {
    const av = window.Profile ? Profile.ownAvatar(pk) : "";
    const face = (window.Profile && Profile.isImg(av))
      ? `<span class="um-av" style="padding:0;overflow:hidden"><img src="${av}" alt="" style="width:100%;height:100%;object-fit:cover"></span>`
      : `<span class="um-av" style="background:${uColor(pk)}">${escapeHtml(Identity.label(pk).slice(0, 1))}</span>`;
    return `<div class="um-item ${pk === cur ? "on" : ""}" data-pk="${pk}">${face}<span class="um-meta"><b>${escapeHtml(Identity.label(pk))}</b><small class="mono">${shortNode(pk)}</small></span></div>`;
  };
  const subs = {
    status: cur && window.Profile ? `<div class="um-status" id="um-status">${["online", "away", "busy", "dnd"].map((s) => `<button class="um-st${st === s ? " on" : ""}" data-st="${s}"><span class="um-stdot" style="background:${Profile.presenceColor(s)}"></span>${Profile.presenceLabel(s)}</button>`).join("")}</div>` : "",
    account: cur ? [
      ids.map(identRow).join(""),
      `<div class="um-sep"></div>`,
      `<div class="um-item" id="um-copy"><span class="um-av um-plus">${nmIcon("copy")}</span><span class="um-meta"><b>复制用户 id</b></span></div>`,
      `<div class="um-item" id="um-profile"><span class="um-av um-plus">${nmIcon("person")}</span><span class="um-meta"><b>编辑资料</b></span></div>`,
      `<div class="um-item" id="um-passwd"><span class="um-av um-plus">${nmIcon("lock")}</span><span class="um-meta"><b>修改登录密码</b></span></div>`,
      `<div class="um-item" id="um-devices"><span class="um-av um-plus">${nmIcon("device")}</span><span class="um-meta"><b>我的设备</b></span></div>`,
      `<div class="um-sep"></div>`,
      `<div class="um-item" id="um-new"><span class="um-av um-plus">${nmIcon("disconnect")}</span><span class="um-meta"><b>切换账号</b><small>回到登录页</small></span></div>`,
    ].join("") : "",
    look: [
      `<div class="um-item" id="um-theme"><span class="um-av um-plus">${nmIcon(light ? "moon" : "sun")}</span><span class="um-meta"><b>${light ? "切换为深色" : "切换为浅色"}</b></span></div>`,
      `<div class="um-item" id="um-lang"><span class="um-av um-plus">${nmIcon("ticker")}</span><span class="um-meta"><b>${window.t ? t("menu.lang") : "语言"}</b><small>${langName}</small></span></div>`,
    ].join(""),
    app: [
      window.Tray && Tray.available() ? `<div class="um-item" id="um-notify"><span class="um-av um-plus">${nmIcon("bell")}</span><span class="um-meta"><b>通知与托盘</b><small>通知、免打扰、快捷键</small></span></div>` : "",
      `<div class="um-item" id="um-sec"><span class="um-av um-plus">${nmIcon("shield")}</span><span class="um-meta"><b>安全设置</b><small>自动锁定、口令、备份</small></span></div>`,
    ].join(""),
  };
  const fly = document.getElementById("um-fly");
  const placeFly = (row) => {
    const mr = m.getBoundingClientRect();
    const rr = row.getBoundingClientRect();
    fly.hidden = false;
    fly.style.left = (mr.right + 6) + "px";
    fly.style.top = Math.max(8, rr.top) + "px";
    const fr = fly.getBoundingClientRect();
    if (fr.right > window.innerWidth - 8) fly.style.left = Math.max(8, mr.left - fr.width - 6) + "px";
    const fr2 = fly.getBoundingClientRect();
    if (fr2.bottom > window.innerHeight - 8) fly.style.top = Math.max(8, window.innerHeight - fr2.height - 8) + "px";
  };
  m.querySelectorAll(".um-subrow").forEach((row) => row.addEventListener("click", (e) => {
    e.stopPropagation();
    if (fly.dataset.for === row.dataset.sub && !fly.hidden) { fly.hidden = true; row.classList.remove("on"); return; }
    m.querySelectorAll(".um-subrow").forEach((r) => r.classList.remove("on"));
    row.classList.add("on");
    fly.dataset.for = row.dataset.sub;
    fly.innerHTML = subs[row.dataset.sub] || "";
    placeFly(row);
  }));
  fly.addEventListener("click", async (e) => {
    const stBtn = e.target.closest(".um-st");
    const pkEl = e.target.closest(".um-item[data-pk]");
    const act = e.target.closest("#um-copy, #um-profile, #um-passwd, #um-devices, #um-notify, #um-sec, #um-new, #um-theme, #um-lang");
    if (!stBtn && !pkEl && !act) return;
    const id = act ? act.id : "";
    closeAll();
    if (stBtn) { await setMyStatus(stBtn.dataset.st); return; }
    if (pkEl) { if (pkEl.dataset.pk !== cur) switchUser(pkEl.dataset.pk); return; }
    if (id === "um-copy") {
      try { await navigator.clipboard.writeText(cur); if (window.toast) toast("已复制当前用户 id"); }
      catch (_) { if (window.toast) toast("复制失败，请手动选中"); }
    } else if (id === "um-profile" && typeof openProfileModal === "function") openProfileModal();
    else if (id === "um-passwd" && typeof openAccountPassword === "function") openAccountPassword();
    else if (id === "um-devices" && window.Devices) Devices.open();
    else if (id === "um-notify" && window.Tray) Tray.openSettings();
    else if (id === "um-sec" && typeof openSecModal === "function") openSecModal();
    else if (id === "um-new" && typeof imDisconnect === "function") imDisconnect();
    else if (id === "um-theme" && typeof toggleTheme === "function") toggleTheme();
    else if (id === "um-lang" && window.nmSetLocale) nmSetLocale(window.nmLocale && nmLocale() === "en" ? "zh-CN" : "en");
  });
}

async function setMyStatus(st) {
  const cur = Identity.current();
  if (!cur || !window.Profile) return;
  const p = Profile.get(cur); p.status = st; Profile.save(cur, p);
  try { await NM.inv("presence_set", { status: st }); } catch (_) {}
  updateUserChip();
  if (window.Tray) Tray.sync();
  if (window.toast) toast("状态：" + Profile.presenceLabel(st));
}
window.setMyStatus = setMyStatus;

async function switchUser(pk) {
  // Bug 3: 优先用目标账号自己记住的节点服务；没有才回退当前节点（首次切到该账号时）。
  const svc = Identity.svcOf(pk) || window.CURRENT_SVC;
  if (!svc) { if (window.toast) toast("无当前节点服务，请回启动页重连"); return; }
  if (window.toast) toast("切换到 " + Identity.label(pk) + "…");
  try { await NM.inv("disconnect"); } catch (_) {}
  try { await connectAs(pk, svc, Identity.nameOf(pk)); }
  catch (e) { if (window.toast) toast("切换失败：" + (e && e.message ? e.message : e)); if (typeof showLoginView === "function") showLoginView(); }
}

(function initUserMenu() {
  const btn = document.getElementById("tb-user");
  if (btn) btn.addEventListener("click", (e) => { e.stopPropagation(); toggleUserMenu(); });
  document.addEventListener("click", (e) => {
    const m = document.getElementById("user-menu");
    if (m && m.classList.contains("on") && !e.target.closest("#user-menu") && !e.target.closest("#tb-user")) m.classList.remove("on");
  });
})();

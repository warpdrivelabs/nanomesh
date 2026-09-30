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
  m.innerHTML = ids.map((pk) => {
    const av = (window.Profile) ? Profile.ownAvatar(pk) : "";
    const avHtml = (window.Profile && Profile.isImg(av))
      ? `<span class="um-av" style="padding:0;overflow:hidden"><img src="${av}" alt="" style="width:100%;height:100%;object-fit:cover"></span>`
      : `<span class="um-av" style="background:${uColor(pk)}">${escapeHtml(Identity.label(pk).slice(0, 1))}</span>`;
    return `
    <div class="um-item ${pk === cur ? "on" : ""}" data-pk="${pk}">
      ${avHtml}
      <span class="um-meta"><b>${escapeHtml(Identity.label(pk))}</b><small>${shortNode(pk)}</small></span>
      ${pk === cur ? '<span class="um-cur">当前</span>' : ""}
    </div>`;
  }).join("") +
    `<div class="um-sep"></div>
     ${cur && window.Profile ? `<div class="um-status" id="um-status">${["online", "away", "busy", "dnd"].map((s) => `<button class="um-st${(Profile.get(cur).status || "online") === s ? " on" : ""}" data-st="${s}"><span class="um-stdot" style="background:${Profile.presenceColor(s)}"></span>${Profile.presenceLabel(s)}</button>`).join("")}</div><div class="um-sep"></div>` : ""}
     ${cur ? `<div class="um-item um-copy" id="um-copy"><span class="um-av um-plus">${nmIcon("copy")}</span><span class="um-meta"><b>复制当前用户 id</b><small>${shortNode(cur)}</small></span></div>` : ""}
     ${cur ? `<div class="um-item um-profile" id="um-profile"><span class="um-av um-plus">🪪</span><span class="um-meta"><b>编辑资料</b><small>昵称 · 状态 · 简介</small></span></div>` : ""}
     ${cur ? `<div class="um-item" id="um-passwd"><span class="um-av um-plus">${nmIcon("shield")}</span><span class="um-meta"><b>修改登录密码</b><small>在家节点上更新口令哈希</small></span></div>` : ""}
     <div class="um-item um-sec" id="um-sec"><span class="um-av um-plus">${nmIcon("shield")}</span><span class="um-meta"><b>安全设置</b><small>自动锁定 · 改口令 · 备份</small></span></div>
     <div class="um-item um-new" id="um-new"><span class="um-av um-plus">${nmIcon("plus")}</span><span class="um-meta"><b>新建 / 切换账号</b><small>回到启动页</small></span></div>`;
  m.classList.add("on");
  m.querySelectorAll("#um-status .um-st").forEach((b) => b.addEventListener("click", async () => {
    const st = b.dataset.st;
    const p = Profile.get(cur); p.status = st; Profile.save(cur, p);
    try { await NM.inv("presence_set", { status: st }); } catch (_) {}
    if (typeof updateUserChip === "function") updateUserChip();
    m.classList.remove("on");
    if (window.toast) toast("状态：" + Profile.presenceLabel(st));
  }));
  m.querySelectorAll(".um-item[data-pk]").forEach((el) => el.addEventListener("click", () => {
    m.classList.remove("on");
    const pk = el.dataset.pk;
    if (pk !== cur) switchUser(pk);
  }));
  const cp = document.getElementById("um-copy");
  if (cp) cp.addEventListener("click", async () => {
    m.classList.remove("on");
    try { await navigator.clipboard.writeText(cur); if (window.toast) toast("已复制当前用户 id"); }
    catch (_) { if (window.toast) toast("复制失败，请手动选中"); }
  });
  const pf = document.getElementById("um-profile");
  if (pf) pf.addEventListener("click", () => { m.classList.remove("on"); if (typeof openProfileModal === "function") openProfileModal(); });
  const pw = document.getElementById("um-passwd");
  if (pw) pw.addEventListener("click", () => { m.classList.remove("on"); if (typeof openAccountPassword === "function") openAccountPassword(); });
  const sec = document.getElementById("um-sec");
  if (sec) sec.addEventListener("click", () => { m.classList.remove("on"); if (typeof openSecModal === "function") openSecModal(); });  const nw = document.getElementById("um-new");
  if (nw) nw.addEventListener("click", () => { m.classList.remove("on"); if (typeof imDisconnect === "function") imDisconnect(); });
}

async function switchUser(pk) {
  const svc = window.CURRENT_SVC;
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

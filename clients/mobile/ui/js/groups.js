// ── 群组（P3）：建群 · 角色(owner/admin/member) · 加/删成员 · 提升/取消管理员 · 改名/解散 · 进入群聊。
//    群权威状态在群 home 节点；本模块经 Tauri group_* 命令读写，成员名从实体目录解析。
(function () {
  let GROUPS = [];
  let MYID = "";

  const roleOf = (g, id) => (g.owner === id ? "owner" : (g.admins || []).includes(id) ? "admin" : (g.members || []).includes(id) ? "member" : "");
  const roleLabel = (r) => {
    const fb = { owner: "群主", admin: "管理员", member: "成员" }[r] || "";
    const key = { owner: "role.owner", admin: "role.admin", member: "role.member" }[r];
    return key && window.t ? t(key) : fb;
  };
  const myRole = (g) => roleOf(g, MYID);
  const byId = (gid) => GROUPS.find((g) => g.id === gid) || null;
  // 成员显示名：优先实体目录/本地名，其次自己，最后短 id。
  const nameOf = (id) => (id === MYID ? "我" : (window.entityName ? window.entityName(id) : (id || "").slice(0, 8) + "…"));

  function slimGroup(g) {
    const o = Object.assign({}, g);
    if (typeof o.avatar === "string" && o.avatar.indexOf("data:") === 0) delete o.avatar;
    return o;
  }
  function hydrate(list) { GROUPS = Array.isArray(list) ? list.slice() : []; paintGroupsList(); }
  async function refresh() {
    if (!window.NM || !NM.hasTauri || !NM.hasTauri()) return;
    try { MYID = MYID || (await NM.inv("my_id").catch(() => "")); } catch (_) {}
    try {
      GROUPS = (await NM.inv("group_list")) || [];
      if (window.ListCache) ListCache.put({ groups: GROUPS.map(slimGroup) });
    } catch (e) {
      if (!GROUPS.length && window.toast) toast("群列表刷新失败");
    }
  }

  // 群头像：已解析为 data:URI → 图；否则 👥 底色块。
  function groupAv(g, size) {
    const s = size || 40;
    if (window.Profile && Profile.isImg(g.avatar)) return `<img src="${g.avatar}" alt="" style="width:${s}px;height:${s}px;border-radius:var(--avatar-radius);object-fit:cover;display:block">`;
    return `<span class="av" style="width:${s}px;height:${s}px;background:${avatarColor(g.id)}">${nmIcon("groups")}</span>`;
  }

  async function renderGroupsList() {
    await refresh();
    paintGroupsList();
    if (window.Profile) Profile.resolveList(GROUPS, paintGroupsList); // 解析 b3: 群头像后重绘
  }
  function paintGroupsList() {
    const box = document.getElementById("groups-list");
    if (!box) return;
    const q = ((document.getElementById("grp-search") || {}).value || "").trim().toLowerCase();
    const list = GROUPS.filter((g) => !q || (g.name || "").toLowerCase().includes(q) || (g.topic || "").toLowerCase().includes(q));
    if (!list.length) { box.innerHTML = `<div class="ns-empty">${GROUPS.length ? (window.t ? t("list.noGroupHit") : "没有匹配的群") : (window.t ? t("list.noGroup") : "还没有群组。点上方加号新建。")}</div>`; return; }
    box.innerHTML = list.map((g) => `
      <div class="im-item" data-gid="${g.id}" title="${escapeHtml(g.id)}">
        ${groupAv(g)}
        <span class="mid">
          <span class="r1"><span class="nm">${escapeHtml(g.name || "(未命名群)")}</span></span>
          <span class="r2"><span class="msg">${(g.members || []).length} 人 · ${roleLabel(myRole(g)) || "非成员"}</span></span>
        </span>
      </div>`).join("");
    box.querySelectorAll(".im-item").forEach((el) => el.addEventListener("click", () => openDetail(el.dataset.gid)));
    syncSelection();
    if (window.Tabs && Tabs.retitle && Tabs.has) {
      GROUPS.forEach((g) => {
        const title = g.name || "群";
        if (Tabs.has("c:" + g.id)) Tabs.retitle("c:" + g.id, title, "groups");
        if (Tabs.has("g:" + g.id)) Tabs.retitle("g:" + g.id, title, "groups");
      });
    }
  }

  function currentGroupId() {
    const key = window.Tabs && Tabs.active ? Tabs.active() : "";
    if (!key) return "";
    if (key.startsWith("g:")) return key.slice(2);
    if (key.startsWith("c:")) { const id = key.slice(2); return byId(id) ? id : ""; }
    return "";
  }
  function syncSelection() {
    const box = document.getElementById("groups-list");
    if (!box) return;
    const sel = currentGroupId();
    box.querySelectorAll(".im-item").forEach((el) => el.classList.toggle("on", !!sel && el.dataset.gid === sel));
  }

  // ── 主区：群详情 + 管理 ──
  // 打开群详情为一个 tab（切走再回来会重绘）。
  function openDetail(gid) {
    const g = byId(gid);
    const title = g ? (g.name || "群") : (gid || "").slice(0, 8) + "…";
    if (window.Tabs) Tabs.open({ key: "g:" + gid, kind: "group", title, ico: "groups", render: () => renderGroupDetail(gid) });
    else renderGroupDetail(gid);
  }
  function renderGroupDetail(gid, opts) {
    const g = byId(gid);
    const conv = document.getElementById("conv");
    if (!g || !conv) return;
    const role = myRole(g);
    const isOwner = role === "owner", isAdmin = role === "owner" || role === "admin";
    const memberRows = (g.members || []).map((id) => {
      const r = roleOf(g, id);
      const acts = [];
      // 踢人：owner/admin，不能踢 owner；admin 不能踢 admin。
      if (isAdmin && id !== g.owner && id !== MYID && !((r === "admin") && !isOwner)) acts.push(`<button class="ns-btn grp-danger grp-kick" data-id="${id}">移除</button>`);
      if (isOwner && id !== g.owner) acts.push(r === "admin" ? `<button class="ns-btn grp-demote" data-id="${id}">取消管理</button>` : `<button class="ns-btn grp-promote" data-id="${id}">设为管理</button>`);
      return `<div class="nsu-item">
        ${window.Profile ? Profile.faceHtml(id, nameOf(id), 32) : `<span class="av" style="background:${avatarColor(id)}">${escapeHtml(nameOf(id).slice(0, 1))}</span>`}
        <span class="nsu-meta"><b>${escapeHtml(nameOf(id))}</b><small class="grp-role grp-role--${r || "member"}">${roleLabel(r)}</small></span>
        ${acts.join("")}
      </div>`;
    }).join("");
    conv.innerHTML = `
      <div class="conv-head"><b>群资料</b></div>
      <div class="detail-scroll"><div class="detail">
        <div class="detail-hero">
          <div class="ed-avatar ed-avatar-slot" style="background:${avatarColor(gid)}">${groupAv(g, 104)}</div>
          <div>
            <div class="detail-name">${escapeHtml(g.name || "(未命名群)")}</div>
            <div class="detail-sub">${(g.members || []).length} 人${g.topic ? " · " + escapeHtml(g.topic) : ""}</div>
          </div>
        </div>
        <div class="detail-actions">
          <button class="ns-btn ns-primary" id="grp-open-chat">进入群聊</button>
          ${isAdmin ? `<button class="ns-btn" id="grp-edit">编辑群信息</button>` : ""}
          ${isAdmin ? `<button class="ns-btn" id="grp-add">添加成员</button>` : ""}
          ${isOwner ? `<button class="ns-btn ns-danger" id="grp-dissolve">解散群</button>` : `<button class="ns-btn ns-danger" id="grp-leave">退出群</button>`}
        </div>
        <div class="detail-rows">
          <div class="detail-row"><span class="k">我的角色</span><span class="v"><span class="grp-role grp-role--${role || "member"}">${roleLabel(role) || "非成员"}</span></span></div>
          <div class="detail-row"><span class="k">群 id</span><span class="v"><span class="mono">${escapeHtml(shortNode(gid))}</span></span></div>
        </div>
        <div class="ed-users">
          <div class="ed-users-head"><span>成员（${(g.members || []).length}）</span></div>
          <div class="nsu-list">${memberRows}</div>
        </div>
      </div></div>`;
    // 群头像若为 b3: 引用，异步解析后就地替换预览。
    if (window.Profile && Profile.isRef(g.avatar)) {
      Profile.resolveAvatar(g.avatar, g.homeNode).then((uri) => {
        if (!uri) return; g.avatar = uri;
        const slot = conv.querySelector(".ed-avatar-slot"); if (slot) slot.innerHTML = groupAv(g, 72);
      });
    }
    const on = (id, fn) => { const el = document.getElementById(id); if (el) el.addEventListener("click", fn); };
    on("grp-open-chat", () => { if (window.openGroupChat) window.openGroupChat(gid); });
    on("grp-edit", () => editGroupMeta(gid));
    on("grp-add", () => openAddMember(gid));
    on("grp-dissolve", async () => { if (confirm("确定解散该群？此操作不可恢复。")) { await act("group_dissolve", { groupId: gid }, "群已解散"); if (window.Tabs) Tabs.close("g:" + gid); else backToList(); renderGroupsList(); } });
    on("grp-leave", async () => { if (confirm("退出该群？")) { await act("group_leave", { groupId: gid }, "已退出"); if (window.Tabs) Tabs.close("g:" + gid); else backToList(); renderGroupsList(); } });
    conv.querySelectorAll(".grp-kick").forEach((b) => b.addEventListener("click", async () => { await act("group_kick", { groupId: gid, target: b.dataset.id }, "已移除"); openDetail(gid); }));
    conv.querySelectorAll(".grp-promote").forEach((b) => b.addEventListener("click", async () => { await act("group_promote", { groupId: gid, target: b.dataset.id }, "已设为管理员"); openDetail(gid); }));
    conv.querySelectorAll(".grp-demote").forEach((b) => b.addEventListener("click", async () => { await act("group_demote", { groupId: gid, target: b.dataset.id }, "已取消管理员"); openDetail(gid); }));
    const pending = (g.members || []).map((id) => (typeof entityById === "function" ? entityById(id) : null)).filter((c) => c && window.Profile && Profile.isRef(c.avatar));
    if (pending.length && !(opts && opts.skipResolve)) {
      Profile.resolveList(pending, () => {
        if (document.querySelector("#conv .ed-avatar-slot")) renderGroupDetail(gid, { skipResolve: true });
      });
    }
  }

  async function act(cmd, args, okMsg) {
    try { await NM.inv(cmd, args); await refresh(); if (window.toast && okMsg) toast(okMsg); return true; }
    catch (e) { if (window.toast) toast("操作失败：" + (e && e.message ? e.message : e)); return false; }
  }
  function backToList() { const conv = document.getElementById("conv"); if (conv) conv.innerHTML = '<div class="im-center"><div class="ico">' + nmIcon("groups") + '</div><div class="txt">选择或新建一个群</div></div>'; renderGroupsList(); }

  async function createGroup() {
    const name = await textPrompt("新建群", "群名称", "");
    if (name == null) return;
    try {
      const gid = await NM.inv("group_create", { name: name || "新群" });
      await renderGroupsList();
      openDetail(gid);
      if (window.toast) toast("群已创建");
    } catch (e) { if (window.toast) toast("建群失败：" + (e && e.message ? e.message : e)); }
  }

  // 编辑群信息（名称 + 简介 + 头像）：owner/admin。经通用 metaEditor 上传头像 blob 后落 group_set_meta。
  async function editGroupMeta(gid) {
    const g = byId(gid);
    if (!g || !window.Profile || !Profile.metaEditor) return;
    Profile.metaEditor({
      title: "编辑群信息", name: g.name || "", bio: g.topic || "", avatar: g.avatar || "", homeNode: g.homeNode || "",
      namePh: "群名称", bioLabel: "群简介", bioPh: "群公告 / 一句话简介",
      onSave: async ({ name, bio, avatar }) => {
        await NM.inv("group_set_meta", { groupId: gid, name: name || (g.name || ""), topic: bio, avatar });
        await refresh();
        if (window.Tabs) { Tabs.retitle("g:" + gid, name || g.name || "群", "groups"); Tabs.retitle("c:" + gid, name || g.name || "群", "groups"); }
        renderGroupsList();
        const conv = document.getElementById("conv");
        if (conv && conv.querySelector(".ed-avatar-slot")) renderGroupDetail(gid); // 详情页在看 → 重绘
        const mb = document.getElementById("grp-members"); if (mb) renderMembersPanel(gid, mb); // 群聊成员栏在看 → 重绘
        if (window.toast) toast("群信息已更新");
      },
    });
  }

  // 添加成员：从实体目录挑人（person）或粘贴公钥。
  async function openAddMember(gid) {
    const g = byId(gid);
    let contacts = [];
    try { contacts = (await NM.inv("directory_query", { kindPrefix: "" })) || []; } catch (_) {}
    const inGroup = new Set((g.members || []));
    const pick = contacts.filter((c) => !inGroup.has(c.id));
    if (window.Profile) await new Promise((r) => Profile.resolveList(pick, r));
    const rows = pick.map((c) => `<div class="nsu-item">${window.Profile ? Profile.faceHtml(c.id, c.name || "?", 32, "", c.avatar) : `<span class="av" style="background:${avatarColor(c.id)}">${escapeHtml((c.name || "?").slice(0, 1))}</span>`}<span class="nsu-meta"><b>${escapeHtml(c.name || shortNode(c.id))}</b><small>${escapeHtml(shortNode(c.id))}</small></span><button class="ns-btn grp-addone" data-id="${c.id}">添加</button></div>`).join("") || '<div class="ns-empty">目录里没有可添加的联系人</div>';
    const m = modal(`添加成员到「${escapeHtml(g.name || "群")}」`, `
      <div class="nsu-list" style="max-height:46vh;overflow:auto">${rows}</div>
      <div class="sec-row" style="margin-top:8px"><label>或公钥</label><input id="grp-add-pk" class="sec-input" placeholder="粘贴 64 位公钥 hex"></div>
      <div class="sec-actions"><button class="ns-btn" id="grp-add-bypk">按公钥添加</button></div>`);
    m.querySelectorAll(".grp-addone").forEach((b) => b.addEventListener("click", async () => { if (await act("group_add", { groupId: gid, target: b.dataset.id }, "已添加")) { b.textContent = "已添加"; b.disabled = true; } }));
    m.querySelector("#grp-add-bypk").addEventListener("click", async () => {
      const raw = m.querySelector("#grp-add-pk").value.trim();
      const pk = (window.NodeSvc && NodeSvc.pubkeyOf) ? NodeSvc.pubkeyOf(raw) : (/^[0-9a-fA-F]{64}$/.test(raw) ? raw.toLowerCase() : null);
      if (!pk) { if (window.toast) toast("请输入 64 位公钥 hex"); return; }
      if (await act("group_add", { groupId: gid, target: pk }, "已添加")) { closeModal(m); openDetail(gid); }
    });
  }

  // ── 轻量模态 ──
  function modal(title, bodyHtml) {
    closeModal(document.getElementById("grp-modal"));
    const m = document.createElement("div");
    m.className = "sec-overlay on"; m.id = "grp-modal";
    m.innerHTML = `<div class="sec-box"><div class="sec-head">${title}<button class="sec-x" id="grp-modal-x">${nmIcon("close")}</button></div><div class="sec-body"><div class="sec-sec">${bodyHtml}</div></div></div>`;
    document.body.appendChild(m);
    m.addEventListener("click", (e) => { if (e.target === m) closeModal(m); });
    m.querySelector("#grp-modal-x").addEventListener("click", () => closeModal(m));
    return m;
  }
  function closeModal(m) { if (m && m.parentNode) m.parentNode.removeChild(m); }
  function textPrompt(title, ph, val) {
    return new Promise((resolve) => {
      const m = modal(title, `<input id="grp-inp" class="sec-input" placeholder="${escapeHtml(ph)}" value="${escapeHtml(val || "")}"><div class="sec-actions"><button class="ns-btn" id="grp-inp-cancel">取消</button><button class="ns-btn ns-primary" id="grp-inp-ok">确定</button></div>`);
      const inp = m.querySelector("#grp-inp");
      inp.focus();
      const done = (v) => { closeModal(m); resolve(v); };
      m.querySelector("#grp-inp-ok").addEventListener("click", () => done(inp.value.trim()));
      m.querySelector("#grp-inp-cancel").addEventListener("click", () => done(null));
      inp.addEventListener("keydown", (e) => { if (e.key === "Enter") done(inp.value.trim()); if (e.key === "Escape") done(null); });
    });
  }

  (function init() {
    const nw = document.getElementById("grp-new-btn");
    if (nw) nw.addEventListener("click", createGroup);
    const rf = document.getElementById("grp-refresh-btn");
    if (rf) rf.addEventListener("click", renderGroupsList);
    const se = document.getElementById("grp-search");
    if (se) se.addEventListener("input", paintGroupsList);
  })();

  // ── 群聊右侧「群成员」面板：顶部群操作 + 逐成员操作按钮（渲染进给定容器）──
  function renderMembersPanel(gid, box) {
    if (!box) return;
    const g = byId(gid);
    if (!g) { box.innerHTML = '<div class="ns-empty">群不存在或已解散</div>'; return; }
    const role = myRole(g), isOwner = role === "owner", isAdmin = isOwner || role === "admin";
    const top = [];
    const tr = (key, fb) => (window.t ? t(key) : fb);
    if (isAdmin) top.push(`<button class="ns-btn grp-p-add" title="${tr("grp.add", "加人")}">${nmIcon("plus")} ${tr("grp.add", "加人")}</button>`);
    if (isAdmin) top.push(`<button class="ns-btn grp-p-edit" title="${tr("grp.info", "群信息")}">${tr("grp.info", "群信息")}</button>`);
    if (isOwner) top.push(`<button class="ns-btn grp-danger grp-p-dissolve" title="${tr("grp.dissolve", "解散")}">${tr("grp.dissolve", "解散")}</button>`);
    else top.push(`<button class="ns-btn grp-danger grp-p-leave" title="${tr("grp.leave", "退群")}">${tr("grp.leave", "退群")}</button>`);
    const rows = (g.members || []).map((id) => {
      const r = roleOf(g, id), acts = [];
      if (id !== MYID) acts.push(`<button class="ns-btn grp-p-msg" data-id="${id}" title="私聊">${nmIcon("chat")}</button>`);
      if (isOwner && id !== g.owner) acts.push(r === "admin"
        ? `<button class="ns-btn grp-p-demote" data-id="${id}" title="取消管理员">取消管理</button>`
        : `<button class="ns-btn grp-p-promote" data-id="${id}" title="设为管理员">设管理</button>`);
      if (isAdmin && id !== g.owner && id !== MYID && !(r === "admin" && !isOwner)) acts.push(`<button class="ns-btn grp-danger grp-p-kick" data-id="${id}" title="移出群">${nmIcon("close")}</button>`);
      return `<div class="grp-m">${window.Profile ? Profile.faceHtml(id, nameOf(id), 32) : `<span class="av" style="background:${avatarColor(id)}">${escapeHtml(nameOf(id).slice(0, 1))}</span>`}
        <span class="grp-m-meta"><b>${escapeHtml(nameOf(id))}</b><small class="grp-role grp-role--${r || "member"}">${roleLabel(r)}</small></span>
        <span class="grp-m-acts">${acts.join("")}</span></div>`;
    }).join("");
    box.innerHTML = `
      <div class="grp-members-head"><span>${tr("grp.members", "成员")} <b>${(g.members || []).length}</b></span><div class="grp-members-top">${top.join("")}</div></div>
      <div class="grp-members-list">${rows}</div>`;
    const redraw = () => renderMembersPanel(gid, box);
    const q = (sel, fn) => { const el = box.querySelector(sel); if (el) el.addEventListener("click", fn); };
    const each = (sel, fn) => box.querySelectorAll(sel).forEach((el) => el.addEventListener("click", () => fn(el.dataset.id)));
    q(".grp-p-add", () => openAddMember(gid));
    q(".grp-p-edit", () => editGroupMeta(gid));
    q(".grp-p-dissolve", async () => { if (confirm("解散该群？此操作不可恢复。")) { await act("group_dissolve", { groupId: gid }, "群已解散"); if (window.Tabs) Tabs.close("c:" + gid); renderGroupsList(); } });
    q(".grp-p-leave", async () => { if (confirm("退出该群？")) { await act("group_leave", { groupId: gid }, "已退出"); if (window.Tabs) Tabs.close("c:" + gid); renderGroupsList(); } });
    each(".grp-p-msg", (id) => { if (typeof selectContact === "function") selectContact(id); });
    each(".grp-p-kick", async (id) => { await act("group_kick", { groupId: gid, target: id }, "已移除"); redraw(); refreshHeaderCount(gid); });
    each(".grp-p-promote", async (id) => { await act("group_promote", { groupId: gid, target: id }, "已设为管理员"); redraw(); });
    each(".grp-p-demote", async (id) => { await act("group_demote", { groupId: gid, target: id }, "已取消管理员"); redraw(); });
    const pending = (g.members || []).map((id) => (typeof entityById === "function" ? entityById(id) : null)).filter((c) => c && window.Profile && Profile.isRef(c.avatar));
    if (pending.length && !box.dataset.avatars) {
      box.dataset.avatars = "1";
      Profile.resolveList(pending, () => { if (box.isConnected) renderMembersPanel(gid, box); });
    }
  }
  function refreshHeaderCount(gid) { const g = byId(gid); const el = document.getElementById("conv-grp-count"); if (el && g) el.textContent = window.t ? t("conv.people", { n: (g.members || []).length }) : ((g.members || []).length + " 人"); }

  window.Groups = { refresh, hydrate, byId, myRole, nameOf, roleLabel, list: () => GROUPS, renderMembersPanel, syncSelection };
  window.renderGroupsList = renderGroupsList;
  window.openGroupDetail = openDetail;
})();

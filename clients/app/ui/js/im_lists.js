function renderPanels() {
  renderConversations();
  renderEntities();
  renderAgents();
  renderModels();
  paintActUnread();
  if (window.Tray) Tray.sync();
}

function bindPanelUI() {
  const s1 = document.getElementById("im-search-input");
  if (s1 && !s1._bound) { s1._bound = true; s1.addEventListener("input", renderConversations); }
  const s2 = document.getElementById("entity-search-input");
  if (s2 && !s2._bound) { s2._bound = true; s2.addEventListener("input", renderEntities); }
  const s3 = document.getElementById("agent-search-input");
  if (s3 && !s3._bound) { s3._bound = true; s3.addEventListener("input", renderAgents); }
  const s4 = document.getElementById("llm-search");
  if (s4 && !s4._bound) { s4._bound = true; s4.addEventListener("input", renderModels); }
  const add = document.getElementById("ent-add-btn");
  if (add && !add._bound) { add._bound = true; add.addEventListener("click", toggleEntityForm); }
}

// ── 💬 消息：有会话记录的对端，按最近消息倒序 ──
function renderConversations() {
  const box = document.getElementById("im-list");
  if (!box) return;
  const q = (document.getElementById("im-search-input").value || "").trim().toLowerCase();
  const index = window.ChatLog ? (ChatLog.index().convos || {}) : CONVOS;
  const peers = Object.keys(index).map((id) => {
    const c = peerOf(id);
    const last = index[id] && index[id].last ? index[id].last : (Array.isArray(index[id]) ? index[id].slice(-1)[0] : null);
    return { c, last, ts: last ? last.ts : 0 };
  }).filter(({ c }) => !q || (c.name || "").toLowerCase().includes(q) || c.id.includes(q))
    .sort((a, b) => b.ts - a.ts);
  if (!peers.length) {
    box.innerHTML = `<div class="im-empty">${window.t ? t("list.noChat") : "还没有会话。去左侧「实体目录」选一个朋友开始聊。"}</div>`;
    return;
  }
  box.innerHTML = peers.map(({ c, last }) => {
    const shown = last ? (window.Composer ? Composer.preview(last) : last.body) : "";
    const preview = last ? escapeHtml((last.from === MY_ID ? "我: " : "") + shown) : "";
    return itemHtml(c, preview, UNREAD[c.id] || 0, ACTIVE, { time: listTime(last && last.ts), inlineHandle: false });
  }).join("");
  wireItems(box, selectContact);
}

// ── 📇 实体目录：仅朋友 + 群聊 ──
function renderEntities() {
  const box = document.getElementById("entity-list");
  if (!box) return;
  const q = (document.getElementById("entity-search-input").value || "").trim().toLowerCase();
  const match = (name, id, handle) =>
    !q ||
    (name || "").toLowerCase().includes(q) ||
    (id || "").includes(q) ||
    (handle || "").toLowerCase().includes(q);

  const friends = mergedEntities().filter(
    (c) => c.id !== MY_ID && kindType(c.kind) === "person" && match(c.name, c.id, c.handle)
  );
  const groupRows = ((window.Groups && Groups.list && Groups.list()) || [])
    .map((g) => ({
      id: g.id,
      name: g.name || "群",
      kind: "group",
      avatar: g.avatar || "",
      added: false,
    }))
    .filter((c) => match(c.name, c.id, ""));

  const sections = [
    renderGroup("person", "person", "朋友", friends),
    renderGroup("group", "groups", "群组", groupRows),
  ];
  box.innerHTML = sections.join("") || `<div class="im-empty">暂无朋友或群组</div>`;

  box.querySelectorAll('.ent-group[data-type="person"] .im-item').forEach((el) => {
    el.addEventListener("click", () => showEntityDetail(el.dataset.id));
  });
  box.querySelectorAll('.ent-group[data-type="group"] .im-item').forEach((el) => {
    el.addEventListener("click", () => {
      if (window.openGroupChat) openGroupChat(el.dataset.id);
      else if (window.Groups && Groups.open) Groups.open(el.dataset.id);
    });
  });
  box.querySelectorAll(".ent-head").forEach((head) =>
    head.addEventListener("click", () => {
      const g = head.closest(".ent-group");
      const key = g.dataset.type;
      if (ENT_FOLDED.has(key)) ENT_FOLDED.delete(key);
      else ENT_FOLDED.add(key);
      const folded = ENT_FOLDED.has(key);
      g.classList.toggle("folded", folded);
      head.setAttribute("aria-expanded", folded ? "false" : "true");
    })
  );
}

// ── 🤖 智能体：目录里的 agent.* 实体，点击直接开聊（复用联系人会话管线）──
function renderAgents() {
  const box = document.getElementById("agents-list");
  if (!box) return;
  const q = (document.getElementById("agent-search-input").value || "").trim().toLowerCase();
  const list = mergedEntities().filter((c) =>
    c.id !== MY_ID && kindType(c.kind) === "agent" &&
    (!q || (c.name || "").toLowerCase().includes(q) || (c.id || "").includes(q) || (c.handle || "").toLowerCase().includes(q)));
  if (!list.length) {
    const hint = window.t ? t("list.noAgent") : "未发现智能体。在节点上运行 nm-agentd --serve，刷新后即可在此对话。";
    box.innerHTML = `<div class="im-empty">${hint}</div>`;
    return;
  }
  box.innerHTML = list.map((c) => {
    const sub = c.model ? escapeHtml(c.model) : (window.t ? t("agent.sub") : "AI 助手");
    return itemHtml(c, sub, UNREAD[c.id] || 0, ACTIVE, { inlineHandle: false });
  }).join("");
  wireItems(box, selectContact);
}

// ── 大模型列表：由 llm_models.js 渲染（本地 / 远端分组）──
function renderModels() {
  if (window.renderLlmModelList) window.renderLlmModelList();
}
function renderGroup(key, icon, label, items) {
  const q = (document.getElementById("entity-search-input").value || "").trim();
  const folded = !q && (ENT_FOLDED.has(key) || !items.length);
  const person = key === "person";
  const group = key === "group";
  const rows = items.map((c) => {
    const tag = c.added ? " · 手动" : "";
    if (person) {
      const sub = c.handle
        ? `<span class="nm-handle">${escapeHtml(c.handle)}</span>${tag}`
        : (tag ? tag.slice(3) : "");
      return itemHtml(c, sub, 0, DETAIL_ID, { inlineHandle: false });
    }
    if (group) {
      return itemHtml(c, escapeHtml("群组") + tag, UNREAD[c.id] || 0, ACTIVE || DETAIL_ID, { inlineHandle: false });
    }
    return itemHtml(c, escapeHtml(typeMeta(c.kind).label) + tag, 0, DETAIL_ID);
  }).join("");
  return `<div class="ent-group${folded ? " folded" : ""}" data-type="${key}">
    <button type="button" class="ent-head" aria-expanded="${folded ? "false" : "true"}">
      <span class="ent-chev">${nmIcon("chevron")}</span>
      <span class="ent-ico">${nmIcon(icon)}</span>
      <span class="ent-label">${escapeHtml(label)}</span>
      <span class="ent-count">${items.length}</span>
    </button>
    <div class="ent-body">${rows}</div>
  </div>`;
}

function itemHtml(c, sub, unread, sel, opts) {
  opts = opts || {};
  const inlineHandle = opts.inlineHandle !== false && !opts.time;
  const dot = (window.Profile && c.presence) ? Profile.presenceDot(c.presence, 11) : "";
  const face = roomFace(c, 40, dot);
  const time = opts.time ? `<span class="tm">${escapeHtml(opts.time)}</span>` : "";
  const u = formatUnread(unread);
  const badge = u ? `<span class="unread">${u}</span>` : "";
  return `<div class="im-item ${c.id === sel ? "on" : ""}" data-id="${c.id}" title="${escapeHtml(c.id)}">
    ${face}
    <span class="mid">
      <span class="r1"><span class="nm">${escapeHtml(c.name || shortId(c.id))}</span>${time}${inlineHandle && c.handle ? `<span class="nm-handle nm-handle--inline">${escapeHtml(c.handle)}</span>` : ""}</span>
      <span class="r2"><span class="msg">${sub || ""}</span>${badge}</span>
    </span></div>`;
}
function roomFace(c, size, extra) {
  const s = size || 40;
  if (c.kind === "group" || c.kind === "channel") {
    const ico = c.kind === "group" ? "groups" : "channels";
    if (window.Profile && Profile.isImg(c.avatar)) {
      return `<span class="av av-img" style="width:${s}px;height:${s}px;flex:0 0 ${s}px;background:${avatarColor(c.id)}"><img src="${c.avatar}" alt="" style="width:100%;height:100%;object-fit:cover;display:block"></span>`;
    }
    return `<span class="av" style="width:${s}px;height:${s}px;flex:0 0 ${s}px;background:${avatarColor(c.id)}">${nmIcon(ico)}</span>`;
  }
  return window.Profile
    ? Profile.faceHtml(c.id, c.name || "?", s, extra || "", c.avatar)
    : `<span class="av" style="background:${avatarColor(c.id)}">${escapeHtml((c.name || "?").slice(0, 1))}</span>`;
}
function peerOf(id) {
  const g = window.Groups && Groups.byId && Groups.byId(id);
  if (g) return { id, name: g.name || "群", kind: "group", avatar: g.avatar || "" };
  const ch = window.Channels && Channels.byId && Channels.byId(id);
  if (ch) return { id, name: ch.name || "频道", kind: "channel", avatar: ch.avatar || "" };
  // 目录尚未刷到时：用已存消息的 group/channel 标志回退，避免被拉进群/刚订频道后看起来像私聊。
  const last = window.ChatLog && ChatLog.meta ? (ChatLog.meta(id).last || null) : null;
  if (last && last.group) return { id, name: "群", kind: "group", avatar: "" };
  if (last && last.channel) return { id, name: "频道", kind: "channel", avatar: "" };
  return entityById(id) || { id, name: "", kind: "" };
}
function wireItems(box, handler) {
  box.querySelectorAll(".im-item").forEach((el) => el.addEventListener("click", () => handler(el.dataset.id)));
}

// ── 实体详情（主区）：完整信息 + 复制公钥 + 发消息 ──
function showEntityDetail(id) {
  const c = entityById(id) || { id, name: "", kind: "" };
  const title = c.name || shortId(id);
  const ico = (typeof typeMeta === "function" ? (typeMeta(c.kind).icon || "contacts") : "contacts");
  if (window.Tabs) Tabs.open({ key: "e:" + id, kind: "entity", title, ico, render: () => renderEntityDetail(id) });
  else renderEntityDetail(id);
}
function renderEntityDetail(id) {
  DETAIL_ID = id; ACTIVE = null;
  renderPanels();
  const conv = document.getElementById("conv");
  if (!conv) return;
  const c = entityById(id) || { id, name: "", kind: "" };
  const tm = typeMeta(c.kind);
  const isCompute = kindType(c.kind) === "compute";
  const face = (window.Profile && Profile.displayAvatar(id, c.avatar))
    ? `<img src="${Profile.displayAvatar(id, c.avatar)}" alt="">`
    : escapeHtml((c.name || "?").slice(0, 1));
  const rows = [
    ["类型", escapeHtml(isCompute ? "远端模型" : tm.label)],
    c.handle ? ["账号", escapeHtml(c.handle)] : "",
    c.model ? ["模型", escapeHtml(c.model)] : "",
    c.provider ? ["厂商", escapeHtml(c.provider)] : "",
    c.metering ? ["计量", escapeHtml(c.metering)] : "",
    c.billing ? ["收费", escapeHtml(c.billing)] : "",
    c.price ? ["价格", escapeHtml(c.price)] : "",
    c.auth ? ["授权", escapeHtml(c.auth)] : "",
    c.statusText ? ["签名", escapeHtml(c.statusText)] : "",
    c.bio ? ["简介", escapeHtml(c.bio)] : "",
    (c.links && c.links.length) ? ["链接", c.links.map((l) => escapeHtml(l)).join(" · ")] : "",
    ["公钥", `<span class="mono">${escapeHtml(shortId(id))}</span><button type="button" class="ns-btn ns-ico" id="ed-copy" title="复制公钥">${nmIcon("copy")}</button>`],
    ["来源", c.added ? "手动添加" : (isCompute ? "P2P 目录发现" : "目录发现")],
  ].filter(Boolean);
  const proxyBox = isCompute ? `
      <div class="detail-rows" id="ed-proxy-box" style="margin-top:12px">
        <div class="detail-row"><span class="k" style="font-weight:600">模型代理</span><span class="v" id="ed-proxy-status">查询中…</span></div>
        <div class="detail-row" style="font-size:11px;color:var(--muted)"><span class="k"></span><span class="v">进程内 · 使用当前登录身份转发</span></div>
        <div class="detail-row"><span class="k">监听</span><span class="v"><input id="ed-proxy-listen" class="ns-inp" style="width:100%" placeholder="空=自动 18081+" value=""></span></div>
        <div class="detail-row"><span class="k">Token</span><span class="v"><input id="ed-proxy-token" class="ns-inp" style="width:100%" value="nm-local-token"></span></div>
        <div class="detail-row"><span class="k">Grant</span><span class="v"><input id="ed-proxy-grant" class="ns-inp" style="width:100%" placeholder="可选 base64 Grant"></span></div>
        <div class="detail-actions" style="padding:8px 0">
          <button type="button" class="ns-btn ns-ico ns-primary" id="ed-proxy-start" title="启动代理">${nmIcon("play")}</button>
          <button type="button" class="ns-btn ns-ico" id="ed-proxy-stop" style="display:none" title="停止代理">${nmIcon("stop")}</button>
          <button type="button" class="ns-btn ns-ico" id="ed-proxy-manage" style="display:none" title="管理与监控">${nmIcon("settings")}</button>
          <button type="button" class="ns-btn ns-ico" id="ed-proxy-copy" style="display:none" title="复制 BASE_URL">${nmIcon("copy")}</button>
        </div>
        <div class="detail-row" id="ed-proxy-hint" style="display:none"><span class="k">智能体</span><span class="v mono" id="ed-proxy-env" style="font-size:11px;word-break:break-all"></span></div>
      </div>` : "";
  conv.innerHTML = `
    <div class="conv-head"><b>${isCompute ? "远端模型" : "资料"}</b></div>
    <div class="detail-scroll"><div class="detail">
      <div class="detail-hero">
        <div class="ed-avatar" style="background:${avatarColor(id)}">${face}${(window.Profile && c.presence) ? Profile.presenceDot(c.presence, 14) : ""}</div>
        <div style="min-width:0;flex:1">
          <div class="detail-name">${escapeHtml(c.name || "(未命名)")}</div>
          <div class="detail-sub">${(window.Profile && c.presence) ? Profile.presenceLabel(c.presence) : "离线"}${c.statusText ? " · " + escapeHtml(c.statusText) : ""}</div>
        </div>
        <div class="detail-actions">
          ${isCompute ? "" : `<button type="button" class="ns-btn ns-ico ns-primary" id="ed-msg" title="发消息">${nmIcon("chat")}</button>`}
          ${c.added ? `<button type="button" class="ns-btn ns-ico ns-danger" id="ed-del" title="从目录移除">${nmIcon("trash")}</button>` : ""}
        </div>
      </div>
      <div class="detail-rows">${rows.map(([k, v]) => `<div class="detail-row"><span class="k">${k}</span><span class="v">${v}</span></div>`).join("")}</div>
      ${proxyBox}
    </div></div>`;
  document.getElementById("ed-copy").addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(id); if (window.toast) toast("公钥已复制"); }
    catch (_) { if (window.toast) toast("复制失败，请手动选中"); }
  });
  const msgBtn = document.getElementById("ed-msg");
  if (msgBtn) msgBtn.addEventListener("click", () => selectContact(id));
  const del = document.getElementById("ed-del");
  if (del) del.addEventListener("click", () => removeEntity(id));
  if (isCompute && window.bindModelProxyUi) window.bindModelProxyUi(id, c);
}

// ── 添加实体（类型可选 + 公钥粘贴/输入）──
function toggleEntityForm() {
  const f = document.getElementById("entity-form");
  if (!f) return;
  if (f.style.display !== "none" && f.innerHTML) { f.style.display = "none"; f.innerHTML = ""; return; }
  f.innerHTML = `
    <div class="ns-ftitle">添加朋友</div>
    <div class="ns-frow"><label>公钥（64 位 hex，可粘贴）</label><input class="ef-key" type="text" placeholder="粘贴或输入 64 位公钥 hex"></div>
    <div class="ns-frow"><label>名称（可选）</label><input class="ef-name" type="text" placeholder="备注名"></div>
    <div class="ns-factions"><button class="ns-btn ef-cancel">取消</button><button class="ns-btn ns-primary ef-save">添加</button></div>`;
  f.style.display = "block";
  const q = (s) => f.querySelector(s);
  q(".ef-cancel").addEventListener("click", () => { f.style.display = "none"; f.innerHTML = ""; });
  q(".ef-save").addEventListener("click", () => {
    const kind = "person";
    const raw = q(".ef-key").value.trim();
    const name = q(".ef-name").value.trim();
    const pk = (window.NodeSvc && NodeSvc.pubkeyOf) ? NodeSvc.pubkeyOf(raw) : (/^[0-9a-fA-F]{64}$/.test(raw) ? raw.toLowerCase() : null);
    if (!pk) { if (window.toast) toast("请输入 64 位公钥 hex（或含该公钥的 NM_NODE_ADDR）"); return; }
    addEntity(pk, kind, name);
    f.style.display = "none"; f.innerHTML = "";
  });
  q(".ef-key").focus();
}
async function addEntity(id, kind, name, handle, remark) {
  handle = handle || ""; remark = remark || "";
  // Optimistic local update first so UI feels instant.
  ADDED = ADDED.filter((e) => e.id !== id);
  ADDED.unshift({ id, kind, name: name || id.slice(0, 8), handle, remark });
  if (window.ListCache) ListCache.put({ added: ADDED });
  // Persist to home node (source of truth for multi-device sync).
  try {
    const entry = await NM.inv("roster_add", { id, kind, name: name || id.slice(0, 8), handle, remark });
    if (entry && entry.id) {
      // Replace optimistic entry with server-returned canonical entry.
      ADDED = ADDED.filter((e) => e.id !== id);
      ADDED.unshift(entry);
      if (window.ListCache) ListCache.put({ added: ADDED });
    }
  } catch (e) { if (window.toast) toast("联系人已添加（离线，下次同步）"); }
  saveAdded();
  renderPanels();
  if (window.toast) toast("已添加到实体目录");
  showEntityDetail(id);
}
async function removeEntity(id) {
  ADDED = ADDED.filter((e) => e.id !== id); // optimistic local remove
  try { await NM.inv("roster_remove", { id }); } catch (_) {}
  saveAdded();
  DETAIL_ID = null;
  renderPanels();
  renderConversation();
  if (window.toast) toast("已从实体目录移除");
}

function selectContact(id) {
  if (window.Groups && Groups.byId && Groups.byId(id)) { openGroupChat(id); return; }
  if (window.Channels && Channels.byId && Channels.byId(id)) { openChannel(id); return; }
  // 目录滞后时仍按消息标志打开群/频道视图，避免误进私聊。
  const hint = peerOf(id);
  if (hint.kind === "group" && window.openGroupChat) { openGroupChat(id); return; }
  if (hint.kind === "channel" && window.openChannel) { openChannel(id); return; }
  const c = entityById(id) || { id, name: "" };
  const title = c.name || shortId(id);
  const draw = () => { ACTIVE = id; DETAIL_ID = null; UNREAD[id] = 0; syncRead(id); scheduleUnread(); renderPanels(); renderConversation(); };
  if (window.Tabs) Tabs.open({ key: "c:" + id, kind: "chat", title, ico: "chat", render: draw });
  else draw();
}
window.selectContact = selectContact;

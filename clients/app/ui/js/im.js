// ── 即时通讯控制器：💬 消息（会话）+ 📇 实体目录（按类型分组 + 详情 + 手动添加）。
//    directory_query → 发现的实体；手动添加的实体存本地(按当前身份隔离)，合并展示。

let MY_ID = "";
let CONTACTS = [];              // 当前身份会话所见的发现实体 [{id,kind,name}]
let ADDED = [];                 // 当前身份手动添加的实体 [{id,kind,name}]
let CONVOS = {};                // 当前身份的会话：id -> [{id,from,body,ts}]
let ACTIVE = null;              // 当前会话对端 id（消息视图高亮）
let DETAIL_ID = null;          // 当前查看详情的实体 id（实体目录高亮）
let UNREAD = {};                // 当前身份的未读：id -> 数
const ENT_FOLDED = new Set();  // 实体目录已收起的类型
let _coreUnlisten = null;
let CONVOS_BY_USER = {};
let UNREAD_BY_USER = {};

const ENTITY_TYPES = [
  { key: "person",  label: "朋友",   icon: "person", prefixes: ["person"] },
  { key: "device",  label: "设备",   icon: "device", prefixes: ["device"] },
  { key: "vehicle", label: "车辆",   icon: "vehicle", prefixes: ["vehicle"] },
  { key: "agent",   label: "智能体", icon: "agent", prefixes: ["agent"] },
  { key: "robot",   label: "机器人", icon: "robot",  prefixes: ["robot"] },
  { key: "compute", label: "算力体", icon: "compute", prefixes: ["compute"] },
];
function kindType(kind) {
  const k = (kind || "").toLowerCase();
  for (const t of ENTITY_TYPES) if (t.prefixes.some((p) => k === p || k.startsWith(p + "."))) return t.key;
  return "other";
}
function typeMeta(kind) { return ENTITY_TYPES.find((t) => t.key === kindType(kind)) || { key: "other", label: "其他", icon: "file" }; }

// 发现实体 + 手动添加合并（去重；发现的以直播目录为准）。
function mergedEntities() {
  const map = {};
  ADDED.forEach((a) => { map[a.id] = { ...a, added: true }; });
  CONTACTS.forEach((c) => { map[c.id] = { ...map[c.id], ...c, added: !!(map[c.id] && map[c.id].added) }; });
  return Object.values(map);
}
function entityById(id) { return mergedEntities().find((c) => c.id === id); }
window.entityById = entityById;
// 供 groups.js 等解析显示名：本人→"我"；目录/本地有名→名；否则短 id。
window.entityName = function (id) { if (id === MY_ID) return "我"; const c = entityById(id); return (c && c.name) ? c.name : shortId(id); };
// 打开某群的群聊（作为一个 tab）。
window.openGroupChat = function (gid) {
  const g = window.Groups && Groups.byId(gid);
  const title = g ? (g.name || "群") : shortId(gid);
  const draw = () => { ACTIVE = gid; DETAIL_ID = null; UNREAD[gid] = 0; renderPanels(); renderConversation(); };
  if (window.Tabs) Tabs.open({ key: "c:" + gid, kind: "chat", title, ico: "groups", render: draw });
  else draw();
};
// 打开某频道的订阅流（作为一个 tab）。
window.openChannel = function (cid) {
  const c = window.Channels && Channels.byId(cid);
  const title = c ? (c.name || "频道") : shortId(cid);
  const draw = () => { ACTIVE = cid; DETAIL_ID = null; UNREAD[cid] = 0; renderPanels(); renderConversation(); };
  if (window.Tabs) Tabs.open({ key: "c:" + cid, kind: "chat", title, ico: "channels", render: draw });
  else draw();
};
// 供 channels.js 回填历史消息到会话缓存（不计未读；已存 id 去重）。
window.imBackfill = function (convId, msgs) {
  const arr = (CONVOS[convId] = CONVOS[convId] || []);
  for (const m of (msgs || [])) if (!arr.some((x) => x.id === m.id)) arr.push(m);
  arr.sort((a, b) => (a.ts || 0) - (b.ts || 0));
  if (convId === ACTIVE) renderConversation(); else renderPanels();
};

// ── 手动添加实体的本地存储（按当前身份隔离）──
function loadAdded() { try { return (JSON.parse(localStorage.getItem("nmspace-entities") || "{}")[MY_ID]) || []; } catch (_) { return []; } }
function saveAdded() {
  try {
    const all = JSON.parse(localStorage.getItem("nmspace-entities") || "{}");
    all[MY_ID] = ADDED;
    localStorage.setItem("nmspace-entities", JSON.stringify(all));
  } catch (_) {}
}

async function imStart(myId) {
  MY_ID = myId || "";
  CONVOS = CONVOS_BY_USER[MY_ID] = CONVOS_BY_USER[MY_ID] || {};
  UNREAD = UNREAD_BY_USER[MY_ID] = UNREAD_BY_USER[MY_ID] || {};
  ADDED = loadAdded();
  ACTIVE = null; DETAIL_ID = null; CONTACTS = [];
  if (window.Tabs) Tabs.closeAll(); // 每次连接（含切换身份）重置多 tab 工作区
  if (typeof updateUserChip === "function") updateUserChip();
  if (typeof window.renderNodeSvcList === "function") window.renderNodeSvcList(); // 连接后刷新节点服务面板（hydrate 后数据已就绪）
  if (typeof window.renderGroupsList === "function") window.renderGroupsList(); // P3：刷新群组列表
  if (typeof window.renderChannelsList === "function") window.renderChannelsList(); // P4：刷新频道列表
  if (window.Channels && Channels.primeChannels) Channels.primeChannels(); // P4：订阅已知频道 + 回填历史
  if (window.Profile) { Profile.publish(); Profile.applyPresence(); } // 连接后发布资料 + 应用在线状态(P2)
  if (window._nmPresenceTimer) clearInterval(window._nmPresenceTimer);
  window._nmPresenceTimer = setInterval(() => { if (MY_ID) imRefresh(); }, 15000); // P2：定期刷新在线状态
  if (!_coreUnlisten) _coreUnlisten = await NM.onCoreEvent(onCoreEvent);
  bindPanelUI();
  await imRefresh();
  renderConversation();
}

async function imRefresh() {
  try {
    CONTACTS = await NM.inv("directory_query", { kindPrefix: "" });
  } catch (e) {
    CONTACTS = [];
    if (window.toast) toast("目录刷新失败：" + (e && e.message ? e.message : e));
  }
  renderPanels();
  // P1：把 b3: 头像引用异步解析为可展示的 data:URI（拉取+缓存），完成后重渲染。
  if (window.Profile) Profile.resolveList(CONTACTS, () => renderPanels());
}

function renderPanels() { renderConversations(); renderEntities(); }

function bindPanelUI() {
  const s1 = document.getElementById("im-search-input");
  if (s1 && !s1._bound) { s1._bound = true; s1.addEventListener("input", renderConversations); }
  const s2 = document.getElementById("entity-search-input");
  if (s2 && !s2._bound) { s2._bound = true; s2.addEventListener("input", renderEntities); }
  const add = document.getElementById("ent-add-btn");
  if (add && !add._bound) { add._bound = true; add.addEventListener("click", toggleEntityForm); }
}

// ── 💬 消息：有会话记录的对端，按最近消息倒序 ──
function renderConversations() {
  const box = document.getElementById("im-list");
  if (!box) return;
  const q = (document.getElementById("im-search-input").value || "").trim().toLowerCase();
  const peers = Object.keys(CONVOS).map((id) => {
    const c = entityById(id) || ((window.Groups && Groups.byId(id)) ? { id, name: Groups.byId(id).name || "群", kind: "group" } : { id, name: "", kind: "" });
    const last = (CONVOS[id] || []).slice(-1)[0];
    return { c, last, ts: last ? last.ts : 0 };
  }).filter(({ c }) => !q || (c.name || "").toLowerCase().includes(q) || c.id.includes(q))
    .sort((a, b) => b.ts - a.ts);
  if (!peers.length) {
    box.innerHTML = '<div class="im-empty">还没有会话。去左侧「实体目录」选一个实体开始聊。</div>';
    return;
  }
  box.innerHTML = peers.map(({ c, last }) => {
    const preview = last ? escapeHtml((last.from === MY_ID ? "我: " : "") + last.body) : "";
    return itemHtml(c, preview, UNREAD[c.id] || 0, ACTIVE);
  }).join("");
  wireItems(box, selectContact);
}

// ── 📇 实体目录：合并实体按类型分组；点击看详情 ──
function renderEntities() {
  const box = document.getElementById("entity-list");
  if (!box) return;
  const q = (document.getElementById("entity-search-input").value || "").trim().toLowerCase();
  const list = mergedEntities().filter((c) => !q || (c.name || "").toLowerCase().includes(q) || (c.id || "").includes(q));
  const groups = {};
  list.forEach((c) => { (groups[kindType(c.kind)] = groups[kindType(c.kind)] || []).push(c); });
  const sections = ENTITY_TYPES.map((t) => renderGroup(t.key, t.icon, t.label, groups[t.key] || []));
  if ((groups.other || []).length) sections.push(renderGroup("other", "file", "其他", groups.other));
  box.innerHTML = sections.join("");
  wireItems(box, showEntityDetail);
  box.querySelectorAll(".ent-head").forEach((head) => head.addEventListener("click", () => {
    const g = head.closest(".ent-group");
    const key = g.dataset.type;
    if (ENT_FOLDED.has(key)) ENT_FOLDED.delete(key); else ENT_FOLDED.add(key);
    const folded = ENT_FOLDED.has(key);
    g.classList.toggle("folded", folded);
    head.setAttribute("aria-expanded", folded ? "false" : "true");
  }));
}
function renderGroup(key, icon, label, items) {
  const q = (document.getElementById("entity-search-input").value || "").trim();
  const folded = !q && ENT_FOLDED.has(key);
  const rows = items.length
    ? items.map((c) => itemHtml(c, escapeHtml(c.kind || "") + (c.added ? " · 手动" : ""), 0, DETAIL_ID)).join("")
    : '<div class="ent-empty">暂无</div>';
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

function itemHtml(c, sub, unread, sel) {
  const dot = (window.Profile && c.presence) ? Profile.presenceDot(c.presence, 11) : "";
  const face = window.Profile ? Profile.faceHtml(c.id, c.name || "?", 40, dot, c.avatar) : `<span class="av" style="background:${avatarColor(c.id)}">${escapeHtml((c.name || "?").slice(0, 1))}</span>`;
  return `<div class="im-item ${c.id === sel ? "on" : ""}" data-id="${c.id}" title="${escapeHtml(c.id)}">
    ${face}
    <span class="mid">
      <span class="r1"><span class="nm">${escapeHtml(c.name || shortId(c.id))}</span>${c.handle ? `<span class="nm-handle" style="font-size:11px;color:var(--aqua);margin-left:6px;font-family:ui-monospace,Menlo,monospace">${escapeHtml(c.handle)}</span>` : ""}</span>
      <span class="r2"><span class="msg">${sub || ""}</span>${unread ? `<span class="unread">${unread}</span>` : ""}</span>
    </span></div>`;
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
  conv.innerHTML = `
    <div class="conv-head">
      <b>实体详情</b><span class="sp"></span>
      <span class="conv-kind">${nmIcon(tm.icon)} ${escapeHtml(tm.label)}</span>
    </div>
    <div class="ent-detail">
      <div class="ed-avatar" style="background:${avatarColor(id)}">${(window.Profile && Profile.displayAvatar(id, c.avatar)) ? `<img src="${Profile.displayAvatar(id, c.avatar)}" alt="" style="width:100%;height:100%;object-fit:cover">` : escapeHtml((c.name || "?").slice(0, 1))}${(window.Profile && c.presence) ? Profile.presenceDot(c.presence, 12) : ""}</div>
      <div class="ed-name">${escapeHtml(c.name || "(未命名)")}</div>
      ${(window.Profile && c.presence) ? `<div style="margin:1px 0 4px;font-size:12px;color:${Profile.presenceColor(c.presence)}">● ${Profile.presenceLabel(c.presence)}</div>` : ""}
      <div class="ed-type">${nmIcon(tm.icon)} ${escapeHtml(tm.label)}${c.kind ? ` · <code>${escapeHtml(c.kind)}</code>` : ""}</div>
      ${c.statusText ? `<div style="margin:2px 0 6px;color:var(--muted)">${escapeHtml(c.statusText)}</div>` : ""}
      ${c.bio ? `<div style="margin:4px 0;color:var(--text)">${escapeHtml(c.bio)}</div>` : ""}
      ${(c.links && c.links.length) ? `<div style="margin:4px 0;font-size:12px">${c.links.map((l) => `<code>${escapeHtml(l)}</code>`).join(" · ")}</div>` : ""}
      <div class="ed-field">
        <label>公钥 (EntityId)</label>
        <div class="ed-key"><code id="ed-pubkey">${escapeHtml(id)}</code><button class="ns-btn ns-primary" id="ed-copy">复制</button></div>
      </div>
      <div class="ed-src">来源：${c.added ? "手动添加" : "目录发现"}</div>
      <div class="ed-actions">
        <button class="ns-btn ns-primary" id="ed-msg">发消息</button>
        ${c.added ? '<button class="ns-btn" id="ed-del">从目录移除</button>' : ""}
      </div>
    </div>`;
  document.getElementById("ed-copy").addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(id); if (window.toast) toast("公钥已复制"); }
    catch (_) { if (window.toast) toast("复制失败，请手动选中"); }
  });
  document.getElementById("ed-msg").addEventListener("click", () => selectContact(id));
  const del = document.getElementById("ed-del");
  if (del) del.addEventListener("click", () => removeEntity(id));
}

// ── 添加实体（类型可选 + 公钥粘贴/输入）──
function toggleEntityForm() {
  const f = document.getElementById("entity-form");
  if (!f) return;
  if (f.style.display !== "none" && f.innerHTML) { f.style.display = "none"; f.innerHTML = ""; return; }
  f.innerHTML = `
    <div class="ns-ftitle">添加实体</div>
    <div class="ns-frow"><label>类型</label><select class="ef-type">${ENTITY_TYPES.map((t) => `<option value="${t.key}">${t.label}</option>`).join("")}</select></div>
    <div class="ns-frow"><label>公钥（64 位 hex，可粘贴）</label><input class="ef-key" type="text" placeholder="粘贴或输入 64 位公钥 hex"></div>
    <div class="ns-frow"><label>名称（可选）</label><input class="ef-name" type="text" placeholder="备注名"></div>
    <div class="ns-factions"><button class="ns-btn ef-cancel">取消</button><button class="ns-btn ns-primary ef-save">添加</button></div>`;
  f.style.display = "block";
  const q = (s) => f.querySelector(s);
  q(".ef-cancel").addEventListener("click", () => { f.style.display = "none"; f.innerHTML = ""; });
  q(".ef-save").addEventListener("click", () => {
    const kind = q(".ef-type").value;
    const raw = q(".ef-key").value.trim();
    const name = q(".ef-name").value.trim();
    const pk = (window.NodeSvc && NodeSvc.pubkeyOf) ? NodeSvc.pubkeyOf(raw) : (/^[0-9a-fA-F]{64}$/.test(raw) ? raw.toLowerCase() : null);
    if (!pk) { if (window.toast) toast("请输入 64 位公钥 hex（或含该公钥的 NM_NODE_ADDR）"); return; }
    addEntity(pk, kind, name);
    f.style.display = "none"; f.innerHTML = "";
  });
  q(".ef-key").focus();
}
function addEntity(id, kind, name) {
  ADDED = ADDED.filter((e) => e.id !== id);
  ADDED.unshift({ id, kind, name });
  saveAdded();
  renderPanels();
  if (window.toast) toast("已添加到实体目录");
  showEntityDetail(id);
}
function removeEntity(id) {
  ADDED = ADDED.filter((e) => e.id !== id);
  saveAdded();
  DETAIL_ID = null;
  renderPanels();
  renderConversation();
  if (window.toast) toast("已从实体目录移除");
}

function selectContact(id) {
  const c = entityById(id) || { id, name: "" };
  const title = c.name || shortId(id);
  const draw = () => { ACTIVE = id; DETAIL_ID = null; UNREAD[id] = 0; renderPanels(); renderConversation(); };
  if (window.Tabs) Tabs.open({ key: "c:" + id, kind: "chat", title, ico: "chat", render: draw });
  else draw();
}

// ── 主区会话（头部 + 消息流 + 输入条）──
function renderConversation() {
  const conv = document.getElementById("conv");
  if (!conv) return;
  if (!ACTIVE) {
    // 实体详情等非会话标签自己占着主区，不要盖掉。没有任何标签时才是介绍页。
    if (window.Tabs && Tabs.active()) return;
    if (window.Tabs && Tabs.showEmpty) Tabs.showEmpty();
    return;
  }
  const msgs = CONVOS[ACTIVE] || [];
  const grp = window.Groups ? Groups.byId(ACTIVE) : null;
  const chn = (!grp && window.Channels) ? Channels.byId(ACTIVE) : null;
  const showSender = !!grp || !!chn;
  const c = grp ? { id: ACTIVE, name: grp.name || "群", kind: "group", avatar: grp.avatar }
    : chn ? { id: ACTIVE, name: chn.name || "频道", kind: "channel", avatar: chn.avatar }
    : (entityById(ACTIVE) || { id: ACTIVE, name: "", kind: "" });
  const hasImg = window.Profile && Profile.isImg(c.avatar);
  const headAv = hasImg
    ? `<span class="av av-sm" id="conv-hav" style="background:${avatarColor(ACTIVE)};padding:0;overflow:hidden"><img src="${c.avatar}" alt="" style="width:100%;height:100%;object-fit:cover;border-radius:inherit"></span>`
    : (grp || chn)
    ? `<span class="av av-sm" id="conv-hav" style="background:${avatarColor(ACTIVE)}">${nmIcon(grp ? "groups" : "channels")}</span>`
    : (window.Profile ? Profile.faceHtml(ACTIVE, c.name || "?", 26, "", c.avatar, "conv-hav") : `<span class="av av-sm" id="conv-hav" style="background:${avatarColor(ACTIVE)}">${escapeHtml((c.name || "?").slice(0, 1))}</span>`);
  const headMeta = grp
    ? `<span class="conv-kind">群 · <b id="conv-grp-count">${(grp.members || []).length}</b> 人</span>`
    : chn ? `<span class="conv-kind">${nmIcon("channels")} 频道${chn.topic ? " · " + escapeHtml(chn.topic) : ""}</span>`
    : (c.kind ? `<span class="conv-kind">${escapeHtml(c.kind)}</span>` : "");
  const logHtml = `<div class="log" id="conv-log">${msgs.map((m) => msgHtml(m, showSender)).join("") || '<div class="im-empty im-empty--center">暂无消息</div>'}</div>`;
  const ph = grp ? "群内发言，⏎ 发送…" : chn ? "发布到频道，⏎ 发送…" : "输入消息，⏎ 发送…";
  const composerHtml = `<div class="im-composer"><input id="conv-input" type="text" placeholder="${ph}" autocomplete="off" /><button id="conv-send" class="send" title="发送 ⏎">${nmIcon("send")}</button></div>`;
  // 群聊：左会话 + 右成员栏。私聊 / 频道：单栏（频道=订阅流 + 发布框）。
  const body = grp
    ? `<div class="grp-body"><div class="grp-chat">${logHtml}${composerHtml}</div><div class="grp-members" id="grp-members"></div></div>`
    : `${logHtml}${composerHtml}`;
  const chnOwner = chn && window.Channels && Channels.isOwner && Channels.isOwner(ACTIVE);
  conv.innerHTML = `
    <div class="conv-head">
      ${headAv}
      <b>${escapeHtml(c.name || shortId(ACTIVE))}</b>
      ${headMeta}
      <span class="sp"></span>
      ${chnOwner ? `<button class="conv-gear" id="conv-chn-edit" title="编辑频道信息">${nmIcon("settings")}</button>` : ""}
      <code class="conv-id" id="conv-id" title="点击复制完整 id：${escapeHtml(ACTIVE)}">${escapeHtml(shortId(ACTIVE))}</code>
    </div>
    ${body}`;
  const idEl = document.getElementById("conv-id");
  if (idEl) idEl.addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(ACTIVE); if (window.toast) toast((chn ? "频道" : grp ? "群" : "") + "id 已复制"); }
    catch (_) { if (window.toast) toast("复制失败，请手动选中"); }
  });
  const gearEl = document.getElementById("conv-chn-edit");
  if (gearEl) gearEl.addEventListener("click", () => Channels.editChannel(ACTIVE));
  // 群/频道头像若为 b3: 引用，异步解析后就地替换头部头像。
  const meta = grp || chn;
  if (meta && window.Profile && Profile.isRef(meta.avatar)) {
    Profile.resolveAvatar(meta.avatar, meta.homeNode).then((uri) => {
      if (!uri) return; meta.avatar = uri;
      const el = document.getElementById("conv-hav");
      if (el) { el.style.padding = "0"; el.style.overflow = "hidden"; el.innerHTML = `<img src="${uri}" alt="" style="width:100%;height:100%;object-fit:cover;border-radius:inherit">`; }
    });
  }
  const input = document.getElementById("conv-input");
  const doSend = () => sendMsg(input.value);
  document.getElementById("conv-send").addEventListener("click", doSend);
  input.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); doSend(); } });
  if (grp && window.Groups && Groups.renderMembersPanel) Groups.renderMembersPanel(ACTIVE, document.getElementById("grp-members"));
  input.focus();
  scrollLog();
}

function msgClock(ts) {
  const d = new Date(ts || 0);
  if (!ts || Number.isNaN(d.getTime())) return "";
  return String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
}
function msgHtml(m, showSender) {
  const mine = m.from === MY_ID;
  const who = mine ? "我" : (window.entityName ? entityName(m.from) : (m.from || "").slice(0, 6) + "…");
  const time = msgClock(m.ts);
  const sender = (showSender && !mine)
    ? `<span class="im-sender">${escapeHtml(who)}${time ? `<span class="im-time">${time}</span>` : ""}</span>`
    : "";
  const stamp = (!sender && time) ? `<span class="im-time">${time}</span>` : "";
  const face = window.Profile ? Profile.faceHtml(m.from, who, 28) : "";
  return `<div class="im-msg ${mine ? "me" : ""} with-av">
    ${face}
    <span class="im-stack">
      ${sender}
      <span class="im-bubble">${escapeHtml(m.body)}</span>
      ${stamp}
    </span>
  </div>`;
}

async function sendMsg(text) {
  text = (text || "").trim();
  if (!text || !ACTIVE) return;
  const input = document.getElementById("conv-input");
  const isGroup = !!(window.Groups && Groups.byId(ACTIVE));
  const isChannel = !isGroup && !!(window.Channels && Channels.byId(ACTIVE));
  try {
    if (isChannel) {
      await NM.inv("channel_publish", { channelId: ACTIVE, body: text }); // 节点会回投给本地订阅者(含自己)，不做乐观插入以免重复
    } else if (isGroup) {
      await NM.inv("send_group", { groupId: ACTIVE, text });
      pushMsg(ACTIVE, { id: "l" + Date.now(), from: MY_ID, body: text, ts: Date.now(), group: true, to: ACTIVE });
    } else {
      await NM.inv("send_to", { target: ACTIVE, text });
      pushMsg(ACTIVE, { id: "l" + Date.now(), from: MY_ID, body: text, ts: Date.now(), to: ACTIVE });
    }
    if (input) { input.value = ""; input.focus(); }
  } catch (e) {
    if (window.toast) toast("发送失败：" + (e && e.message ? e.message : e));
  }
}

function onCoreEvent(ev) {
  if (!ev || ev.type !== "message" || !ev.msg) return;
  const m = ev.msg;
  const key = (m.group || m.channel) ? m.to : m.from; // 群/频道按其 id 归会话；私聊按发送方
  pushMsg(key, m);
  if (key !== ACTIVE) UNREAD[key] = (UNREAD[key] || 0) + 1;
  renderPanels();
}

function pushMsg(peer, m) {
  (CONVOS[peer] = CONVOS[peer] || []).push(m);
  if (peer === ACTIVE) {
    const log = document.getElementById("conv-log");
    if (log) { if (log.querySelector(".im-empty")) log.innerHTML = ""; log.insertAdjacentHTML("beforeend", msgHtml(m, !!(m.group || m.channel))); scrollLog(); }
  }
  renderPanels();
}

function scrollLog() { const log = document.getElementById("conv-log"); if (log) log.scrollTop = log.scrollHeight; }

async function imDisconnect() {
  try { await NM.inv("disconnect"); } catch (_) {}
  MY_ID = ""; CONTACTS = []; ADDED = []; CONVOS = {}; UNREAD = {}; ACTIVE = null; DETAIL_ID = null;
  if (window.Tabs) Tabs.closeAll();
  if (typeof showLoginView === "function") showLoginView();
}
window.imStart = imStart;
window.imRefresh = imRefresh;
window.imDisconnect = imDisconnect;
// 供节点服务详情「添加用户到实体目录」调用：静默加入(不跳转主区)。
function addKnownEntity(id, kind, name) {
  if (!id) return false;
  ADDED = ADDED.filter((e) => e.id !== id);
  ADDED.unshift({ id, kind: kind || "", name: name || "" });
  saveAdded();
  renderPanels();
  return true;
}
window.addKnownEntity = addKnownEntity;

// ── 小工具 ──
function shortId(id) { id = id || ""; return id.length <= 12 ? id : id.slice(0, 8) + "…"; }
function avatarColor(id) {
  const palette = ["#3987e5", "#1fb182", "#9085e9", "#d55181", "#eb6834", "#22c55e", "#e5b95f"];
  let h = 0; for (let i = 0; i < (id || "").length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return palette[h % palette.length];
}

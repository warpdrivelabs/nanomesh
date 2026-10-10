// ── 即时通讯控制器：💬 消息（会话）+ 📇 实体目录（按类型分组 + 详情 + 手动添加）。
//    directory_query → 发现的实体；手动添加的实体存本地(按当前身份隔离)，合并展示。

let MY_ID = "";
let CONTACTS = [];              // 当前身份会话所见的发现实体 [{id,kind,name}]
let ADDED = [];                 // 当前身份手动添加的实体 [{id,kind,name}]
let PRESENCE = {};              // A：id -> 在线状态（含跨节点好友），presence_query 批量刷新
let CONVOS = {};                // 当前身份的会话：id -> [{id,from,body,ts}]
let ACTIVE = null;              // 当前会话对端 id（消息视图高亮）
let DETAIL_ID = null;          // 当前查看详情的实体 id（实体目录高亮）
let UNREAD = {};                // 当前身份的未读：id -> 数
const ENT_FOLDED = new Set();  // 实体目录已收起的类型
let _coreUnlisten = null;
let SHOW_MEMBERS = true;
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
  // A：用批量查到的 presence 盖到所有项（跨节点 ADDED 好友本来无 presence）；
  // 本地 directory 已带非空 presence 时以其为准（同节点权威）。
  Object.values(map).forEach((c) => { if (!c.presence && PRESENCE[c.id]) c.presence = PRESENCE[c.id]; });
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
  const draw = () => { ACTIVE = gid; DETAIL_ID = null; UNREAD[gid] = 0; scheduleUnread(); renderPanels(); renderConversation(); };
  if (window.Tabs) Tabs.open({ key: "c:" + gid, kind: "chat", title, ico: "groups", render: draw });
  else draw();
};
// 打开某频道的订阅流（作为一个 tab）。
window.openChannel = function (cid) {
  const c = window.Channels && Channels.byId(cid);
  const title = c ? (c.name || "频道") : shortId(cid);
  const draw = () => { ACTIVE = cid; DETAIL_ID = null; UNREAD[cid] = 0; scheduleUnread(); renderPanels(); renderConversation(); };
  if (window.Tabs) Tabs.open({ key: "c:" + cid, kind: "chat", title, ico: "channels", render: draw });
  else draw();
};
// 供 channels.js 回填历史消息到会话缓存（不计未读；已存 id 去重）。
window.imBackfill = function (convId, msgs) {
  const list = (msgs || []).map((raw) => (window.Composer ? Composer.absorb(raw) : raw));
  if (!list.length || !window.ChatLog) return;
  ChatLog.appendMany(convId, list).then((n) => {
    if (!n) return;
    if (convId === ACTIVE) openView(convId, !!(window.Groups && Groups.byId(convId)) || !!(window.Channels && Channels.byId(convId)));
    renderPanels();
  });
};

function saveAdded() {
  if (window.ListCache) ListCache.put({ added: ADDED });
}

function slimMsg(m) {
  const o = {
    id: m.id, from: m.from, to: m.to, body: m.body || "", ts: m.ts || 0,
    group: !!m.group, channel: !!m.channel, typeUrl: m.typeUrl || "",
    kind: m.kind || "text", text: m.text || "",
  };
  const media = m.media;
  if (media && typeof media === "object") {
    o.media = {
      v: media.v || 1, kind: media.kind || "", text: media.text || "",
      mime: media.mime || "", name: media.name || "", size: media.size || 0,
      w: media.w || 0, h: media.h || 0, dur: media.dur || 0,
      parts: media.parts || [], home: media.home || "", thumb: media.thumb || "",
      card: media.card || "",
    };
  }
    if (m.seq) o.seq = m.seq;
    return o;
}
window.slimMsg = slimMsg;
let unreadTimer = null;
function scheduleUnread() {
  clearTimeout(unreadTimer);
  unreadTimer = setTimeout(() => { if (window.ChatLog) ChatLog.setUnread(UNREAD); }, 200);
}
window.addEventListener("pagehide", () => { if (window.ChatLog) ChatLog.setUnread(UNREAD); });

async function imStart(myId) {
  MY_ID = myId || "";
  CONVOS = CONVOS_BY_USER[MY_ID] = {};
  UNREAD = UNREAD_BY_USER[MY_ID] = {};
  if (window.ChatLog) await ChatLog.open(MY_ID);
  if (MY_ID !== (myId || "")) return;
  UNREAD = Object.assign(UNREAD, window.ChatLog ? ChatLog.unread() : {});
  ACTIVE = null; DETAIL_ID = null;
  if (window.ListCache) await ListCache.bind(MY_ID);
  const snap = window.ListCache ? ListCache.snapshot() : {};
  ADDED = (snap.added || []).slice(); // local cache seed (fast start)
  // Roster: sync from home node (authoritative, multi-device). Merge: server wins on conflicts.
  try {
    const remote = await NM.inv("roster_list", {});
    if (Array.isArray(remote) && remote.length > 0) {
      // Build a map of local entries keyed by id for O(1) merge.
      const localMap = {};
      ADDED.forEach((e) => { if (e.id) localMap[e.id] = e; });
      remote.forEach((e) => { if (e.id) localMap[e.id] = e; }); // server wins
      ADDED = Object.values(localMap);
      if (window.ListCache) ListCache.put({ added: ADDED });
    }
  } catch (_) { /* home node unreachable: proceed with local cache */ }
  CONTACTS = snap.directory || [];
  lastDirSig = dirSig(CONTACTS);
  if (window.Groups && Groups.hydrate) Groups.hydrate(snap.groups || []);
  if (window.Channels && Channels.hydrate) Channels.hydrate(snap.channels || []);
  renderPanels();
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
  // 前端已就绪（监听器+MY_ID+本地库）→ 通知后端放水，补投离线消息不再因早于监听器而丢失。
  try { await NM.inv("im_ready"); } catch (_) {}
}

function dirSig(list) {
  return (list || []).map((c) => [c.id, c.name, c.presence, c.statusText, c.handle, c.kind].join("\u0001")).join("\n");
}
let lastDirSig = "";
async function imRefresh() {
  try {
    const next = await NM.inv("directory_query", { kindPrefix: "" });
    const sig = dirSig(next);
    if (sig === lastDirSig) return;
    lastDirSig = sig;
    CONTACTS = next || [];
    if (window.ListCache) ListCache.put({ directory: CONTACTS });
  } catch (e) {
    if (!CONTACTS.length && window.toast) toast("目录刷新失败：" + (e && e.message ? e.message : e));
  }
  renderPanels();
  if (window.Profile) Profile.resolveList(CONTACTS, () => renderPanels());
  refreshPresence(); // A：每次刷新都重查在线态（不受 dir-sig 早退影响）
}

// A：批量查所有列表项的在线态（含跨节点 ADDED 好友），写入 PRESENCE 并重绘。
async function refreshPresence() {
  const ids = Array.from(new Set(mergedEntities().map((c) => c.id).filter((id) => id && id !== MY_ID)));
  if (!ids.length) return;
  try {
    const m = await NM.inv("presence_query", { ids });
    if (m && typeof m === "object") { PRESENCE = m; renderPanels(); }
    // Sync handle for any ADDED contact whose directory entry now has a name.
    ADDED.forEach((e) => {
      if (!e.id) return;
      const c = CONTACTS.find((x) => x.id === e.id);
      const newHandle = c && c.handle ? c.handle : (c && c.name && c.name.includes("@") ? c.name : "");
      if (newHandle && newHandle !== (e.handle || "")) {
        e.handle = newHandle;
        NM.inv("roster_update", { id: e.id, handle: newHandle }).catch(() => {});
      }
    });
  } catch (_) { /* 节点不支持/暂不可达：保留旧值，不打扰 */ }
}
window.refreshPresence = refreshPresence;

function renderPanels() { renderConversations(); renderEntities(); renderAgents(); if (window.Tray) Tray.sync(); }

function bindPanelUI() {
  const s1 = document.getElementById("im-search-input");
  if (s1 && !s1._bound) { s1._bound = true; s1.addEventListener("input", renderConversations); }
  const s2 = document.getElementById("entity-search-input");
  if (s2 && !s2._bound) { s2._bound = true; s2.addEventListener("input", renderEntities); }
  const s3 = document.getElementById("agent-search-input");
  if (s3 && !s3._bound) { s3._bound = true; s3.addEventListener("input", renderAgents); }
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
    box.innerHTML = `<div class="im-empty">${window.t ? t("list.noChat") : "还没有会话。去左侧「实体目录」选一个实体开始聊。"}</div>`;
    return;
  }
  box.innerHTML = peers.map(({ c, last }) => {
    const shown = last ? (window.Composer ? Composer.preview(last) : last.body) : "";
    const preview = last ? escapeHtml((last.from === MY_ID ? "我: " : "") + shown) : "";
    return itemHtml(c, preview, UNREAD[c.id] || 0, ACTIVE, { time: listTime(last && last.ts), inlineHandle: false });
  }).join("");
  wireItems(box, selectContact);
}

// ── 📇 实体目录：合并实体按类型分组；点击看详情 ──
function renderEntities() {
  const box = document.getElementById("entity-list");
  if (!box) return;
  const q = (document.getElementById("entity-search-input").value || "").trim().toLowerCase();
  const list = mergedEntities().filter((c) => c.id !== MY_ID && (!q || (c.name || "").toLowerCase().includes(q) || (c.id || "").includes(q) || (c.handle || "").toLowerCase().includes(q)));
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
function renderGroup(key, icon, label, items) {
  const q = (document.getElementById("entity-search-input").value || "").trim();
  const folded = !q && (ENT_FOLDED.has(key) || !items.length);
  const person = key === "person";
  const rows = items.map((c) => {
    const tag = c.added ? " · 手动" : "";
    if (person) {
      const sub = c.handle
        ? `<span class="nm-handle">${escapeHtml(c.handle)}</span>${tag}`
        : (tag ? tag.slice(3) : "");
      return itemHtml(c, sub, 0, DETAIL_ID, { inlineHandle: false });
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
  return `<div class="im-item ${c.id === sel ? "on" : ""}" data-id="${c.id}" title="${escapeHtml(c.id)}">
    ${face}
    <span class="mid">
      <span class="r1"><span class="nm">${escapeHtml(c.name || shortId(c.id))}</span>${time}${inlineHandle && c.handle ? `<span class="nm-handle nm-handle--inline">${escapeHtml(c.handle)}</span>` : ""}</span>
      <span class="r2"><span class="msg">${sub || ""}</span>${unread ? `<span class="unread">${unread}</span>` : ""}</span>
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
  const face = (window.Profile && Profile.displayAvatar(id, c.avatar))
    ? `<img src="${Profile.displayAvatar(id, c.avatar)}" alt="">`
    : escapeHtml((c.name || "?").slice(0, 1));
  const rows = [
    ["类型", escapeHtml(tm.label)],
    c.handle ? ["账号", escapeHtml(c.handle)] : "",
    c.statusText ? ["签名", escapeHtml(c.statusText)] : "",
    c.bio ? ["简介", escapeHtml(c.bio)] : "",
    (c.links && c.links.length) ? ["链接", c.links.map((l) => escapeHtml(l)).join(" · ")] : "",
    ["公钥", `<span class="mono">${escapeHtml(shortId(id))}</span><button class="ns-btn" id="ed-copy">复制</button>`],
    ["来源", c.added ? "手动添加" : "目录发现"],
  ].filter(Boolean);
  conv.innerHTML = `
    <div class="conv-head"><b>资料</b></div>
    <div class="detail-scroll"><div class="detail">
      <div class="detail-hero">
        <div class="ed-avatar" style="background:${avatarColor(id)}">${face}${(window.Profile && c.presence) ? Profile.presenceDot(c.presence, 14) : ""}</div>
        <div>
          <div class="detail-name">${escapeHtml(c.name || "(未命名)")}</div>
          <div class="detail-sub">${(window.Profile && c.presence) ? Profile.presenceLabel(c.presence) : "离线"}${c.statusText ? " · " + escapeHtml(c.statusText) : ""}</div>
        </div>
      </div>
      <div class="detail-actions">
        <button class="ns-btn ns-primary" id="ed-msg">发消息</button>
        ${c.added ? '<button class="ns-btn ns-danger" id="ed-del">从目录移除</button>' : ""}
      </div>
      <div class="detail-rows">${rows.map(([k, v]) => `<div class="detail-row"><span class="k">${k}</span><span class="v">${v}</span></div>`).join("")}</div>
    </div></div>`;
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
  const c = entityById(id) || { id, name: "" };
  const title = c.name || shortId(id);
  const draw = () => { ACTIVE = id; DETAIL_ID = null; UNREAD[id] = 0; scheduleUnread(); renderPanels(); renderConversation(); };
  if (window.Tabs) Tabs.open({ key: "c:" + id, kind: "chat", title, ico: "chat", render: draw });
  else draw();
}
window.selectContact = selectContact;

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
  const people = window.t ? t("conv.people", { n: (grp && grp.members || []).length }) : ((grp && grp.members || []).length + " 人");
  const headMeta = grp
    ? `<span class="conv-status" id="conv-grp-count">${people}</span>`
    : chn ? `<span class="conv-status">${escapeHtml(chn.topic || (window.t ? t("conv.channel") : "频道"))}</span>`
    : `<span class="conv-status">${(window.Profile && c.presence) ? Profile.presenceLabel(c.presence) : ""}${c.statusText ? (c.presence ? " · " : "") + escapeHtml(c.statusText) : ""}</span>`;
  const logHtml = `<div class="log" id="conv-log"></div>`;
  if (window.Composer) Composer.prepare(ACTIVE, !!(grp || chn));
  const composerHtml = window.Composer ? Composer.markup() : "";
  // 群聊：左会话 + 右成员栏。私聊 / 频道：单栏（频道=订阅流 + 发布框）。
  const body = grp
    ? `<div class="grp-body${SHOW_MEMBERS ? "" : " members-off"}"><div class="grp-chat">${logHtml}${composerHtml}</div><div class="grp-members" id="grp-members"></div></div>`
    : `${logHtml}${composerHtml}`;
  const chnOwner = chn && window.Channels && Channels.isOwner && Channels.isOwner(ACTIVE);
  conv.innerHTML = `
    <div class="conv-head">
      ${headAv}
      <span class="conv-who"><b>${escapeHtml(c.name || shortId(ACTIVE))}</b>${headMeta}</span>
      <span class="sp"></span>
      <button class="conv-gear" id="conv-search" title="${window.t ? t("conv.search") : "搜索此会话"}">${nmIcon("search")}</button>
      ${grp ? `<button class="conv-gear${SHOW_MEMBERS ? " is-on" : ""}" id="conv-members" title="${window.t ? t("conv.members") : "成员"}">${nmIcon("groups")}</button>` : ""}
      ${!grp && !chn ? `<button class="conv-gear" id="conv-profile" title="${window.t ? t("conv.profile") : "查看资料"}">${nmIcon("contacts")}</button>` : ""}
      ${chnOwner ? `<button class="conv-gear" id="conv-chn-edit" title="${window.t ? t("conv.editChannel") : "编辑频道信息"}">${nmIcon("settings")}</button>` : ""}
      <button class="conv-gear" id="conv-copy" title="${window.t ? t("conv.copyId") : "复制 id"}">${nmIcon("copy")}</button>
    </div>
    ${body}`;
  const copyEl = document.getElementById("conv-copy");
  if (copyEl) copyEl.addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(ACTIVE); if (window.toast) toast((chn ? "频道" : grp ? "群" : "") + " id 已复制"); }
    catch (_) { if (window.toast) toast("复制失败"); }
  });
  const searchEl = document.getElementById("conv-search");
  if (searchEl) searchEl.addEventListener("click", () => { if (window.Composer && Composer.toggleHistory) Composer.toggleHistory(); });
  const profileEl = document.getElementById("conv-profile");
  if (profileEl) profileEl.addEventListener("click", () => showEntityDetail(ACTIVE));
  const memEl = document.getElementById("conv-members");
  if (memEl) memEl.addEventListener("click", () => {
    SHOW_MEMBERS = !SHOW_MEMBERS;
    const host = conv.querySelector(".grp-body");
    if (host) host.classList.toggle("members-off", !SHOW_MEMBERS);
    memEl.classList.toggle("is-on", SHOW_MEMBERS);
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
  if (window.Composer) {
    Composer.attach({
      sendText: (text) => sendMsg(text),
      sendRich: (media) => sendRich(media),
      mentions: mentionTargets,
      cards: cardTargets,
      messages: () => VIEW.rows.slice(),
      search: (q) => ChatLog.search(ACTIVE, q, 80),
      reveal: (id) => revealMsg(id),
    });
  }
  openView(ACTIVE, showSender);
  if (grp && window.Groups && Groups.renderMembersPanel) Groups.renderMembersPanel(ACTIVE, document.getElementById("grp-members"));
}

function mentionTargets() {
  const grp = window.Groups && Groups.byId(ACTIVE);
  if (grp) return (grp.members || []).filter((id) => id !== MY_ID).map((id) => ({ id, name: entityName(id) }));
  return mergedEntities().filter((c) => c.id !== MY_ID).slice(0, 40).map((c) => ({ id: c.id, name: c.name || shortId(c.id) }));
}
function cardTargets() {
  const meName = (window.Identity && Identity.nameOf && Identity.nameOf(MY_ID)) || "我";
  const rest = mergedEntities().filter((c) => c.id && c.id !== MY_ID).slice(0, 40).map((c) => ({ id: c.id, name: c.name || shortId(c.id) }));
  return [{ id: MY_ID, name: meName }, ...rest];
}

function msgClock(ts) {
  const d = new Date(ts || 0);
  if (!ts || Number.isNaN(d.getTime())) return "";
  return String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
}
function dayLabel(ts) {
  const d = new Date(ts || 0);
  if (!ts || Number.isNaN(d.getTime())) return "";
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return window.t ? t("day.today") : "今天";
  const y = new Date(now); y.setDate(now.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return window.t ? t("day.yesterday") : "昨天";
  return d.getFullYear() + "年" + (d.getMonth() + 1) + "月" + d.getDate() + "日";
}
function listTime(ts) {
  if (!ts) return "";
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return "";
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return msgClock(ts);
  const y = new Date(now); y.setDate(now.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return window.t ? t("day.yesterday") : "昨天";
  if (d.getFullYear() === now.getFullYear()) return (d.getMonth() + 1) + "/" + d.getDate();
  return d.getFullYear() + "/" + (d.getMonth() + 1) + "/" + d.getDate();
}
function sameDay(a, b) { return new Date(a || 0).toDateString() === new Date(b || 0).toDateString(); }
function continues(prev, m) {
  return !!(prev && prev.from === m.from && sameDay(prev.ts, m.ts) && (m.ts || 0) - (prev.ts || 0) < 5 * 60 * 1000);
}
function renderLog(msgs, showSender) {
  let html = "", prev = null;
  for (const m of msgs) {
    if (!prev || !sameDay(prev.ts, m.ts)) html += `<div class="im-day"><span>${dayLabel(m.ts)}</span></div>`;
    html += msgHtml(m, showSender, continues(prev, m));
    prev = m;
  }
  return html;
}
function msgHtml(m, showSender, cont) {
  const mine = m.from === MY_ID;
  const named = mine
    ? ((window.Identity && Identity.label && Identity.label(MY_ID)) || "我")
    : (window.entityName ? entityName(m.from) : (m.from || "").slice(0, 6) + "…");
  const who = mine ? (window.t ? t("conv.me") : "我") : named;
  const time = msgClock(m.ts);
  const sender = (showSender && !cont)
    ? `<span class="im-sender">${escapeHtml(who)}${time ? `<span class="im-time">${time}</span>` : ""}</span>`
    : "";
  const stamp = (!sender && time) ? `<span class="im-time">${time}</span>` : "";
  const face = window.Profile ? Profile.faceHtml(m.from, named, 32) : "";
  const rich = window.Composer ? Composer.bubble(m) : { cls: "", html: escapeHtml(m.body || "") };
  return `<div class="im-msg ${mine ? "me" : ""} with-av${cont ? " cont" : ""}" data-mid="${escapeHtml(m.id || "")}">
    ${face}
    <span class="im-stack">
      ${sender}
      <span class="im-bubble ${rich.cls}">${rich.html}</span>
      ${stamp}
    </span>
  </div>`;
}

let VIEW = { id: "", start: 0, total: 0, rows: [], sender: false };
const ROW_H = new Map();
const EST = 88;
let viewKey = "";
let viewPainting = false;
let viewOlder = false;

function rowHeight(m) { return ROW_H.get(m.id) || EST; }
function viewPrefixes() {
  const pref = new Array(VIEW.rows.length + 1);
  pref[0] = VIEW.start * EST;
  for (let i = 0; i < VIEW.rows.length; i++) pref[i + 1] = pref[i] + rowHeight(VIEW.rows[i]);
  return pref;
}
function bindLog() {
  const log = document.getElementById("conv-log");
  if (!log || log._vbound) return;
  log._vbound = "1";
  let lastTop = log.scrollTop;
  log.addEventListener("scroll", () => {
    if (viewPainting) return;
    const top = log.scrollTop;
    const movedUp = top + 8 < lastTop;
    lastTop = top;
    const canScroll = log.scrollHeight > log.clientHeight + 32;
    if (movedUp && canScroll && top < 64 && VIEW.start > 0) loadOlder();
  });
}
async function openView(id, sender) {
  const total = window.ChatLog ? (ChatLog.meta(id).count || 0) : 0;
  VIEW = { id, start: 0, total, rows: [], sender: !!sender };
  viewKey = "";
  const rows = window.ChatLog ? await ChatLog.tail(id, 60) : [];
  if (VIEW.id !== id) return;
  VIEW.rows = rows;
  VIEW.total = Math.max(total, rows.length);
  VIEW.start = Math.max(0, VIEW.total - rows.length);
  paintVirtual(true);
  bindLog();
}
async function loadOlder() {
  if (viewOlder || VIEW.start <= 0 || !window.ChatLog) return;
  viewOlder = true;
  const id = VIEW.id;
  const page = await ChatLog.before(id, VIEW.start, 40);
  viewOlder = false;
  if (VIEW.id !== id || !page.length) return;
  const log = document.getElementById("conv-log");
  const keep = log ? log.scrollHeight - log.scrollTop : 0;
  VIEW.rows = page.concat(VIEW.rows);
  VIEW.start -= page.length;
  if (VIEW.rows.length > 240) {
    const extra = VIEW.rows.length - 240;
    VIEW.rows.splice(VIEW.rows.length - extra, extra);
  }
  viewPainting = true;
  paintVirtual(false);
  if (log) log.scrollTop = Math.max(0, log.scrollHeight - keep);
  viewPainting = false;
}
async function revealMsg(id) {
  if (!window.ChatLog || !ACTIVE) return;
  const pack = await ChatLog.around(ACTIVE, id, 80);
  if (!pack.msgs || !pack.msgs.length) return;
  VIEW.rows = pack.msgs;
  VIEW.start = pack.start || 0;
  VIEW.total = Math.max(VIEW.total, VIEW.start + VIEW.rows.length);
  paintVirtual(false);
  const el = document.querySelector(`.im-msg[data-mid="${CSS.escape(id)}"]`);
  if (el) el.scrollIntoView({ block: "center" });
}
function paintVirtual(stick) {
  const log = document.getElementById("conv-log");
  if (!log || VIEW.id !== ACTIVE) return;
  const keepBottom = stick || log.scrollHeight - log.scrollTop - log.clientHeight < 80;
  if (!VIEW.rows.length) {
    const ico = VIEW.sender ? "groups" : "chat";
    const empty = window.t ? t("conv.empty") : "暂无消息";
    const sub = window.t ? t("conv.emptySub") : "在下方输入，按 Enter 发送";
    log.innerHTML = `<div class="im-empty im-empty--center"><div class="ico">${nmIcon(ico)}</div><div class="txt">${empty}</div><div class="sub">${sub}</div></div>`;
    return;
  }
  const painting = viewPainting;
  viewPainting = true;
  log.innerHTML = `<div class="log-flow">${renderLog(VIEW.rows, VIEW.sender)}</div>`;
  if (window.Composer) Composer.hydrate(log);
  if (keepBottom) log.scrollTop = log.scrollHeight;
  viewPainting = painting;
}
async function sendMsg(text) {
  text = (text || "").trim();
  if (!text || !ACTIVE) return;
  const isGroup = !!(window.Groups && Groups.byId(ACTIVE));
  const isChannel = !isGroup && !!(window.Channels && Channels.byId(ACTIVE));
  if (isChannel) {
    await NM.inv("channel_publish", { channelId: ACTIVE, body: text }); // 节点会回投给本地订阅者(含自己)，不做乐观插入以免重复
  } else if (isGroup) {
    await NM.inv("send_group", { groupId: ACTIVE, text });
    pushMsg(ACTIVE, { id: "l" + Date.now(), from: MY_ID, body: text, ts: Date.now(), group: true, to: ACTIVE });
  } else {
    await NM.inv("send_to", { target: ACTIVE, text });
    pushMsg(ACTIVE, { id: "l" + Date.now(), from: MY_ID, body: text, ts: Date.now(), to: ACTIVE });
  }
}

async function sendRich(media) {
  if (!ACTIVE || !window.Composer) return;
  const isGroup = !!(window.Groups && Groups.byId(ACTIVE));
  const isChannel = !isGroup && !!(window.Channels && Channels.byId(ACTIVE));
  if (isChannel) {
    await NM.inv("channel_publish", { channelId: ACTIVE, body: Composer.channelBody(media) });
  } else {
    await NM.inv("send_rich", { target: ACTIVE, body: Composer.encode(media), group: isGroup });
    pushMsg(ACTIVE, {
      id: "l" + Date.now(), from: MY_ID, to: ACTIVE, ts: Date.now(), group: isGroup,
      typeUrl: "nmspace.v1/chat", body: Composer.encode(media),
    });
  }
}

// ── 托盘 / 通知用到的会话入口 ──
window.imSnapshot = () => {
  const convos = {};
  const index = window.ChatLog ? (ChatLog.index().convos || {}) : {};
  for (const id of Object.keys(index)) if (index[id] && index[id].last) convos[id] = [index[id].last];
  return { myId: MY_ID, unread: UNREAD, convos, active: ACTIVE };
};
window.imOpen = function (id) {
  if (!id || !MY_ID) return;
  if (window.Groups && Groups.byId(id)) openGroupChat(id);
  else if (window.Channels && Channels.byId(id)) openChannel(id);
  else selectContact(id);
};
window.imMarkRead = function (id) {
  if (!UNREAD[id]) return;
  UNREAD[id] = 0;
  scheduleUnread();
  renderPanels();
};
window.imMarkAllRead = function () {
  for (const id of Object.keys(UNREAD)) UNREAD[id] = 0;
  scheduleUnread();
  renderPanels();
};
window.imFlush = () => { if (window.ChatLog) ChatLog.setUnread(UNREAD); };

// ── 🤖 agent 流式回复（打字机）：delta 帧按 streamId 增量长出**同一个**气泡 ──
const AGENT_DELTA_TYPE = "nmspace.agent.delta.v1";
const STREAMS = {}; // streamId -> { peer, text, maxSeq }

function growStreamBubble(sid, peer, text) {
  if (VIEW.id !== peer) return;                       // 该会话未在前台
  let row = VIEW.rows.find((r) => r.id === sid);
  if (!row) {
    const raw = { id: sid, from: peer, to: MY_ID, body: text, ts: Date.now(), typeUrl: "" };
    row = window.Composer ? Composer.absorb(raw) : raw;
    VIEW.rows.push(row);
    if (VIEW.rows.length > 400) { VIEW.rows.shift(); VIEW.start += 1; }
    const log = document.getElementById("conv-log");
    const stick = !log || log.scrollHeight - log.scrollTop - log.clientHeight < 120;
    paintVirtual(stick);
    return;
  }
  row.body = text; row.text = text;
  const sel = (window.CSS && CSS.escape) ? CSS.escape(sid) : sid;
  const el = document.querySelector(`.im-msg[data-mid="${sel}"] .im-bubble`);
  if (el) {
    el.textContent = text;                            // O(1) 原地更新，自动转义
    const log = document.getElementById("conv-log");
    if (log && log.scrollHeight - log.scrollTop - log.clientHeight < 160) log.scrollTop = log.scrollHeight;
  } else {
    paintVirtual(true);                               // 气泡在窗外/已被重绘 → 整窗重绘兜底
  }
}

function handleAgentDelta(m) {
  let d;
  try { d = JSON.parse(m.body || "{}"); } catch (_) { return; }
  if (!d || typeof d.streamId !== "string") return;
  const peer = m.from;
  const sid = d.streamId;
  const seq = typeof d.seq === "number" ? d.seq : 0;
  const st = STREAMS[sid] || (STREAMS[sid] = { peer, text: "", maxSeq: -1 });
  if (seq < st.maxSeq) return;                         // 乱序旧帧丢弃（累计全文，大 seq 为准）
  st.maxSeq = seq;
  if (typeof d.text === "string") st.text = d.text;
  if (peer === ACTIVE) growStreamBubble(sid, peer, st.text);
  if (!d.done) return;
  delete STREAMS[sid];
  const full = { id: sid, from: peer, to: MY_ID, body: st.text, ts: Date.now(), typeUrl: "" };
  if (peer === ACTIVE) {
    // 气泡已在视图里；落库一次（ChatLog 按 id 去重）+ 刷新会话列表预览。
    if (window.ChatLog) ChatLog.append(peer, window.Composer ? Composer.absorb(full) : full);
    renderPanels();
  } else {
    // 会话未打开：按普通消息入库 + 列表 + 未读 + 托盘。
    pushMsg(peer, full).then((fresh) => {
      if (!fresh) return;
      UNREAD[peer] = (UNREAD[peer] || 0) + 1;
      scheduleUnread();
      renderPanels();
      if (window.Tray) Tray.onMessage(peer, full);
    });
  }
}

function onCoreEvent(ev) {
  if (!ev || ev.type !== "message" || !ev.msg) return;
  const m = ev.msg;
  if (m.typeUrl === AGENT_DELTA_TYPE) { handleAgentDelta(m); return; }
  const mine = m.from === MY_ID;
  const key = (m.group || m.channel || mine) ? m.to : m.from;
  pushMsg(key, m).then((fresh) => {
    const seen = key === ACTIVE && (!window.Tray || Tray.focused());
    if (fresh && !mine && !seen) {
      UNREAD[key] = (UNREAD[key] || 0) + 1;
      scheduleUnread();
    }
    renderPanels();
    if (fresh && !mine && !seen && window.Tray) Tray.onMessage(key, m);
  });
}

async function pushMsg(peer, m) {
  m = window.Composer ? Composer.absorb(m) : m;
  if (!window.ChatLog) return false;
  const stored = await ChatLog.append(peer, m);
  if (!stored) return false;
  if (peer === ACTIVE) {
    const coversTail = VIEW.start + VIEW.rows.length >= VIEW.total;
    VIEW.total = (ChatLog.meta(peer).count || VIEW.total + 1);
    if (coversTail) {
      VIEW.rows.push(m);
      if (VIEW.rows.length > 400) { VIEW.rows.shift(); VIEW.start += 1; }
      const log = document.getElementById("conv-log");
      const stick = !log || log.scrollHeight - log.scrollTop - log.clientHeight < 120;
      paintVirtual(stick);
    }
  }
  renderPanels();
  return true;
}

async function imDisconnect() {
  if (window.ChatLog) ChatLog.setUnread(UNREAD);
  try { await NM.inv("disconnect"); } catch (_) {}
  MY_ID = ""; CONTACTS = []; ADDED = []; PRESENCE = {}; CONVOS = {}; UNREAD = {}; ACTIVE = null; DETAIL_ID = null;
  window.CURRENT_SVC = null;
  if (typeof paintHomeNode === "function") paintHomeNode();
  if (window.Tabs) Tabs.closeAll();
  if (window.Tray) Tray.sync();
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

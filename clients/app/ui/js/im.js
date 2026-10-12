// ── 即时通讯控制器：💬 消息（会话）+ 📇 实体目录（按类型分组 + 详情 + 手动添加）。
//    directory_query → 发现的实体；手动添加的实体存本地(按当前身份隔离)，合并展示。

let MY_ID = "";
let CONTACTS = [];              // 当前身份会话所见的发现实体 [{id,kind,name}]
let ADDED = [];                 // 当前身份手动添加的实体 [{id,kind,name}]
let PRESENCE = {};              // A：id -> 在线状态（含跨节点好友），presence_query 批量刷新
let CONVOS = {};                // 当前身份的会话：id -> [{id,from,body,ts}]
let ACTIVE = null;              // 当前打开的聊天对端 id（与主区聊天框一致；关闭 tab 后清空）
let DETAIL_ID = null;          // 当前查看详情的实体 id（实体目录高亮）
let UNREAD = {};                // 当前身份的未读：id -> 数
/** 主区当前正在显示的聊天对端：以 tab 为准，不用列表选中残留。 */
function viewingPeer() {
  if (window.Tabs) {
    const k = Tabs.active();
    if (typeof k === "string" && k.startsWith("c:")) return k.slice(2);
    return null;
  }
  return ACTIVE;
}
function isChatOpen(id) {
  return !!id && viewingPeer() === id;
}
const ENT_FOLDED = new Set();  // 实体目录已收起的类型
let _coreUnlisten = null;
let SHOW_MEMBERS = true;
let CONVOS_BY_USER = {};
let UNREAD_BY_USER = {};


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
    if (isChatOpen(convId)) openView(convId, !!(window.Groups && Groups.byId(convId)) || !!(window.Channels && Channels.byId(convId)));
    renderPanels();
  });
};

/** tab 切换/关闭后：把 ACTIVE 对齐到当前聊天框（关闭后不再当作「正在阅读」）。 */
window.imOnTabChange = function () {
  const peer = viewingPeer();
  if (ACTIVE === peer) return;
  ACTIVE = peer;
  renderPanels();
};

function saveAdded() {
  if (window.ListCache) ListCache.put({ added: ADDED });
}

let unreadTimer = null;
/** 未读数展示：0 不显示；>99 显示「99+」表示太多。 */
function unreadTotal() {
  let n = 0;
  for (const id of Object.keys(UNREAD || {})) n += Number(UNREAD[id]) || 0;
  return n;
}
/** 左侧活动栏「消息」图标上的汇总未读气泡。 */
function paintActUnread() {
  const el = document.getElementById("act-im");
  if (!el) return;
  let badge = el.querySelector(".badge");
  const label = formatUnread(unreadTotal());
  if (!label) {
    if (badge) badge.remove();
    return;
  }
  if (!badge) {
    badge = document.createElement("span");
    badge.className = "badge";
    badge.setAttribute("aria-hidden", "true");
    el.appendChild(badge);
  }
  badge.textContent = label;
  badge.classList.toggle("badge-lg", label.length > 2);
}
function scheduleUnread() {
  paintActUnread();
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

async function imDisconnect() {
  if (window.ChatLog) ChatLog.setUnread(UNREAD);
  try { await NM.inv("disconnect"); } catch (_) {}
  MY_ID = ""; CONTACTS = []; ADDED = []; PRESENCE = {}; CONVOS = {}; UNREAD = {}; ACTIVE = null; DETAIL_ID = null;
  window.CURRENT_SVC = null;
  paintActUnread();
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

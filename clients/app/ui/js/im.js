// ── 即时通讯控制器：💬 消息（会话）+ 📇 实体目录（按类型分组 + 详情 + 手动添加）。
//    directory_query → 发现的实体；手动添加的实体存本地(按当前身份隔离)，合并展示。

let MY_ID = "";
let CONTACTS = [];              // 当前身份会话所见的发现实体 [{id,kind,name}]
let ADDED = [];                 // 当前身份手动添加的实体 [{id,kind,name}]
let CONVOS = {};                // 当前身份的会话：id -> [{id,from,body,ts}]
let ACTIVE = null;              // 当前会话对端 id（消息视图高亮）
let DETAIL_ID = null;          // 当前查看详情的实体 id（实体目录高亮）
let UNREAD = {};                // 当前身份的未读：id -> 数
let _coreUnlisten = null;
let CONVOS_BY_USER = {};
let UNREAD_BY_USER = {};

const ENTITY_TYPES = [
  { key: "person",  label: "人类",   icon: "👤", prefixes: ["person"] },
  { key: "device",  label: "设备",   icon: "📟", prefixes: ["device"] },
  { key: "vehicle", label: "车辆",   icon: "🚗", prefixes: ["vehicle"] },
  { key: "agent",   label: "智能体", icon: "🤖", prefixes: ["agent"] },
  { key: "compute", label: "算力体", icon: "⚡", prefixes: ["compute"] },
];
function kindType(kind) {
  const k = (kind || "").toLowerCase();
  for (const t of ENTITY_TYPES) if (t.prefixes.some((p) => k === p || k.startsWith(p + "."))) return t.key;
  return "other";
}
function typeMeta(kind) { return ENTITY_TYPES.find((t) => t.key === kindType(kind)) || { key: "other", label: "其他", icon: "📦" }; }

// 发现实体 + 手动添加合并（去重；发现的以直播目录为准）。
function mergedEntities() {
  const map = {};
  ADDED.forEach((a) => { map[a.id] = { ...a, added: true }; });
  CONTACTS.forEach((c) => { map[c.id] = { ...map[c.id], ...c, added: !!(map[c.id] && map[c.id].added) }; });
  return Object.values(map);
}
function entityById(id) { return mergedEntities().find((c) => c.id === id); }

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
  if (typeof updateUserChip === "function") updateUserChip();
  if (typeof window.renderNodeSvcList === "function") window.renderNodeSvcList(); // 连接后刷新节点服务面板（hydrate 后数据已就绪）
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
    const c = entityById(id) || { id, name: "", kind: "" };
    const last = (CONVOS[id] || []).slice(-1)[0];
    return { c, last, ts: last ? last.ts : 0 };
  }).filter(({ c }) => !q || (c.name || "").toLowerCase().includes(q) || c.id.includes(q))
    .sort((a, b) => b.ts - a.ts);
  if (!peers.length) {
    box.innerHTML = '<div class="im-empty">还没有会话。去左侧「📇 实体目录」选一个实体开始聊。</div>';
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
  const sections = ENTITY_TYPES.map((t) => renderGroup(t.icon, t.label, groups[t.key] || []));
  if ((groups.other || []).length) sections.push(renderGroup("📦", "其他", groups.other));
  box.innerHTML = sections.join("");
  wireItems(box, showEntityDetail);
}
function renderGroup(icon, label, items) {
  const rows = items.length
    ? items.map((c) => itemHtml(c, escapeHtml(c.kind || "") + (c.added ? " · 手动" : ""), 0, DETAIL_ID)).join("")
    : '<div class="ent-empty">暂无</div>';
  return `<div class="ent-group">
    <div class="ent-head"><span class="ent-ico">${icon}</span><span class="ent-label">${label}</span><span class="ent-count">${items.length}</span></div>
    ${rows}
  </div>`;
}

function itemHtml(c, sub, unread, sel) {
  return `<div class="im-item ${c.id === sel ? "on" : ""}" data-id="${c.id}" title="${escapeHtml(c.id)}">
    <span class="av" style="background:${avatarColor(c.id)}">${escapeHtml((c.name || "?").slice(0, 1))}</span>
    <span class="mid">
      <span class="r1"><span class="nm">${escapeHtml(c.name || shortId(c.id))}</span></span>
      <span class="r2"><span class="msg">${sub || ""}</span>${unread ? `<span class="unread">${unread}</span>` : ""}</span>
    </span></div>`;
}
function wireItems(box, handler) {
  box.querySelectorAll(".im-item").forEach((el) => el.addEventListener("click", () => handler(el.dataset.id)));
}

// ── 实体详情（主区）：完整信息 + 复制公钥 + 发消息 ──
function showEntityDetail(id) {
  DETAIL_ID = id; ACTIVE = null;
  renderPanels();
  const conv = document.getElementById("conv");
  if (!conv) return;
  const c = entityById(id) || { id, name: "", kind: "" };
  const tm = typeMeta(c.kind);
  conv.innerHTML = `
    <div class="conv-head">
      <b>实体详情</b><span class="sp"></span>
      <span class="conv-kind">${tm.icon} ${escapeHtml(tm.label)}</span>
    </div>
    <div class="ent-detail">
      <div class="ed-avatar" style="background:${avatarColor(id)}">${escapeHtml((c.name || "?").slice(0, 1))}</div>
      <div class="ed-name">${escapeHtml(c.name || "(未命名)")}</div>
      <div class="ed-type">${tm.icon} ${escapeHtml(tm.label)}${c.kind ? ` · <code>${escapeHtml(c.kind)}</code>` : ""}</div>
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
    <div class="ns-frow"><label>类型</label><select class="ef-type">${ENTITY_TYPES.map((t) => `<option value="${t.key}">${t.icon} ${t.label}</option>`).join("")}</select></div>
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
  ACTIVE = id; DETAIL_ID = null;
  UNREAD[id] = 0;
  renderPanels();
  renderConversation();
}

// ── 主区会话（头部 + 消息流 + 输入条）──
function renderConversation() {
  const conv = document.getElementById("conv");
  if (!conv) return;
  const c = entityById(ACTIVE) || (ACTIVE ? { id: ACTIVE, name: "", kind: "" } : null);
  if (!ACTIVE) {
    conv.innerHTML = `<div class="im-center">
      <div class="ico">💬</div>
      <div class="txt">选择一个实体开始会话</div>
      <div class="sub">在「📇 实体目录」里点实体看详情，再「发消息」；或在「消息」里继续会话</div>
    </div>`;
    return;
  }
  const msgs = CONVOS[ACTIVE] || [];
  conv.innerHTML = `
    <div class="conv-head">
      <span class="av av-sm" style="background:${avatarColor(ACTIVE)}">${escapeHtml((c.name || "?").slice(0, 1))}</span>
      <b>${escapeHtml(c.name || shortId(ACTIVE))}</b>
      ${c.kind ? `<span class="conv-kind">${escapeHtml(c.kind)}</span>` : ""}
      <span class="sp"></span>
      <code class="conv-id" title="${escapeHtml(ACTIVE)}">${escapeHtml(shortId(ACTIVE))}</code>
    </div>
    <div class="log" id="conv-log">${msgs.map(msgHtml).join("") || '<div class="im-empty im-empty--center">暂无消息</div>'}</div>
    <div class="im-composer">
      <input id="conv-input" type="text" placeholder="输入消息，⏎ 发送…" autocomplete="off" />
      <button id="conv-send" class="send" title="发送 ⏎">↑</button>
    </div>`;
  const input = document.getElementById("conv-input");
  const doSend = () => sendMsg(input.value);
  document.getElementById("conv-send").addEventListener("click", doSend);
  input.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); doSend(); } });
  input.focus();
  scrollLog();
}

function msgHtml(m) {
  const mine = m.from === MY_ID;
  return `<div class="im-msg ${mine ? "me" : ""}">
    <span class="im-bubble">${escapeHtml(m.body)}</span>
    <span class="im-from">${mine ? "我" : escapeHtml((m.from || "").slice(0, 6)) + "…"}</span>
  </div>`;
}

async function sendMsg(text) {
  text = (text || "").trim();
  if (!text || !ACTIVE) return;
  const input = document.getElementById("conv-input");
  try {
    await NM.inv("send_to", { target: ACTIVE, text });
    pushMsg(ACTIVE, { id: "l" + Date.now(), from: MY_ID, body: text, ts: Date.now() });
    if (input) { input.value = ""; input.focus(); }
  } catch (e) {
    if (window.toast) toast("发送失败：" + (e && e.message ? e.message : e));
  }
}

function onCoreEvent(ev) {
  if (!ev || ev.type !== "message" || !ev.msg) return;
  const m = ev.msg;
  pushMsg(m.from, m);
  if (m.from !== ACTIVE) UNREAD[m.from] = (UNREAD[m.from] || 0) + 1;
  renderPanels();
}

function pushMsg(peer, m) {
  (CONVOS[peer] = CONVOS[peer] || []).push(m);
  if (peer === ACTIVE) {
    const log = document.getElementById("conv-log");
    if (log) { if (log.querySelector(".im-empty")) log.innerHTML = ""; log.insertAdjacentHTML("beforeend", msgHtml(m)); scrollLog(); }
  }
  renderPanels();
}

function scrollLog() { const log = document.getElementById("conv-log"); if (log) log.scrollTop = log.scrollHeight; }

async function imDisconnect() {
  try { await NM.inv("disconnect"); } catch (_) {}
  MY_ID = ""; CONTACTS = []; ADDED = []; CONVOS = {}; UNREAD = {}; ACTIVE = null; DETAIL_ID = null;
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

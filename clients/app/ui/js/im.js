// ── 即时通讯控制器：💬 消息（会话）+ 📇 实体目录（按类型分组）。数据全走 nmspace Tauri 命令。
//    directory_query → 全体实体；send_to → 发送；core://event → 收消息。

let MY_ID = "";
let CONTACTS = [];              // 全体实体 [{id,kind,name}]
let CONVOS = {};                // id -> [{id,from,body,ts}]
let ACTIVE = null;              // 当前会话对端 id
let UNREAD = {};                // id -> 未读数
let _coreUnlisten = null;

// 实体类型分组（用户指定；kind 前缀匹配 nm-entity 的 KIND：person / device.* / agent.* / compute.*；
// vehicle 后端暂未定义，先占位，定义后自动归入）。
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
function entityById(id) { return CONTACTS.find((c) => c.id === id); }

async function imStart(myId) {
  MY_ID = myId || "";
  CONVOS = {}; UNREAD = {}; ACTIVE = null;
  const me = document.getElementById("tb-me");
  if (me) me.textContent = "我 · " + (MY_ID.slice(0, 10) || "?") + "…";
  if (!_coreUnlisten) _coreUnlisten = await NM.onCoreEvent(onCoreEvent);
  bindSearches();
  await imRefresh();
  renderConversation(); // 主区空态
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

function bindSearches() {
  const s1 = document.getElementById("im-search-input");
  if (s1 && !s1._bound) { s1._bound = true; s1.addEventListener("input", renderConversations); }
  const s2 = document.getElementById("entity-search-input");
  if (s2 && !s2._bound) { s2._bound = true; s2.addEventListener("input", renderEntities); }
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
    const un = UNREAD[c.id] || 0;
    return itemHtml(c, preview, un);
  }).join("");
  wireItems(box);
}

// ── 📇 实体目录：全体实体按类型分组 ──
function renderEntities() {
  const box = document.getElementById("entity-list");
  if (!box) return;
  const q = (document.getElementById("entity-search-input").value || "").trim().toLowerCase();
  const list = CONTACTS.filter((c) => !q || (c.name || "").toLowerCase().includes(q) || (c.id || "").includes(q));
  const groups = {};
  list.forEach((c) => { (groups[kindType(c.kind)] = groups[kindType(c.kind)] || []).push(c); });
  const sections = ENTITY_TYPES.map((t) => renderGroup(t.icon, t.label, groups[t.key] || []));
  if ((groups.other || []).length) sections.push(renderGroup("📦", "其他", groups.other));
  box.innerHTML = sections.join("");
  wireItems(box);
}
function renderGroup(icon, label, items) {
  const rows = items.length
    ? items.map((c) => itemHtml(c, escapeHtml(c.kind || ""), UNREAD[c.id] || 0)).join("")
    : '<div class="ent-empty">暂无</div>';
  return `<div class="ent-group">
    <div class="ent-head"><span class="ent-ico">${icon}</span><span class="ent-label">${label}</span><span class="ent-count">${items.length}</span></div>
    ${rows}
  </div>`;
}

// ── 共用：一行实体/会话项（.im-item）──
function itemHtml(c, sub, unread) {
  return `<div class="im-item ${c.id === ACTIVE ? "on" : ""}" data-id="${c.id}" title="${escapeHtml(c.id)}">
    <span class="av" style="background:${avatarColor(c.id)}">${escapeHtml((c.name || "?").slice(0, 1))}</span>
    <span class="mid">
      <span class="r1"><span class="nm">${escapeHtml(c.name || shortId(c.id))}</span></span>
      <span class="r2"><span class="msg">${sub || ""}</span>${unread ? `<span class="unread">${unread}</span>` : ""}</span>
    </span></div>`;
}
function wireItems(box) {
  box.querySelectorAll(".im-item").forEach((el) => el.addEventListener("click", () => selectContact(el.dataset.id)));
}

function selectContact(id) {
  ACTIVE = id;
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
      <div class="sub">在「📇 实体目录」里挑一个人类/设备/智能体/算力体，或先「刷新目录」</div>
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
  MY_ID = ""; CONTACTS = []; CONVOS = {}; UNREAD = {}; ACTIVE = null;
  if (typeof showLoginView === "function") showLoginView();
}
window.imStart = imStart;
window.imRefresh = imRefresh;
window.imDisconnect = imDisconnect;

// ── 小工具 ──
function shortId(id) { id = id || ""; return id.length <= 12 ? id : id.slice(0, 8) + "…"; }
function avatarColor(id) {
  const palette = ["#3987e5", "#1fb182", "#9085e9", "#d55181", "#eb6834", "#22c55e", "#e5b95f"];
  let h = 0; for (let i = 0; i < (id || "").length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return palette[h % palette.length];
}

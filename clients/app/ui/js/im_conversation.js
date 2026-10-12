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
  } catch (e) {
    // 发送失败（含重连后仍失败）：不乐观回显，避免幽灵本地消息；提示用户。
    if (window.toast) toast("发送失败：" + (e && e.message ? e.message : e));
  }
}

async function sendRich(media) {
  if (!ACTIVE || !window.Composer) return;
  const isGroup = !!(window.Groups && Groups.byId(ACTIVE));
  const isChannel = !isGroup && !!(window.Channels && Channels.byId(ACTIVE));
  try {
    if (isChannel) {
      await NM.inv("channel_publish", { channelId: ACTIVE, body: Composer.channelBody(media) });
    } else {
      await NM.inv("send_rich", { target: ACTIVE, body: Composer.encode(media), group: isGroup });
      pushMsg(ACTIVE, {
        id: "l" + Date.now(), from: MY_ID, to: ACTIVE, ts: Date.now(), group: isGroup,
        typeUrl: "nmspace.v1/chat", body: Composer.encode(media),
      });
    }
  } catch (e) {
    if (window.toast) toast("发送失败：" + (e && e.message ? e.message : e));
  }
}

// ── 🤖 agent 流式回复（打字机）：delta 帧按 streamId 增量长出**同一个**气泡 ──
const AGENT_DELTA_TYPE = "nmspace.agent.delta.v1";
// wire streamId → { peer, text, maxSeq, localId }
// localId：落库/气泡 id；若 wire streamId 撞上历史消息则改用新 id，避免改旧气泡时间与位置。
const STREAMS = {};

function growStreamBubble(localId, peer, text, ts) {
  if (VIEW.id !== peer) return;                       // 该会话未在前台
  let row = VIEW.rows.find((r) => r.id === localId);
  if (!row) {
    const raw = { id: localId, from: peer, to: MY_ID, body: text, ts: ts || Date.now(), typeUrl: "" };
    row = window.Composer ? Composer.absorb(raw) : raw;
    VIEW.rows.push(row);
    if (VIEW.rows.length > 400) { VIEW.rows.shift(); VIEW.start += 1; }
    const log = document.getElementById("conv-log");
    const stick = !log || log.scrollHeight - log.scrollTop - log.clientHeight < 120;
    paintVirtual(stick);
    return;
  }
  row.body = text; row.text = text;
  const sel = (window.CSS && CSS.escape) ? CSS.escape(localId) : localId;
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
  // 权威时间取 wire timestamp_ms（首帧）；勿在 done 时用 Date.now() 覆盖，否则时钟/会话排序漂移。
  const wireTs = Number(m.ts) || 0;
  let st = STREAMS[sid];
  if (!st) {
    // 首帧：wire id 已存在于当前视图 → 视为撞历史，换本地唯一 id。
    let localId = sid;
    if (isChatOpen(peer) && VIEW.rows.some((r) => r.id === sid)) {
      localId = sid + "~" + Date.now();
    }
    st = STREAMS[sid] = { peer, text: "", maxSeq: -1, localId, ts: wireTs || Date.now() };
  } else if (wireTs && !st.ts) {
    st.ts = wireTs;
  }
  if (seq < st.maxSeq) return;                         // 乱序旧帧丢弃（累计全文，大 seq 为准）
  st.maxSeq = seq;
  if (typeof d.text === "string") st.text = d.text;
  if (isChatOpen(peer)) growStreamBubble(st.localId, peer, st.text, st.ts);
  if (!d.done) return;
  const localId = st.localId;
  const ts = st.ts || wireTs || Date.now();
  delete STREAMS[sid];
  const full = { id: localId, from: peer, to: MY_ID, body: st.text, ts, typeUrl: "" };
  if (isChatOpen(peer)) {
    // 气泡已在视图里；同步气泡 ts 后落库 + 刷新会话列表预览。
    const row = VIEW.rows.find((r) => r.id === localId);
    if (row) row.ts = ts;
    const packed = window.Composer ? Composer.absorb(full) : full;
    if (window.ChatLog) {
      ChatLog.append(peer, packed).then((ok) => {
        if (ok) { renderPanels(); return; }
        const retry = Object.assign({}, packed, { id: localId + "~" + Date.now() });
        if (row) row.id = retry.id;
        ChatLog.append(peer, retry).then(() => renderPanels());
      });
    } else {
      renderPanels();
    }
  } else {
    // 会话未打开：按普通消息入库 + 列表 + 未读 + 托盘。
    pushMsg(peer, full).then((fresh) => {
      if (!fresh) {
        // INSERT OR IGNORE 撞历史 id：换 id 再试，避免静默丢回复。
        const retry = Object.assign({}, full, { id: localId + "~" + Date.now() });
        return pushMsg(peer, retry).then((ok2) => {
          if (!ok2) return;
          UNREAD[peer] = (UNREAD[peer] || 0) + 1;
          scheduleUnread();
          renderPanels();
          if (window.Tray) Tray.onMessage(peer, retry);
        });
      }
      UNREAD[peer] = (UNREAD[peer] || 0) + 1;
      scheduleUnread();
      renderPanels();
      if (window.Tray) Tray.onMessage(peer, full);
    });
  }
}

// 送达回执：对方已收到本人消息 → 把对应本人气泡升为双勾✓✓(status=2)。
// ev: {ref:原gram_id, from:对方账号}。即使会话未打开也落库，重开时显示双勾。
function handleReceipt(ev) {
  const conv = ev.from, g = ev.ref;
  if (!conv || !g) return;
  if (typeof VIEW !== "undefined" && VIEW.id === conv) {
    const row = VIEW.rows.find((r) => r.from === MY_ID && r.gramId === g);
    if (row && (row.status || 0) < 2) { row.status = 2; if (window.paintVirtual) paintVirtual(false); }
  }
  if (window.ChatLog && ChatLog.mark) ChatLog.mark(conv, { gramId: g }, 2);
}

function onCoreEvent(ev) {
  if (!ev) return;
  // 重连生命周期：后端发送失败→自动重连时 emit 这些，驱动状态行进度提示。
  if (ev.type === "reconnecting") {
    if (window.setReconnecting) setReconnecting(true);
    if (window.toast) toast("连接已断，正在重连…");
    return;
  }
  if (ev.type === "reconnected") {
    if (window.setReconnecting) setReconnecting(false);
    if (window.paintHomeNode) paintHomeNode();
    if (window.toast) toast("已重连");
    return;
  }
  if (ev.type === "reconnect_failed") {
    if (window.setReconnecting) setReconnecting(false);
    if (window.paintHomeNode) paintHomeNode();
    if (window.toast) toast("重连失败，请检查网络");
    return;
  }
  if (ev.type === "receipt") { handleReceipt(ev); return; }
  if (ev.type === "read") { if (window.applyReadMarker) applyReadMarker(ev.conversation, ev.readUpToMs); return; } // P1-6 本账号其它设备已读同步
  if (ev.type !== "message" || !ev.msg) return;
  const m = ev.msg;
  if (m.typeUrl === AGENT_DELTA_TYPE) { handleAgentDelta(m); return; }
  const mine = m.from === MY_ID;
  const key = (m.group || m.channel || mine) ? m.to : m.from;
  pushMsg(key, m).then((fresh) => {
    // 「已读」只看主区是否正在显示该聊天框，不看消息列表残留选中。
    const seen = isChatOpen(key) && (!window.Tray || Tray.focused());
    if (seen) {
      // 正在看该会话 → 视为已读：推进已读位并（去抖）同步本账号其它设备。
      if (window.syncRead) syncRead(key);
    } else if (fresh && !mine && m.ts > (READ_UPTO[key] || 0)) {
      // 未在看、他人新消息、且比已读位新（排除别设备已读过的）→ 计未读。
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
  if (isChatOpen(peer)) {
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

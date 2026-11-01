// 委托助手（模型代答）：每个会话（私聊/群聊）独立选一个在线模型做"代答模型"。
// 私聊：收到好友消息自动转模型、回复以本人身份 send_to 发回。
// 群聊：仅当消息 @ 当前用户（文本匹配显示名）才代答，回复 send_group 发回群；@所有人不答。
// 标记仅本侧可见（byAgent 徽标）；对方收到干净文本。纯前端编排，app 开着才生效。
// 配置按 MY_ID 分键存 localStorage：{ chats: { <convId>: { model:{entityId,kind,name} } } }。
// 接缝：im_events.js（onCoreEvent 触发 onInbound；handleAgentDelta 前置拦 onFrame）；
//      im_conversation.js（标题栏助手按钮 → openDelegatePicker）。
(function () {
  // ── 调参（防死循环）──
  const RATE_WINDOW_MS = 60_000;   // 限速窗
  const RATE_MAX = 6;              // 每窗每会话最多代答次数
  const MIN_GAP_MS = 8_000;        // 同会话两次代答最小间隔
  const CONSEC_MAX = 8;            // 连续代答上限（疑似两 AI ping-pong）→ 暂停该会话
  const CONSEC_COOLDOWN_MS = 5 * 60_000; // 触顶后冷却
  const HIST_N = 20;               // 带入上下文的历史条数
  const CTX_BUDGET = 6000;         // 上下文字符预算

  // ── 运行态（内存，app 关即清；均按 convId=会话 id 维度）──
  const inflight = new Map();   // convId -> true（在飞）
  const pending = new Map();    // convId -> 最新待处理 {senderId,m,isGroup}
  const rateLog = new Map();    // convId -> [ts,...]（限速窗内时间戳）
  const consec = new Map();     // convId -> n（连续代答计数）
  const pausedUntil = new Map();// convId -> ts（冷却到期）
  const streamMeta = new Map(); // streamId -> {convId, isGroup}（done 帧回查）

  // MY_ID 是 im.js 顶层 let（经典脚本全局词法绑定，跨 <script> 以裸名可达；非 window 属性）。
  function myId() { try { return (typeof MY_ID !== "undefined" && MY_ID) || ""; } catch (_) { return ""; } }

  function esc(s) { return String(s == null ? "" : s); }

  // presence 语义：后端返回 "online|away|busy|dnd|offline"（或空=未知）。"offline" 是真值字符串，
  // 不能用 !!presence 判在线。可代答=在线态：online/away/busy/dnd 皆可达；仅空与 "offline" 不可用。
  function isOnlinePresence(p) {
    const s = (p || "").toLowerCase();
    return s !== "" && s !== "offline";
  }

  // ── 配置（localStorage，按 MY_ID 分键；每会话一条）──
  function key() { return "nm-delegate:" + myId(); }
  function allCfg() {
    try {
      const raw = JSON.parse(localStorage.getItem(key()) || "{}");
      const chats = (raw && raw.chats && typeof raw.chats === "object") ? raw.chats : {};
      return { chats };
    } catch (_) { return { chats: {} }; }
  }
  function saveAll(next) {
    try { localStorage.setItem(key(), JSON.stringify(next)); } catch (_) {}
  }
  function normModel(m) {
    return m && m.entityId ? { entityId: String(m.entityId).toLowerCase(), kind: m.kind || "", name: m.name || "" } : null;
  }

  // 某会话的代答模型（无=未开启）。
  function chatModel(convId) {
    const c = allCfg().chats[(convId || "").toLowerCase()];
    return c && c.model ? normModel(c.model) : null;
  }
  function isOn(convId) { return !!chatModel(convId); }
  function modelNameFor(convId) {
    const m = chatModel(convId);
    return m ? (m.name || m.entityId.slice(0, 8) + "…") : "";
  }
  function setChatModel(convId, m) {
    const id = (convId || "").toLowerCase();
    if (!id) return;
    const all = allCfg();
    const nm = normModel(m);
    if (nm) all.chats[id] = { model: nm };
    else delete all.chats[id];
    saveAll(all);
  }
  function clearChat(convId) {
    const id = (convId || "").toLowerCase();
    const all = allCfg();
    if (all.chats[id]) { delete all.chats[id]; saveAll(all); }
  }

  // ── 模型在线枚举（与 llm_models.js 同款判定）──
  // 返回 { local:[{entityId,name,model,usable}], remote:[{entityId,name,model,usable}] }
  // usable：本地模型=有密钥即可（聊天只需本机解密，与"对外服务 online"无关）；远端=presence 在线。
  async function listModels() {
    const out = { local: [], remote: [] };
    try {
      const locals = (await NM.inv("llm_model_list")) || [];
      out.local = locals.map((m) => ({
        entityId: (m.entityId || "").toLowerCase(),
        name: m.displayName || m.model || "未命名",
        model: m.model || "",
        usable: m.hasKey !== false,   // 本地：有密钥即可代答（不要求"对外服务中"）
      })).filter((m) => m.entityId);
    } catch (_) {}
    try {
      const localIds = new Set(out.local.map((m) => m.entityId));
      const ents = (typeof mergedEntities === "function" ? mergedEntities() : []);
      out.remote = ents.filter((c) => c && c.id && (typeof kindType === "function" ? kindType(c.kind) === "compute" : false) && !localIds.has((c.id || "").toLowerCase()))
        .map((c) => ({
          entityId: (c.id || "").toLowerCase(),
          name: c.name || c.id.slice(0, 8) + "…",
          model: c.model || "",
          usable: isOnlinePresence(c.presence),   // 远端：presence 在线才可代答
        }));
    } catch (_) {}
    return out;
  }

  // 选定模型当前是否可用（触发前重验）。
  async function modelUsable(entityId) {
    const id = (entityId || "").toLowerCase();
    const { local, remote } = await listModels();
    const hit = local.find((m) => m.entityId === id) || remote.find((m) => m.entityId === id);
    return hit ? hit.usable : false;
  }

  // ── @我识别（群聊门控）：@ 是纯文本，匹配我的显示名集合，词边界避免子串误判。 ──
  function myNames() {
    const names = [];
    try { const p = window.Profile && Profile.get ? Profile.get(myId()) : null; if (p && p.displayName) names.push(p.displayName); } catch (_) {}
    try { const n = window.Identity && Identity.nameOf ? Identity.nameOf(myId()) : ""; if (n) names.push(n); } catch (_) {}
    return Array.from(new Set(names.map((s) => (s || "").trim()).filter(Boolean)));
  }
  function mentionsMe(m) {
    const names = myNames();
    if (!names.length) return false;
    const text = (m && (m.text || m.body)) || "";
    if (!text) return false;
    const low = text.toLowerCase();
    for (const nm of names) {
      const n = nm.toLowerCase();
      let idx = low.indexOf("@" + n);
      while (idx >= 0) {
        // @name 后须是边界：空白/标点/字符串结尾（避免 @张三丰 误匹配 @张三；@所有人≠我名自然不中）。
        const after = low[idx + 1 + n.length];
        if (after === undefined || /[\s,:：，。!！?？、]/.test(after)) return true;
        idx = low.indexOf("@" + n, idx + 1);
      }
    }
    return false;
  }

  // ── 上下文组装（代答语义：对端=user，本人=assistant）──
  // isGroup：群聊取群历史、system 提示点明"群里仅被 @ 时代答"。
  async function buildMessages(convId, isGroup) {
    let hist = [];
    try { hist = (window.ChatLog && (await ChatLog.tail(convId, HIST_N))) || []; } catch (_) {}
    const me = myId();
    const out = [];
    let used = 0;
    for (let i = hist.length - 1; i >= 0; i--) {
      const m = hist[i];
      const t = (m.text || m.body || "").trim();
      if (!t || (m.kind && m.kind !== "text")) continue;           // 仅文本
      if (used + t.length > CTX_BUDGET) break;
      used += t.length;
      // 群聊：本人消息=assistant，其他成员=user（带发言人名，帮助模型理解多人语境）。
      const mine = m.from === me;
      let content = t;
      if (isGroup && !mine) {
        const who = (typeof entityName === "function") ? entityName(m.from) : (m.from || "").slice(0, 8);
        content = who + "：" + t;
      }
      out.unshift({ role: mine ? "assistant" : "user", content });
    }
    const myName = (window.Identity && Identity.label) ? Identity.label(me) : "我";
    if (isGroup) {
      out.unshift({
        role: "system",
        content: `你是「${myName}」在群聊里的 AI 助手。有人 @${myName} 时代其回复。请用第一人称、简洁自然地直接回复，不要暴露你是 AI，不要 @ 任何人，不要加前缀或署名。`,
      });
    } else {
      const friendName = (typeof entityName === "function") ? entityName(convId) : (convId || "").slice(0, 8);
      out.unshift({
        role: "system",
        content: `你是「${myName}」的 AI 助手，正在代替其回复好友「${friendName}」。请用第一人称、简洁自然地直接回复，不要暴露你是 AI，不要加任何前缀或署名。`,
      });
    }
    return out;
  }

  // ── 限速 / 连续上限闸门（按 convId）──
  function gateOk(convId) {
    const now = Date.now();
    const until = pausedUntil.get(convId) || 0;
    if (until > now) return false;                                 // 冷却中
    const log = (rateLog.get(convId) || []).filter((ts) => now - ts < RATE_WINDOW_MS);
    rateLog.set(convId, log);
    if (log.length >= RATE_MAX) return false;
    if (log.length && now - log[log.length - 1] < MIN_GAP_MS) return false;
    if ((consec.get(convId) || 0) >= CONSEC_MAX) {                // 连续触顶 → 冷却并清零
      pausedUntil.set(convId, now + CONSEC_COOLDOWN_MS);
      consec.set(convId, 0);
      return false;
    }
    return true;
  }
  function noteSent(convId) {
    const now = Date.now();
    const log = rateLog.get(convId) || [];
    log.push(now); rateLog.set(convId, log);
    consec.set(convId, (consec.get(convId) || 0) + 1);
  }
  // 我手动在该会话发消息 → 重置连续计数与冷却（打断疑似循环的人工介入）。
  function noteHumanSend(convId) {
    if (!convId) return;
    const id = (convId || "").toLowerCase();
    consec.set(id, 0);
    pausedUntil.delete(id);
  }

  // ── 入站触发（onCoreEvent 调）──
  // convId：私聊=好友 id；群聊=群 id。senderId：发言人。isGroup：是否群聊。
  async function onInbound(convId, senderId, m, isGroup) {
    try {
      convId = (convId || "").toLowerCase();
      senderId = (senderId || "").toLowerCase();
      const model = chatModel(convId);
      if (!model) return;                                          // 该会话未开代答
      if (isGroup) {
        if (!mentionsMe(m)) return;                                // 群聊：没 @我不答（@所有人也不答）
      } else {
        // 私聊：发送方实体类型从目录查（m.kind 是媒体类型）。排除模型/智能体/机器人/设备/车辆。
        const ent = (typeof entityById === "function") ? entityById(senderId) : null;
        const et = ent && typeof kindType === "function" ? kindType(ent.kind) : "person";
        if (et !== "person" && et !== "other") return;
      }
      if (!gateOk(convId)) return;
      if (inflight.get(convId)) { pending.set(convId, { senderId, m, isGroup }); return; } // 在飞：只留最新
      if (!(await modelUsable(model.entityId))) return;            // 离线静默跳过
      await fire(convId, model.entityId, isGroup);
    } catch (e) { /* 代答失败不影响正常收消息 */ }
  }

  async function fire(convId, modelEntityId, isGroup) {
    inflight.set(convId, true);
    const messages = await buildMessages(convId, isGroup);
    const rnd = (crypto.randomUUID && crypto.randomUUID()) || (Date.now() + "" + Math.random().toString(16).slice(2));
    const streamId = "delegate:" + convId + ":" + rnd;
    streamMeta.set(streamId, { convId, isGroup: !!isGroup });
    try {
      await NM.inv("llm_chat_stream", { target: modelEntityId, messages, streamId });
    } catch (e) {
      streamMeta.delete(streamId);
      inflight.delete(convId);
      // 启动失败（未连节点/参数错）：丢弃，不打扰对方。
    }
  }

  // ── 模型回帧（handleAgentDelta 前置拦截调；只认 done 帧）──
  async function onFrame(d) {
    try {
      if (!d || typeof d.streamId !== "string" || !d.done) return;   // 累计全文，仅尾帧
      const meta = streamMeta.get(d.streamId) || { convId: (d.streamId.split(":")[1] || "").toLowerCase(), isGroup: false };
      const convId = meta.convId;
      const isGroup = !!meta.isGroup;
      streamMeta.delete(d.streamId);
      inflight.delete(convId);
      const text = (d.text || "").trim();
      const failed = !text || text.startsWith("⚠");                 // llm_chat.rs 错误 sink 统一 ⚠ 前缀
      if (!failed) {
        const localId = "a" + Date.now();
        // 乐观落库到会话（本侧带 byAgent 徽标）；群聊 group:true。仿 im_conversation.js 发送路径。
        if (typeof pushMsg === "function") {
          await pushMsg(convId, { id: localId, from: myId(), to: convId, body: text, ts: Date.now(), status: 0, byAgent: true, group: isGroup });
        }
        // 会话正打开时气泡以 status=0（转圈）插入 VIEW —— 收到 gram_id 后须同步更新内存 VIEW 行 + 重绘，
        // 否则对号只落库、屏幕一直转（仿 im_conversation.js 私聊/群聊发送收尾）。
        const liveRow = () => (typeof VIEW !== "undefined" && VIEW.id === convId) ? VIEW.rows.find((r) => r.id === localId) : null;
        try {
          const g = isGroup
            ? await NM.inv("send_group", { groupId: convId, text })
            : await NM.inv("send_to", { target: convId, text });
          if (g) {
            const row = liveRow();
            if (row) { row.gramId = g; row.status = 1; if (typeof paintVirtual === "function") paintVirtual(false); }
            if (window.ChatLog && ChatLog.mark) ChatLog.mark(convId, { id: localId, gramId: g }, 1);
          }
        } catch (e) {
          const row = liveRow();
          if (row) { row.status = -1; if (typeof paintVirtual === "function") paintVirtual(false); }
          if (window.ChatLog && ChatLog.mark) ChatLog.mark(convId, { id: localId }, -1); // 发送失败 ✗
        }
        noteSent(convId);
        if (typeof renderPanels === "function") renderPanels();
      }
      // 在飞期间又来消息 → 补一轮（其 tail 上下文天然含被缓冲消息）。群聊仅当新消息仍 @我。
      const next = pending.get(convId);
      if (next) {
        pending.delete(convId);
        const model = chatModel(convId);
        if (model && gateOk(convId)) {
          const ok = next.isGroup ? mentionsMe(next.m) : true;
          if (ok && (await modelUsable(model.entityId))) await fire(convId, model.entityId, next.isGroup);
        }
      }
    } catch (e) { /* 吞掉：代答收尾不影响主流程 */ }
  }

  // 诊断：devtools 控制台 `await Delegate.debug("<会话id，留空取当前会话>")` 查看为何不代答。
  async function debug(convId) {
    convId = (convId || (typeof ACTIVE !== "undefined" ? ACTIVE : "") || "").toLowerCase();
    const isGroup = !!(window.Groups && Groups.byId && Groups.byId(convId));
    const model = chatModel(convId);
    const usable = model ? await modelUsable(model.entityId) : false;
    const info = {
      myId: myId(), myNames: myNames(), configKey: key(),
      convId, isGroup, model, modelUsableNow: usable,
      gateOk: gateOk(convId), inflight: !!inflight.get(convId),
      consec: consec.get(convId) || 0, pausedUntilMs: pausedUntil.get(convId) || 0,
    };
    info.wouldReply = !!model && usable && info.gateOk && !info.inflight;
    console.log("[Delegate.debug]", info);
    return info;
  }

  window.Delegate = {
    chatModel, isOn, modelNameFor, setChatModel, clearChat,
    listModels, mentionsMe, debug,
    onInbound, onFrame, noteHumanSend,
  };
})();

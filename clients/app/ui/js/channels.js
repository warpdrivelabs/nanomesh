// ── 频道 / 主题（P4）：iroh-gossip 上的开放 pub/sub。活动栏「📡 频道」面板 + 订阅/发布 + 历史回填。
//    频道消息经核心事件(channel=true, to=频道id)进入会话缓存，复用会话视图展示为「订阅流」。
(function () {
  let CHANNELS = [];
  const byId = (id) => CHANNELS.find((c) => c.id === id) || null;

  async function refresh() {
    if (!window.NM || !NM.hasTauri || !NM.hasTauri()) { CHANNELS = []; return; }
    try { CHANNELS = await NM.inv("channel_list"); } catch (_) { CHANNELS = []; }
  }

  async function renderChannelsList() {
    await refresh();
    const box = document.getElementById("channels-list");
    if (!box) return;
    if (!CHANNELS.length) { box.innerHTML = '<div class="ns-empty">还没有频道。点上方「＋」新建，或「⇩」粘贴频道 id 订阅。</div>'; return; }
    box.innerHTML = CHANNELS.map((c) => `
      <div class="im-item" data-cid="${c.id}" title="${escapeHtml(c.id)}">
        <span class="av" style="background:${avatarColor(c.id)}">📡</span>
        <span class="mid"><span class="r1"><span class="nm">${escapeHtml(c.name || "(未命名频道)")}</span></span>
        <span class="r2"><span class="msg">${escapeHtml(c.topic || "频道")}</span></span></span>
      </div>`).join("");
    box.querySelectorAll(".im-item").forEach((el) => el.addEventListener("click", () => { if (window.openChannel) openChannel(el.dataset.cid); }));
  }

  async function createChannel() {
    const name = await prompt2("新建频道", "频道名称", "");
    if (name == null) return;
    const topic = await prompt2("频道简介（可选）", "一句话简介", "");
    try {
      const cid = await NM.inv("channel_create", { name: name || "新频道", topic: topic || "" });
      await renderChannelsList();
      if (window.openChannel) openChannel(cid);
      if (window.toast) toast("频道已创建");
    } catch (e) { if (window.toast) toast("建频道失败：" + (e && e.message ? e.message : e)); }
  }

  async function subscribeByPaste() {
    const raw = await prompt2("订阅频道", "粘贴频道 id（64 位 hex）", "");
    if (!raw) return;
    const cid = /^[0-9a-fA-F]{64}$/.test(raw.trim()) ? raw.trim().toLowerCase() : null;
    if (!cid) { if (window.toast) toast("请输入 64 位频道 id hex"); return; }
    try {
      await NM.inv("channel_sub", { channelId: cid, name: "" });
      await primeOne(cid, "");
      await renderChannelsList();
      if (window.openChannel) openChannel(cid);
      if (window.toast) toast("已订阅");
    } catch (e) { if (window.toast) toast("订阅失败：" + (e && e.message ? e.message : e)); }
  }

  // 连接后：订阅已知频道并回填历史到会话缓存。
  async function primeChannels() {
    await refresh();
    for (const c of CHANNELS) await primeOne(c.id, c.name || "");
  }
  async function primeOne(cid, name) {
    try {
      await NM.inv("channel_sub", { channelId: cid, name });
      const msgs = await NM.inv("channel_backfill", { channelId: cid, sinceSeq: 0 });
      if (window.imBackfill) window.imBackfill(cid, (msgs || []).map((m) => ({ id: "c" + m.seq, from: m.from, body: m.body, ts: m.ts, channel: true, to: cid })));
    } catch (_) {}
  }

  // 轻量文本输入弹窗（复用 sec-overlay 样式，自包含）。
  function prompt2(title, ph, val) {
    return new Promise((resolve) => {
      const m = document.createElement("div"); m.className = "sec-overlay on";
      m.innerHTML = `<div class="sec-box"><div class="sec-head">${escapeHtml(title)}<button class="sec-x">✕</button></div><div class="sec-body"><div class="sec-sec">
        <input class="sec-input pv-inp" placeholder="${escapeHtml(ph)}" value="${escapeHtml(val || "")}">
        <div class="sec-actions"><button class="ns-btn pv-c">取消</button><button class="ns-btn ns-primary pv-ok">确定</button></div></div></div>`;
      document.body.appendChild(m);
      const inp = m.querySelector(".pv-inp"); inp.focus();
      const done = (v) => { m.remove(); resolve(v); };
      m.querySelector(".sec-x").onclick = () => done(null);
      m.querySelector(".pv-c").onclick = () => done(null);
      m.querySelector(".pv-ok").onclick = () => done(inp.value.trim());
      m.addEventListener("click", (e) => { if (e.target === m) done(null); });
      inp.addEventListener("keydown", (e) => { if (e.key === "Enter") done(inp.value.trim()); if (e.key === "Escape") done(null); });
    });
  }

  (function init() {
    const nw = document.getElementById("chn-new-btn"); if (nw) nw.addEventListener("click", createChannel);
    const sb = document.getElementById("chn-sub-btn"); if (sb) sb.addEventListener("click", subscribeByPaste);
    const rf = document.getElementById("chn-refresh-btn"); if (rf) rf.addEventListener("click", renderChannelsList);
  })();

  window.Channels = { refresh, byId, list: () => CHANNELS, primeChannels };
  window.renderChannelsList = renderChannelsList;
})();

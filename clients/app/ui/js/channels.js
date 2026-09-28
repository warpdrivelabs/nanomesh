// ── 频道 / 主题（P4）：iroh-gossip 上的开放 pub/sub。活动栏「📡 频道」面板 + 订阅/发布 + 历史回填。
//    频道消息经核心事件(channel=true, to=频道id)进入会话缓存，复用会话视图展示为「订阅流」。
(function () {
  let CHANNELS = [];
  let MYID = "";
  const byId = (id) => CHANNELS.find((c) => c.id === id) || null;
  const isOwner = (id) => { const c = byId(id); return !!(c && MYID && c.owner === MYID); };

  async function refresh() {
    if (!window.NM || !NM.hasTauri || !NM.hasTauri()) { CHANNELS = []; return; }
    try { MYID = MYID || (await NM.inv("my_id").catch(() => "")); } catch (_) {}
    try { CHANNELS = await NM.inv("channel_list"); } catch (_) { CHANNELS = []; }
  }

  // 频道头像：已解析为 data:URI → 图；否则 📡 底色块。
  function channelAv(c, size) {
    const s = size || 40;
    if (window.Profile && Profile.isImg(c.avatar)) return `<img src="${c.avatar}" alt="" style="width:${s}px;height:${s}px;border-radius:50%;object-fit:cover;display:block">`;
    return `<span class="av" style="width:${s}px;height:${s}px;background:${avatarColor(c.id)}">${nmIcon("channels")}</span>`;
  }

  async function renderChannelsList() {
    await refresh();
    paintChannelsList();
    if (window.Profile) Profile.resolveList(CHANNELS, paintChannelsList); // 解析 b3: 频道头像后重绘
  }
  function paintChannelsList() {
    const box = document.getElementById("channels-list");
    if (!box) return;
    if (!CHANNELS.length) { box.innerHTML = '<div class="ns-empty">还没有频道。点上方加号新建，或订阅按钮粘贴频道 id。</div>'; return; }
    box.innerHTML = CHANNELS.map((c) => `
      <div class="im-item" data-cid="${c.id}" title="${escapeHtml(c.id)}">
        ${channelAv(c)}
        <span class="mid"><span class="r1"><span class="nm">${escapeHtml(c.name || "(未命名频道)")}</span></span>
        <span class="r2"><span class="msg">${escapeHtml(c.topic || "频道")}</span></span></span>
      </div>`).join("");
    box.querySelectorAll(".im-item").forEach((el) => el.addEventListener("click", () => { if (window.openChannel) openChannel(el.dataset.cid); }));
    syncSelection();
  }

  function currentChannelId() {
    const key = window.Tabs && Tabs.active ? Tabs.active() : "";
    if (!key || !key.startsWith("c:")) return "";
    const id = key.slice(2);
    if (window.Groups && Groups.byId && Groups.byId(id)) return "";
    return byId(id) ? id : "";
  }
  function syncSelection() {
    const box = document.getElementById("channels-list");
    if (!box) return;
    const sel = currentChannelId();
    box.querySelectorAll(".im-item").forEach((el) => el.classList.toggle("on", !!sel && el.dataset.cid === sel));
  }

  async function createChannel() {
    if (!window.Profile || !Profile.metaEditor) return;
    Profile.metaEditor({
      title: "新建频道", name: "", bio: "", avatar: "",
      namePh: "频道名称", bioLabel: "频道简介", bioPh: "一句话简介（可选）",
      onSave: async ({ name, bio, avatar }) => {
        const cid = await NM.inv("channel_create", { name: name || "新频道", topic: bio || "", avatar });
        await renderChannelsList();
        if (window.openChannel) openChannel(cid);
        if (window.toast) toast("频道已创建");
      },
    });
  }

  // 编辑频道信息（owner）：名称 + 简介 + 头像 → channel_set_meta（经 gossip 广播给订阅者）。
  async function editChannel(cid) {
    const c = byId(cid);
    if (!c || !window.Profile || !Profile.metaEditor) return;
    Profile.metaEditor({
      title: "编辑频道信息", name: c.name || "", bio: c.topic || "", avatar: c.avatar || "", homeNode: c.homeNode || "",
      namePh: "频道名称", bioLabel: "频道简介", bioPh: "一句话简介",
      onSave: async ({ name, bio, avatar }) => {
        await NM.inv("channel_set_meta", { channelId: cid, name: name || (c.name || ""), topic: bio, avatar });
        await renderChannelsList();
        if (window.Tabs) Tabs.retitle("c:" + cid, name || c.name || "频道", "channels");
        if (typeof renderConversation === "function") renderConversation(); // 频道流头部在看 → 重绘
        if (window.toast) toast("频道信息已更新");
      },
    });
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
      m.innerHTML = `<div class="sec-box"><div class="sec-head">${escapeHtml(title)}<button class="sec-x">${nmIcon("close")}</button></div><div class="sec-body"><div class="sec-sec">
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

  window.Channels = { refresh, byId, isOwner, editChannel, list: () => CHANNELS, primeChannels, syncSelection };
  window.renderChannelsList = renderChannelsList;
})();

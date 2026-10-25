// ── 频道 / 主题（P4）：iroh-gossip 上的开放 pub/sub。活动栏「📡 频道」面板 + 订阅/发布 + 历史回填。
//    频道消息经核心事件(channel=true, to=频道id)进入会话缓存；主区由 channel_view.js 以 Hero/Feed/Dock 展示。
(function () {
  let CHANNELS = [];
  let MYID = "";
  const byId = (id) => CHANNELS.find((c) => c.id === id) || null;
  const isOwner = (id) => { const c = byId(id); return !!(c && MYID && c.owner === MYID); };

  function slimChannel(c) {
    const o = Object.assign({}, c);
    if (typeof o.avatar === "string" && o.avatar.indexOf("data:") === 0) delete o.avatar;
    if (typeof o.cover === "string" && o.cover.indexOf("data:") === 0) delete o.cover;
    return o;
  }
  // 当前账号是否可向该频道发布（owner，或 pubPolicy=OPEN）。
  function canPublish(id) {
    const c = byId(id);
    if (!c || !MYID) return false;
    if (c.owner === MYID) return true;
    return Number(c.pubPolicy) !== 1; // 1 = OWNER_ONLY
  }
  function hydrate(list) { CHANNELS = Array.isArray(list) ? list.slice() : []; paintChannelsList(); }
  async function refresh() {
    if (!window.NM || !NM.hasTauri || !NM.hasTauri()) return;
    try { MYID = MYID || (await NM.inv("my_id").catch(() => "")); } catch (_) {}
    try {
      CHANNELS = (await NM.inv("channel_list")) || [];
      if (window.ListCache) ListCache.put({ channels: CHANNELS.map(slimChannel) });
    } catch (_) {
      if (!CHANNELS.length && window.toast) toast("频道列表刷新失败");
    }
  }

  // 频道头像：已解析为 data:URI → 图；否则 📡 底色块。
  function channelAv(c, size) {
    const s = size || 40;
    if (window.Profile && Profile.isImg(c.avatar)) return `<img src="${c.avatar}" alt="" style="width:${s}px;height:${s}px;border-radius:var(--avatar-radius);object-fit:cover;display:block">`;
    return `<span class="av" style="width:${s}px;height:${s}px;background:${avatarColor(c.id)}">${nmIcon("channels")}</span>`;
  }

  async function resolveChannelAssets(onDone) {
    if (!window.Profile) { if (onDone) onDone(); return; }
    const pend = CHANNELS.filter((c) => c && (Profile.isRef(c.avatar) || Profile.isRef(c.cover)));
    if (!pend.length) { if (onDone) onDone(); return; }
    await Promise.all(pend.map(async (c) => {
      if (Profile.isRef(c.avatar)) { const uri = await Profile.resolveAvatar(c.avatar, c.homeNode); if (uri) c.avatar = uri; }
      if (Profile.isRef(c.cover)) { const uri = await Profile.resolveAvatar(c.cover, c.homeNode); if (uri) c.cover = uri; }
    }));
    if (onDone) onDone();
  }

  async function renderChannelsList() {
    await refresh();
    paintChannelsList();
    await resolveChannelAssets(() => {
      paintChannelsList();
      if (typeof ACTIVE !== "undefined" && byId(ACTIVE) && typeof renderConversation === "function") renderConversation();
    });
  }
  function paintChannelsList() {
    const box = document.getElementById("channels-list");
    if (!box) return;
    const q = ((document.getElementById("chn-search") || {}).value || "").trim().toLowerCase();
    const list = CHANNELS.filter((c) => !q || (c.name || "").toLowerCase().includes(q) || (c.topic || "").toLowerCase().includes(q));
    if (!list.length) { box.innerHTML = `<div class="ns-empty">${CHANNELS.length ? (window.t ? t("list.noChannelHit") : "没有匹配的频道") : (window.t ? t("list.noChannel") : "还没有频道。点上方加号新建，或用订阅按钮粘贴频道 id。")}</div>`; return; }
    box.innerHTML = list.map((c) => `
      <div class="im-item" data-cid="${c.id}" title="${escapeHtml(c.id)}">
        ${channelAv(c)}
        <span class="mid"><span class="r1"><span class="nm">${escapeHtml(c.name || "(未命名频道)")}</span></span>
        <span class="r2"><span class="msg">${escapeHtml(c.topic || "频道")}</span></span></span>
      </div>`).join("");
    box.querySelectorAll(".im-item").forEach((el) => el.addEventListener("click", () => { if (window.openChannel) openChannel(el.dataset.cid); }));
    syncSelection();
    if (window.Tabs && Tabs.retitle && Tabs.has) {
      CHANNELS.forEach((c) => { if (Tabs.has("c:" + c.id)) Tabs.retitle("c:" + c.id, c.name || "频道", "channels"); });
    }
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
      title: "新建频道", name: "", bio: "", avatar: "", cover: "", withCover: true,
      namePh: "频道名称", bioLabel: "频道简介", bioPh: "一句话简介（可选）",
      onSave: async ({ name, bio, avatar, cover }) => {
        const cid = await NM.inv("channel_create", { name: name || "新频道", topic: bio || "", avatar });
        // 封面经 set_meta 写入（create 命令暂未透传 cover，避免改动 create 签名）
        if (cover) {
          try { await NM.inv("channel_set_meta", { channelId: cid, name: name || "新频道", topic: bio || "", avatar: avatar || "", cover }); }
          catch (_) {}
        }
        await renderChannelsList();
        if (window.openChannel) openChannel(cid);
        if (window.toast) toast("频道已创建");
        shareChannel(cid); // 创建后立即弹出邀请链接，方便 owner 分享（否则别人无从加入）
      },
    });
  }

  // 编辑频道信息（owner）：名称 + 简介 + 头像 + 封面 → channel_set_meta（经 gossip 广播给订阅者）。
  async function editChannel(cid) {
    const c = byId(cid);
    if (!c || !window.Profile || !Profile.metaEditor) return;
    Profile.metaEditor({
      title: "编辑频道信息", name: c.name || "", bio: c.topic || "", avatar: c.avatar || "", cover: c.cover || "",
      homeNode: c.homeNode || "", withCover: true,
      namePh: "频道名称", bioLabel: "频道简介", bioPh: "一句话简介",
      onSave: async ({ name, bio, avatar, cover }) => {
        await NM.inv("channel_set_meta", {
          channelId: cid, name: name || (c.name || ""), topic: bio, avatar, cover: cover || "",
        });
        await renderChannelsList();
        if (window.Tabs) Tabs.retitle("c:" + cid, name || c.name || "频道", "channels");
        if (typeof renderConversation === "function") renderConversation(); // 频道流头部在看 → 重绘
        if (window.toast) toast("频道信息已更新");
      },
    });
  }

  // 邀请链接：nmspace:channel/<64hex>?name=<URL编码名称>。编码公钥 id + 可选名称提示。
  function buildInvite(cid, name) {
    const q = name ? ("?name=" + encodeURIComponent(name)) : "";
    return "nmspace:channel/" + cid + q;
  }
  // 从「邀请链接」或「裸 64hex」解析出 {cid, name}。无效返回 null。
  function parseInvite(raw) {
    const s = (raw || "").trim();
    const m = s.match(/^nmspace:channel\/([0-9a-fA-F]{64})(?:\?name=([^&]*))?/);
    if (m) return { cid: m[1].toLowerCase(), name: m[2] ? decodeURIComponent(m[2]) : "" };
    if (/^[0-9a-fA-F]{64}$/.test(s)) return { cid: s.toLowerCase(), name: "" };
    return null;
  }
  // 复制频道邀请链接到剪贴板（owner/订阅者分享用）。
  async function copyInvite(cid) {
    const c = byId(cid);
    const link = buildInvite(cid, c && c.name ? c.name : "");
    try { await navigator.clipboard.writeText(link); if (window.toast) toast("邀请链接已复制"); }
    catch (_) { if (window.toast) toast("复制失败"); }
  }

  // 分享频道弹窗：展示邀请链接（只读可选中）+ 一键复制。对方粘贴到「订阅」即可加入。
  function shareChannel(cid) {
    const c = byId(cid);
    const link = buildInvite(cid, c && c.name ? c.name : "");
    const m = document.createElement("div"); m.className = "sec-overlay on";
    m.innerHTML = `<div class="sec-box"><div class="sec-head">分享频道<button class="sec-x">${nmIcon("close")}</button></div>
      <div class="sec-body"><div class="sec-sec">
        <div class="sec-hint">把邀请链接发给好友，对方在「频道 → 订阅」粘贴即可加入：</div>
        <input class="sec-input sh-link" readonly value="${escapeHtml(link)}">
        <div class="sec-actions"><button class="ns-btn sh-c">关闭</button><button class="ns-btn ns-primary sh-ok">复制链接</button></div>
      </div></div></div>`;
    document.body.appendChild(m);
    const inp = m.querySelector(".sh-link"); inp.focus(); inp.select();
    const done = () => m.remove();
    m.querySelector(".sec-x").onclick = done;
    m.querySelector(".sh-c").onclick = done;
    m.querySelector(".sh-ok").onclick = async () => {
      try { await navigator.clipboard.writeText(link); if (window.toast) toast("邀请链接已复制"); } catch (_) {}
      done();
    };
    m.addEventListener("click", (e) => { if (e.target === m) done(); });
  }

  async function subscribeByPaste() {
    const raw = await prompt2("订阅频道", "粘贴频道邀请链接或 64 位 hex id", "");
    if (!raw) return;
    const inv = parseInvite(raw);
    if (!inv) { if (window.toast) toast("请输入邀请链接或 64 位频道 id"); return; }
    try {
      await NM.inv("channel_sub", { channelId: inv.cid, name: inv.name });
      // 先刷目录再回填，避免回填消息进列表时 peerOf 还当私聊。
      await renderChannelsList();
      await primeOne(inv.cid, inv.name);
      if (window.openChannel) openChannel(inv.cid);
      if (window.toast) toast("已订阅");
    } catch (e) { if (window.toast) toast("订阅失败：" + (e && e.message ? e.message : e)); }
  }

  // 连接后：订阅已知频道并回填历史到会话缓存。
  async function primeChannels() {
    await refresh();
    for (const c of CHANNELS) await primeOne(c.id, c.name || "");
  }
  const pulling = new Set();
  async function primeOne(cid, name) {
    if (!cid || pulling.has(cid)) return;
    pulling.add(cid);
    try {
      await NM.inv("channel_sub", { channelId: cid, name: name || "" });
      // 本机日志往往是空的（home 在别的节点）。先拉一次，再隔几秒重试，等 home 把历史推回来。
      for (let i = 0; i < 6; i++) {
        const last = window.ChatLog ? ChatLog.meta(cid).last : null;
        const since = last && last.seq ? last.seq : 0;
        const msgs = await NM.inv("channel_backfill", { channelId: cid, sinceSeq: since });
        if (window.imBackfill && msgs && msgs.length) {
          window.imBackfill(cid, msgs.map((m) => ({ id: cid + ":" + m.seq, from: m.from, body: m.body, ts: m.ts, seq: m.seq, channel: true, to: cid })));
        }
        if (msgs && msgs.length && msgs.length < 500 && i >= 1) break;
        if (i >= 5) break;
        await new Promise((r) => setTimeout(r, 1500));
      }
    } catch (_) {}
    finally { pulling.delete(cid); }
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
    const se = document.getElementById("chn-search"); if (se) se.addEventListener("input", paintChannelsList);
  })();

  window.Channels = { refresh, hydrate, byId, isOwner, canPublish, editChannel, list: () => CHANNELS, primeChannels, pullHistory: (cid) => { const c = byId(cid); return primeOne(cid, (c && c.name) || ""); }, syncSelection, copyInvite, buildInvite, shareChannel };
  window.renderChannelsList = renderChannelsList;
})();

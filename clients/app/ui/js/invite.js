// P2 邀请链接消费端：生成/打开 nmspace://invite/<base64> 链接。
// 生成(需已登录)：调后端 invite_create → 复制链接。
// 打开(登录前)：粘贴 → 后端 invite_parse 验签 → 写 selfhost 配置(nm-* 键，自动持久化) + 把种子存为节点服务 →
//   用户照常登录(login 即 connect，infra() 会读到刚写的配置)。可选群/频道在连接后由 pendingAutoJoin 自动加入。
(function () {
  // 打开邀请后、连接成功前暂存的自动加入目标（imStart 完成后消费）。
  let PENDING = null;

  // 把校验过的邀请字段落地为本机连接配置（nm-* 键 + 节点服务），返回摘要供提示。
  function applyParsed(p) {
    try {
      if (Array.isArray(p.relayUrls) && p.relayUrls.length) localStorage.setItem("nm-relay-urls", p.relayUrls.join(","));
      if (p.pkarrUrl) localStorage.setItem("nm-pkarr-url", p.pkarrUrl);
      const fed = (p.federation || "").trim();
      if (fed) localStorage.setItem("nm-federation", fed);
    } catch (_) {}
    // 把签发节点(种子)存为一个 selfhost 节点服务，连接表单可直接选。
    if (window.NodeSvc && p.seedNode) {
      NodeSvc.put({
        mode: "selfhost", node: p.seedNode, name: (p.federation || "nmspace") + " 邀请节点",
        relayUrls: p.relayUrls || [], pkarrUrl: p.pkarrUrl || null,
      });
      if (window.renderServiceOptions) renderServiceOptions();
      if (window.renderNodeSvcList) window.renderNodeSvcList();
    }
    PENDING = { groupId: p.groupId || null, channelId: p.channelId || null };
  }

  // 连接成功后（imStart 末尾）调用：自动订阅频道 / 加入群。幂等、尽力而为。
  async function consumePending() {
    const pend = PENDING; PENDING = null;
    if (!pend || !window.NM) return;
    if (pend.channelId) {
      try {
        await NM.inv("channel_sub", { channelId: pend.channelId, name: "" });
        if (window.Channels && Channels.pullHistory) await Channels.pullHistory(pend.channelId);
        if (window.renderChannelsList) await renderChannelsList();
        if (window.toast) toast("已按邀请订阅频道");
      } catch (e) { if (window.toast) toast("订阅频道失败：" + (e && e.message ? e.message : e)); }
    }
    if (pend.groupId) {
      try {
        await NM.inv("group_join", { groupId: pend.groupId });
        if (window.renderGroupsList) renderGroupsList();
        if (window.toast) toast("已按邀请加入群");
      } catch (e) { if (window.toast) toast("加入群失败：" + (e && e.message ? e.message : e)); }
    }
  }

  // 打开一条邀请链接：验签 → 落地配置。成功返回摘要对象，失败抛错。
  async function applyLink(link) {
    if (!window.NM) throw new Error("仅桌面版可用");
    const p = await NM.inv("invite_parse", { link: (link || "").trim() });
    applyParsed(p);
    return p;
  }

  // 生成邀请链接（已连接时）。opts: {groupId?, channelId?, ttlSecs?}。返回链接字符串。
  async function generate(opts) {
    opts = opts || {};
    if (!window.NM) throw new Error("仅桌面版可用");
    return await NM.inv("invite_create", {
      groupId: opts.groupId || null,
      channelId: opts.channelId || null,
      ttlSecs: opts.ttlSecs || 0,
    });
  }

  // 生成并复制到剪贴板 + toast（UI 按钮直接调）。
  async function generateAndCopy(opts) {
    try {
      const link = await generate(opts);
      try { await navigator.clipboard.writeText(link); if (window.toast) toast("邀请链接已复制"); }
      catch (_) { if (window.toast) toast("已生成（复制失败，请手动复制）：" + link); }
      return link;
    } catch (e) { if (window.toast) toast("生成邀请失败：" + (e && e.message ? e.message : e)); throw e; }
  }

  // 简易粘贴弹窗（不依赖其它 IIFE 内的 prompt2）。
  function pastePrompt() {
    return new Promise((resolve) => {
      const v = window.prompt ? window.prompt("粘贴邀请链接（nmspace://invite/…）") : null;
      resolve(v);
    });
  }
  // 连接屏"打开邀请链接"入口：粘贴 → 校验落地 → 提示用户照常登录。
  async function openFromPaste() {
    const raw = await pastePrompt();
    if (!raw) return;
    try {
      const p = await applyLink(raw);
      const extra = (p.groupId ? "（含群，连接后自动加入）" : p.channelId ? "（含频道，连接后自动订阅）" : "");
      if (window.toast) toast("邀请已应用" + extra + "：relay/种子已填好，请照常登录");
    } catch (e) { if (window.toast) toast("邀请无效：" + (e && e.message ? e.message : e)); }
  }

  window.Invite = { applyLink, generate, generateAndCopy, openFromPaste, consumePending };
})();

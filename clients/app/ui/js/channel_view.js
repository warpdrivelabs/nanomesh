// ── 频道主区：Hero + Post Feed + Publisher Dock（与群/私聊气泡会话分叉）──
(function () {
  const DRAFT_KEY = "nm.chn.draft.";
  const DOCK_KEY = "nm.chn.dock.";
  let dockOpen = false;
  let topicExpanded = false;

  function activeChn() {
    return (window.Channels && ACTIVE && Channels.byId(ACTIVE)) || null;
  }
  function canPublish(c) {
    if (!c || !window.Channels) return false;
    if (Channels.canPublish) return Channels.canPublish(c.id);
    if (Channels.isOwner && Channels.isOwner(c.id)) return true;
    return Number(c.pubPolicy) !== 1; // 1 = OWNER_ONLY
  }
  function colorOf(id) {
    return typeof avatarColor === "function" ? avatarColor(id) : "#1fb182";
  }
  function draftGet(cid) {
    try { return localStorage.getItem(DRAFT_KEY + cid) || ""; } catch (_) { return ""; }
  }
  function draftSet(cid, text) {
    try {
      if (text) localStorage.setItem(DRAFT_KEY + cid, text);
      else localStorage.removeItem(DRAFT_KEY + cid);
    } catch (_) {}
  }
  function dockPref(cid) {
    try { return localStorage.getItem(DOCK_KEY + cid) === "1"; } catch (_) { return false; }
  }
  function setDockPref(cid, on) {
    try { localStorage.setItem(DOCK_KEY + cid, on ? "1" : "0"); } catch (_) {}
  }

  function renderMarkdown(src) {
    if (window.ChannelMd && ChannelMd.render) return ChannelMd.render(src);
    return typeof escapeHtml === "function" ? escapeHtml(src || "") : String(src || "");
  }

  function heroCoverHtml(c) {
    const tint = colorOf(c.id);
    if (window.Profile && Profile.isImg(c.cover)) {
      return `<div class="chn-cover" style="--chn-tint:${tint}"><img src="${c.cover}" alt=""></div>`;
    }
    if (window.Profile && Profile.isRef(c.cover)) {
      return `<div class="chn-cover chn-cover--pending" data-cover-ref="${escapeHtml(c.cover)}" data-home="${escapeHtml(c.homeNode || "")}" style="--chn-tint:${tint};background:linear-gradient(135deg,${tint} 0%,color-mix(in srgb,${tint} 35%,#0d0d0d) 100%)"></div>`;
    }
    return `<div class="chn-cover chn-cover--tint" style="--chn-tint:${tint};background:
      radial-gradient(ellipse 70% 80% at 18% 30%, color-mix(in srgb, ${tint} 55%, transparent), transparent 60%),
      radial-gradient(ellipse 50% 60% at 88% 70%, color-mix(in srgb, ${tint} 28%, transparent), transparent 55%),
      linear-gradient(145deg, color-mix(in srgb, ${tint} 42%, #121212), #0e0e0e)"></div>`;
  }

  function heroAvatarHtml(c) {
    const tint = colorOf(c.id);
    if (window.Profile && Profile.isImg(c.avatar)) {
      return `<span class="chn-av" style="background:${tint}"><img src="${c.avatar}" alt=""></span>`;
    }
    return `<span class="chn-av" style="background:${tint}">${nmIcon("channels")}</span>`;
  }

  function heroHtml(c) {
    const pub = canPublish(c);
    const owner = Channels.isOwner && Channels.isOwner(c.id);
    const topic = c.topic || "";
    const topicCls = topicExpanded ? "is-open" : "";
    const topicBtn = topic.length > 72
      ? `<button type="button" class="chn-topic-more" data-act="topic-toggle">${topicExpanded ? "收起" : "展开"}</button>`
      : "";
    return `
      <header class="chn-hero">
        ${heroCoverHtml(c)}
        <div class="chn-hero-body">
          ${heroAvatarHtml(c)}
          <div class="chn-hero-meta">
            <div class="chn-hero-row">
              <h1 class="chn-title">${escapeHtml(c.name || "频道")}</h1>
              <span class="chn-badge">频道</span>
            </div>
            <div class="chn-topic ${topicCls}">${escapeHtml(topic || "暂无简介")}${topicBtn}</div>
            <div class="chn-sub">${owner ? "你是管理员 · " : ""}已订阅${pub ? " · 可发布" : ""}</div>
          </div>
          <div class="chn-hero-actions">
            <button type="button" class="chn-act" data-act="share" title="分享邀请">${nmIcon("copy")}<span>分享</span></button>
            <button type="button" class="chn-act" data-act="search" title="搜索">${nmIcon("search")}</button>
            ${owner ? `<button type="button" class="chn-act" data-act="edit" title="编辑频道">${nmIcon("settings")}</button>` : ""}
          </div>
        </div>
      </header>`;
  }

  function postHtml(m, cont) {
    const named = m.from === MY_ID
      ? ((window.Identity && Identity.label && Identity.label(MY_ID)) || "我")
      : (window.entityName ? entityName(m.from) : shortId(m.from));
    const time = typeof msgClock === "function" ? msgClock(m.ts) : "";
    const seq = m.seq ? `<span class="chn-seq">#${m.seq}</span>` : "";
    const face = window.Profile ? Profile.faceHtml(m.from, named, 28) : "";
    const rich = window.Composer ? Composer.bubble(m) : { cls: "", html: escapeHtml(m.body || "") };
    const isPlain = !m.media && (!m.kind || m.kind === "text");
    const body = isPlain
      ? `<div class="chn-post-md">${renderMarkdown(m.text || m.body || "")}</div>`
      : `<div class="chn-post-media ${rich.cls}">${rich.html}${m.text || (m.media && m.media.text) ? `<div class="chn-post-cap">${renderMarkdown(m.text || m.media.text || "")}</div>` : ""}</div>`;
    return `<article class="chn-post${cont ? " is-cont" : ""}" data-mid="${escapeHtml(m.id || "")}">
      <div class="chn-post-head">${face}<div class="chn-post-who"><b>${escapeHtml(named)}</b><span>${time}${seq}</span></div></div>
      <div class="chn-post-body">${body}</div>
    </article>`;
  }

  function feedHtml(rows) {
    if (!rows || !rows.length) {
      return `<div class="chn-empty">
        <div class="chn-empty-ico">${nmIcon("channels")}</div>
        <div class="chn-empty-t">还没有帖子</div>
        <div class="chn-empty-s">${canPublish(activeChn()) ? "在下方撰写并发布第一篇内容" : "等待管理员发布内容"}</div>
      </div>`;
    }
    let html = "", prev = null;
    for (const m of rows) {
      if (!prev || !sameDay(prev.ts, m.ts)) html += `<div class="chn-day"><span>${dayLabel(m.ts)}</span></div>`;
      html += postHtml(m, continues(prev, m));
      prev = m;
    }
    return `<div class="chn-flow">${html}</div>`;
  }

  function dockHtml(c) {
    if (!canPublish(c)) return "";
    const open = dockOpen || dockPref(c.id);
    dockOpen = open;
    if (window.Composer) Composer.prepare(c.id, true, true);
    const composer = window.Composer ? Composer.markup() : "";
    return `<section class="chn-dock${open ? " is-open" : ""}" id="chn-dock">
      <button type="button" class="chn-dock-toggle" data-act="dock-toggle">
        <span class="chn-dock-lab">${nmIcon("plus")}<b>撰写新帖</b><small>支持 Markdown · ⌘/Ctrl+Enter 发布</small></span>
        <span class="chn-dock-chev">${nmIcon("chevron")}</span>
      </button>
      <div class="chn-dock-pane">${composer}</div>
    </section>`;
  }

  function bindHero(root, c) {
    root.querySelectorAll("[data-act]").forEach((btn) => {
      const act = btn.getAttribute("data-act");
      if (act === "share") btn.addEventListener("click", () => Channels.shareChannel && Channels.shareChannel(c.id));
      if (act === "search") btn.addEventListener("click", () => { if (window.Composer && Composer.toggleHistory) Composer.toggleHistory(); });
      if (act === "edit") btn.addEventListener("click", () => Channels.editChannel(c.id));
      if (act === "topic-toggle") btn.addEventListener("click", () => { topicExpanded = !topicExpanded; render(); });
      if (act === "dock-toggle") btn.addEventListener("click", () => {
        dockOpen = !dockOpen;
        setDockPref(c.id, dockOpen);
        const dock = document.getElementById("chn-dock");
        if (dock) dock.classList.toggle("is-open", dockOpen);
        if (dockOpen) {
          const input = document.getElementById("conv-input");
          if (input) { input.focus(); fitDockInput(input); }
        }
      });
    });
  }

  function fitDockInput(input) {
    if (!input) return;
    input.style.height = "auto";
    input.style.height = Math.min(220, Math.max(96, input.scrollHeight)) + "px";
  }

  function resolveAssets(c) {
    if (!window.Profile) return;
    const tasks = [];
    if (Profile.isRef(c.avatar)) {
      tasks.push(Profile.resolveAvatar(c.avatar, c.homeNode).then((uri) => {
        if (uri) { c.avatar = uri; const el = document.querySelector(".chn-av"); if (el) el.innerHTML = `<img src="${uri}" alt="">`; }
      }));
    }
    if (Profile.isRef(c.cover)) {
      tasks.push(Profile.resolveAvatar(c.cover, c.homeNode).then((uri) => {
        if (!uri) return;
        c.cover = uri;
        const el = document.querySelector(".chn-cover");
        if (el) { el.classList.remove("chn-cover--pending", "chn-cover--tint"); el.style.background = ""; el.innerHTML = `<img src="${uri}" alt="">`; }
      }));
    }
    return Promise.all(tasks);
  }

  function attachComposer(c) {
    if (!window.Composer || !canPublish(c)) return;
    const input = document.getElementById("conv-input");
    if (input && !input.value) {
      const d = draftGet(c.id);
      if (d) input.value = d;
    }
    Composer.attach({
      sendText: (text) => sendMsg(text),
      sendRich: (media) => sendRich(media),
      mentions: typeof mentionTargets === "function" ? mentionTargets : () => [],
      cards: typeof cardTargets === "function" ? cardTargets : () => [],
      messages: () => (typeof VIEW !== "undefined" && VIEW.rows ? VIEW.rows.slice() : []),
      search: (q) => ChatLog.search(ACTIVE, q, 80),
      reveal: (id) => revealMsg(id),
    });
    if (input) {
      input.placeholder = "写一篇帖子… 完整 Markdown / GFM（标题、列表、表格、代码块、任务、图片…）";
      input.addEventListener("input", () => { draftSet(c.id, input.value); fitDockInput(input); });
      input.addEventListener("keydown", (e) => {
        if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
          e.preventDefault();
          const send = document.getElementById("conv-send");
          if (send) send.click();
        }
        if (e.key === "Escape") {
          dockOpen = false;
          setDockPref(c.id, false);
          const dock = document.getElementById("chn-dock");
          if (dock) dock.classList.toggle("is-open", false);
        }
      });
      fitDockInput(input);
    }
    // 发布成功后清草稿：拦截 send 后的清空由 Composer 自己处理；这里在发送按钮旁改文案
    const sendBtn = document.getElementById("conv-send");
    if (sendBtn) sendBtn.setAttribute("title", "发布到频道");
  }

  function render() {
    const conv = document.getElementById("conv");
    const c = activeChn();
    if (!conv || !c) return;
    dockOpen = dockPref(c.id);
    topicExpanded = false;
    if (window.Composer) Composer.prepare(c.id, true, true);
    conv.innerHTML = `<div class="chn-view">
      ${heroHtml(c)}
      <div class="log chn-feed" id="conv-log"></div>
      ${dockHtml(c)}
    </div>`;
    bindHero(conv, c);
    resolveAssets(c);
    attachComposer(c);
    openView(c.id, true);
  }

  function paint(stick) {
    const log = document.getElementById("conv-log");
    const c = activeChn();
    if (!log || !c || VIEW.id !== ACTIVE) return false;
    const keepBottom = stick || log.scrollHeight - log.scrollTop - log.clientHeight < 80;
    log.innerHTML = feedHtml(VIEW.rows);
    if (keepBottom) log.scrollTop = log.scrollHeight;
    if (window.Composer) {
      Promise.resolve(Composer.hydrate(log)).then(() => {
        if (keepBottom) {
          const el = document.getElementById("conv-log");
          if (el) el.scrollTop = el.scrollHeight;
        }
      }).catch(() => {});
    }
    // 发布后清空草稿
    const input = document.getElementById("conv-input");
    if (input && !input.value.trim()) draftSet(c.id, "");
    return true;
  }

  function isActive() {
    return !!(activeChn() && document.querySelector(".chn-view"));
  }

  window.ChannelView = { render, paint, isActive, renderMarkdown, canPublish };
})();

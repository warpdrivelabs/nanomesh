// 大模型：侧栏「本地模型 / 远端模型」分组；本地可启停/配置/Grant/监控，远端可查看并创建代理。
(function () {
  const esc = (s) => (typeof escapeHtml === "function" ? escapeHtml(s) : String(s == null ? "" : s));
  let CACHE = [];
  let SEL = null; // localId or "r:"+entityId
  const FOLDED = new Set();
  let monTimer = null;
  // 模型头像：档案里存内容寻址引用（b3:<hash>，与人/群头像同一套），展示前需解析成 data:URI。
  const AV_URI = new Map(); // "b3:<hash>" -> data:URI
  const AV_MISS = new Set(); // 已尝试且未取到的引用（不每次重绘都重走磁盘/网络）

  function ensureCss() {
    if (document.getElementById("llm-models-css")) return;
    const st = document.createElement("style");
    st.id = "llm-models-css";
    st.textContent = `
.llm-dot{width:8px;height:8px;border-radius:50%;flex:0 0 auto}
.llm-dot.on{background:var(--ok,#1fb182);box-shadow:0 0 0 3px color-mix(in srgb,var(--ok,#1fb182) 25%,transparent)}
.llm-dot.off{background:var(--muted);opacity:.45}
.llm-sec{font-size:12px;font-weight:650;color:var(--muted);letter-spacing:.02em;margin:18px 0 8px;text-transform:uppercase}
.llm-form-grid{display:flex;flex-direction:column;gap:10px}
.llm-form-grid label{display:flex;flex-direction:column;gap:4px;font-size:12px;color:var(--muted);font-weight:600}
.llm-form-grid .ns-inp,.llm-form-grid textarea.ns-inp,.llm-form-grid select.ns-inp{
  height:34px;padding:0 10px;border-radius:var(--r-ctrl);border:1px solid var(--border);
  background:var(--surface);color:var(--ink);font-size:13px;font-family:inherit;outline:none;width:100%;box-sizing:border-box}
.llm-form-grid textarea.ns-inp{height:auto;min-height:64px;padding:8px 10px;resize:vertical}
.llm-form-grid .ns-inp:focus{border-color:var(--aqua)}
.llm-form-row{display:flex;gap:8px}
.llm-form-row>*{flex:1;min-width:0}
.llm-check{flex-direction:row!important;align-items:center;gap:8px!important;font-weight:500!important;color:var(--ink)!important}
.llm-check input{width:16px;height:16px}
.llm-log{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:11px;line-height:1.45;
  background:var(--panel2);border:1px solid var(--border);border-radius:8px;padding:10px 12px;
  max-height:280px;overflow:auto;white-space:pre-wrap;word-break:break-all;color:var(--ink2)}
.llm-stat-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(110px,1fr));gap:8px;margin:8px 0 12px}
.llm-stat{background:var(--panel2);border:1px solid var(--border);border-radius:10px;padding:10px 12px}
.llm-stat-v{font-size:15px;font-weight:650}.llm-stat-k{font-size:11px;color:var(--muted);margin-top:4px}
.llm-hint{font-size:11px;font-weight:500;color:var(--muted);margin-top:4px;line-height:1.4}
.llm-adv-toggle{display:flex;align-items:center;gap:6px;margin:14px 0 0;padding:0;border:0;background:none;color:var(--aqua);font:inherit;font-size:12px;font-weight:650;cursor:pointer}
.llm-adv-toggle .nm-ico{width:14px;height:14px;transition:transform .15s}
.llm-adv-toggle[aria-expanded="true"] .nm-ico{transform:rotate(90deg)}
.llm-adv-body{display:none;flex-direction:column;gap:10px;margin-top:10px}
.llm-adv-body.open{display:flex}
.llm-base-row{display:flex;gap:8px;align-items:flex-end}
.llm-base-row label{flex:1;min-width:0}
.llm-base-row .ns-btn{flex:0 0 auto;height:34px}
.llm-av-row{display:flex;align-items:center;gap:10px}
.llm-av-prev{flex:0 0 auto;display:block;line-height:0}
.llm-av-ops{display:flex;gap:6px;flex:0 0 auto}
.llm-av-ops .ns-btn{height:30px;padding:0 10px}
`;
    document.head.appendChild(st);
  }

  function defaultHomeFromNodeSvc() {
    try {
      if (!window.NodeSvc || !NodeSvc.list) return { homeNode: "", mode: "nat", relayUrls: [] };
      const list = NodeSvc.list() || [];
      const s = list[0];
      if (!s) return { homeNode: "", mode: "nat", relayUrls: [] };
      return {
        homeNode: s.node || "",
        mode: s.mode || "nat",
        relayUrls: Array.isArray(s.relayUrls) ? s.relayUrls.slice() : [],
        pkarrUrl: s.pkarrUrl || null,
        dnsOrigin: s.dnsOrigin || null,
      };
    } catch (_) {
      return { homeNode: "", mode: "nat", relayUrls: [] };
    }
  }

  async function load() {
    if (!window.NM) return [];
    try {
      CACHE = (await NM.inv("llm_model_list")) || [];
    } catch (e) {
      CACHE = [];
      if (window.toast) toast(String(e));
    }
    return CACHE;
  }

  function find(id) {
    return CACHE.find((x) => x.localId === id) || null;
  }

  // 可直接交给 faceHtml 的头像值：内联 data:URI 原样用，b3: 引用查已解析缓存（未解析则空=回退首字）。
  function avatarHint(m) {
    const a = (m && m.avatar) || "";
    if (!a) return "";
    if (typeof Profile === "undefined" || !Profile.isRef) return a;
    return Profile.isRef(a) ? AV_URI.get(a) || "" : a;
  }

  // 批量解析档案里的头像引用（命中 Profile 的 avCache/本地磁盘时零网络）。返回是否有新图。
  async function resolveAvatars() {
    if (typeof Profile === "undefined" || !Profile.resolveAvatar) return false;
    const todo = [];
    CACHE.forEach((m) => {
      const a = m && m.avatar;
      if (!a || !Profile.isRef(a) || AV_URI.has(a) || AV_MISS.has(a)) return;
      todo.push(m);
    });
    if (!todo.length) return false;
    let changed = false;
    await Promise.all(
      todo.map(async (m) => {
        const uri = await Profile.resolveAvatar(m.avatar, m.homeNode || "");
        if (uri) {
          AV_URI.set(m.avatar, uri);
          changed = true;
        } else {
          AV_MISS.add(m.avatar);
        }
      })
    );
    return changed;
  }

  function statusLabel(m) {
    if (m && m.online) return m.pid ? "对外服务中 · pid " + m.pid : "对外服务中";
    return "未对外提供";
  }

  function subtitle(m) {
    const parts = [];
    if (m.model) parts.push(m.model);
    if (m.provider) parts.push(m.provider);
    if (!m.hasKey) parts.push("缺 Key");
    else if (m.online) parts.push("对外");
    return parts.join(" · ") || "未配置上游";
  }

  function stopMon() {
    if (monTimer) {
      clearInterval(monTimer);
      monTimer = null;
    }
  }

  function sectionHtml(key, icon, label, bodyHtml, count) {
    const q = ((document.getElementById("llm-search") || {}).value || "").trim();
    const folded = !q && (FOLDED.has(key) || count === 0);
    return `<div class="ent-group${folded ? " folded" : ""}" data-type="${key}">
      <button type="button" class="ent-head" aria-expanded="${folded ? "false" : "true"}">
        <span class="ent-chev">${nmIcon("chevron")}</span>
        <span class="ent-ico">${nmIcon(icon)}</span>
        <span class="ent-label">${esc(label)}</span>
        <span class="ent-count">${count}</span>
      </button>
      <div class="ent-body">${bodyHtml || `<div class="ns-empty" style="padding:8px 10px">暂无</div>`}</div>
    </div>`;
  }

  // ── 侧栏：本地 + 远端 ──
  async function renderList() {
    ensureCss();
    const box = document.getElementById("llm-list");
    if (!box) return;
    const q = ((document.getElementById("llm-search") || {}).value || "").trim().toLowerCase();
    await load();
    await resolveAvatars();

    const localIds = new Set(
      CACHE.map((m) => (m.entityId || "").toLowerCase()).filter(Boolean)
    );

    const localFiltered = CACHE.filter((m) => {
      if (!q) return true;
      return [m.displayName, m.model, m.provider, m.entityId, m.localId]
        .join(" ")
        .toLowerCase()
        .includes(q);
    });

    const remotes = (typeof mergedEntities === "function" ? mergedEntities() : []).filter((c) => {
      if (!c || c.id === (typeof MY_ID !== "undefined" ? MY_ID : "")) return false;
      if (typeof kindType === "function" && kindType(c.kind) !== "compute") return false;
      if (localIds.has((c.id || "").toLowerCase())) return false;
      if (!q) return true;
      return [c.name, c.model, c.provider, c.price, c.id, c.handle]
        .join(" ")
        .toLowerCase()
        .includes(q);
    });

    const localRows = localFiltered
      .map((m) => {
        const face =
          typeof Profile !== "undefined" && Profile.faceHtml
            ? Profile.faceHtml(m.entityId || m.localId, m.displayName, 40, "", avatarHint(m))
            : `<span class="av">${esc((m.displayName || "?")[0])}</span>`;
        const on = SEL === m.localId ? " on" : "";
        return `<div class="im-item${on}" data-kind="local" data-id="${esc(m.localId)}" data-eid="${esc(m.entityId || "")}" title="${esc(statusLabel(m))}">
          ${face}
          <span class="mid">
            <span class="r1"><span class="nm">${esc(m.displayName || m.model || "未命名")}</span></span>
            <span class="r2"><span class="msg">${esc(subtitle(m))}</span></span>
          </span>
          <button type="button" class="ns-btn ns-ico llm-chat-btn" data-chat="${esc(m.entityId || "")}" title="对话" style="flex:none">${typeof nmIcon === "function" ? nmIcon("chat") : "💬"}</button>
          <span class="llm-dot ${m.online ? "on" : "off"}" title="${esc(statusLabel(m))}"></span>
        </div>`;
      })
      .join("");

    const remoteRows = remotes
      .map((c) => {
        const bits = [];
        if (c.model) bits.push(c.model);
        if (c.provider) bits.push(c.provider);
        if (c.price) bits.push(c.price);
        const tag = c.added ? " · 手动" : "";
        const sub = esc((bits.join(" · ") || "远端模型") + tag);
        const item =
          typeof itemHtml === "function"
            ? itemHtml(c, sub, 0, SEL === "r:" + c.id ? c.id : DETAIL_ID, { inlineHandle: false })
            : `<div class="im-item" data-id="${esc(c.id)}"><span class="mid"><span class="nm">${esc(c.name || c.id)}</span></span></div>`;
        return item.replace(/class="im-item/, 'data-kind="remote" class="im-item');
      })
      .join("");

    if (!localFiltered.length && !remotes.length) {
      box.innerHTML = `<div class="im-empty">暂无大模型。<br>点 + 添加本机模型并对外提供；远端模型上线后会出现在此。</div>`;
      return;
    }

    box.innerHTML =
      sectionHtml("llm-local", "device", "本地模型", localRows, localFiltered.length) +
      sectionHtml("llm-remote", "llm", "远端模型", remoteRows, remotes.length);

    box.querySelectorAll(".ent-head").forEach((head) => {
      head.onclick = () => {
        const g = head.closest(".ent-group");
        const key = g.dataset.type;
        if (FOLDED.has(key)) FOLDED.delete(key);
        else FOLDED.add(key);
        const folded = FOLDED.has(key);
        g.classList.toggle("folded", folded);
        head.setAttribute("aria-expanded", folded ? "false" : "true");
      };
    });
    box.querySelectorAll('.im-item[data-kind="local"]').forEach((el) => {
      el.onclick = () => openDetail(el.dataset.id);
      const chat = el.querySelector(".llm-chat-btn");
      if (chat)
        chat.onclick = (ev) => {
          ev.stopPropagation(); // 别触发行点击(进详情)
          const eid = chat.dataset.chat;
          if (eid && typeof selectContact === "function") selectContact(eid);
        };
    });
    box.querySelectorAll('.im-item[data-kind="remote"]').forEach((el) => {
      el.onclick = () => {
        SEL = "r:" + el.dataset.id;
        if (typeof showEntityDetail === "function") showEntityDetail(el.dataset.id);
      };
    });
  }

  function openDetail(localId, opts) {
    SEL = localId;
    stopMon();
    renderList();
    const m = find(localId);
    const title = (m && (m.displayName || m.model)) || (opts && opts.create ? "新本地模型" : "本地模型");
    if (window.Tabs) {
      Tabs.open({
        key: "llm:" + (localId || "new"),
        kind: "llm-model",
        title,
        ico: "llm",
        render: () => {
          if (opts && opts.edit) renderEdit(localId);
          else if (opts && opts.create) renderEdit(null);
          else renderView(localId);
        },
      });
    } else if (opts && (opts.edit || opts.create)) renderEdit(localId);
    else renderView(localId);
  }

  function openCreate() {
    SEL = null;
    const ready = Promise.all([
      load(),
      window.NodeSvc && NodeSvc.load ? NodeSvc.load() : Promise.resolve(),
    ]);
    ready.then(() => openDetail(null, { create: true }));
  }

  async function refreshMon(localId) {
    const logEl = document.getElementById("llm-mon-log");
    const grid = document.getElementById("llm-mon-grid");
    if (!logEl && !grid) {
      stopMon();
      return;
    }
    try {
      const r = await NM.inv("llm_model_runtime", { localId });
      if (grid) {
        grid.innerHTML = `
          <div class="llm-stat"><div class="llm-stat-v" style="color:${r.online ? "var(--ok,#1fb182)" : "var(--muted)"}">${r.online ? "对外中" : "已停"}</div><div class="llm-stat-k">服务</div></div>
          <div class="llm-stat"><div class="llm-stat-v">${r.pid || "—"}</div><div class="llm-stat-k">PID</div></div>
          <div class="llm-stat"><div class="llm-stat-v">${r.logBytes != null ? (r.logBytes < 1024 ? r.logBytes + " B" : (r.logBytes / 1024).toFixed(1) + " KB") : "—"}</div><div class="llm-stat-k">日志大小</div></div>
          <div class="llm-stat"><div class="llm-stat-v">${r.concurrency || "—"}</div><div class="llm-stat-k">并发</div></div>`;
      }
      if (logEl) {
        const atBottom = logEl.scrollTop + logEl.clientHeight >= logEl.scrollHeight - 24;
        logEl.textContent = r.logTail || (r.online ? "（暂无日志输出）" : "进程未运行");
        if (atBottom) logEl.scrollTop = logEl.scrollHeight;
      }
      const pathEl = document.getElementById("llm-mon-logpath");
      if (pathEl && r.logPath) pathEl.textContent = r.logPath;
      const badge = document.getElementById("llm-head-badge");
      if (badge) {
        badge.textContent = r.online ? "● 运行中" : "○ 已停止";
        badge.style.color = r.online ? "var(--ok,#1fb182)" : "var(--muted)";
      }
    } catch (_) {}
  }

  function startMon(localId, tabKey) {
    stopMon();
    refreshMon(localId);
    const key = tabKey || "llm-mon:" + localId;
    monTimer = setInterval(() => {
      if (window.Tabs && Tabs.active && Tabs.active() !== key) return;
      refreshMon(localId);
    }, 2500);
  }

  async function doStart(localId) {
    await NM.inv("llm_model_online", { localId });
    if (window.toast) toast("已启动对外服务");
    await load();
    renderList();
  }

  async function doStop(localId) {
    await NM.inv("llm_model_offline", { localId });
    if (window.toast) toast("已停止服务");
    await load();
    renderList();
  }

  async function doDelete(localId, name) {
    const label = name || "该本地模型";
    if (!confirm(`确认删除本地模型「${label}」？\n\n• 将停止正在运行的对外服务\n• 档案、Key 与运行配置将被清除\n• 此操作不可恢复`)) {
      return false;
    }
    stopMon();
    await NM.inv("llm_model_delete", { localId });
    SEL = null;
    if (window.Tabs) {
      Tabs.close("llm:" + localId);
      Tabs.close("llm-mon:" + localId);
    }
    renderList();
    if (window.toast) toast("已删除");
    return true;
  }

  function openMonitor(localId) {
    const m = find(localId);
    const title = "监控 · " + ((m && (m.displayName || m.model)) || localId.slice(0, 8));
    if (window.Tabs) {
      Tabs.open({
        key: "llm-mon:" + localId,
        kind: "llm-monitor",
        title,
        ico: "list",
        render: () => renderMonitor(localId),
      });
    } else {
      renderMonitor(localId);
    }
  }

  // ── 专用监控页 ──
  function renderMonitor(localId) {
    const conv = document.getElementById("conv");
    if (!conv) return;
    ensureCss();
    const m = find(localId);
    if (!m) {
      conv.innerHTML = `<div class="im-center"><div class="txt" style="color:var(--muted)">模型不存在或已删除</div></div>`;
      stopMon();
      return;
    }
    const online = !!m.online;
    conv.innerHTML = `
      <div class="conv-head">
        <b>监控 · ${esc(m.displayName || m.model || "本地模型")}</b>
        <span id="llm-head-badge" style="margin-left:8px;font-size:12px;color:${online ? "var(--ok,#1fb182)" : "var(--muted)"}">${online ? "● 运行中" : "○ 已停止"}</span>
        <div style="margin-left:auto;display:flex;gap:4px">
          ${
            online
              ? `<button type="button" class="ns-btn ns-ico" id="llm-mon-stop" title="停止">${nmIcon("stop")}</button>`
              : `<button type="button" class="ns-btn ns-ico ns-primary" id="llm-mon-start" title="启动">${nmIcon("play")}</button>`
          }
          <button type="button" class="ns-btn ns-ico" id="llm-mon-back" title="属性">${nmIcon("file")}</button>
        </div>
      </div>
      <div class="detail-scroll"><div class="detail" style="max-width:min(720px,90%)">
        <div class="llm-sec" style="margin-top:4px">运行状态</div>
        <div id="llm-mon-grid" class="llm-stat-grid"></div>

        <div class="llm-sec">服务参数</div>
        <div class="detail-rows">
          <div class="detail-row"><span class="k">上游</span><span class="v">${esc(m.provider || "")} / ${esc(m.model || "—")}</span></div>
          <div class="detail-row"><span class="k">Base URL</span><span class="v mono" style="font-size:11px">${esc(m.baseUrl || "—")}</span></div>
          <div class="detail-row"><span class="k">计量 / 收费</span><span class="v">${esc((m.metering || "token") + " / " + (m.billing || "friend_quota"))}${m.price ? " · " + esc(m.price) : ""}</span></div>
          <div class="detail-row"><span class="k">Grant</span><span class="v">${m.requireGrant !== false ? "需要" : "不需要"}</span></div>
          <div class="detail-row"><span class="k">并发 / 上下文</span><span class="v">${esc(String(m.concurrency || 4))} / ${esc(m.maxContext ? String(m.maxContext) : "默认")}</span></div>
          <div class="detail-row"><span class="k">实体 ID</span><span class="v mono" style="font-size:11px">${esc(m.entityId || "—")}</span></div>
          <div class="detail-row"><span class="k">日志路径</span><span class="v mono" style="font-size:11px" id="llm-mon-logpath">—</span></div>
        </div>

        <div class="llm-sec">实时日志</div>
        <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:8px">
          <span style="font-size:12px;color:var(--muted)">gatewayd 输出 · 约每 2.5s 刷新</span>
          <button class="ns-btn ns-ico" id="llm-mon-refresh" type="button" title="立即刷新">${nmIcon("refresh")}</button>
        </div>
        <pre class="llm-log" id="llm-mon-log" style="max-height:min(52vh,480px)">加载中…</pre>
      </div></div>`;

    const start = document.getElementById("llm-mon-start");
    if (start) {
      start.onclick = async () => {
        try {
          await doStart(localId);
          renderMonitor(localId);
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
    const stop = document.getElementById("llm-mon-stop");
    if (stop) {
      stop.onclick = async () => {
        try {
          await doStop(localId);
          renderMonitor(localId);
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
    const back = document.getElementById("llm-mon-back");
    if (back) back.onclick = () => openDetail(localId);
    const rf = document.getElementById("llm-mon-refresh");
    if (rf) rf.onclick = () => refreshMon(localId);

    // 日志路径填入
    NM.inv("llm_model_runtime", { localId })
      .then((r) => {
        const p = document.getElementById("llm-mon-logpath");
        if (p) p.textContent = r.logPath || "—";
      })
      .catch(() => {});

    startMon(localId, "llm-mon:" + localId);
  }

  // ── 本地模型属性面板（启停 / 配置 / 删除 / 进监控）──
  function renderView(localId) {
    const conv = document.getElementById("conv");
    if (!conv) return;
    ensureCss();
    stopMon();
    const m = find(localId);
    if (!m) {
      conv.innerHTML = `<div class="im-center"><div class="txt" style="color:var(--muted)">模型不存在或已删除</div></div>`;
      return;
    }
    const faceHint = avatarHint(m);
    // 头像引用尚未解析（首次打开）→ 解析后重绘本页；解析成功会写 AV_URI，故不会自激循环。
    if (!faceHint && m.avatar) {
      resolveAvatars().then((changed) => {
        if (changed && window.Tabs && Tabs.active && Tabs.active() === "llm:" + localId) {
          renderView(localId);
        }
      });
    }
    const face =
      typeof Profile !== "undefined" && Profile.faceHtml
        ? Profile.faceHtml(m.entityId || m.localId, m.displayName, 72, "", faceHint)
        : `<div class="ed-avatar" style="background:var(--accent-soft);color:var(--aqua)">${esc((m.displayName || "?")[0])}</div>`;
    const online = !!m.online;

    conv.innerHTML = `
      <div class="conv-head">
        <b>本地模型</b>
        <span id="llm-head-badge" style="margin-left:8px;font-size:12px;color:${online ? "var(--ok,#1fb182)" : "var(--muted)"}">${online ? "● 运行中" : "○ 已停止"}</span>
      </div>
      <div class="detail-scroll"><div class="detail">
        <div class="detail-hero">
          <div class="detail-av-wrap">${face}</div>
          <div style="min-width:0;flex:1">
            <div class="detail-name">${esc(m.displayName || m.model || "未命名")}</div>
            <div class="detail-sub">${esc(subtitle(m))}</div>
          </div>
          <div class="detail-actions">
            ${
              online
                ? `<button type="button" class="ns-btn ns-ico" id="llm-off" title="停止">${nmIcon("stop")}</button>`
                : `<button type="button" class="ns-btn ns-ico ns-primary" id="llm-on" title="启动">${nmIcon("play")}</button>`
            }
            <button type="button" class="ns-btn ns-ico" id="llm-monitor" title="监控">${nmIcon("list")}</button>
            <button type="button" class="ns-btn ns-ico" id="llm-edit" title="编辑属性">${nmIcon("edit")}</button>
            <button type="button" class="ns-btn ns-ico ns-danger" id="llm-del" title="删除">${nmIcon("trash")}</button>
          </div>
        </div>

        <div class="llm-sec">档案</div>
        <div class="detail-rows">
          <div class="detail-row"><span class="k">上游模型</span><span class="v">${esc(m.model || "—")}</span></div>
          <div class="detail-row"><span class="k">厂商 / URL</span><span class="v"><span class="mono" style="font-size:11px">${esc(m.provider || "")} · ${esc(m.baseUrl || "")}</span></span></div>
          <div class="detail-row"><span class="k">API Key</span><span class="v">${m.hasKey ? "已保存" : '<span style="color:var(--danger,#d1495b)">未设置</span>'}</span></div>
          <div class="detail-row"><span class="k">Home / 模式</span><span class="v">${esc(m.mode || "nat")}${m.homeNode ? " · " + esc(m.homeNode.length > 40 ? m.homeNode.slice(0, 40) + "…" : m.homeNode) : ""}</span></div>
          <div class="detail-row"><span class="k">计量 / 收费</span><span class="v">${esc((m.metering || "token") + " / " + (m.billing || "friend_quota"))}</span></div>
          <div class="detail-row"><span class="k">价格</span><span class="v">${esc(m.price || "—")}</span></div>
          <div class="detail-row"><span class="k">需要 Grant</span><span class="v">${m.requireGrant !== false ? "是" : "否"}</span></div>
          <div class="detail-row"><span class="k">并发 / 上下文</span><span class="v">${esc(String(m.concurrency || 4))} / ${esc(m.maxContext ? String(m.maxContext) : "默认")}</span></div>
          <div class="detail-row"><span class="k">简介</span><span class="v">${esc(m.bio || "—")}</span></div>
          <div class="detail-row"><span class="k">实体 ID</span><span class="v mono" style="font-size:11px">${esc(m.entityId || "—")}${m.entityId ? ` <button type="button" class="ns-btn ns-ico" id="llm-copy-id" title="复制">${nmIcon("copy")}</button>` : ""}</span></div>
        </div>

        <div class="llm-sec">授权 Grant</div>
        <div class="detail-rows">
          <div class="detail-row"><span class="k">消费方公钥</span><span class="v"><input class="ns-inp" id="llm-g-aud" style="width:100%" placeholder="64 hex" /></span></div>
          <div class="detail-row"><span class="k">有效天数</span><span class="v"><input class="ns-inp" id="llm-g-days" type="number" min="0" style="width:100%" value="7" /></span></div>
        </div>
        <div class="detail-actions" style="padding:8px 0">
          <button type="button" class="ns-btn ns-ico ns-primary" id="llm-grant" title="签发并复制 Grant">${nmIcon("key")}</button>
        </div>
      </div></div>`;

    const copy = document.getElementById("llm-copy-id");
    if (copy) {
      copy.onclick = async () => {
        try {
          await navigator.clipboard.writeText(m.entityId);
          if (window.toast) toast("实体 ID 已复制");
        } catch (_) {
          if (window.toast) toast("复制失败");
        }
      };
    }
    const onBtn = document.getElementById("llm-on");
    if (onBtn) {
      onBtn.onclick = async () => {
        try {
          await doStart(m.localId);
          renderView(m.localId);
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
    const offBtn = document.getElementById("llm-off");
    if (offBtn) {
      offBtn.onclick = async () => {
        try {
          await doStop(m.localId);
          renderView(m.localId);
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
    const monBtn = document.getElementById("llm-monitor");
    if (monBtn) monBtn.onclick = () => openMonitor(m.localId);
    const grantBtn = document.getElementById("llm-grant");
    if (grantBtn) {
      grantBtn.onclick = async () => {
        const audience = ((document.getElementById("llm-g-aud") || {}).value || "").trim();
        if (!audience) {
          if (window.toast) toast("请填写消费方公钥");
          return;
        }
        const days = parseInt((document.getElementById("llm-g-days") || {}).value || "7", 10);
        try {
          const out = await NM.inv("llm_model_issue_grant", {
            localId: m.localId,
            audience,
            days: Number.isFinite(days) ? days : 7,
          });
          try {
            await navigator.clipboard.writeText(out.grantB64 || "");
          } catch (_) {}
          if (window.toast) toast("Grant 已复制到剪贴板");
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
    const editBtn = document.getElementById("llm-edit");
    if (editBtn) editBtn.onclick = () => openDetail(m.localId, { edit: true });
    const delBtn = document.getElementById("llm-del");
    if (delBtn) {
      delBtn.onclick = async () => {
        try {
          await doDelete(m.localId, m.displayName || m.model);
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
  }

  // ── 新建 / 编辑（厂商预设驱动，尽量少手填）──
  function renderEdit(localId) {
    const conv = document.getElementById("conv");
    if (!conv) return;
    ensureCss();
    stopMon();
    const m = localId ? find(localId) : null;
    const isNew = !m;
    const title = isNew ? "添加本地模型" : "编辑 · " + (m.displayName || m.model || "");
    const homeDef = isNew ? defaultHomeFromNodeSvc() : null;
    const vendors = (window.LlmPresets && LlmPresets.list()) || [];
    const matched =
      (window.LlmPresets &&
        LlmPresets.match((m && m.provider) || "", (m && m.baseUrl) || "", (m && m.model) || "")) ||
      null;
    const initVendorId = (matched && matched.id) || (vendors[0] && vendors[0].id) || "custom";

    const vendorOpts = vendors
      .map((v) => `<option value="${esc(v.id)}"${v.id === initVendorId ? " selected" : ""}>${esc(v.label)}</option>`)
      .join("");

    conv.innerHTML = `
      <div class="conv-head"><b>${esc(title)}</b></div>
      <div class="detail-scroll"><div class="detail">
        <div class="llm-form-grid" id="llm-edit-form">
          <div class="llm-sec">上游连接</div>
          <label>模型厂商
            <select class="ns-inp" id="llm-f-vendor">${vendorOpts}</select>
            <span class="llm-hint" id="llm-f-vendor-hint"></span>
          </label>
          <label>模型
            <select class="ns-inp" id="llm-f-model-sel"></select>
            <span class="llm-hint" id="llm-f-model-hint"></span>
          </label>
          <label id="llm-f-model-wrap" style="display:none">上游模型 ID（手工）
            <input class="ns-inp" id="llm-f-model" value="${esc((m && m.model) || "")}" placeholder="如 gpt-4o / qwen-plus" />
          </label>
          <input type="hidden" id="llm-f-provider" value="${esc((m && m.provider) || "openai")}" />
          <label>显示名称
            <input class="ns-inp" id="llm-f-name" value="${esc((m && m.displayName) || "")}" placeholder="对端目录里显示的名字" />
          </label>
          <label>模型头像
            <div class="llm-av-row">
              <span class="llm-av-prev" id="llm-f-av-prev"></span>
              <span class="llm-av-ops">
                <button type="button" class="ns-btn" id="llm-f-av-pick">选择图片</button>
                <button type="button" class="ns-btn" id="llm-f-av-clear">移除</button>
              </span>
            </div>
            <input type="file" id="llm-f-av-file" accept="image/*" hidden />
            <span class="llm-hint">正方形最佳；保存后随档案发布到对端目录（与人/群头像同一套内容寻址存储）</span>
          </label>
          <label>API Key${m && m.hasKey ? "（已保存，留空不改）" : ""}
            <input class="ns-inp" id="llm-f-key" type="password" placeholder="${m && m.hasKey ? "••••••••" : "sk-…"}" autocomplete="off" />
            <span class="llm-hint" id="llm-f-key-hint"></span>
          </label>
          <div class="llm-base-row">
            <label>Base URL
              <input class="ns-inp" id="llm-f-base" value="${esc((m && m.baseUrl) || "")}" />
            </label>
            <button type="button" class="ns-btn" id="llm-f-base-lock" title="锁定/解锁 URL">预设</button>
          </div>
          <span class="llm-hint" id="llm-f-base-hint">选择厂商后自动填入；点「预设」可改为手工编辑</span>

          <button type="button" class="llm-adv-toggle" id="llm-adv-btn" aria-expanded="false">
            ${typeof nmIcon === "function" ? nmIcon("chevron") : "›"} 高级选项（网络 / 收费 / 简介）
          </button>
          <div class="llm-adv-body" id="llm-adv-body">
            <label>简介（对端可见）
              <input class="ns-inp" id="llm-f-bio" value="${esc((m && m.bio) || "")}" />
            </label>
            <label>状态文案
              <input class="ns-inp" id="llm-f-status" value="${esc((m && m.statusText) || "")}" placeholder="闲置 / 高峰限流…" />
            </label>
            <div class="llm-sec" style="margin-top:4px">P2P 网络</div>
            <label>Home 节点
              <textarea class="ns-inp" id="llm-f-home" rows="3" placeholder="节点公钥 hex 或 NM_NODE_ADDR JSON">${esc((m && m.homeNode) || (homeDef && homeDef.homeNode) || "")}</textarea>
              <span class="llm-hint">${isNew && homeDef && homeDef.homeNode ? "已从当前节点服务自动填入" : "新建时会尝试用节点服务列表首项预填"}</span>
            </label>
            <label>连接模式
              <select class="ns-inp" id="llm-f-mode">
                <option value="nat"${(m ? m.mode : (homeDef && homeDef.mode) || "nat") === "nat" ? " selected" : ""}>nat（N0）</option>
                <option value="lan"${(m ? m.mode : homeDef && homeDef.mode) === "lan" ? " selected" : ""}>lan</option>
                <option value="selfhost"${(m ? m.mode : homeDef && homeDef.mode) === "selfhost" ? " selected" : ""}>selfhost</option>
              </select>
            </label>
            <div class="llm-sec" style="margin-top:4px">收费与授权</div>
            <div class="llm-form-row">
              <label>计量方式
                <select class="ns-inp" id="llm-f-meter">
                  ${["token", "request", "time"].map((x) => `<option value="${x}"${(m && m.metering) === x || (!m && x === "token") ? " selected" : ""}>${x}</option>`).join("")}
                </select>
              </label>
              <label>收费模式
                <select class="ns-inp" id="llm-f-bill">
                  ${["friend_quota", "free", "paid", "pass"].map((x) => `<option value="${x}"${(m && m.billing) === x || (!m && x === "friend_quota") ? " selected" : ""}>${x}</option>`).join("")}
                </select>
              </label>
            </div>
            <label>价格文案
              <input class="ns-inp" id="llm-f-price" value="${esc((m && m.price) || "")}" placeholder="好友免费 / 日 200k tokens" />
            </label>
            <div class="llm-form-row">
              <label>并发上限
                <input class="ns-inp" id="llm-f-conc" type="number" min="1" value="${(m && m.concurrency) || 4}" />
              </label>
              <label>最大上下文
                <input class="ns-inp" id="llm-f-ctx" type="number" min="0" value="${(m && m.maxContext) || 0}" placeholder="0=默认" />
              </label>
            </div>
            <label class="llm-check"><input type="checkbox" id="llm-f-grant" ${!m || m.requireGrant !== false ? "checked" : ""} /> 需要 Grant 才能调用</label>
            <label class="llm-check"><input type="checkbox" id="llm-f-auto" ${m && m.autostart ? "checked" : ""} /> 解锁后自动上线(期望态自启)</label>
          </div>
        </div>
        <div class="detail-actions" style="margin-top:20px">
          <button class="ns-btn ns-primary" id="llm-f-save">保存</button>
          <button class="ns-btn" id="llm-f-cancel">取消</button>
        </div>
        <p style="font-size:12px;color:var(--muted);margin-top:12px">主路径：选厂商 → 选模型 → 填 Key → 保存。保存后在详情页点「对外提供服务」上线到 P2P。</p>
      </div></div>`;

    const form = document.getElementById("llm-edit-form");
    const vendorEl = document.getElementById("llm-f-vendor");
    const modelSel = document.getElementById("llm-f-model-sel");
    const modelInp = document.getElementById("llm-f-model");
    const modelWrap = document.getElementById("llm-f-model-wrap");
    const providerEl = document.getElementById("llm-f-provider");
    const nameEl = document.getElementById("llm-f-name");
    const baseEl = document.getElementById("llm-f-base");
    const baseLock = document.getElementById("llm-f-base-lock");
    const keyEl = document.getElementById("llm-f-key");
    const ctxEl = document.getElementById("llm-f-ctx");
    const vendorHint = document.getElementById("llm-f-vendor-hint");
    const modelHint = document.getElementById("llm-f-model-hint");
    const keyHint = document.getElementById("llm-f-key-hint");
    let nameTouched = !!(m && m.displayName);
    let baseManual = false;
    let applying = false;

    // ── 模型头像：null=未改动 / ""=移除 / 字符串=新引用(b3: 或内联 data:URI) ──
    let pendingAvatar = null;
    let avCur = avatarHint(m || {}); // 当前预览（可直接显示的 data:URI）
    const avPrev = document.getElementById("llm-f-av-prev");
    const avFile = document.getElementById("llm-f-av-file");
    function paintAv() {
      if (!avPrev) return;
      // 预览只认 avCur（可直接显示的 data:URI；选图/移除/未改都已同步到它）。
      // 不能用 pendingAvatar——putBlob 成功后它是 b3:hash 引用，avatarHtml 渲染不出图。
      const src = avCur || "";
      const name = (nameEl && nameEl.value.trim()) || (m && (m.displayName || m.model)) || "?";
      avPrev.innerHTML =
        typeof Profile !== "undefined" && Profile.avatarHtml
          ? Profile.avatarHtml(src, name.slice(0, 1), 56)
          : `<span class="av" style="width:56px;height:56px;flex:0 0 56px">${esc(name.slice(0, 1))}</span>`;
    }
    // 已有档案且头像为内容寻址引用 → 解析后补画预览。
    if (m && m.avatar && !avCur && typeof Profile !== "undefined" && Profile.resolveAvatar) {
      Profile.resolveAvatar(m.avatar, m.homeNode || "").then((uri) => {
        if (!uri || pendingAvatar !== null) return;
        avCur = uri;
        paintAv();
      });
    }
    paintAv();
    const avPick = document.getElementById("llm-f-av-pick");
    if (avPick && avFile) avPick.onclick = () => avFile.click();
    if (avFile) {
      avFile.onchange = async () => {
        const f = avFile.files && avFile.files[0];
        avFile.value = "";
        if (!f || typeof Profile === "undefined" || !Profile.fileToAvatar) return;
        try {
          let data = await Profile.fileToAvatar(f, 256, 0.85);
          if (!data) return;
          // 超限则降质重编码（与群/个人头像一致）
          if (data.length > 200 * 1024) {
            data = await new Promise((resolve, reject) => {
              const img = new Image();
              img.onload = () => {
                const c = document.createElement("canvas");
                c.width = 192;
                c.height = 192;
                c.getContext("2d").drawImage(img, 0, 0, 192, 192);
                resolve(c.toDataURL("image/jpeg", 0.72));
              };
              img.onerror = () => reject(new Error("重编码失败"));
              img.src = data;
            });
          }
          if (data.length > 200 * 1024) {
            if (window.toast) toast("图片太大，请换更简单的图");
            return;
          }
          avCur = data; // 先画预览（零延迟），再后台上传
          paintAv();
          let ref = Profile.putBlob ? await Profile.putBlob(data) : null;
          pendingAvatar = ref || data; // 保存写 b3: 引用；putBlob 失败则内联 data:URI 兜底
        } catch (e) {
          if (window.toast) toast("设置头像失败：" + (e && e.message ? e.message : e));
        }
      };
    }
    const avClear = document.getElementById("llm-f-av-clear");
    if (avClear) {
      avClear.onclick = () => {
        pendingAvatar = "";
        avCur = "";
        paintAv();
      };
    }

    if (nameEl) {
      nameEl.addEventListener("input", () => {
        nameTouched = true;
      });
    }

    function setBaseLocked(locked) {
      baseManual = !locked;
      if (baseEl) baseEl.readOnly = locked;
      if (baseLock) baseLock.textContent = locked ? "预设" : "手工";
      if (baseEl) baseEl.style.opacity = locked ? "0.85" : "1";
    }

    function fillModelSelect(vendor, preferModel) {
      if (!modelSel) return;
      const models = (vendor && vendor.models) || [];
      const prefer = preferModel || "";
      let html = models
        .map((x) => {
          const ctx = x.context ? ` · ${x.context >= 1000 ? Math.round(x.context / 1000) + "k" : x.context}` : "";
          return `<option value="${esc(x.id)}">${esc(x.label || x.id)}${esc(ctx)}</option>`;
        })
        .join("");
      html += `<option value="__custom__">其他（手工输入模型 ID）</option>`;
      modelSel.innerHTML = html;
      const hit = models.some((x) => x.id === prefer);
      if (prefer && hit) modelSel.value = prefer;
      else if (prefer && !hit) modelSel.value = "__custom__";
      else if (models.length) modelSel.value = models[0].id;
      else modelSel.value = "__custom__";
    }

    function applyModelChoice(fromVendorChange) {
      if (!form || applying) return;
      applying = true;
      try {
        const vendor = (window.LlmPresets && LlmPresets.get(vendorEl.value)) || null;
        const sel = modelSel.value;
        const isCustomModel = sel === "__custom__" || !vendor || vendor.id === "custom";
        if (modelWrap) modelWrap.style.display = isCustomModel || sel === "__custom__" ? "" : "none";
        if (!isCustomModel && sel !== "__custom__") {
          if (modelInp) modelInp.value = sel;
        } else if (fromVendorChange && vendor && vendor.id === "custom" && modelInp && !m) {
          modelInp.value = "";
        }
        const mid = (modelInp && modelInp.value.trim()) || (sel !== "__custom__" ? sel : "");
        const preset = vendor && window.LlmPresets ? LlmPresets.modelInVendor(vendor, mid) : null;
        if (preset && ctxEl && (fromVendorChange || !m || !m.maxContext)) {
          ctxEl.value = String(preset.context || 0);
        }
        if (preset && nameEl && (!nameTouched || fromVendorChange) && (isNew || fromVendorChange)) {
          if (!nameTouched) nameEl.value = preset.label || preset.id;
        }
        if (modelHint) {
          modelHint.textContent = preset
            ? `模型 ID：${preset.id}${preset.context ? " · 上下文约 " + preset.context.toLocaleString() + " tokens" : ""}`
            : isCustomModel
              ? "请填写上游实际接受的 model 字段"
              : "";
        }
      } finally {
        applying = false;
      }
    }

    function applyVendor(preferModel, opts) {
      opts = opts || {};
      const vendor = (window.LlmPresets && LlmPresets.get(vendorEl.value)) || null;
      if (!vendor) return;
      if (providerEl) providerEl.value = vendor.provider || vendor.id;
      if (vendorHint) {
        vendorHint.textContent = vendor.keyHelp || (vendor.id === "custom" ? "手动填写 Base URL 与模型 ID" : "");
      }
      if (keyHint) keyHint.textContent = vendor.keyHelp || "";
      if (keyEl && !(m && m.hasKey)) keyEl.placeholder = vendor.keyHint || "sk-…";
      const keepBase = opts.keepBase || baseManual;
      if (!keepBase && baseEl) {
        if (vendor.baseUrl) baseEl.value = vendor.baseUrl;
        else if (vendor.id === "custom" && isNew && !m) baseEl.value = "";
      }
      setBaseLocked(vendor.id !== "custom" && !baseManual && !!vendor.baseUrl);
      fillModelSelect(vendor, preferModel || (m && m.model) || "");
      applyModelChoice(true);
    }

    if (baseLock) {
      baseLock.onclick = () => {
        const vendor = (window.LlmPresets && LlmPresets.get(vendorEl.value)) || null;
        if (baseManual) {
          // 回到预设 URL
          if (vendor && vendor.baseUrl && baseEl) baseEl.value = vendor.baseUrl;
          setBaseLocked(!!(vendor && vendor.baseUrl));
        } else {
          setBaseLocked(false);
        }
      };
    }
    if (vendorEl) {
      vendorEl.onchange = () => {
        baseManual = false;
        applyVendor("", { keepBase: false });
      };
    }
    if (modelSel) {
      modelSel.onchange = () => applyModelChoice(false);
    }
    if (modelInp) {
      modelInp.addEventListener("input", () => {
        if (modelSel && modelSel.value !== "__custom__") modelSel.value = "__custom__";
        applyModelChoice(false);
      });
    }

    const advBtn = document.getElementById("llm-adv-btn");
    const advBody = document.getElementById("llm-adv-body");
    if (advBtn && advBody) {
      // 编辑已有档案或缺 Home 时默认展开高级项
      const needAdv = !isNew || !(homeDef && homeDef.homeNode);
      if (needAdv) {
        advBody.classList.add("open");
        advBtn.setAttribute("aria-expanded", "true");
      }
      advBtn.onclick = () => {
        const open = !advBody.classList.contains("open");
        advBody.classList.toggle("open", open);
        advBtn.setAttribute("aria-expanded", open ? "true" : "false");
      };
    }

    // 初始：按已有档案或默认厂商填充
    applyVendor((m && m.model) || "", { keepBase: !!(m && m.baseUrl) });
    if (m && m.baseUrl && baseEl) baseEl.value = m.baseUrl;
    if (m && m.model && modelInp) modelInp.value = m.model;
    if (m && matched && window.LlmPresets && !LlmPresets.modelInVendor(matched, m.model)) {
      if (modelSel) modelSel.value = "__custom__";
      if (modelWrap) modelWrap.style.display = "";
    }
    applyModelChoice(false);

    document.getElementById("llm-f-cancel").onclick = () => {
      if (isNew) {
        if (window.Tabs) Tabs.close("llm:new");
        if (window.Tabs) Tabs.showEmpty();
      } else renderView(localId);
    };
    document.getElementById("llm-f-save").onclick = async () => {
      if (!form) return;
      const name = (nameEl && nameEl.value.trim()) || "";
      const model =
        modelSel && modelSel.value && modelSel.value !== "__custom__"
          ? modelSel.value
          : ((modelInp && modelInp.value.trim()) || "");
      if (!model) {
        if (window.toast) toast("请选择或填写上游模型");
        return;
      }
      const baseUrl = (baseEl && baseEl.value.trim()) || "";
      if (!baseUrl) {
        if (window.toast) toast("请填写 Base URL");
        return;
      }
      if (isNew && !(keyEl && keyEl.value.trim()) && !(m && m.hasKey)) {
        if (window.toast) toast("请填写 API Key");
        return;
      }
      const homeNode = ((document.getElementById("llm-f-home") || {}).value || "").trim();
      if (!homeNode) {
        if (advBody && !advBody.classList.contains("open")) {
          advBody.classList.add("open");
          if (advBtn) advBtn.setAttribute("aria-expanded", "true");
        }
        if (window.toast) toast("请填写 Home 节点（高级选项）");
        return;
      }
      const vendor = (window.LlmPresets && LlmPresets.get(vendorEl.value)) || null;
      const share = {
        localId: (m && m.localId) || "",
        entityId: (m && m.entityId) || "",
        displayName: name || model,
        model,
        provider: (providerEl && providerEl.value.trim()) || (vendor && vendor.provider) || "openai",
        baseUrl,
        homeNode,
        mode: (document.getElementById("llm-f-mode") || {}).value || "nat",
        bio: ((document.getElementById("llm-f-bio") || {}).value || "").trim(),
        statusText: ((document.getElementById("llm-f-status") || {}).value || "").trim(),
        metering: ((document.getElementById("llm-f-meter") || {}).value || "").trim() || "token",
        billing: ((document.getElementById("llm-f-bill") || {}).value || "").trim() || "friend_quota",
        price: ((document.getElementById("llm-f-price") || {}).value || "").trim(),
        requireGrant: (() => {
          const el = document.getElementById("llm-f-grant");
          return el ? !!el.checked : true;
        })(),
        autostart: !!((document.getElementById("llm-f-auto") || {}).checked),
        concurrency: Number((document.getElementById("llm-f-conc") || {}).value) || 4,
        maxContext: Number((document.getElementById("llm-f-ctx") || {}).value) || 0,
        avatar: pendingAvatar === null ? (m && m.avatar) || "" : pendingAvatar,
        links: (m && m.links) || [],
        locale: (m && m.locale) || "",
        relayUrls: (m && m.relayUrls) || (homeDef && homeDef.relayUrls) || [],
        pkarrUrl: (m && m.pkarrUrl) || (homeDef && homeDef.pkarrUrl) || null,
        dnsOrigin: (m && m.dnsOrigin) || (homeDef && homeDef.dnsOrigin) || null,
        hasKey: !!(m && m.hasKey),
        online: false,
      };
      try {
        const saved = await NM.inv("llm_model_upsert", { share });
        const key = keyEl && keyEl.value;
        if (key && key.trim()) {
          await NM.inv("llm_model_set_key", { localId: saved.localId, apiKey: key.trim() });
        }
        if (window.toast) toast("已保存");
        await load();
        SEL = saved.localId;
        if (isNew && window.Tabs) Tabs.close("llm:new");
        if (window.Tabs) {
          Tabs.open({
            key: "llm:" + saved.localId,
            kind: "llm-model",
            title: saved.displayName || saved.model || "本地模型",
            ico: "llm",
            render: () => renderView(saved.localId),
          });
        } else renderView(saved.localId);
        renderList();
      } catch (e) {
        if (window.toast) toast(String(e));
      }
    };
  }

  function bind() {
    const nw = document.getElementById("llm-new");
    if (nw && !nw._llmBound) {
      nw._llmBound = true;
      nw.addEventListener("click", openCreate);
      nw.title = "增加本地模型";
    }
    const se = document.getElementById("llm-search");
    if (se && !se._llmBound) {
      se._llmBound = true;
      se.addEventListener("input", () => renderList());
    }
  }

  window.renderLlmModelList = renderList;
  window.renderModels = renderList;
  window.openLlmProvide = openCreate;
  // 聊天直聊模型:按 entityId 查本地模型(sendMsg 分支判定 + 会话头显示名)。
  window.LlmModels = window.LlmModels || {};
  window.LlmModels.byEntity = function (eid) {
    const k = (eid || "").toLowerCase();
    return CACHE.find((m) => (m.entityId || "").toLowerCase() === k) || null;
  };
  // 会话头/列表用：把档案里的头像（可能是 b3: 引用）换成可直接显示的值。
  window.LlmModels.avatarOf = (m) => avatarHint(m);
  document.addEventListener("DOMContentLoaded", bind);
  if (document.readyState !== "loading") bind();

  if (window.NM && NM.on) {
    NM.on("core://event", (ev) => {
      if (!ev || ev.type !== "llm_model") return;
      load().then(() => {
        renderList();
        if (!window.Tabs || !Tabs.active) return;
        const act = Tabs.active();
        if (ev.localId && act === "llm:" + ev.localId) {
          const conv = document.getElementById("conv");
          if (conv && conv.querySelector("#llm-on, #llm-off, #llm-monitor") && !conv.querySelector("#llm-edit-form")) {
            renderView(ev.localId);
          }
        } else if (ev.localId && act === "llm-mon:" + ev.localId) {
          renderMonitor(ev.localId);
        }
      });
    });
  }
})();

// 大模型：侧栏「本地模型 / 远端模型」分组；本地可启停/配置/Grant/监控，远端可查看并创建代理。
(function () {
  const esc = (s) => (typeof escapeHtml === "function" ? escapeHtml(s) : String(s == null ? "" : s));
  let CACHE = [];
  let SEL = null; // localId or "r:"+entityId
  const FOLDED = new Set();
  let monTimer = null;

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
`;
    document.head.appendChild(st);
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
            ? Profile.faceHtml(m.entityId || m.localId, m.displayName, 40, "", m.avatar)
            : `<span class="av">${esc((m.displayName || "?")[0])}</span>`;
        const on = SEL === m.localId ? " on" : "";
        return `<div class="im-item${on}" data-kind="local" data-id="${esc(m.localId)}" title="${esc(statusLabel(m))}">
          ${face}
          <span class="mid">
            <span class="r1"><span class="nm">${esc(m.displayName || m.model || "未命名")}</span></span>
            <span class="r2"><span class="msg">${esc(subtitle(m))}</span></span>
          </span>
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
    load().then(() => openDetail(null, { create: true }));
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
    const face =
      typeof Profile !== "undefined" && Profile.faceHtml
        ? Profile.faceHtml(m.entityId || m.localId, m.displayName, 72, "", m.avatar)
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

  // ── 新建 / 编辑 ──
  function renderEdit(localId) {
    const conv = document.getElementById("conv");
    if (!conv) return;
    ensureCss();
    stopMon();
    const m = localId ? find(localId) : null;
    const isNew = !m;
    const title = isNew ? "添加本地模型" : "编辑 · " + (m.displayName || m.model || "");

    conv.innerHTML = `
      <div class="conv-head"><b>${esc(title)}</b></div>
      <div class="detail-scroll"><div class="detail">
        <div class="llm-form-grid" id="llm-edit-form">
          <div class="llm-sec">上游连接</div>
          <label>显示名称
            <input class="ns-inp" id="llm-f-name" value="${esc((m && m.displayName) || "")}" placeholder="如 GPT-4o 共享" />
          </label>
          <div class="llm-form-row">
            <label>上游模型名
              <input class="ns-inp" id="llm-f-model" value="${esc((m && m.model) || "")}" placeholder="gpt-4o" />
            </label>
            <label>厂商
              <input class="ns-inp" id="llm-f-provider" value="${esc((m && m.provider) || "openai")}" />
            </label>
          </div>
          <label>Base URL
            <input class="ns-inp" id="llm-f-base" value="${esc((m && m.baseUrl) || "https://api.openai.com/v1")}" />
          </label>
          <label>API Key${m && m.hasKey ? "（已保存，留空不改）" : ""}
            <input class="ns-inp" id="llm-f-key" type="password" placeholder="${m && m.hasKey ? "••••••••" : "sk-…"}" autocomplete="off" />
          </label>
          <label>简介（对端可见）
            <input class="ns-inp" id="llm-f-bio" value="${esc((m && m.bio) || "")}" />
          </label>
          <label>状态文案
            <input class="ns-inp" id="llm-f-status" value="${esc((m && m.statusText) || "")}" placeholder="闲置 / 高峰限流…" />
          </label>

          <div class="llm-sec">P2P 网络</div>
          <label>Home 节点
            <textarea class="ns-inp" id="llm-f-home" rows="3" placeholder="节点公钥 hex 或 NM_NODE_ADDR JSON">${esc((m && m.homeNode) || "")}</textarea>
          </label>
          <label>连接模式
            <select class="ns-inp" id="llm-f-mode">
              <option value="nat"${!m || m.mode === "nat" ? " selected" : ""}>nat（N0）</option>
              <option value="lan"${m && m.mode === "lan" ? " selected" : ""}>lan</option>
              <option value="selfhost"${m && m.mode === "selfhost" ? " selected" : ""}>selfhost</option>
            </select>
          </label>

          <div class="llm-sec">收费与授权</div>
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
        </div>
        <div class="detail-actions" style="margin-top:20px">
          <button class="ns-btn ns-primary" id="llm-f-save">保存</button>
          <button class="ns-btn" id="llm-f-cancel">取消</button>
        </div>
        <p style="font-size:12px;color:var(--muted);margin-top:12px">保存后需在详情页点「对外提供服务」才会以独立进程上线到 P2P。</p>
      </div></div>`;

    document.getElementById("llm-f-cancel").onclick = () => {
      if (isNew) {
        if (window.Tabs) Tabs.close("llm:new");
        if (window.Tabs) Tabs.showEmpty();
      } else renderView(localId);
    };
    document.getElementById("llm-f-save").onclick = async () => {
      const form = document.getElementById("llm-edit-form");
      if (!form) return;
      const name = form.querySelector("#llm-f-name").value.trim();
      const model = form.querySelector("#llm-f-model").value.trim();
      if (!name && !model) {
        if (window.toast) toast("请填写显示名称或上游模型名");
        return;
      }
      const share = {
        localId: (m && m.localId) || "",
        entityId: (m && m.entityId) || "",
        displayName: name || model,
        model,
        provider: form.querySelector("#llm-f-provider").value.trim() || "openai",
        baseUrl: form.querySelector("#llm-f-base").value.trim(),
        homeNode: form.querySelector("#llm-f-home").value.trim(),
        mode: form.querySelector("#llm-f-mode").value,
        bio: form.querySelector("#llm-f-bio").value.trim(),
        statusText: form.querySelector("#llm-f-status").value.trim(),
        metering: form.querySelector("#llm-f-meter").value.trim() || "token",
        billing: form.querySelector("#llm-f-bill").value.trim() || "friend_quota",
        price: form.querySelector("#llm-f-price").value.trim(),
        requireGrant: form.querySelector("#llm-f-grant").checked,
        concurrency: Number(form.querySelector("#llm-f-conc").value) || 4,
        maxContext: Number(form.querySelector("#llm-f-ctx").value) || 0,
        avatar: (m && m.avatar) || "",
        links: (m && m.links) || [],
        locale: (m && m.locale) || "",
        relayUrls: (m && m.relayUrls) || [],
        pkarrUrl: (m && m.pkarrUrl) || null,
        dnsOrigin: (m && m.dnsOrigin) || null,
        hasKey: !!(m && m.hasKey),
        online: false,
      };
      try {
        const saved = await NM.inv("llm_model_upsert", { share });
        const key = form.querySelector("#llm-f-key").value;
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

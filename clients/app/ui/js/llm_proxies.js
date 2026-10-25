// 消费方：本地代理列表 + 每端口管理/监控页（可滚动 + 启停）
(function () {
  const esc = (s) => (typeof escapeHtml === "function" ? escapeHtml(s) : String(s == null ? "" : s));
  let CACHE = [];
  /** providerId → 最近一次配置（停机后可重启） */
  const LAST = Object.create(null);
  let pollTimer = null;
  let activeProvider = null;

  function fmtDur(sec) {
    sec = Number(sec) || 0;
    if (sec < 60) return sec + "s";
    if (sec < 3600) return Math.floor(sec / 60) + "m " + (sec % 60) + "s";
    const h = Math.floor(sec / 3600);
    const m = Math.floor((sec % 3600) / 60);
    return h + "h " + m + "m";
  }

  function fmtBytes(n) {
    n = Number(n) || 0;
    if (n < 1024) return n + " B";
    if (n < 1048576) return (n / 1024).toFixed(1) + " KB";
    return (n / 1048576).toFixed(2) + " MB";
  }

  function fmtTime(ms) {
    if (!ms) return "—";
    return new Date(ms).toLocaleTimeString();
  }

  function healthBadge(p) {
    if (!p || !p.running) return `<span style="color:var(--muted);font-size:11px">○ 已停</span>`;
    if (p.inFlight > 0) return `<span style="color:var(--accent,#2f6fdd);font-size:11px">● 处理中 ${p.inFlight}</span>`;
    if (p.errors > 0 && p.success === 0) return `<span style="color:var(--danger,#d1495b);font-size:11px">● 异常</span>`;
    return `<span style="color:var(--ok,#1fb182);font-size:11px">● 运行中</span>`;
  }

  function remember(d) {
    if (!d || !d.providerId) return;
    const prev = LAST[d.providerId] || {};
    LAST[d.providerId] = {
      providerId: d.providerId,
      displayName: d.displayName || prev.displayName || "",
      model: d.model || prev.model || "",
      listen: d.listen || prev.listen || "",
      localToken: d.localToken || prev.localToken || "nm-local-token",
      timeoutSecs: d.timeoutSecs || prev.timeoutSecs || 120,
      grantB64: prev.grantB64 || "",
      hasGrant: !!d.hasGrant,
    };
  }

  function readForm(root, pid) {
    const prev = LAST[pid] || {};
    const grantEl = root.querySelector("#px-grant");
    const grantB64 = grantEl && grantEl.value.trim() ? grantEl.value.trim() : prev.grantB64 || "";
    return {
      providerId: pid,
      displayName: ((root.querySelector("#px-name") || {}).value || prev.displayName || "").trim(),
      model: ((root.querySelector("#px-model") || {}).value || prev.model || "").trim(),
      listen: ((root.querySelector("#px-listen") || {}).value || "").trim(),
      localToken: ((root.querySelector("#px-token") || {}).value || prev.localToken || "nm-local-token").trim(),
      timeoutSecs: Number((root.querySelector("#px-timeout") || {}).value) || prev.timeoutSecs || 120,
      grantB64,
    };
  }

  async function load() {
    if (!window.NM) return [];
    try {
      CACHE = (await NM.inv("llm_proxy_list")) || [];
    } catch (_) {
      CACHE = [];
    }
    return CACHE;
  }

  async function renderList() {
    const box = document.getElementById("px-list");
    if (!box) return;
    const q = ((document.getElementById("px-search") || {}).value || "").trim().toLowerCase();
    await load();
    let rows = CACHE;
    if (q) {
      rows = rows.filter(
        (p) =>
          (p.displayName || "").toLowerCase().includes(q) ||
          (p.listen || "").includes(q) ||
          (p.providerId || "").toLowerCase().includes(q) ||
          (p.model || "").toLowerCase().includes(q)
      );
    }
    if (!rows.length) {
      box.innerHTML = `<div class="empty-hint" style="padding:16px;color:var(--muted)">暂无运行中的模型代理。<br>在「大模型」中选中远端模型并启动；每个模型占用一个端口。</div>`;
      return;
    }
    box.innerHTML = rows
      .map((p) => {
        const errHint = p.errors ? ` · 错 ${p.errors}` : "";
        return `<div class="ns-row" data-id="${esc(p.providerId)}">
          <div class="ns-main" style="display:flex;gap:10px;align-items:center;cursor:pointer">
            <div style="min-width:0;flex:1">
              <div style="font-weight:600;display:flex;gap:8px;align-items:center">${esc(p.displayName || p.model || "代理")} ${healthBadge(p)}</div>
              <div style="font-size:11px;color:var(--muted)">${esc(p.listen)} · 请求 ${p.requests || 0}${errHint}${p.lastLatencyMs ? " · " + p.lastLatencyMs + "ms" : ""}</div>
              <div style="font-size:10px;color:var(--muted)">${esc((p.providerId || "").slice(0, 12))}…</div>
            </div>
          </div>
        </div>`;
      })
      .join("");
    box.querySelectorAll(".ns-row").forEach((el) => {
      el.onclick = () => openProxyDetail(el.dataset.id);
    });
  }

  function stopPoll() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
    activeProvider = null;
  }

  function startPoll(providerId) {
    stopPoll();
    activeProvider = providerId;
    pollTimer = setInterval(() => {
      if (!activeProvider) return;
      if (window.Tabs && Tabs.has && !Tabs.has("px:" + activeProvider)) {
        stopPoll();
        return;
      }
      if (window.Tabs && Tabs.active && Tabs.active() !== "px:" + activeProvider) return;
      refreshDetailBody(activeProvider);
    }, 2000);
  }

  function ensureCss() {
    if (document.getElementById("px-stat-css")) return;
    const st = document.createElement("style");
    st.id = "px-stat-css";
    st.textContent = `
/* 占满 #conv，让内部 .detail-scroll 可纵向滚动 */
.px-page{flex:1;min-height:0;height:100%;display:flex;flex-direction:column;overflow:hidden}
.px-stat{background:var(--panel2);border:1px solid var(--border);border-radius:10px;padding:10px 12px}
.px-stat-v{font-size:16px;font-weight:650;line-height:1.2}
.px-stat-v small{font-size:11px;font-weight:500;margin-left:2px;color:var(--muted)}
.px-stat-k{font-size:11px;color:var(--muted);margin-top:4px}
.px-head-acts{display:flex;align-items:center;gap:8px;margin-left:auto}
`;
    document.head.appendChild(st);
  }

  function monHtml(d) {
    if (!d.running) {
      return `<div style="padding:8px 0 4px;color:var(--muted);font-size:13px">代理已停止。可修改下方配置后点击「启动」。</div>`;
    }
    const logRows = (d.recent || [])
      .slice()
      .reverse()
      .map((e) => {
        const color = e.ok ? "var(--ok,#1fb182)" : "var(--danger,#d1495b)";
        return `<tr>
          <td style="white-space:nowrap;color:var(--muted);padding:4px 8px">${esc(fmtTime(e.tsMs))}</td>
          <td style="padding:4px 8px">${esc(e.path || "")}</td>
          <td style="color:${color};padding:4px 8px">${e.status}</td>
          <td style="padding:4px 8px">${e.latencyMs}ms</td>
          <td style="padding:4px 8px">${fmtBytes(e.bytesIn)}/${fmtBytes(e.bytesOut)}</td>
          <td style="max-width:220px;overflow:hidden;text-overflow:ellipsis;color:var(--muted);padding:4px 8px" title="${esc(e.error || "")}">${esc(e.error || (e.ok ? "ok" : ""))}</td>
        </tr>`;
      })
      .join("");
    return `
      <div style="display:flex;align-items:center;gap:8px;margin-bottom:10px">
        <span style="font-weight:600">监控</span>${healthBadge(d)}
        <span style="font-size:11px;color:var(--muted);margin-left:auto">每 2s 刷新</span>
      </div>
      <div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(110px,1fr));gap:8px;margin-bottom:12px">
        <div class="px-stat"><div class="px-stat-v">${esc(d.listen || "—")}</div><div class="px-stat-k">监听</div></div>
        <div class="px-stat"><div class="px-stat-v">${d.requests || 0}</div><div class="px-stat-k">总请求</div></div>
        <div class="px-stat"><div class="px-stat-v" style="color:var(--ok,#1fb182)">${d.success || 0}</div><div class="px-stat-k">成功</div></div>
        <div class="px-stat"><div class="px-stat-v" style="color:var(--danger,#d1495b)">${d.errors || 0}</div><div class="px-stat-k">失败</div></div>
        <div class="px-stat"><div class="px-stat-v">${d.inFlight || 0}</div><div class="px-stat-k">进行中</div></div>
        <div class="px-stat"><div class="px-stat-v">${d.lastLatencyMs || 0}<small>ms</small></div><div class="px-stat-k">最近延迟</div></div>
        <div class="px-stat"><div class="px-stat-v">${d.avgLatencyMs || 0}<small>ms</small></div><div class="px-stat-k">平均延迟</div></div>
        <div class="px-stat"><div class="px-stat-v">${fmtDur(d.uptimeSecs)}</div><div class="px-stat-k">运行时长</div></div>
      </div>
      <div class="detail-rows" style="margin-bottom:12px">
        <div class="detail-row"><span class="k">流量</span><span class="v">入 ${fmtBytes(d.bytesIn)} · 出 ${fmtBytes(d.bytesOut)} · 鉴权失败 ${d.authFails || 0}</span></div>
        <div class="detail-row"><span class="k">最近成功</span><span class="v">${esc(fmtTime(d.lastOkAtMs))}</span></div>
        <div class="detail-row"><span class="k">最近错误</span><span class="v" style="color:${d.lastError ? "var(--danger,#d1495b)" : "inherit"}">${esc(d.lastError || "—")}${d.lastErrorAtMs ? " · " + fmtTime(d.lastErrorAtMs) : ""}</span></div>
        <div class="detail-row"><span class="k">Grant</span><span class="v">${d.hasGrant ? "已配置" : "未配置"}</span></div>
      </div>
      <div style="font-weight:600;margin:4px 0 8px">最近请求</div>
      <div style="border:1px solid var(--border);border-radius:8px;margin-bottom:8px">
        <table style="width:100%;border-collapse:collapse;font-size:12px">
          <thead><tr style="text-align:left;color:var(--muted);background:var(--panel2)">
            <th style="padding:6px 8px">时间</th><th style="padding:6px 8px">路径</th><th style="padding:6px 8px">状态</th><th style="padding:6px 8px">延迟</th><th style="padding:6px 8px">入/出</th><th style="padding:6px 8px">说明</th>
          </tr></thead>
          <tbody>${logRows || `<tr><td colspan="6" style="padding:12px;color:var(--muted)">尚无请求</td></tr>`}</tbody>
        </table>
      </div>`;
  }

  function setRunningUi(root, running) {
    const start = root.querySelector("#px-start");
    const stop = root.querySelector("#px-stop");
    const save = root.querySelector("#px-save");
    const probe = root.querySelector("#px-probe");
    const clear = root.querySelector("#px-clear");
    const clearGrant = root.querySelector("#px-clear-grant");
    const listen = root.querySelector("#px-listen");
    if (start) start.style.display = running ? "none" : "";
    if (stop) stop.style.display = running ? "" : "none";
    if (save) save.style.display = running ? "" : "none";
    if (probe) probe.style.display = running ? "" : "none";
    if (clear) clear.style.display = running ? "" : "none";
    if (clearGrant) clearGrant.disabled = !running;
    if (listen) listen.disabled = !!running;
  }

  function bindMgmt(root, pid) {
    const save = root.querySelector("#px-save");
    const copy = root.querySelector("#px-copy");
    const probe = root.querySelector("#px-probe");
    const clear = root.querySelector("#px-clear");
    const stop = root.querySelector("#px-stop");
    const start = root.querySelector("#px-start");
    const clearGrant = root.querySelector("#px-clear-grant");

    if (start) {
      start.onclick = async () => {
        try {
          const cfg = readForm(root, pid);
          LAST[pid] = { ...(LAST[pid] || {}), ...cfg };
          const info = await NM.inv("llm_proxy_start", {
            args: {
              providerId: pid,
              displayName: cfg.displayName,
              model: cfg.model,
              listen: cfg.listen,
              localToken: cfg.localToken || "nm-local-token",
              grantB64: cfg.grantB64 || "",
              timeoutSecs: cfg.timeoutSecs || 120,
            },
          });
          remember(info);
          if (window.toast) toast("代理已启动 " + (info.listen || ""));
          startPoll(pid);
          await refreshDetailBody(pid);
          renderList();
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
    if (stop) {
      stop.onclick = async () => {
        try {
          const cfg = readForm(root, pid);
          LAST[pid] = { ...(LAST[pid] || {}), ...cfg };
          await NM.inv("llm_proxy_stop", { providerId: pid });
          if (window.toast) toast("代理已停止");
          // 保留 tab，切到停机态，可再启动
          await refreshDetailBody(pid);
          renderList();
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
    if (save) {
      save.onclick = async () => {
        try {
          const cfg = readForm(root, pid);
          const args = {
            providerId: pid,
            localToken: cfg.localToken,
            timeoutSecs: cfg.timeoutSecs,
            model: cfg.model,
            displayName: cfg.displayName,
          };
          const grantEl = root.querySelector("#px-grant");
          if (grantEl && grantEl.value.trim()) {
            args.grantB64 = grantEl.value.trim();
            LAST[pid] = { ...(LAST[pid] || {}), grantB64: args.grantB64 };
          }
          await NM.inv("llm_proxy_update", { args });
          if (grantEl) grantEl.value = "";
          if (window.toast) toast("配置已保存（不停端口）");
          refreshDetailBody(pid);
          renderList();
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
    if (clearGrant) {
      clearGrant.onclick = async () => {
        try {
          await NM.inv("llm_proxy_update", { args: { providerId: pid, grantB64: "" } });
          if (LAST[pid]) LAST[pid].grantB64 = "";
          if (window.toast) toast("已清除 Grant");
          refreshDetailBody(pid);
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
    if (copy) {
      copy.onclick = async () => {
        const env = root.querySelector("#px-env");
        try {
          await navigator.clipboard.writeText(env ? env.textContent : "");
          if (window.toast) toast("已复制环境变量");
        } catch (_) {
          if (window.toast) toast("复制失败");
        }
      };
    }
    if (probe) {
      probe.onclick = async () => {
        const out = root.querySelector("#px-probe-out");
        if (out) {
          out.textContent = "探测中…（最长约 30s）";
          out.style.color = "var(--muted)";
        }
        try {
          const r = await NM.inv("llm_proxy_probe", { providerId: pid });
          if (out) {
            out.textContent = r.ok
              ? `探测成功 · ${r.latencyMs}ms · ${r.bytes || 0} bytes`
              : `探测失败 · ${r.latencyMs}ms · ${r.error || ""}`;
            out.style.color = r.ok ? "var(--ok,#1fb182)" : "var(--danger,#d1495b)";
          }
          refreshDetailBody(pid);
        } catch (e) {
          if (out) {
            out.textContent = String(e);
            out.style.color = "var(--danger,#d1495b)";
          }
        }
      };
    }
    if (clear) {
      clear.onclick = async () => {
        try {
          await NM.inv("llm_proxy_clear_stats", { providerId: pid });
          if (window.toast) toast("统计已清空");
          refreshDetailBody(pid);
          renderList();
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
  }

  function paintDetail(root, d) {
    ensureCss();
    remember(d);
    const last = LAST[d.providerId] || {};
    const env = d.running
      ? "OPENAI_BASE_URL=" + (d.baseUrl || "") + "\nOPENAI_API_KEY=" + (d.localToken || "nm-local-token")
      : "(启动后显示)";
    const title = d.displayName || last.displayName || d.model || "未命名";
    root.className = "px-page";
    root.innerHTML = `
      <div class="conv-head">
        <b>代理 · ${esc(title)}</b>
        <span id="px-head-badge" style="margin-left:8px">${healthBadge(d)}</span>
        <div class="px-head-acts">
          <button class="ns-btn ns-primary" id="px-start" style="${d.running ? "display:none" : ""}">启动</button>
          <button class="ns-btn ns-danger" id="px-stop" style="${d.running ? "" : "display:none"}">停止</button>
        </div>
      </div>
      <div class="detail-scroll">
        <div class="detail" style="padding:12px 16px 32px">
          <div id="px-mon">${monHtml(d)}</div>

          <div style="font-weight:600;margin:18px 0 8px">管理</div>
          <div class="detail-rows" id="px-mgmt">
            <div class="detail-row"><span class="k">显示名</span><span class="v"><input class="ns-inp" id="px-name" style="width:100%" value="${esc(d.displayName || last.displayName || "")}" /></span></div>
            <div class="detail-row"><span class="k">监听</span><span class="v"><input class="ns-inp" id="px-listen" style="width:100%" value="${esc(d.listen || last.listen || "")}" placeholder="空=自动 18081+" ${d.running ? "disabled" : ""} /></span></div>
            <div class="detail-row"><span class="k">Token</span><span class="v"><input class="ns-inp" id="px-token" style="width:100%" value="${esc(d.localToken || last.localToken || "nm-local-token")}" /></span></div>
            <div class="detail-row"><span class="k">超时(秒)</span><span class="v"><input class="ns-inp" id="px-timeout" type="number" min="30" style="width:100%" value="${d.timeoutSecs || last.timeoutSecs || 120}" /></span></div>
            <div class="detail-row"><span class="k">模型名</span><span class="v"><input class="ns-inp" id="px-model" style="width:100%" value="${esc(d.model || last.model || "")}" placeholder="请求未带 model 时回填" /></span></div>
            <div class="detail-row"><span class="k">新 Grant</span><span class="v" style="display:flex;gap:6px"><input class="ns-inp" id="px-grant" style="flex:1" placeholder="粘贴 base64；启动或保存时生效" value="" /><button class="ns-btn" id="px-clear-grant" type="button" ${d.running ? "" : "disabled"}>清除</button></span></div>
            <div class="detail-row"><span class="k">Provider</span><span class="v mono" style="font-size:11px">${esc(d.providerId || "")}</span></div>
            <div class="detail-row"><span class="k">智能体</span><span class="v mono" style="font-size:11px;word-break:break-all" id="px-env">${esc(env)}</span></div>
          </div>
          <div class="detail-actions" style="padding:10px 0;display:flex;flex-wrap:wrap;gap:8px">
            <button class="ns-btn ns-primary" id="px-start-2" style="${d.running ? "display:none" : ""}">启动代理</button>
            <button class="ns-btn ns-primary" id="px-save" style="${d.running ? "" : "display:none"}">保存配置</button>
            <button class="ns-btn" id="px-copy">复制环境变量</button>
            <button class="ns-btn" id="px-probe" style="${d.running ? "" : "display:none"}">连通性探测</button>
            <button class="ns-btn" id="px-clear" style="${d.running ? "" : "display:none"}">清空统计</button>
            <button class="ns-btn ns-danger" id="px-stop-2" style="${d.running ? "" : "display:none"}">停止代理</button>
          </div>
          <div id="px-probe-out" style="font-size:12px;color:var(--muted);min-height:18px"></div>
        </div>
      </div>`;

    // 底部启停与顶栏同步
    const s2 = root.querySelector("#px-start-2");
    const t2 = root.querySelector("#px-stop-2");
    if (s2) s2.onclick = () => root.querySelector("#px-start") && root.querySelector("#px-start").click();
    if (t2) t2.onclick = () => root.querySelector("#px-stop") && root.querySelector("#px-stop").click();
    bindMgmt(root, d.providerId);
    setRunningUi(root, !!d.running);
  }

  function paintStopped(root, providerId) {
    const last = LAST[providerId] || { providerId, localToken: "nm-local-token", timeoutSecs: 120 };
    paintDetail(root, {
      providerId,
      displayName: last.displayName || "",
      model: last.model || "",
      listen: last.listen || "",
      localToken: last.localToken || "nm-local-token",
      baseUrl: "",
      running: false,
      timeoutSecs: last.timeoutSecs || 120,
      hasGrant: !!(last.grantB64 || last.hasGrant),
      requests: 0,
      success: 0,
      errors: 0,
      authFails: 0,
      inFlight: 0,
      lastLatencyMs: 0,
      avgLatencyMs: 0,
      uptimeSecs: 0,
      bytesIn: 0,
      bytesOut: 0,
      lastError: "",
      lastErrorAtMs: 0,
      lastOkAtMs: 0,
      recent: [],
    });
  }

  async function refreshDetailBody(providerId) {
    const root = document.getElementById("px-detail-" + providerId);
    if (!root || !window.NM) return;
    try {
      const st = await NM.inv("llm_proxy_status", { providerId });
      if (!st || !st.running) {
        // 停机：整页切到可启动态（保留表单输入）
        const focused = document.activeElement;
        const editing = focused && root.contains(focused) && /^(INPUT|TEXTAREA|SELECT)$/.test(focused.tagName);
        if (!root.querySelector("#px-mon") || root.dataset.running === "1") {
          if (!editing) paintStopped(root, providerId);
          else {
            const mon = root.querySelector("#px-mon");
            if (mon) mon.innerHTML = monHtml({ running: false });
            setRunningUi(root, false);
          }
        } else {
          const mon = root.querySelector("#px-mon");
          if (mon) mon.innerHTML = monHtml({ running: false });
          setRunningUi(root, false);
        }
        root.dataset.running = "0";
        return;
      }
      const d = await NM.inv("llm_proxy_detail", { providerId });
      remember(d);
      root.dataset.running = "1";
      const mon = root.querySelector("#px-mon");
      if (mon && root.dataset.built === "1") {
        mon.innerHTML = monHtml(d);
        const env = root.querySelector("#px-env");
        if (env) {
          env.textContent =
            "OPENAI_BASE_URL=" +
            (d.baseUrl || "") +
            "\nOPENAI_API_KEY=" +
            (d.localToken || "nm-local-token");
        }
        const head = root.querySelector("#px-head-badge");
        if (head) head.innerHTML = healthBadge(d);
        setRunningUi(root, true);
        // 运行中时同步 listen 显示（禁用）
        const listen = root.querySelector("#px-listen");
        if (listen && document.activeElement !== listen) listen.value = d.listen || "";
      } else {
        paintDetail(root, d);
        root.dataset.built = "1";
      }
    } catch (_) {
      paintStopped(root, providerId);
      root.dataset.built = "1";
      root.dataset.running = "0";
    }
  }

  function openProxyDetail(providerId, seed) {
    if (!providerId) return;
    if (seed) {
      LAST[providerId] = { ...(LAST[providerId] || {}), ...seed, providerId };
    }
    const title =
      (CACHE.find((p) => p.providerId === providerId) || {}).displayName ||
      (LAST[providerId] && LAST[providerId].displayName) ||
      "代理 " + providerId.slice(0, 8);
    if (window.Tabs) {
      Tabs.open({
        key: "px:" + providerId,
        kind: "proxy",
        title,
        ico: "proxy",
        render: () => {
          const conv = document.getElementById("conv");
          if (!conv) return;
          ensureCss();
          conv.innerHTML = `<div class="px-page" id="px-detail-${esc(providerId)}" data-built="0" data-running=""></div>`;
          startPoll(providerId);
          refreshDetailBody(providerId);
        },
      });
    }
  }

  window.renderLlmProxies = renderList;
  window.openLlmProxyDetail = openProxyDetail;

  document.addEventListener("DOMContentLoaded", () => {
    const btn = document.getElementById("px-refresh");
    if (btn) btn.onclick = () => renderList();
    const search = document.getElementById("px-search");
    if (search) search.addEventListener("input", () => renderList());
  });

  if (window.NM && NM.on) {
    NM.on("core://event", (ev) => {
      if (!ev || ev.type !== "llm_proxy") return;
      renderList();
      if (ev.providerId && (activeProvider === ev.providerId || (window.Tabs && Tabs.has && Tabs.has("px:" + ev.providerId)))) {
        refreshDetailBody(ev.providerId);
      }
    });
  }
})();

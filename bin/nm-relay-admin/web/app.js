(() => {
  const $ = (id) => document.getElementById(id);
  let peersCache = [];

  async function api(path, opts = {}) {
    const res = await fetch(path, {
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", ...(opts.headers || {}) },
      ...opts,
    });
    if (res.status === 401) throw Object.assign(new Error("unauthorized"), { status: 401 });
    const ct = res.headers.get("content-type") || "";
    const body = ct.includes("json") ? await res.json() : await res.text();
    if (!res.ok) {
      const err = (body && body.error) || res.statusText;
      throw new Error(err);
    }
    return body;
  }

  function fmtBytes(n) {
    n = Number(n) || 0;
    if (n < 1024) return n.toFixed(0) + " B";
    if (n < 1048576) return (n / 1024).toFixed(1) + " KB";
    if (n < 1073741824) return (n / 1048576).toFixed(2) + " MB";
    return (n / 1073741824).toFixed(2) + " GB";
  }
  function fmtRate(n) {
    n = Number(n) || 0;
    return fmtBytes(n) + "/s";
  }
  function ago(sec) {
    if (!sec) return "—";
    const d = Math.max(0, Math.floor(Date.now() / 1000 - sec));
    if (d < 60) return d + "s 前";
    if (d < 3600) return Math.floor(d / 60) + "m 前";
    if (d < 86400) return Math.floor(d / 3600) + "h 前";
    return Math.floor(d / 86400) + "d 前";
  }
  function shortId(id) {
    id = id || "";
    return id.length <= 20 ? id : id.slice(0, 10) + "…" + id.slice(-8);
  }
  function card(label, value) {
    return `<div class="stat"><span>${label}</span><b>${value}</b></div>`;
  }

  async function boot() {
    try {
      const s = await api("/api/session");
      if (s.authed) {
        showMain();
        if (s.must_change) $("chg-dlg").showModal();
        await refresh();
      } else {
        showLogin();
      }
    } catch (_) {
      showLogin();
    }
  }

  function showLogin() {
    $("login-view").classList.remove("hidden");
    $("main-view").classList.add("hidden");
  }
  function showMain() {
    $("login-view").classList.add("hidden");
    $("main-view").classList.remove("hidden");
  }

  $("btn-login").onclick = async () => {
    $("login-err").textContent = "";
    try {
      const r = await api("/api/login", {
        method: "POST",
        body: JSON.stringify({ username: $("user").value, password: $("pass").value }),
      });
      showMain();
      if (r.must_change) $("chg-dlg").showModal();
      await refresh();
    } catch (e) {
      $("login-err").textContent = e.message || "登录失败";
    }
  };
  $("pass").addEventListener("keydown", (e) => {
    if (e.key === "Enter") $("btn-login").click();
  });

  $("btn-logout").onclick = async () => {
    await api("/api/logout", { method: "POST", body: "{}" });
    showLogin();
  };
  $("btn-refresh").onclick = () => refresh();

  $("chg-ok").onclick = async (e) => {
    e.preventDefault();
    $("chg-err").textContent = "";
    try {
      await api("/api/change-password", {
        method: "POST",
        body: JSON.stringify({ old: $("chg-old").value, new: $("chg-new").value }),
      });
      $("chg-dlg").close();
    } catch (err) {
      $("chg-err").textContent = err.message;
    }
  };

  document.querySelectorAll(".tabs button").forEach((btn) => {
    btn.onclick = () => {
      document.querySelectorAll(".tabs button").forEach((b) => b.classList.remove("on"));
      btn.classList.add("on");
      ["overview", "peers", "blacklist", "pkarr"].forEach((t) => {
        $("tab-" + t).classList.toggle("hidden", t !== btn.dataset.tab);
      });
    };
  });

  async function refresh() {
    const ov = await api("/api/overview");
    const m = ov.metrics || {};
    const r = m.relay || {};
    const rates = m.rates || {};
    $("health-line").textContent =
      `relay ${m.relay_up ? "●" : "○"} · dns/pkarr ${m.dns_up ? "●" : "○"} · 观测节点 ${ov.peer_count} · 黑名单 ${ov.blacklist_count}`;
    $("overview-cards").innerHTML = [
      card("Relay", m.relay_up ? "在线" : "不可达"),
      card("DNS/Pkarr", m.dns_up ? "在线" : "不可达"),
      card("发送流量", fmtBytes(r.relayserver_bytes_sent_total)),
      card("接收流量", fmtBytes(r.relayserver_bytes_recv_total)),
      card("发送速率", fmtRate(rates.bytes_tx_per_s)),
      card("接收速率", fmtRate(rates.bytes_rx_per_s)),
      card("累计接入", (r.relayserver_accepts_total || 0).toLocaleString()),
      card("当前估算在线", Math.max(0, (r.relayserver_accepts_total || 0) - (r.relayserver_disconnects_total || 0)).toFixed(0)),
      card("丢包(send)", (r.relayserver_send_packets_dropped_total || 0).toLocaleString()),
      card("观测节点", ov.peer_count),
    ].join("");
    $("overview-raw").textContent = JSON.stringify({ relay: r, dns: m.dns, rates }, null, 2);

    const pr = await api("/api/peers");
    peersCache = pr.peers || [];
    renderPeers();

    const bl = await api("/api/blacklist");
    $("bl-list").innerHTML = (bl.blacklist || [])
      .map(
        (id) =>
          `<li><span class="mono">${escapeHtml(id)}</span><button class="ghost" data-unban="${escapeHtml(id)}">解除</button></li>`
      )
      .join("") || `<li class="muted">暂无拉黑</li>`;
    $("bl-list").querySelectorAll("[data-unban]").forEach((b) => {
      b.onclick = async () => {
        await api("/api/unban", { method: "POST", body: JSON.stringify({ id: b.dataset.unban }) });
        await refresh();
      };
    });

    const pk = await api("/api/pkarr");
    $("pkarr-cards").innerHTML = [
      card("Pkarr 服务", pk.up ? "在线" : "不可达"),
      card("HTTP 请求", (pk.http_requests || 0).toLocaleString()),
      card("成功 / 错误", `${pk.http_success || 0} / ${pk.http_error || 0}`),
      card("发布更新", (pk.pkarr_publish_update || 0).toLocaleString()),
      card("入库 / 更新", `${pk.store_inserted || 0} / ${pk.store_updated || 0}`),
      card("过期删除", (pk.store_expired || 0).toLocaleString()),
    ].join("");
  }

  function renderPeers() {
    const q = ($("peer-filter").value || "").trim().toLowerCase();
    const rows = peersCache.filter((p) => !q || (p.id || "").toLowerCase().includes(q));
    $("peer-count").textContent = `${rows.length} / ${peersCache.length}`;
    $("peers-body").innerHTML = rows
      .map((p) => {
        const st = p.status || "stale";
        return `<tr>
          <td class="code" title="${escapeHtml(p.id)}">${escapeHtml(shortId(p.id))}</td>
          <td><span class="badge ${st}">${st}</span></td>
          <td>${p.connects || 0}</td>
          <td>${ago(p.last_seen)}</td>
          <td>${
            p.banned
              ? `<button class="ok" data-unban="${escapeHtml(p.id)}">解禁</button>`
              : `<button class="danger" data-ban="${escapeHtml(p.id)}">拉黑</button>`
          }</td>
        </tr>`;
      })
      .join("");
    $("peers-body").querySelectorAll("[data-ban]").forEach((b) => {
      b.onclick = async () => {
        await api("/api/ban", { method: "POST", body: JSON.stringify({ id: b.dataset.ban }) });
        await refresh();
      };
    });
    $("peers-body").querySelectorAll("[data-unban]").forEach((b) => {
      b.onclick = async () => {
        await api("/api/unban", { method: "POST", body: JSON.stringify({ id: b.dataset.unban }) });
        await refresh();
      };
    });
  }

  $("peer-filter").oninput = renderPeers;
  $("btn-ban").onclick = async () => {
    const id = $("ban-id").value.trim();
    if (!id) return;
    await api("/api/ban", {
      method: "POST",
      body: JSON.stringify({ id, note: $("ban-note").value.trim() }),
    });
    $("ban-id").value = "";
    await refresh();
  };
  $("btn-pkarr").onclick = async () => {
    const key = $("pkarr-key").value.trim();
    if (!key) return;
    try {
      const r = await api("/api/pkarr/" + encodeURIComponent(key));
      $("pkarr-result").textContent = JSON.stringify(r, null, 2);
    } catch (e) {
      $("pkarr-result").textContent = String(e.message || e);
    }
  };

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  boot();
  setInterval(() => {
    if (!$("main-view").classList.contains("hidden")) refresh().catch(() => {});
  }, 10000);
})();

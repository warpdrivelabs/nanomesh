import { api } from "./api.js";
import { PeerGraph } from "./peer_graph.js";
import { bindCopy, copyIconBtn, esc, fmtBytes, fmtDur, fmtLoss, fmtRate, fmtRtt, fmtTime, fmtTimeSec, metric, pathKindLabel, sampleRate, shortId, sinceMs } from "./helpers.js";

let TRAF = []; // 流量历史采样
let USER_PAGE = 1;
const USER_PAGE_SIZE = 20;

export function renderPanel(tab, host, d, refresh) {
  if (tab === "overview") {
    host.innerHTML = `<div class="grid">
      ${metric("在线连接", d.online)}
      ${metric("联邦对等", d.peers)}
      ${metric("实体 / 用户", d.entities)}
      ${metric("群组", d.groups)}
      ${metric("黑名单", d.banned)}
      ${metric("运行时长", fmtDur(d.uptime_s))}
      ${metric("内存 RSS", fmtBytes(d.rss_bytes))}
      ${metric("CPU", (Number(d.cpu_pct) || 0).toFixed(1) + "%")}
      ${metric("存储占用", fmtBytes(d.db_bytes))}
      ${metric("累计接收", fmtBytes(d.traffic?.bytes_in))}
      ${metric("累计发送", fmtBytes(d.traffic?.bytes_out))}
      ${metric("节点", `<code class="mono">${shortId(d.id)}</code>`)}
    </div>
    <div class="card restart-card">
      <div>
        <b>nmd 服务</b>
        <div class="muted">重启本机节点。进行中的连接会短暂断开。</div>
        <div class="restart-msg" hidden></div>
      </div>
      <button class="btn btn--danger" type="button" id="nmd-restart">重启 nmd</button>
    </div>`;
    const btn = host.querySelector("#nmd-restart");
    const msg = host.querySelector(".restart-msg");
    btn.onclick = async () => {
      if (!confirm("重启 nmd？当前连接会短暂断开。")) return;
      btn.disabled = true;
      btn.textContent = "正在重启…";
      msg.hidden = true;
      try {
        const r = await api("/api/nmd/restart", { method: "POST" });
        msg.className = "restart-msg " + (r.back ? "ok" : "err");
        msg.textContent = r.back ? "nmd 已重启并恢复。" : "已发出重启，节点尚未恢复，请稍后刷新。";
        msg.hidden = false;
        if (r.back) setTimeout(refresh, 800);
      } catch (e) {
        msg.className = "restart-msg err";
        msg.textContent = e.message;
        msg.hidden = false;
      } finally {
        btn.disabled = false;
        btn.textContent = "重启 nmd";
      }
    };
  } else if (tab === "connections") {
    const rows = (d.connections || []).map((c) => {
      const rate = sampleRate(c.id, c.bytes_rx, c.bytes_tx);
      const loss = fmtLoss(c.lost_packets, c.datagrams_tx);
      return `<tr>
      <td class="nowrap"><span class="copyline"><code class="mono">${shortId(c.id)}</code>${copyIconBtn(`data-copy="${esc(c.id)}"`)}</span></td>
      <td class="stack"><span>${fmtTime(c.since_unix_ms)}</span><span class="muted">${sinceMs(c.since_unix_ms)}</span></td>
      <td class="stack"><span>↓ ${fmtBytes(c.bytes_rx)}</span><span>↑ ${fmtBytes(c.bytes_tx)}</span>
        <span class="muted">${fmtRate(rate.down)} / ${fmtRate(rate.up)}</span></td>
      <td class="stack"><span>${fmtRtt(c.rtt_ms)}</span><span class="muted">丢包 ${loss}</span>
        <span class="muted">${pathKindLabel(c.path_kind)}${c.mtu ? ` · MTU ${c.mtu}` : ""}</span></td>
      <td class="mono muted small" title="${esc(c.remote_addr || "")}">${esc(c.remote_addr) || "—"}</td>
      <td><span class="badge">${esc(c.alpn)}</span></td>
      <td class="right"><button class="btn btn--sm btn--danger" data-kick="${c.id}">踢下线</button></td>
    </tr>`;
    }).join("");
    host.innerHTML = `<div class="card"><table class="tbl">
      <thead><tr><th>公钥</th><th>上线 / 在线</th><th>流量 / 带宽</th><th>链路质量</th><th>远端地址</th><th>协议</th><th></th></tr></thead>
      <tbody>${rows || '<tr><td colspan="7" class="muted center">暂无活动连接</td></tr>'}</tbody></table></div>`;
    host.querySelectorAll("[data-copy]").forEach((b) => bindCopy(b, b.dataset.copy));
    host.querySelectorAll("[data-kick]").forEach((b) => (b.onclick = async () => {
      b.disabled = true;
      try { await api("/api/kick", { method: "POST", body: { id: b.dataset.kick } }); refresh(); } catch (e) { alert(e.message); b.disabled = false; }
    }));
  } else if (tab === "users") {
    const all = d.users || [];
    const pages = Math.max(1, Math.ceil(all.length / USER_PAGE_SIZE));
    if (USER_PAGE > pages) USER_PAGE = pages;
    if (USER_PAGE < 1) USER_PAGE = 1;
    const start = (USER_PAGE - 1) * USER_PAGE_SIZE;
    const rows = all.slice(start, start + USER_PAGE_SIZE).map((u) => `<tr>
      <td><b>${esc(u.name) || "(无名)"}</b></td>
      <td><span class="badge">${esc(u.kind)}</span></td>
      <td class="nowrap"><span class="copyline"><code class="mono">${shortId(u.id)}</code>${copyIconBtn(`data-copy="${esc(u.id)}"`)}</span></td>
      <td>${u.banned ? '<span class="badge badge--danger">已封禁</span>' : '<span class="badge badge--ok">正常</span>'}</td>
      <td class="right">${u.banned
        ? `<button class="btn btn--sm" data-unban="${u.id}">解封</button>`
        : `<button class="btn btn--sm btn--danger" data-ban="${u.id}">拉黑</button>`}</td>
    </tr>`).join("");
    host.innerHTML = `<div class="card"><table class="tbl">
      <thead><tr><th>名称</th><th>类型</th><th>公钥</th><th>状态</th><th></th></tr></thead>
      <tbody>${rows || '<tr><td colspan="5" class="muted center">暂无实体，点连接后刷新</td></tr>'}</tbody></table>
      <div class="pager">
        <span class="muted">共 ${all.length} 条 · 每页 ${USER_PAGE_SIZE} 条 · 第 ${USER_PAGE} / ${pages} 页</span>
        <span>
          <button class="btn btn--sm" type="button" data-page="prev" ${USER_PAGE <= 1 ? "disabled" : ""}>上一页</button>
          <button class="btn btn--sm" type="button" data-page="next" ${USER_PAGE >= pages ? "disabled" : ""}>下一页</button>
        </span>
      </div></div>`;
    const act = (sel, url) => host.querySelectorAll(sel).forEach((b) => (b.onclick = async () => {
      b.disabled = true;
      try { await api(url, { method: "POST", body: { id: b.dataset[url.includes("unban") ? "unban" : "ban"] } }); refresh(); } catch (e) { alert(e.message); b.disabled = false; }
    }));
    act("[data-ban]", "/api/ban");
    act("[data-unban]", "/api/unban");
    host.querySelectorAll("[data-copy]").forEach((b) => bindCopy(b, b.dataset.copy));
    const go = (delta) => { const n = USER_PAGE + delta; if (n < 1 || n > pages) return; USER_PAGE = n; refresh(); };
    host.querySelector("[data-page=prev]").onclick = () => go(-1);
    host.querySelector("[data-page=next]").onclick = () => go(1);
  } else if (tab === "storage") {
    host.innerHTML = `<div class="grid">
      ${metric("实体", d.entities)}
      ${metric("群组", d.groups)}
      ${metric("数据库大小", fmtBytes(d.db_bytes))}
    </div>
    <div class="card"><div class="kv"><span class="muted">数据库路径</span><code class="mono">${esc(d.db_path)}</code></div></div>`;
  } else if (tab === "traffic") {
    TRAF.push({ t: Date.now(), i: Number(d.bytes_in) || 0, o: Number(d.bytes_out) || 0 });
    if (TRAF.length > 80) TRAF.shift();
    host.innerHTML = `<div class="grid">
      ${metric("累计接收", fmtBytes(d.bytes_in))}
      ${metric("累计发送", fmtBytes(d.bytes_out))}
      ${metric("接收条数", d.grams_in)}
      ${metric("发送条数", d.grams_out)}
    </div>
    <div class="card"><div class="card__title">吞吐（字节 / 秒）
      <span class="legend"><i class="dot dot--in"></i>接收 <i class="dot dot--out"></i>发送</span></div>
      <canvas class="chart"></canvas></div>`;
    drawChart(host.querySelector(".chart"));
  } else if (tab === "system") {
    const relays = Array.isArray(d.relays) ? d.relays : [];
    const ips = Array.isArray(d.ip_addrs) ? d.ip_addrs : [];
    const roamN = Number(d.roam_changes) || 0;
    host.innerHTML = `<div class="grid">
      ${metric("进程 PID", d.pid)}
      ${metric("内存 RSS", fmtBytes(d.rss_bytes))}
      ${metric("CPU 占用", (Number(d.cpu_pct) || 0).toFixed(1) + "%")}
      ${metric("运行时长", fmtDur(d.uptime_s))}
      ${metric("在线连接", d.online)}
      ${metric("换网次数", roamN)}
      ${metric("最近换网", roamN ? sinceMs(Number(d.roam_last_ms)) + " 前" : "—")}
    </div>
    <div class="card"><div class="card__title">网络地址（换网自愈可观测）</div>
      <p class="muted">本节点当前对外地址：换网后由发现服务自动重发布，对端按公钥 id 仍可寻址。</p>
      <div class="kv"><span>中继 relay</span><b>${relays.length ? relays.map(esc).join("<br>") : "—"}</b></div>
      <div class="kv"><span>直连 IP</span><b>${ips.length ? ips.map(esc).join("<br>") : "—"}</b></div></div>
    <div class="card"><p class="muted">CPU / 内存为进程实时监控。真正的 CPU / 内存<strong>硬配额</strong>属部署级（systemd / cgroup），此处仅展示用量，不做强制限制。</p></div>`;
  } else if (tab === "identity") {
    host.innerHTML = `
    <div class="idcards">
      <div class="card idcard">
        <div class="idcard__head"><span class="badge badge--node">Node ID · 节点公钥</span></div>
        <img class="qr" src="/api/qr?kind=node&t=${Date.now()}" alt="node id QR" />
        <div class="idcard__val copyline"><code class="mono wrap">${esc(d.node_id)}</code>${copyIconBtn('data-copy="node"')}</div>
      </div>
      <div class="card idcard">
        <div class="idcard__head"><span class="badge badge--addr">完整地址 · NM_NODE_ADDR</span></div>
        <img class="qr" src="/api/qr?kind=addr&t=${Date.now()}" alt="addr QR" />
        <div class="idcard__val copyline"><code class="mono wrap">${esc(d.addr)}</code>${copyIconBtn('data-copy="addr"')}</div>
      </div>
    </div>
    <p class="muted">二维码按类型着色（<b style="color:var(--accent)">靛蓝=Node ID</b> / <b style="color:var(--sky)">天蓝=完整地址</b>），载荷带类型前缀 <code>nmspace:node:</code> / <code>nmspace:addr:</code> 以区分类型。</p>`;
    const copyNode = host.querySelector('[data-copy="node"]');
    const copyAddr = host.querySelector('[data-copy="addr"]');
    if (copyNode) bindCopy(copyNode, d.node_id || "");
    if (copyAddr) bindCopy(copyAddr, d.addr || "");
  } else if (tab === "peers") {
    renderPeersPanel(host, d, refresh);
  }
}

/** 对等节点面板：拓扑图跨刷新保活，表格/指标增量更新。 */
function renderPeersPanel(host, d, refresh) {
  const peers = d.peers || [];
  const fed = d.federation || "nmspace";
  const selfId = d.self_id || "";
  const onlineN = d.online ?? peers.filter((p) => p.connected).length;
  const offlineN = d.offline ?? Math.max(0, peers.length - onlineN);
  const ago = (s) => {
    if (!s) return "";
    const dlt = Math.max(0, Math.floor(Date.now() / 1000) - s);
    if (dlt < 60) return dlt + "s前";
    if (dlt < 3600) return Math.floor(dlt / 60) + "分前";
    if (dlt < 86400) return Math.floor(dlt / 3600) + "时前";
    return Math.floor(dlt / 86400) + "天前";
  };
  const rows = peers.map((p, i) => {
    const disc = p.source === "discovered";
    const badge = disc
      ? `<span class="badge badge--discovered">自动发现</span>`
      : `<span class="badge badge--manual">手工</span>`;
    const connBadge = p.connected
      ? `<span class="badge badge--ok">在线</span>`
      : `<span class="badge badge--off">离线</span>`;
    const rate = p.connected ? sampleRate(p.id, p.bytes_rx, p.bytes_tx) : { down: null, up: null };
    const loss = p.connected ? fmtLoss(p.lost_packets, p.datagrams_tx) : "—";
    const seen = p.last_seen
      ? `<span title="${esc(fmtTimeSec(p.last_seen))}">${ago(p.last_seen)}</span>`
      : `<span class="muted">—</span>`;
    const upTime = p.connected
      ? `<span class="stack"><span>${fmtTime(p.since_unix_ms)}</span><span class="muted">${sinceMs(p.since_unix_ms)}</span></span>`
      : `<span class="muted">—</span>`;
    const link = p.connected
      ? `<span class="stack">
          <span>${fmtRtt(p.rtt_ms)} · 丢包 ${loss}</span>
          <span class="muted">${pathKindLabel(p.path_kind)}${p.mtu ? ` · MTU ${p.mtu}` : ""}${p.cwnd != null ? ` · cwnd ${fmtBytes(p.cwnd)}` : ""}</span>
          ${p.remote_addr ? `<span class="muted mono small" title="${esc(p.remote_addr)}">${esc(p.remote_addr)}</span>` : ""}
        </span>`
      : `<span class="muted">无会话</span>`;
    const traf = p.connected
      ? `<span class="stack">
          <span>↓ ${fmtBytes(p.bytes_rx)} · ↑ ${fmtBytes(p.bytes_tx)}</span>
          <span class="muted">${fmtRate(rate.down)} / ${fmtRate(rate.up)}</span>
        </span>`
      : `<span class="muted">—</span>`;
    const metaBits = [p.address, p.email, p.mobile, p.gps].filter(Boolean).map(esc);
    const nameCell = `<td>
        <b>${esc(p.name) || '<span class="muted">—</span>'}</b>
        ${metaBits.length ? `<div class="muted small peer-meta">${metaBits.join(" · ")}</div>` : ""}
      </td>`;
    const actions = disc
      ? `<button class="btn btn--sm btn--danger" data-ban="${p.id}" title="永久排除：断开连接且不再被自动发现学回">封禁</button>
         <button class="btn btn--sm" data-del="${p.id}" title="瞬时移除；对方仍在广播会被再次发现">移除</button>`
      : `<button class="btn btn--sm" data-edit="${i}">编辑</button>
         <button class="btn btn--sm btn--danger" data-del="${p.id}">删除</button>`;
    return `<tr class="${p.connected ? "peer-online" : "peer-offline"}">
    ${nameCell}
    <td class="nowrap">${connBadge}</td>
    <td class="nowrap">${badge}</td>
    <td class="nowrap"><span class="badge badge--fed">${esc(p.federation || fed)}</span></td>
    <td class="nowrap"><span class="copyline"><code class="mono">${shortId(p.id)}</code>${copyIconBtn(`data-copy="${esc(p.id)}"`)}</span></td>
    <td class="nowrap">${upTime}</td>
    <td class="nowrap muted">${seen}</td>
    <td>${link}</td>
    <td>${traf}</td>
    <td class="right nowrap">${actions}</td>
  </tr>`;
  }).join("");

  const metricsHtml = `
    ${metric("对等总数", d.total ?? peers.length)}
    ${metric("在线", onlineN)}
    ${metric("离线", offlineN)}
    ${metric("本节点联邦", `<span class="badge badge--fed">${esc(fed)}</span>`)}`;

  const first = !host.querySelector(".peer-panel");
  if (first) {
    host.innerHTML = `
    <div class="peer-panel">
      <div class="grid peer-metrics">${metricsHtml}</div>
      <div class="card peer-map">
        <div class="card__title peer-map__title">节点连接传输图
          <span class="peer-map__legend">
            <span class="lg"><i class="lg--self"></i>本节点</span>
            <span class="lg"><i class="lg--on"></i>在线</span>
            <span class="lg"><i class="lg--off"></i>离线</span>
            <span class="lg"><i class="lg--relay"></i>中继路径</span>
            <span class="muted">线宽/粒子 ≈ 带宽 · 色调 ≈ RTT</span>
          </span>
        </div>
        <div class="peer-map__canvas-wrap">
          <canvas class="peer-map__canvas"></canvas>
          <div class="peer-map__tip"></div>
        </div>
      </div>
      <div class="card">
        <div class="card__title" id="pf-title">添加对等节点（手工/种子 · 运行时生效 · 无需重启 · 连接按 Node ID 发现）</div>
        <p class="muted" style="margin:-4px 0 12px">手工添加的即「种子」并作为 gossip 引导，将加入此联邦；其余成员经成员频道<b>自动发现</b>。连通性来自当前 QUIC 会话；链路 RTT / 丢包 / MTU / 带宽为实时采样。</p>
        <form class="peerform">
          <label class="field"><span>名称</span><input name="name" placeholder="如：北京机房 nmd"></label>
          <label class="field"><span>Node ID（64 位 hex，必填）</span><input name="id" spellcheck="false" placeholder="对端 node 公钥"></label>
          <label class="field"><span>物理地址（街道门牌等）</span><input name="address" placeholder="如：北京市海淀区 XX 路 8 号"></label>
          <div class="peergrid">
            <label class="field"><span>Email</span><input name="email" placeholder="ops@example.com"></label>
            <label class="field"><span>手机</span><input name="mobile" placeholder="+86 …"></label>
            <label class="field"><span>GPS</span><input name="gps" placeholder="39.90,116.40"></label>
          </div>
          <div class="err" hidden></div>
          <div class="peerbtns"><button class="btn btn--primary" type="submit">保存对等</button>
            <button class="btn btn--ghost" type="reset">清空</button></div>
        </form>
      </div>
      <div class="card card--scroll"><table class="tbl tbl--peers">
        <thead><tr>
          <th>名称</th><th>连通</th><th>来源</th><th>联邦</th><th>Node ID</th>
          <th>上线 / 在线</th><th>最近存活</th><th>链路质量</th><th>流量 / 带宽</th><th></th>
        </tr></thead>
        <tbody class="peer-tbody">${rows || '<tr><td colspan="10" class="muted center">暂无对等节点</td></tr>'}</tbody></table></div>
    </div>`;
    const form = host.querySelector(".peerform"), err = host.querySelector(".err");
    const setf = (k, v) => { const el = form.querySelector(`[name="${k}"]`); if (el) el.value = v || ""; };
    form.addEventListener("submit", async (e) => {
      e.preventDefault(); err.hidden = true;
      const fd = new FormData(form); const g = (k) => (fd.get(k) || "").toString().trim();
      const body = { id: g("id"), name: g("name"), address: g("address"), email: g("email"), mobile: g("mobile"), gps: g("gps") };
      if (!/^[0-9a-fA-F]{64}$/.test(body.id)) { err.textContent = "请填写 64 位十六进制 Node ID"; err.hidden = false; return; }
      try { await api("/api/add-peer", { method: "POST", body }); form.reset(); host.querySelector("#pf-title").textContent = "添加对等节点（手工/种子 · 运行时生效 · 无需重启 · 连接按 Node ID 发现）"; refresh(); }
      catch (ex) { err.textContent = ex.message; err.hidden = false; }
    });
    host._peerBindActions = () => {
      host.querySelectorAll("[data-edit]").forEach((b) => (b.onclick = () => {
        const p = host._peers[+b.dataset.edit];
        if (!p) return;
        setf("name", p.name); setf("id", p.id); setf("address", p.address);
        setf("email", p.email); setf("mobile", p.mobile); setf("gps", p.gps);
        host.querySelector("#pf-title").textContent = "编辑对等节点（Node ID 相同即覆盖保存）";
        form.scrollIntoView({ behavior: "smooth", block: "start" });
      }));
      host.querySelectorAll("[data-del]").forEach((b) => (b.onclick = async () => {
        if (!confirm("删除该对等节点？")) return;
        b.disabled = true;
        try { await api("/api/remove-peer", { method: "POST", body: { id: b.dataset.del } }); refresh(); }
        catch (ex) { alert(ex.message); b.disabled = false; }
      }));
      host.querySelectorAll("[data-copy]").forEach((b) => bindCopy(b, b.dataset.copy));
      host.querySelectorAll("[data-ban]").forEach((b) => (b.onclick = async () => {
        if (!confirm("封禁该节点？将断开其连接，并不再被自动发现学回（永久排除）。")) return;
        b.disabled = true;
        try {
          await api("/api/ban", { method: "POST", body: { id: b.dataset.ban } });
          await api("/api/remove-peer", { method: "POST", body: { id: b.dataset.ban } });
          refresh();
        } catch (ex) { alert(ex.message); b.disabled = false; }
      }));
    };
    const wrap = host.querySelector(".peer-map__canvas-wrap");
    host._peerGraph = new PeerGraph(wrap);
  } else {
    host.querySelector(".peer-metrics").innerHTML = metricsHtml;
    host.querySelector(".peer-tbody").innerHTML = rows || '<tr><td colspan="10" class="muted center">暂无对等节点</td></tr>';
  }
  host._peers = peers;
  host._peerBindActions();
  if (host._peerGraph) host._peerGraph.setData({ selfId, federation: fed, peers });
}


export function renderPassword(host) {
  host.innerHTML = `
  <div class="card auth-inline">
    <div class="card__title">修改管理员密码</div>
    <form class="pwform">
      <label class="field"><span>原密码</span><input name="o" type="password" autocomplete="current-password"></label>
      <label class="field"><span>新密码（至少 6 位）</span><input name="n" type="password" autocomplete="new-password"></label>
      <label class="field"><span>确认新密码</span><input name="c" type="password" autocomplete="new-password"></label>
      <div class="err" hidden></div>
      <div class="okmsg" hidden>✓ 密码已修改</div>
      <button class="btn btn--primary" type="submit">保存</button>
    </form>
  </div>`;
  const form = host.querySelector(".pwform"), err = host.querySelector(".err"), ok = host.querySelector(".okmsg");
  form.addEventListener("submit", async (e) => {
    e.preventDefault(); err.hidden = true; ok.hidden = true;
    if (form.n.value !== form.c.value) { err.textContent = "两次新密码不一致"; err.hidden = false; return; }
    try {
      await api("/api/change-password", { method: "POST", body: { oldPassword: form.o.value, newPassword: form.n.value } });
      form.reset(); ok.hidden = false;
    } catch (ex) { err.textContent = ex.message; err.hidden = false; }
  });
}

/* ---------------- 域名管理面板 ---------------- */
export function renderNames(host, dom, names, pending, refresh, devices) {
  const domains = (dom && dom.domains) || [];
  const list = (names && names.names) || [];
  // 按账号公钥分组设备（devices_snapshot 返回全部设备；命名登记的 pubkey 即账号公钥）。
  const devByAccount = new Map();
  ((devices && devices.devices) || []).forEach((d) => {
    const k = (d.account || "").toLowerCase();
    if (!k) return;
    if (!devByAccount.has(k)) devByAccount.set(k, []);
    devByAccount.get(k).push(d);
  });
  const waiting = ((pending && pending.items) || []).filter((a) => a.status === "pending");
  const pendingErr = pending && pending.error;
  const approved = ((pending && pending.items) || []).filter((a) => a.status === "approved");
  const approvedByDomain = new Map(approved.map((a) => [String(a.domain).toLowerCase(), a]));
  const ownedRows = domains.length
    ? domains.map((d) => {
        const a = approvedByDomain.get(String(d).toLowerCase());
        return `<tr><td><b>${esc(d)}</b></td><td>${esc(a && a.email || "—")}</td><td>${esc(fmtTime(a && a.created_ms))}</td><td>${esc(fmtTime(a && a.decided_ms))}</td></tr>`;
      }).join("")
    : '<tr><td colspan="4" class="muted center">本节点还没有已注册域名</td></tr>';
  const waitRows = waiting.length
    ? waiting.map((a) => `<tr><td><b>${esc(a.domain)}</b></td><td>${esc(a.email || "—")}</td><td>等待审核</td><td>${esc(fmtTime(a.created_ms))}</td></tr>`).join("")
    : '<tr><td colspan="4" class="muted center">没有正在等待审核的域名</td></tr>';
  const domOpts = domains.map((d) => `<option value="${esc(d)}">${esc(d)}</option>`).join("");
  const rows = list.length
    ? list.map((r) => {
        const devs = devByAccount.get(String(r.pubkey || "").toLowerCase()) || [];
        const devRows = devs.length
          ? devs.map((d) => `<div class="nm-devrow" style="display:flex;gap:8px;align-items:center;padding:3px 0;flex-wrap:wrap">
                <span class="badge ${d.online ? "badge--ok" : "badge--off"}" style="font-size:10.5px">${d.online ? "在线" : "离线"}</span>
                <span style="min-width:90px">${esc(d.label || "（未命名设备）")}</span>
                <span class="muted" style="font-size:10.5px">${esc(d.role || "member")}${d.revoked ? " · 已吊销" : ""}</span>
                <span style="font-family:monospace;font-size:11px;word-break:break-all;flex:1;min-width:280px">${esc(d.device)}</span>
              </div>`).join("")
          : '<div class="muted" style="font-size:12px;padding:3px 0">该账号在本节点无登记设备</div>';
        return `<tr>
          <td><b>${esc(r.local_part)}</b></td>
          <td>${esc(r.domain)}</td>
          <td style="font-family:monospace;font-size:11.5px;word-break:break-all">${esc(r.pubkey)}</td>
          <td><button class="btn btn--ghost nm-del" data-d="${esc(r.domain)}" data-l="${esc(r.local_part)}">删除</button></td>
        </tr>
        <tr class="nm-devices"><td></td><td colspan="3" style="padding-top:0">
          <div class="muted" style="font-size:11.5px;margin:2px 0 4px">设备（${devs.length}）：设备名 · 角色 · 设备公钥</div>
          ${devRows}
        </td></tr>`;
      }).join("")
    : '<tr><td colspan="4" class="muted center">该节点尚未登记任何命名</td></tr>';
  host.innerHTML = `
    <div class="card">
      <div class="card__title">本节点域名</div>
      <div class="muted" style="margin:8px 0 4px">已注册</div>
      <table class="tbl"><thead><tr><th>域名</th><th>邮箱</th><th>申请时间</th><th>审核通过时间</th></tr></thead><tbody>${ownedRows}</tbody></table>
      <div class="muted" style="margin:14px 0 4px">等待审核</div>
      ${pendingErr ? `<div class="err">${esc(pendingErr)}</div>` : ""}
      <table class="tbl"><thead><tr><th>域名</th><th>邮箱</th><th>状态</th><th>申请时间</th></tr></thead><tbody>${waitRows}</tbody></table>
      <form class="domform" style="margin-top:12px;display:flex;gap:8px;flex-wrap:wrap">
        <input name="domain" placeholder="申请域名，如 example.nm" style="flex:1;min-width:180px" autocomplete="off">
        <input name="email" type="email" placeholder="邮箱，如 ops@example.nm" style="flex:1;min-width:180px" autocomplete="off" required>
        <button class="btn btn--primary" type="submit">申请域名</button>
      </form>
      <div class="err" hidden style="margin-top:8px"></div>
      <div class="wait" hidden style="margin-top:8px"></div>
      <div class="muted" style="margin-top:6px">申请提交到注册中心，通过或驳回后由网格消息写回本节点。一个节点可持有多个域名。</div>
    </div>
    <div class="card" style="margin-top:16px">
      <div class="card__title">域内命名登记（local-part → 公钥）</div>
      <form class="nameform" style="display:flex;gap:8px;flex-wrap:wrap;align-items:center;margin-bottom:12px">
        <input name="local_part" placeholder="local-part（@ 前）" autocomplete="off">
        <span class="muted">@</span>
        <select name="domain" ${domains.length ? "" : "disabled"}>${domOpts}</select>
        <span class="muted" aria-hidden="true">→</span>
        <input name="pubkey" placeholder="公钥 64 位 hex" style="flex:1;min-width:300px" autocomplete="off">
        <button class="btn btn--primary" type="submit" ${domains.length ? "" : "disabled"}>登记 / 更新</button>
      </form>
      <div class="err2" hidden style="margin-bottom:8px"></div>
      <table class="tbl"><thead><tr><th>local-part</th><th>域名</th><th>公钥</th><th></th></tr></thead>
      <tbody>${rows}</tbody></table>
      <div class="muted" style="margin-top:8px">一个公钥可登记多个名字；一个名字只指向一个公钥。每条登记下展开该账号在本节点的设备列表与各设备公钥。登记/删除即经联邦 gossip 广播，各节点收敛。</div>
    </div>`;
  const err = host.querySelector(".err"), err2 = host.querySelector(".err2");
  const wait = host.querySelector(".wait");
  host.querySelector(".domform").addEventListener("submit", async (e) => {
    e.preventDefault(); err.hidden = true;
    const domain = e.target.domain.value.trim();
    const email = e.target.email.value.trim();
    if (!domain) return;
    if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
      err.textContent = "请填写正确的邮箱地址";
      err.hidden = false;
      return;
    }
    try {
      const r = await api("/api/names/signup", { method: "POST", body: { domain, email } });
      if (r && r.ok === false) throw new Error(r.error || "申请失败");
      const want = (r.domain || domain).toLowerCase();
      wait.hidden = false;
      wait.textContent = `已提交 ${want}，等待注册中心审批…`;
      const deadline = Date.now() + 10 * 60 * 1000;
      const timer = setInterval(async () => {
        if (!wait.isConnected) { clearInterval(timer); return; }
        if (Date.now() > deadline) {
          clearInterval(timer);
          wait.textContent = "仍在等待审批。通过或驳回后会经网格送到本节点。";
          return;
        }
        try {
          const n = await api("/api/names/domain-notices");
          const hit = (n.notices || []).find((x) => String(x.domain).toLowerCase() === want);
          if (!hit) return;
          clearInterval(timer);
          if (hit.approved) {
            wait.textContent = `${want} 已通过，已写入本节点域名列表。`;
          } else {
            wait.textContent = `${want} 已被驳回。`;
          }
          refresh();
        } catch (ex) { wait.textContent = ex.message; }
      }, 3000);
    } catch (ex) { err.textContent = ex.message; err.hidden = false; }
  });
  host.querySelector(".nameform").addEventListener("submit", async (e) => {
    e.preventDefault(); err2.hidden = true;
    const domain = e.target.domain.value;
    const local_part = e.target.local_part.value.trim();
    const pubkey = e.target.pubkey.value.trim();
    if (!local_part || !pubkey) { err2.textContent = "请填写 local-part 与公钥"; err2.hidden = false; return; }
    try {
      const r = await api("/api/names/set", { method: "POST", body: { domain, local_part, pubkey } });
      if (r && r.ok === false) throw new Error(r.error || "登记失败");
      e.target.local_part.value = ""; e.target.pubkey.value = "";
      refresh();
    } catch (ex) { err2.textContent = ex.message; err2.hidden = false; }
  });
  host.querySelectorAll(".nm-del").forEach((b) => b.addEventListener("click", async () => {
    if (!confirm(`删除命名 ${b.dataset.l}@${b.dataset.d}？此操作将广播给联邦。`)) return;
    try {
      const r = await api("/api/names/del", { method: "POST", body: { domain: b.dataset.d, local_part: b.dataset.l } });
      if (r && r.ok === false) throw new Error(r.error || "删除失败");
      refresh();
    } catch (ex) { err2.textContent = ex.message; err2.hidden = false; }
  }));
}

function drawChart(cv) {
  if (!cv) return;
  const dpr = window.devicePixelRatio || 1;
  const w = (cv.width = cv.clientWidth * dpr);
  const h = (cv.height = 170 * dpr);
  const ctx = cv.getContext("2d");
  ctx.clearRect(0, 0, w, h);
  if (TRAF.length < 2) return;
  const rates = [];
  for (let k = 1; k < TRAF.length; k++) {
    const dt = (TRAF[k].t - TRAF[k - 1].t) / 1000 || 1;
    rates.push({ i: Math.max(0, (TRAF[k].i - TRAF[k - 1].i) / dt), o: Math.max(0, (TRAF[k].o - TRAF[k - 1].o) / dt) });
  }
  const max = Math.max(1, ...rates.map((r) => Math.max(r.i, r.o)));
  const css = (v, f) => (getComputedStyle(document.documentElement).getPropertyValue(v).trim() || f);
  const plot = (key, color, fill) => {
    ctx.beginPath();
    rates.forEach((r, i) => {
      const x = (i / (rates.length - 1)) * w;
      const y = h - (r[key] / max) * (h - 12 * dpr) - 6 * dpr;
      i ? ctx.lineTo(x, y) : ctx.moveTo(x, y);
    });
    ctx.strokeStyle = color; ctx.lineWidth = 2 * dpr; ctx.lineJoin = "round"; ctx.stroke();
    ctx.lineTo(w, h); ctx.lineTo(0, h); ctx.closePath(); ctx.fillStyle = fill; ctx.fill();
  };
  plot("i", css("--sky", "#0ea5e9"), "rgba(14,165,233,.10)");
  plot("o", css("--accent", "#5b5bf0"), "rgba(91,91,240,.10)");
}

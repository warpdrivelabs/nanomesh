// ── 连接页控制器（复用太空品牌页）：选择一个「节点服务」+ 显示名 → 连接。
//    节点服务由 services.js(NodeSvc) 统一管理，与主界面「🖧 节点服务」面板共用同一份数据。

// ── 视图切换 ──
function showLoginView() {
  document.getElementById("main-view").style.display = "none";
  document.getElementById("login-view").style.display = "block";
  const starsFar = document.getElementById("space-stars-far");
  if (starsFar && !starsFar.childElementCount) seedLoginSpace();
  NodeSvc.migrate();
  renderServiceOptions();
}
function showMainView() {
  document.getElementById("login-view").style.display = "none";
  document.getElementById("main-view").style.display = "flex";
}
window.showLoginView = showLoginView;
window.showMainView = showMainView;

// ── 太空装饰（seed 星点/流星；抄自 cmx-agent login.js，纯前端动画）──
function seedSpaceStars(host, count, sizeMin, sizeMax) {
  if (!host) return;
  for (let i = 0; i < count; i++) {
    const el = document.createElement("span"); el.className = "space-star";
    const size = sizeMin + Math.random() * (sizeMax - sizeMin);
    el.style.width = size + "px"; el.style.height = size + "px";
    el.style.left = (Math.random() * 100) + "%"; el.style.top = (Math.random() * 100) + "%";
    el.style.setProperty("--star-o-min", (0.12 + Math.random() * 0.28).toFixed(2));
    el.style.setProperty("--star-o-max", (0.45 + Math.random() * 0.55).toFixed(2));
    el.style.animationDuration = (2.2 + Math.random() * 4.5) + "s";
    el.style.animationDelay = (-Math.random() * 6) + "s";
    host.appendChild(el);
  }
}
function seedLoginSpace() {
  seedSpaceStars(document.getElementById("space-stars-far"), 52, 1, 1.6);
  seedSpaceStars(document.getElementById("space-stars-mid"), 28, 1.6, 2.8);
  const rushHost = document.getElementById("space-rush");
  for (let i = 0; rushHost && i < 58; i++) {
    const el = document.createElement("span"); el.className = "space-rush-star";
    const angle = Math.random() * Math.PI * 2, dist = 32 + Math.random() * 58;
    el.style.left = (75 + Math.random() * 8 - 4) + "%"; el.style.top = (45 + Math.random() * 8 - 4) + "%";
    el.style.setProperty("--dx", (Math.cos(angle) * dist) + "vmax");
    el.style.setProperty("--dy", (Math.sin(angle) * dist) + "vmax");
    el.style.setProperty("--star-size", (1 + Math.random() * 2.2).toFixed(1) + "px");
    el.style.setProperty("--star-scale", (1.4 + Math.random() * 2.8).toFixed(2));
    el.style.animationDuration = (1.6 + Math.random() * 2.6) + "s";
    el.style.animationDelay = (-Math.random() * 4.5) + "s";
    rushHost.appendChild(el);
  }
  const shooterHost = document.getElementById("space-shooters");
  for (let i = 0; shooterHost && i < 7; i++) {
    const el = document.createElement("span"); el.className = "space-shooter";
    el.style.setProperty("--shoot-angle", (-18 + Math.random() * 36) + "deg");
    el.style.top = (8 + Math.random() * 72) + "%"; el.style.left = (-12 + Math.random() * 18) + "%";
    el.style.animationDuration = (3.8 + Math.random() * 5.5) + "s";
    el.style.animationDelay = (-Math.random() * 12) + "s";
    shooterHost.appendChild(el);
  }
}

// ── 节点服务下拉（数据源 = NodeSvc，与「节点服务」面板同一份）──
function svcLabel(s) {
  const pk = NodeSvc.pubkeyOf(s.node);
  return (s.name ? s.name + " · " : "") + s.mode + " · " + shortNode(pk || s.node);
}
function renderServiceOptions(selId) {
  const sel = document.getElementById("c-service");
  if (!sel) return;
  const cur = selId || sel.value;
  const list = NodeSvc.list();
  if (!list.length) {
    sel.innerHTML = '<option value="" disabled selected>（还没有节点服务，点右侧「导入」或去「🖧 节点服务」面板新增）</option>';
    return;
  }
  sel.innerHTML = list.map((s) => `<option value="${s.id}">${escapeHtml(svcLabel(s))}</option>`).join("");
  if (cur && list.some((s) => s.id === cur)) sel.value = cur; else sel.selectedIndex = 0;
}
window.renderServiceOptions = renderServiceOptions;

// ── 导入：粘贴公钥/地址 → 快速新增一个 nat 节点服务 ──
function openImport() {
  const box = document.getElementById("import-box");
  const show = box.style.display === "none";
  box.style.display = show ? "flex" : "none";
  if (show) { const i = document.getElementById("import-input"); i.value = ""; i.focus(); }
}
function confirmImport() {
  const raw = document.getElementById("import-input").value.trim();
  const pk = NodeSvc.pubkeyOf(raw);
  if (!pk) { if (window.toast) toast("请输入 64 位公钥 hex，或有效的 NM_NODE_ADDR"); return; }
  // 粘的是完整地址(JSON)则原样存（保留 LAN 直连信息）；裸公钥直接存。默认 nat 模式。
  const svc = NodeSvc.put({ name: "", mode: "nat", node: raw.startsWith("{") ? raw : pk });
  renderServiceOptions(svc.id);
  document.getElementById("import-box").style.display = "none";
  if (window.toast) toast("已导入节点服务：" + shortNode(pk));
}

// ── 连接提交 ──
async function submitConnect(e) {
  if (e) e.preventDefault();
  const svc = NodeSvc.get(document.getElementById("c-service").value);
  const displayName = document.getElementById("c-name").value.trim() || "nmspace 用户";
  const errBox = document.getElementById("c-error");
  const btn = document.getElementById("c-submit");
  errBox.textContent = "";
  if (!svc) { errBox.textContent = "请先「导入」或在「🖧 节点服务」面板新增一个节点服务"; return; }
  btn.disabled = true; btn.textContent = "连接中…";
  try {
    const myId = await NM.inv("connect", {
      mode: svc.mode, node: svc.node, displayName,
      relayUrls: svc.relayUrls, pkarrUrl: svc.pkarrUrl, dnsOrigin: svc.dnsOrigin,
    });
    showMainView();
    if (typeof imStart === "function") await imStart(myId);
  } catch (err) {
    errBox.textContent = "连接失败：" + (err && err.message ? err.message : String(err));
  } finally {
    btn.disabled = false; btn.textContent = "连 接";
  }
}

function shortNode(n) { if (!n) return ""; return n.length <= 24 ? n : n.slice(0, 12) + "…" + n.slice(-8); }
function escapeHtml(s) { return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])); }

// ── 装配 ──
(function initConnect() {
  const y = document.getElementById("year"); if (y) y.textContent = String(new Date().getFullYear());
  NodeSvc.migrate();
  renderServiceOptions();
  document.getElementById("c-import").addEventListener("click", openImport);
  document.getElementById("import-ok").addEventListener("click", confirmImport);
  document.getElementById("import-cancel").addEventListener("click", () => { document.getElementById("import-box").style.display = "none"; });
  document.getElementById("import-input").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); confirmImport(); } });
  document.getElementById("connect-form").addEventListener("submit", submitConnect);
  showLoginView();
})();

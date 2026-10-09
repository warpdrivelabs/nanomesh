// ── 连接页控制器（复用太空品牌页）：选「用户身份(公钥)」+「节点服务」→ 连接。
//    多账号：一人可有多个身份(公钥)，可新建。身份 = Identity(identity.js)，节点服务 = NodeSvc(services.js)。
//    connectAs() 为登录提交与标题栏切号共用。

function showLoginView() {
  document.getElementById("main-view").style.display = "none";
  document.getElementById("login-view").style.display = "block";
  const starsFar = document.getElementById("space-stars-far");
  if (starsFar && !starsFar.childElementCount) seedLoginSpace();
  renderServiceOptions();
  renderUserOptions();
}
function showMainView() {
  document.getElementById("login-view").style.display = "none";
  document.getElementById("main-view").style.display = "flex";
  paintHomeNode();
}
function paintHomeNode() {
  const text = document.getElementById("sb-node-text");
  const dot = document.getElementById("sb-dot");
  const host = document.getElementById("sb-node");
  if (!text) return;
  const svc = window.CURRENT_SVC;
  if (!svc || !svc.node) {
    text.textContent = "Home Node 未连接";
    if (dot) dot.classList.remove("on");
    if (host) host.title = "";
    return;
  }
  const pk = window.NodeSvc ? NodeSvc.pubkeyOf(svc.node) : "";
  const short = pk ? pk.slice(0, 8) : "";
  const mode = svc.mode === "nat" ? "穿透" : svc.mode === "lan" ? "同网" : svc.mode === "selfhost" ? "自建" : (svc.mode || "");
  const who = svc.name || "Home Node";
  text.textContent = [who, mode].filter(Boolean).join(" · ");
  if (dot) dot.classList.add("on");
  if (host) host.title = [pk || String(svc.node), short].filter(Boolean).join("\n");
}
window.paintHomeNode = paintHomeNode;
window.showLoginView = showLoginView;
window.showMainView = showMainView;

// ── 太空装饰（seed 星点/流星）──
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

// ── 用户身份下拉 ──
async function renderUserOptions(selPk) {
  const sel = document.getElementById("c-user");
  if (!sel) return;
  const cur = selPk || sel.value || Identity.current();
  const ids = await Identity.list();
  if (!ids.length) {
    sel.innerHTML = '<option value="" disabled selected>（还没有身份，点右侧「新建」）</option>';
    return;
  }
  sel.innerHTML = ids.map((pk) => `<option value="${pk}">${escapeHtml(Identity.label(pk))} · ${shortNode(pk)}</option>`).join("");
  if (cur && ids.includes(cur)) sel.value = cur; else sel.selectedIndex = 0;
}
window.renderUserOptions = renderUserOptions;

// 新建身份（内联起昵称）
function openNewUser() {
  const b = document.getElementById("newuser-box");
  const show = b.style.display === "none";
  b.style.display = show ? "flex" : "none";
  if (show) { const i = document.getElementById("newuser-name"); i.value = ""; i.focus(); }
}
async function confirmNewUser() {
  const name = document.getElementById("newuser-name").value.trim();
  try {
    const pk = await Identity.create(name);
    Identity.setCurrent(pk);
    document.getElementById("newuser-box").style.display = "none";
    await renderUserOptions(pk);
    if (window.toast) toast("已新建身份：" + (name || shortNode(pk)));
  } catch (e) { if (window.toast) toast("新建失败：" + (e && e.message ? e.message : e)); }
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
    sel.innerHTML = '<option value="" disabled selected>（还没有节点服务，点右侧「导入」或去「节点服务」面板新增）</option>';
    return;
  }
  sel.innerHTML = list.map((s) => `<option value="${s.id}">${escapeHtml(svcLabel(s))}</option>`).join("");
  if (cur && list.some((s) => s.id === cur)) sel.value = cur; else sel.selectedIndex = 0;
}
window.renderServiceOptions = renderServiceOptions;

// 新建节点服务（复用富表单 buildSvcForm；可填公钥/地址、名称、联系方式、地理位置等）
function openNewSvc() {
  const box = document.getElementById("svc-form-box");
  if (box.style.display !== "none" && box.innerHTML) { box.style.display = "none"; box.innerHTML = ""; return; }
  buildSvcForm(box, null, (saved) => { renderServiceOptions(saved.id); if (window.renderNodeSvcList) window.renderNodeSvcList(); });
}

// ── 连接（登录提交 & 标题栏切号共用）──
async function connectAs(user, svc, name) {
  const myId = await NM.inv("connect", {
    user, mode: svc.mode, node: svc.node, displayName: name || "nmspace 用户",
    relayUrls: svc.relayUrls, pkarrUrl: svc.pkarrUrl, dnsOrigin: svc.dnsOrigin,
  });
  Identity.setCurrent(user);
  window.CURRENT_SVC = svc;
  Identity.setSvc(user, svc); // Bug 3: 记住本账号连的节点，切号时各用各的，不串号
  showMainView();
  if (typeof updateUserChip === "function") updateUserChip();
  if (typeof imStart === "function") await imStart(myId);
  return myId;
}
window.connectAs = connectAs;

async function submitConnect(e) {
  if (e) e.preventDefault();
  const user = document.getElementById("c-user").value;
  const svc = NodeSvc.get(document.getElementById("c-service").value);
  const errBox = document.getElementById("c-error");
  const btn = document.getElementById("c-submit");
  errBox.textContent = "";
  if (!user) { errBox.textContent = "请先选择或「新建」一个用户身份"; return; }
  if (!svc) { errBox.textContent = "请先「导入」或在「节点服务」面板新增一个节点服务"; return; }
  btn.disabled = true; btn.textContent = "连接中…";
  try { await connectAs(user, svc, Identity.nameOf(user)); }
  catch (err) { errBox.textContent = "连接失败：" + (err && err.message ? err.message : String(err)); }
  finally { btn.disabled = false; btn.textContent = "连接"; }
}

function shortNode(n) { if (!n) return ""; return n.length <= 24 ? n : n.slice(0, 12) + "…" + n.slice(-8); }
function escapeHtml(s) { return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])); }

// ── 装配 ──
(function initConnect() {
  const y = document.getElementById("year"); if (y) y.textContent = String(new Date().getFullYear());
  renderServiceOptions();
  renderUserOptions();
  const bind = (id, ev, fn) => { const el = document.getElementById(id); if (el) el.addEventListener(ev, fn); };
  bind("c-newuser", "click", openNewUser);
  bind("newuser-ok", "click", confirmNewUser);
  bind("newuser-cancel", "click", () => { document.getElementById("newuser-box").style.display = "none"; });
  bind("newuser-name", "keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); confirmNewUser(); } });
  bind("c-newsvc", "click", openNewSvc);
  bind("connect-form", "submit", submitConnect);
  // 启动视图由 auth.js 的 routeStart 决定（先过锁屏），此处不直接 showLoginView。
})();

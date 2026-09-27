// ── 「🖧 节点服务」面板 + 共享的「新建/编辑服务」表单（buildSvcForm）。
//    表单支持：名称、连接模式、节点公钥/地址（可导入公钥或 NM_NODE_ADDR）、
//    selfhost 的 relay/pkarr/dns、联系方式、地理位置、备注。登录页与面板共用同一表单。
//    复用 connect.js 的全局 escapeHtml / shortNode / renderServiceOptions。

// 通用富表单：渲染进 container，保存后回调 onDone(savedSvc)。用 class + container 作用域（避免 id 冲突）。
function buildSvcForm(container, svc, onDone) {
  const s = svc || { id: "", name: "", mode: "nat", node: "", relayUrls: [], pkarrUrl: "", dnsOrigin: "", contact: "", location: "", note: "" };
  const editing = !!(svc && svc.id);
  container.innerHTML = `
    <div class="ns-ftitle">${editing ? "编辑节点服务" : "新建节点服务"}</div>
    <div class="ns-frow"><label>名称</label><input class="f-name" type="text" placeholder="如 演示节点 / 上海机房" value="${escapeHtml(s.name)}"></div>
    <div class="ns-frow"><label>连接模式</label><select class="f-mode">
      <option value="nat">nat · 公共中继 + 打洞</option>
      <option value="lan">lan · 仅同网直连</option>
      <option value="selfhost">selfhost · 自建 relay + pkarr</option>
    </select></div>
    <div class="ns-frow"><label>节点公钥 / 地址</label><input class="f-node" type="text" placeholder="64 位公钥 hex 或 NM_NODE_ADDR(JSON)" value="${escapeHtml(s.node)}"></div>
    <div class="f-selfhost" style="display:none">
      <div class="ns-frow"><label>中继 relay（逗号分隔）</label><input class="f-relays" type="text" value="${escapeHtml((s.relayUrls || []).join(", "))}"></div>
      <div class="ns-frow"><label>pkarr 端点</label><input class="f-pkarr" type="text" value="${escapeHtml(s.pkarrUrl || "")}"></div>
      <div class="ns-frow"><label>DNS origin（可选）</label><input class="f-dns" type="text" value="${escapeHtml(s.dnsOrigin || "")}"></div>
    </div>
    <div class="ns-frow"><label>联系方式</label><input class="f-contact" type="text" placeholder="电话 / 邮箱 / 微信（可选）" value="${escapeHtml(s.contact)}"></div>
    <div class="ns-frow"><label>地理位置</label><input class="f-location" type="text" placeholder="城市 / 地址 / 经纬度（可选）" value="${escapeHtml(s.location)}"></div>
    <div class="ns-frow"><label>备注</label><input class="f-note" type="text" placeholder="其他说明（可选）" value="${escapeHtml(s.note)}"></div>
    <div class="ns-factions"><button type="button" class="ns-btn f-cancel">取消</button><button type="button" class="ns-btn ns-primary f-save">保存</button></div>`;
  container.style.display = "block";
  const q = (sel) => container.querySelector(sel);
  q(".f-mode").value = s.mode || "nat";
  const syncSH = () => { q(".f-selfhost").style.display = q(".f-mode").value === "selfhost" ? "block" : "none"; };
  syncSH();
  q(".f-mode").addEventListener("change", syncSH);
  const close = () => { container.style.display = "none"; container.innerHTML = ""; };
  q(".f-cancel").addEventListener("click", close);
  q(".f-save").addEventListener("click", () => {
    const node = q(".f-node").value.trim();
    if (!NodeSvc.pubkeyOf(node)) { if (window.toast) toast("节点需为 64 位公钥 hex 或有效的 NM_NODE_ADDR"); return; }
    const saved = NodeSvc.put({
      id: s.id || undefined,
      name: q(".f-name").value.trim(),
      mode: q(".f-mode").value,
      node,
      relayUrls: q(".f-relays").value.split(",").map((x) => x.trim()).filter(Boolean),
      pkarrUrl: q(".f-pkarr").value.trim() || null,
      dnsOrigin: q(".f-dns").value.trim() || null,
      contact: q(".f-contact").value.trim(),
      location: q(".f-location").value.trim(),
      note: q(".f-note").value.trim(),
    });
    close();
    if (window.toast) toast(editing ? "已更新节点服务" : "已新建节点服务");
    if (onDone) onDone(saved);
  });
  q(".f-name").focus();
}
window.buildSvcForm = buildSvcForm;
// 直接引用函数本身（不能用 () => renderNodeSvcList()：经典脚本里 window.renderNodeSvcList
// 就是该函数声明本身，赋成箭头会让它自我递归 → 爆栈）。
window.renderNodeSvcList = renderNodeSvcList;

// ── 面板：列表 + 新增/编辑/删除 + 详情 ──
let NSVC_SEL = null; // 当前查看详情的服务 id（列表高亮）
function nsRefresh() {
  renderNodeSvcList();
  if (typeof renderServiceOptions === "function") renderServiceOptions();
}
function renderNodeSvcList() {
  const box = document.getElementById("nodesvc-list");
  if (!box) return;
  const list = NodeSvc.list();
  if (!list.length) { box.innerHTML = '<div class="ns-empty">还没有节点服务。点上方「＋ 新增」。</div>'; return; }
  box.innerHTML = list.map((s) => {
    const pk = NodeSvc.pubkeyOf(s.node) || s.node;
    const extra = [s.contact, s.location].filter(Boolean).join(" · ");
    return `<div class="ns-item ${s.id === NSVC_SEL ? "on" : ""}" data-id="${s.id}">
      <span class="ns-mode">${s.mode}</span>
      <span class="ns-meta">
        <b>${escapeHtml(s.name || "(未命名)")}</b>
        <small>${escapeHtml(shortNode(pk))}</small>
        ${extra ? `<small class="ns-extra">${escapeHtml(extra)}</small>` : ""}
      </span>
      <button class="ns-edit" data-edit="${s.id}" title="编辑">✎</button>
      <button class="ns-del" data-del="${s.id}" title="删除">✕</button>
    </div>`;
  }).join("");
  box.querySelectorAll(".ns-item").forEach((el) => el.addEventListener("click", (e) => {
    if (e.target.closest(".ns-edit") || e.target.closest(".ns-del")) return;
    showNodeSvcDetail(el.dataset.id);
  }));
  box.querySelectorAll(".ns-edit").forEach((b) => b.addEventListener("click", () => buildSvcForm(document.getElementById("nodesvc-form"), NodeSvc.get(b.dataset.edit), nsRefresh)));
  box.querySelectorAll(".ns-del").forEach((b) => b.addEventListener("click", () => { NodeSvc.remove(b.dataset.del); nsRefresh(); }));
}

// ── 节点服务详情（主区）：完整属性 + 复制公钥/地址 + 编辑/删除 ──
function showNodeSvcDetail(id) {
  const s = NodeSvc.get(id);
  const title = s ? (s.name || "(未命名)") : id;
  if (window.Tabs) Tabs.open({ key: "n:" + id, kind: "nodesvc", title, ico: "🖧", render: () => renderNodeSvcDetail(id) });
  else renderNodeSvcDetail(id);
}
function renderNodeSvcDetail(id) {
  NSVC_SEL = id;
  renderNodeSvcList();
  const s = NodeSvc.get(id);
  const conv = document.getElementById("conv");
  if (!s || !conv) return;
  const fields = [];
  if (s.mode === "selfhost") {
    if ((s.relayUrls || []).length) fields.push(["中继 relay", s.relayUrls.join(", ")]);
    if (s.pkarrUrl) fields.push(["pkarr 端点", s.pkarrUrl]);
    if (s.dnsOrigin) fields.push(["DNS origin", s.dnsOrigin]);
  }
  if (s.contact) fields.push(["联系方式", s.contact]);
  if (s.location) fields.push(["地理位置", s.location]);
  if (s.note) fields.push(["备注", s.note]);
  const fieldHtml = fields.map(([l, v]) => `<div class="ed-field"><label>${escapeHtml(l)}</label><div class="ed-plain">${escapeHtml(v)}</div></div>`).join("");
  conv.innerHTML = `
    <div class="conv-head"><b>节点服务详情</b><span class="sp"></span><span class="conv-kind">${escapeHtml(s.mode)}</span></div>
    <div class="ent-detail">
      <div class="ed-avatar" style="background:var(--accent-soft);color:var(--aqua);font-size:34px">🖧</div>
      <div class="ed-name">${escapeHtml(s.name || "(未命名)")}</div>
      <div class="ed-field"><label>节点公钥 / 地址</label>
        <div class="ed-key"><code>${escapeHtml(s.node)}</code><button class="ns-btn ns-primary" id="nsd-copy">复制</button></div></div>
      ${fieldHtml}
      <div class="ed-actions"><button class="ns-btn ns-primary" id="nsd-edit">编辑</button><button class="ns-btn" id="nsd-del">删除</button></div>
      <div class="ed-users">
        <div class="ed-users-head"><span>此节点上的用户</span><button class="ns-btn" id="nsd-refresh">刷新</button></div>
        <div class="nsu-list" id="nsd-users"><div class="ns-empty">点「刷新」加载该节点的用户</div></div>
      </div>
    </div>`;
  document.getElementById("nsd-copy").addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(s.node); if (window.toast) toast("已复制节点公钥/地址"); }
    catch (_) { if (window.toast) toast("复制失败，请手动选中"); }
  });
  document.getElementById("nsd-edit").addEventListener("click", () => buildSvcForm(document.getElementById("nodesvc-form"), NodeSvc.get(id), nsRefresh));
  document.getElementById("nsd-del").addEventListener("click", () => {
    NodeSvc.remove(id); NSVC_SEL = null; nsRefresh();
    conv.innerHTML = '<div class="im-center"><div class="ico">🖧</div><div class="txt">节点服务已删除</div></div>';
  });
  document.getElementById("nsd-refresh").addEventListener("click", () => loadNodeUsers(s));
}

// 拉取并渲染「此节点服务上的用户」（临时拨号查目录），每个可加入实体目录。
async function loadNodeUsers(s) {
  const box = document.getElementById("nsd-users");
  if (!box) return;
  const btn = document.getElementById("nsd-refresh");
  box.innerHTML = '<div class="ns-empty">加载中…（临时连接该节点查询目录）</div>';
  if (btn) btn.disabled = true;
  try {
    const users = await NM.inv("node_users", {
      mode: s.mode, node: s.node,
      relayUrls: s.relayUrls || [], pkarrUrl: s.pkarrUrl || null, dnsOrigin: s.dnsOrigin || null,
    });
    if (!users.length) { box.innerHTML = '<div class="ns-empty">该节点暂无已注册用户</div>'; return; }
    box.innerHTML = users.map((u) => `
      <div class="nsu-item">
        <span class="av" style="background:${avatarColor(u.id)}">${escapeHtml((u.name || "?").slice(0, 1))}</span>
        <span class="nsu-meta"><b>${escapeHtml(u.name || shortId(u.id))}</b><small>${escapeHtml(u.kind || "")} · ${escapeHtml(shortNode(u.id))}</small></span>
        <button class="ns-btn nsu-add" data-id="${u.id}" data-kind="${escapeHtml(u.kind || "")}" data-name="${escapeHtml(u.name || "")}">添加</button>
      </div>`).join("");
    box.querySelectorAll(".nsu-add").forEach((b) => b.addEventListener("click", () => {
      if (window.addKnownEntity && window.addKnownEntity(b.dataset.id, b.dataset.kind, b.dataset.name)) {
        b.textContent = "已添加"; b.disabled = true;
        if (window.toast) toast("已添加到实体目录");
      }
    }));
  } catch (e) {
    box.innerHTML = '<div class="ns-empty">加载失败：' + escapeHtml(e && e.message ? e.message : String(e)) + '</div>';
  } finally {
    if (btn) btn.disabled = false;
  }
}

// ── 装配 ──
(function initNodeSvc() {
  const nw = document.getElementById("ns-new");
  if (nw) nw.addEventListener("click", () => buildSvcForm(document.getElementById("nodesvc-form"), null, nsRefresh));
  renderNodeSvcList();
})();

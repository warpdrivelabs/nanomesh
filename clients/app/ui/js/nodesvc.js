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

// ── 面板：列表 + 新增/编辑/删除 ──
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
    return `<div class="ns-item" data-id="${s.id}">
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
  box.querySelectorAll(".ns-edit").forEach((b) => b.addEventListener("click", () => buildSvcForm(document.getElementById("nodesvc-form"), NodeSvc.get(b.dataset.edit), nsRefresh)));
  box.querySelectorAll(".ns-del").forEach((b) => b.addEventListener("click", () => { NodeSvc.remove(b.dataset.del); nsRefresh(); }));
}

// ── 装配 ──
(function initNodeSvc() {
  const nw = document.getElementById("ns-new");
  if (nw) nw.addEventListener("click", () => buildSvcForm(document.getElementById("nodesvc-form"), null, nsRefresh));
  renderNodeSvcList();
})();

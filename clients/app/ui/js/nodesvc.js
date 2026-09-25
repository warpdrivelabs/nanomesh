// ── 「🖧 节点服务」面板：新增 / 导入 / 编辑 / 删除。与登录页共用 NodeSvc；
//    任何改动后同步刷新登录页下拉（renderServiceOptions，由 connect.js 暴露）。
//    复用 connect.js 的全局 escapeHtml / shortNode。

function nsRefresh() {
  renderNodeSvcList();
  if (typeof renderServiceOptions === "function") renderServiceOptions();
}

function renderNodeSvcList() {
  const box = document.getElementById("nodesvc-list");
  if (!box) return;
  const list = NodeSvc.list();
  if (!list.length) { box.innerHTML = '<div class="ns-empty">还没有节点服务。点上方「＋ 新增」或「导入」。</div>'; return; }
  box.innerHTML = list.map((s) => {
    const pk = NodeSvc.pubkeyOf(s.node) || s.node;
    return `<div class="ns-item" data-id="${s.id}">
      <span class="ns-mode">${s.mode}</span>
      <span class="ns-meta"><b>${escapeHtml(s.name || "(未命名)")}</b><small>${escapeHtml(shortNode(pk))}</small></span>
      <button class="ns-edit" data-edit="${s.id}" title="编辑">✎</button>
      <button class="ns-del" data-del="${s.id}" title="删除">✕</button>
    </div>`;
  }).join("");
  box.querySelectorAll(".ns-edit").forEach((b) => b.addEventListener("click", () => openForm(NodeSvc.get(b.dataset.edit))));
  box.querySelectorAll(".ns-del").forEach((b) => b.addEventListener("click", () => { NodeSvc.remove(b.dataset.del); nsRefresh(); }));
}

// 完整表单（新增 / 编辑）
function openForm(svc) {
  const f = document.getElementById("nodesvc-form");
  const s = svc || { id: "", name: "", mode: "nat", node: "", relayUrls: [], pkarrUrl: "", dnsOrigin: "" };
  f.dataset.id = s.id || "";
  f.innerHTML = `
    <div class="ns-ftitle">${s.id ? "编辑节点服务" : "新增节点服务"}</div>
    <div class="ns-frow"><label>名称</label><input id="ns-name" type="text" placeholder="如 演示节点 / 本机 nmd" value="${escapeHtml(s.name)}"></div>
    <div class="ns-frow"><label>连接模式</label><select id="ns-mode">
      <option value="nat">nat · 公共中继 + 打洞</option>
      <option value="lan">lan · 仅同网直连</option>
      <option value="selfhost">selfhost · 自建 relay + pkarr</option>
    </select></div>
    <div class="ns-frow"><label>节点公钥 / 地址</label><input id="ns-node" type="text" placeholder="64 位公钥 hex 或 NM_NODE_ADDR(JSON)" value="${escapeHtml(s.node)}"></div>
    <div id="ns-selfhost" style="display:none">
      <div class="ns-frow"><label>中继 relay（逗号分隔）</label><input id="ns-relays" type="text" value="${escapeHtml((s.relayUrls || []).join(", "))}"></div>
      <div class="ns-frow"><label>pkarr 端点</label><input id="ns-pkarr" type="text" value="${escapeHtml(s.pkarrUrl || "")}"></div>
      <div class="ns-frow"><label>DNS origin（可选）</label><input id="ns-dns" type="text" value="${escapeHtml(s.dnsOrigin || "")}"></div>
    </div>
    <div class="ns-factions"><button class="ns-btn" id="ns-cancel">取消</button><button class="ns-btn ns-primary" id="ns-save">保存</button></div>`;
  f.style.display = "block";
  document.getElementById("ns-mode").value = s.mode || "nat";
  const syncSH = () => { document.getElementById("ns-selfhost").style.display = document.getElementById("ns-mode").value === "selfhost" ? "block" : "none"; };
  syncSH();
  document.getElementById("ns-mode").addEventListener("change", syncSH);
  document.getElementById("ns-cancel").addEventListener("click", closeForm);
  document.getElementById("ns-save").addEventListener("click", saveFull);
  document.getElementById("ns-name").focus();
}
function saveFull() {
  const node = document.getElementById("ns-node").value.trim();
  if (!node) { if (window.toast) toast("请填写节点公钥或地址"); return; }
  NodeSvc.put({
    id: document.getElementById("nodesvc-form").dataset.id || undefined,
    name: document.getElementById("ns-name").value.trim(),
    mode: document.getElementById("ns-mode").value,
    node,
    relayUrls: document.getElementById("ns-relays").value.split(",").map((x) => x.trim()).filter(Boolean),
    pkarrUrl: document.getElementById("ns-pkarr").value.trim() || null,
    dnsOrigin: document.getElementById("ns-dns").value.trim() || null,
  });
  closeForm(); nsRefresh();
  if (window.toast) toast("已保存节点服务");
}

// 快速导入（只粘公钥/地址，nat 模式）
function openImport() {
  const f = document.getElementById("nodesvc-form");
  f.dataset.id = "";
  f.innerHTML = `
    <div class="ns-ftitle">导入节点服务</div>
    <div class="ns-frow"><label>节点公钥 / 地址</label><input id="ns-imp-node" type="text" placeholder="64 位公钥 hex 或 NM_NODE_ADDR(JSON)"></div>
    <div class="ns-fhint">默认 nat 模式；如需 selfhost/命名请用「＋ 新增」。</div>
    <div class="ns-factions"><button class="ns-btn" id="ns-cancel">取消</button><button class="ns-btn ns-primary" id="ns-save">导入</button></div>`;
  f.style.display = "block";
  document.getElementById("ns-cancel").addEventListener("click", closeForm);
  document.getElementById("ns-save").addEventListener("click", saveImport);
  const inp = document.getElementById("ns-imp-node");
  inp.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); saveImport(); } });
  inp.focus();
}
function saveImport() {
  const raw = document.getElementById("ns-imp-node").value.trim();
  const pk = NodeSvc.pubkeyOf(raw);
  if (!pk) { if (window.toast) toast("请输入 64 位公钥 hex，或有效的 NM_NODE_ADDR"); return; }
  NodeSvc.put({ name: "", mode: "nat", node: raw.startsWith("{") ? raw : pk });
  closeForm(); nsRefresh();
  if (window.toast) toast("已导入节点服务：" + shortNode(pk));
}

function closeForm() {
  const f = document.getElementById("nodesvc-form");
  f.style.display = "none"; f.innerHTML = ""; f.dataset.id = "";
}

// ── 装配 ──
(function initNodeSvc() {
  const nw = document.getElementById("ns-new"), imp = document.getElementById("ns-import");
  if (nw) nw.addEventListener("click", () => openForm(null));
  if (imp) imp.addEventListener("click", openImport);
  renderNodeSvcList();
})();

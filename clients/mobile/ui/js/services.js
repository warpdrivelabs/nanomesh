// ── 节点服务：统一的「可连接节点」记录，登录页与「节点服务」面板共用（本机 SQLite）。
//    一条服务 = { id, name, mode, node, relayUrls, pkarrUrl, dnsOrigin }。
//    node 可为 64 位公钥 hex 或 NM_NODE_ADDR(JSON)；mode = nat|lan|selfhost。
(function () {
  let LIST = [];
  function uid() { return "s" + Date.now().toString(36) + Math.random().toString(36).slice(2, 6); }
  async function load() {
    LIST = [];
    if (!window.NM) return LIST;
    try {
      const arr = JSON.parse((await NM.inv("server_load")) || "[]");
      LIST = Array.isArray(arr) ? arr.slice(0, 50) : [];
    } catch (_) {}
    return LIST;
  }
  function save(list) {
    LIST = list.slice(0, 50);
    if (window.NM) NM.inv("server_save", { data: JSON.stringify(LIST) }).catch(() => {});
  }

  /** 从 node 值提取 64 位公钥（裸 hex 或 NM_NODE_ADDR 的 id）；取不到返回 null。 */
  function pubkeyOf(node) {
    const t = (node || "").trim();
    if (/^[0-9a-fA-F]{64}$/.test(t)) return t.toLowerCase();
    try { const a = JSON.parse(t); if (a && typeof a.id === "string" && /^[0-9a-fA-F]{64}$/.test(a.id)) return a.id.toLowerCase(); } catch (_) {}
    return null;
  }
  function norm(svc) {
    return {
      id: svc.id || uid(),
      name: (svc.name || "").trim(),
      mode: svc.mode || "nat",
      node: (svc.node || "").trim(),
      relayUrls: Array.isArray(svc.relayUrls) ? svc.relayUrls : [],
      pkarrUrl: svc.pkarrUrl || null,
      dnsOrigin: svc.dnsOrigin || null,
      contact: (svc.contact || "").trim(),   // 联系方式（电话/邮箱/微信…）
      location: (svc.location || "").trim(), // 地理位置（城市/地址/经纬度）
      note: (svc.note || "").trim(),         // 备注（其他属性）
    };
  }

  function list() { return LIST.slice(); }
  function get(id) { return LIST.find((s) => s.id === id) || null; }
  /** 新增或更新（按 node+mode 去重）。返回落库后的服务。 */
  function put(svc) {
    const s = norm(svc);
    const l = LIST.slice();
    const i = l.findIndex((x) => x.id === s.id || (x.node === s.node && x.mode === s.mode));
    if (i >= 0) { s.id = l[i].id; l[i] = s; } else { l.unshift(s); }
    save(l);
    return s;
  }
  function remove(id) { save(LIST.filter((s) => s.id !== id)); }

  window.NodeSvc = { list, get, put, remove, load, pubkeyOf };
})();

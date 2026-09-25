// ── 节点服务：统一的「可连接节点」记录，登录页与「节点服务」面板共用（localStorage）。
//    一条服务 = { id, name, mode, node, relayUrls, pkarrUrl, dnsOrigin }。
//    node 可为 64 位公钥 hex 或 NM_NODE_ADDR(JSON)；mode = nat|lan|selfhost。
(function () {
  const KEY = "nmspace-node-services";
  function load() { try { return JSON.parse(localStorage.getItem(KEY) || "[]"); } catch (_) { return []; } }
  function save(list) { try { localStorage.setItem(KEY, JSON.stringify(list.slice(0, 50))); } catch (_) {} }
  function uid() { return "s" + Date.now().toString(36) + Math.random().toString(36).slice(2, 6); }

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

  function list() { return load(); }
  function get(id) { return load().find((s) => s.id === id) || null; }
  /** 新增或更新（按 node+mode 去重）。返回落库后的服务。 */
  function put(svc) {
    const s = norm(svc);
    const l = load();
    const i = l.findIndex((x) => x.id === s.id || (x.node === s.node && x.mode === s.mode));
    if (i >= 0) { s.id = l[i].id; l[i] = s; } else { l.unshift(s); }
    save(l);
    return s;
  }
  function remove(id) { save(load().filter((s) => s.id !== id)); }

  /** 一次性把旧的 nmspace-servers / nmspace-pubkeys 迁入服务表（仅当服务表为空时）。 */
  function migrate() {
    if (load().length) return;
    const seeded = [];
    try {
      JSON.parse(localStorage.getItem("nmspace-servers") || "[]").forEach((s) =>
        seeded.push(norm({ name: s.name, mode: s.mode, node: s.node, relayUrls: s.relayUrls, pkarrUrl: s.pkarrUrl, dnsOrigin: s.dnsOrigin })));
    } catch (_) {}
    try {
      JSON.parse(localStorage.getItem("nmspace-pubkeys") || "[]").forEach((p) => {
        if (!seeded.some((s) => pubkeyOf(s.node) === p.key)) seeded.push(norm({ name: p.label, mode: "nat", node: p.key }));
      });
    } catch (_) {}
    if (seeded.length) save(seeded);
  }

  window.NodeSvc = { list, get, put, remove, migrate, pubkeyOf };
})();

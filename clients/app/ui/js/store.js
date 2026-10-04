// ── UI 状态持久化：把 nmspace-* / nm-* 的 localStorage 镜像到后端文件，
//    跨 webview 源变化 / dev 重建 / 重装都不丢（localStorage 仅作当前源缓存）。
//    覆盖 Storage.prototype 的方法（而非实例赋值——WebKit 上实例赋值会被当成存一个 item）。
(function () {
  const RE = /^(nmspace|nm-)/; // 需要耐久化的键前缀
  const proto = Object.getPrototypeOf(localStorage);
  const origSet = proto.setItem;
  const origRemove = proto.removeItem;

  proto.setItem = function (k, v) {
    origSet.call(this, k, v);
    if (RE.test(k) && window.NM) NM.inv("ui_kv_set", { key: k, value: String(v) }).catch(() => {});
  };
  proto.removeItem = function (k) {
    origRemove.call(this, k);
    if (RE.test(k) && window.NM) NM.inv("ui_kv_set", { key: k, value: null }).catch(() => {});
  };

  // 回灌：启动时用后端(耐久)覆盖当前源 localStorage，保证换源/重建后仍见到数据。
  async function hydrate() {
    if (!window.NM) return;
    try {
      const obj = JSON.parse((await NM.inv("ui_kv_get_all")) || "{}");
      for (const [k, v] of Object.entries(obj)) if (v != null) origSet.call(localStorage, k, v);
    } catch (_) {}
  }
  function dropDomainJson() {
    const keys = [];
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k) keys.push(k);
    }
    keys.forEach((k) => {
      if (k === "nmspace-entities" || k === "nmspace-node-services" || k === "nmspace-servers" || k === "nmspace-pubkeys" || k.indexOf("nmspace:profile:") === 0) {
        localStorage.removeItem(k);
      }
    });
  }
  window.UIStore = { hydrate, dropDomainJson };

  // 诊断：捕获未处理 JS 错误，落到后端(nm-lasterror)便于排查。
  window.addEventListener("error", function (e) {
    try {
      if (window.NM) NM.inv("ui_kv_set", { key: "nm-lasterror", value: (e.message || "") + " @ " + String(e.filename || "").split("/").pop() + ":" + (e.lineno || 0) });
    } catch (_) {}
  });
})();

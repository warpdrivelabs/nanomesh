// 实体目录、手动添加的实体、群、频道：按当前身份存在本机 SQLite。打开时先画上次的结果，远程成功后再覆盖。
(function () {
  let USER = "";
  let DATA = { directory: [], added: [], groups: [], channels: [] };
  let timer = null;
  const dirty = new Set();

  function slim(list) {
    return (list || []).map((item) => {
      const o = Object.assign({}, item);
      if (typeof o.avatar === "string" && o.avatar.indexOf("data:") === 0) delete o.avatar;
      return o;
    });
  }

  async function bind(user) {
    USER = user || "";
    DATA = { directory: [], added: [], groups: [], channels: [] };
    dirty.clear();
    if (!USER || !window.NM) return DATA;
    try {
      const obj = JSON.parse((await NM.inv("catalog_load", { user: USER })) || "{}");
      DATA = {
        directory: Array.isArray(obj.directory) ? obj.directory : [],
        added: Array.isArray(obj.added) ? obj.added : [],
        groups: Array.isArray(obj.groups) ? obj.groups : [],
        channels: Array.isArray(obj.channels) ? obj.channels : [],
      };
    } catch (_) {}
    return DATA;
  }

  function snapshot() { return DATA; }

  function flush() {
    if (!USER || !window.NM || !dirty.size) return;
    const kinds = [...dirty];
    dirty.clear();
    kinds.forEach((kind) => {
      NM.inv("catalog_save", { user: USER, kind, data: JSON.stringify(DATA[kind] || []) }).catch(() => {});
    });
  }

  function put(patch) {
    ["directory", "added", "groups", "channels"].forEach((kind) => {
      if (!patch[kind]) return;
      DATA[kind] = slim(patch[kind]);
      dirty.add(kind);
    });
    if (!USER || !window.NM) return;
    clearTimeout(timer);
    timer = setTimeout(flush, 400);
  }

  window.addEventListener("pagehide", flush);
  window.ListCache = { bind, snapshot, put, flush };
})();

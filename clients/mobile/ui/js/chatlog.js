// 会话正文在每个身份的 SQLite 库里。侧栏只使用索引里的最后一条，打开聊天时再读一窗。
(function () {
  let USER = "";
  let INDEX = { v: 1, unread: {}, convos: {} };
  let chain = Promise.resolve();

  function meta(id) { return (INDEX.convos && INDEX.convos[id]) || { count: 0, last: null }; }

  function enqueue(fn) {
    const run = chain.then(fn, fn);
    chain = run.then(() => {}, () => {});
    return run;
  }

  async function open(user) {
    USER = user || "";
    INDEX = { v: 1, unread: {}, convos: {} };
    if (!USER || !window.NM) return INDEX;
    try {
      const obj = JSON.parse((await NM.inv("chat_open", { user: USER })) || "{}");
      INDEX = {
        v: 1,
        unread: obj.unread && typeof obj.unread === "object" ? obj.unread : {},
        convos: obj.convos && typeof obj.convos === "object" ? obj.convos : {},
      };
    } catch (_) {}
    return INDEX;
  }

  function note(id, msg, add) {
    const prev = meta(id);
    const count = Math.min(10000, (prev.count || 0) + add);
    const newer = !prev.last || (msg && (msg.ts || 0) >= (prev.last.ts || 0));
    INDEX.convos[id] = { count, last: newer ? (msg || prev.last) : prev.last };
  }

  async function tail(id, limit) {
    if (!USER || !id) return [];
    try { return JSON.parse(await NM.inv("chat_tail", { user: USER, conv: id, limit: limit || 60 })) || []; }
    catch (_) { return []; }
  }
  async function before(id, index, limit) {
    if (!USER || !id || !index) return [];
    try { return JSON.parse(await NM.inv("chat_before", { user: USER, conv: id, before: index, limit: limit || 40 })) || []; }
    catch (_) { return []; }
  }
  async function around(id, msgId, limit) {
    if (!USER || !id) return { start: 0, msgs: [] };
    try { return JSON.parse(await NM.inv("chat_around", { user: USER, conv: id, id: msgId, limit: limit || 80 })) || { start: 0, msgs: [] }; }
    catch (_) { return { start: 0, msgs: [] }; }
  }
  function pack(msg) { return window.slimMsg ? slimMsg(msg) : msg; }

  async function append(id, msg) {
    if (!USER || !id) return false;
    const slim = pack(msg);
    return enqueue(async () => {
      try {
        const ok = await NM.inv("chat_append", { user: USER, conv: id, msg: JSON.stringify(slim) });
        if (ok) note(id, slim, 1);
        return !!ok;
      } catch (_) { return false; }
    });
  }
  async function appendMany(id, msgs) {
    if (!USER || !id || !(msgs || []).length) return 0;
    const slim = msgs.map(pack);
    return enqueue(async () => {
      try {
        const n = await NM.inv("chat_append_many", { user: USER, conv: id, data: JSON.stringify(slim) }) || 0;
        if (n) note(id, slim[slim.length - 1], n);
        return n;
      } catch (_) { return 0; }
    });
  }
  async function search(id, q, limit) {
    if (!USER || !id) return [];
    try { return JSON.parse(await NM.inv("chat_search", { user: USER, conv: id, q: q || "", limit: limit || 80 })) || []; }
    catch (_) { return []; }
  }
  function setUnread(map) {
    INDEX.unread = map || {};
    if (!USER || !window.NM) return;
    enqueue(() => NM.inv("chat_unread_save", { user: USER, data: JSON.stringify(INDEX.unread) }).catch(() => {}));
  }

  window.ChatLog = {
    open, index: () => INDEX, meta, unread: () => INDEX.unread || {}, setUnread,
    tail, before, around, append, appendMany, search,
  };
})();

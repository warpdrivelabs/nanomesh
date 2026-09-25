// ── nmspace 后端桥：Tauri 壳唯一通路（withGlobalTauri: window.__TAURI__）。
//    nmspace 用「一命令一函数」的命令面（connect/send_to/directory_query/my_id/disconnect），
//    不同于 cmx-agent 的单 dispatch("agent")。收消息走 Tauri 事件 core://event。
(function () {
  function core() {
    return (window.__TAURI__ && window.__TAURI__.core) ? window.__TAURI__.core : null;
  }
  /** 调用一个后端命令；无 Tauri（纯浏览器预览）时抛错。 */
  async function inv(cmd, args) {
    const c = core();
    if (!c) throw new Error("仅桌面版（Tauri 壳）可用：window.__TAURI__ 不可用");
    return await c.invoke(cmd, args || {});
  }
  /** 订阅后端事件 core://event（{type:"message", msg:{id,from,body,ts}}）。返回取消函数。 */
  async function onCoreEvent(cb) {
    if (!(window.__TAURI__ && window.__TAURI__.event)) return () => {};
    return await window.__TAURI__.event.listen("core://event", (e) => {
      try { cb(e.payload); } catch (err) { console.error("core://event handler", err); }
    });
  }
  window.NM = { inv, onCoreEvent, hasTauri: () => !!core() };
})();

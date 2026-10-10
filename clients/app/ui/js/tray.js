// ── 系统托盘 / 系统通知 / 「通知与托盘」设置 ──
//    托盘本身在 Rust（src-tauri/src/tray.rs）。这里负责：把未读与在线状态推给托盘、
//    执行托盘菜单回传的操作、决定何时弹系统通知（同会话合并），以及设置对话框。
(function () {
  const has = () => !!(window.__TAURI__ && window.__TAURI__.core);
  let OS = "";
  let PREFS = null;
  let available = false;
  let syncTimer = null;
  let lastSent = "";
  let focused = document.hasFocus();
  let pendingJump = null; // { key, ts }：弹过通知后回到窗口时跳到该会话
  const burst = {};      // 同会话通知合并：key -> { until, extra, last, title }
  const BURST_MS = 4000;

  function win() { return window.__TAURI__ && window.__TAURI__.window ? window.__TAURI__.window.getCurrentWindow() : null; }
  function isFocused() { return focused && !document.hidden; }

  function convName(id) {
    if (window.Groups && Groups.byId(id)) return Groups.byId(id).name || "群聊";
    if (window.Channels && Channels.byId(id)) return Channels.byId(id).name || "频道";
    return window.entityName ? entityName(id) : id.slice(0, 8);
  }

  function snapshot() {
    const s = window.imSnapshot ? imSnapshot() : { myId: "", unread: {}, convos: {} };
    const cur = window.Identity ? Identity.current() : "";
    const loggedIn = !!(s.myId && cur);
    const list = Object.keys(s.unread || {}).filter((id) => (s.unread[id] || 0) > 0).map((id) => {
      const last = ((s.convos || {})[id] || []).slice(-1)[0];
      return { id, name: convName(id), count: s.unread[id], ts: last ? last.ts || 0 : 0 };
    }).sort((a, b) => b.ts - a.ts);
    return {
      loggedIn,
      user: loggedIn ? Identity.label(cur) : "",
      status: loggedIn && window.Profile ? (Profile.get(cur).status || "online") : "",
      connected: loggedIn,
      unread: list.reduce((n, c) => n + c.count, 0),
      convos: list.slice(0, 8).map(({ id, name, count }) => ({ id, name, count })),
      tzOffsetMin: -new Date().getTimezoneOffset(),
    };
  }

  function sync() {
    if (!available) return;
    clearTimeout(syncTimer);
    syncTimer = setTimeout(() => {
      const model = snapshot();
      const key = JSON.stringify(model);
      if (key === lastSent) return;
      lastSent = key;
      NM.inv("tray_sync", { model }).catch(() => {});
    }, 120);
  }

  // ── 系统通知 ──
  function preview(m) {
    let t = window.Composer ? Composer.preview(m) : (m.body || "");
    t = String(t || "").replace(/\s+/g, " ").trim();
    return t.length > 80 ? t.slice(0, 80) + "…" : (t || "[新消息]");
  }
  function titleBody(key, m) {
    const sender = window.entityName ? entityName(m.from) : m.from.slice(0, 8);
    if (m.group || m.channel) return { title: convName(key), body: `${sender}：${preview(m)}` };
    return { title: sender, body: preview(m) };
  }
  async function show(title, body, key) {
    try {
      const shown = await NM.inv("tray_notify", { title, body });
      if (shown) pendingJump = { key, ts: Date.now() };
    } catch (_) {}
  }
  function onMessage(key, m) {
    if (!available || isFocused()) return;
    const now = Date.now();
    const b = burst[key];
    if (b && now < b.until) {
      b.extra += 1;
      b.last = m;
      return;
    }
    const { title, body } = titleBody(key, m);
    show(title, body, key);
    const entry = { until: now + BURST_MS, extra: 0, last: m, title };
    burst[key] = entry;
    setTimeout(() => {
      if (burst[key] !== entry) return;
      delete burst[key];
      if (entry.extra > 0 && !isFocused()) {
        const tb = titleBody(key, entry.last);
        show(entry.title, `又收到 ${entry.extra} 条新消息 · ${tb.body}`, key);
      }
    }, BURST_MS);
  }

  function openLatest() {
    const s = window.imSnapshot ? imSnapshot() : null;
    if (!s) return;
    let best = null, bestTs = -1;
    for (const id of Object.keys(s.unread || {})) {
      if (!(s.unread[id] > 0)) continue;
      const last = ((s.convos || {})[id] || []).slice(-1)[0];
      const ts = last ? last.ts || 0 : 0;
      if (ts > bestTs) { best = id; bestTs = ts; }
    }
    if (best) imOpen(best);
  }

  function onFocus() {
    focused = true;
    const s = window.imSnapshot ? imSnapshot() : null;
    if (pendingJump && Date.now() - pendingJump.ts < 120000 && s && (s.unread[pendingJump.key] || 0) > 0) {
      const k = pendingJump.key;
      pendingJump = null;
      imOpen(k);
      return;
    }
    pendingJump = null;
    if (s && s.active && window.imMarkRead) imMarkRead(s.active);
  }

  async function onAction(p) {
    const a = p && p.action;
    switch (a) {
      case "open": if (p.arg) imOpen(p.arg); break;
      case "open_latest": openLatest(); break;
      case "readall": if (window.imMarkAllRead) imMarkAllRead(); break;
      case "status": if (window.setMyStatus && p.arg) await setMyStatus(p.arg); break;
      case "settings": openSettings(); break;
      case "about": openSettings("about"); break;
      case "prefs": PREFS = Object.assign(PREFS || {}, p.arg || {}); renderPrefs(); break;
      case "quit":
        try { if (window.imFlush) await imFlush(); } catch (_) {}
        try { await NM.inv("disconnect"); } catch (_) {} // 退出前优雅下线，避免节点持「假在线」会话 10s 而丢消息
        try { await NM.inv("app_quit"); } catch (_) {}
        break;
    }
  }

  // ── 设置对话框 ──
  const $ = (id) => document.getElementById(id);
  const IS_MAC = () => OS === "macos";
  function say(t, ok) { const m = $("tp-msg"); if (m) { m.textContent = t || ""; m.classList.toggle("ok", !!ok); } }

  function hkLabel(acc) {
    if (!acc) return "未设置";
    const mac = IS_MAC();
    const map = {
      commandorcontrol: mac ? "⌘" : "Ctrl", cmdorctrl: mac ? "⌘" : "Ctrl", command: "⌘", cmd: "⌘", super: mac ? "⌘" : "Win",
      control: mac ? "⌃" : "Ctrl", ctrl: mac ? "⌃" : "Ctrl", alt: mac ? "⌥" : "Alt", option: "⌥", shift: mac ? "⇧" : "Shift",
    };
    return acc.split("+").map((k) => {
      const v = map[k.toLowerCase()];
      if (v) return `<kbd>${v}</kbd>`;
      return `<kbd>${k.replace(/^Key/, "").replace(/^Digit/, "")}</kbd>`;
    }).join(mac ? "" : "<i>+</i>");
  }

  function muteText() {
    const u = PREFS ? PREFS.muteUntil : 0;
    if (u === -1) return { on: true, t: "免打扰已开启，直到你手动关闭" };
    if (u > Date.now()) {
      const d = new Date(u);
      const sameDay = d.toDateString() === new Date().toDateString();
      const hm = String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
      return { on: true, t: `免打扰中，${sameDay ? "今天" : "明天"} ${hm} 自动恢复` };
    }
    return { on: false, t: "当前未开启免打扰，新消息会正常提醒" };
  }

  function renderPrefs() {
    if (!PREFS) return;
    document.querySelectorAll("#tray-modal [data-pref]").forEach((el) => { el.checked = !!PREFS[el.dataset.pref]; });
    const mt = muteText();
    const now = $("tp-mute-now");
    if (now) { now.textContent = mt.t; now.classList.toggle("on", mt.on); }
    const off = document.querySelector("#tray-modal .mute-off");
    if (off) off.disabled = !mt.on;
    const hkOn = $("tp-hk-on");
    if (hkOn) hkOn.checked = !!PREFS.hotkeyOn;
    const rec = $("tp-hk-rec");
    if (rec && !rec.classList.contains("rec")) { rec.innerHTML = hkLabel(PREFS.hotkey); rec.disabled = !PREFS.hotkeyOn; }
  }

  async function setPref(patch) {
    try {
      PREFS = await NM.inv("tray_prefs_set", { patch });
      renderPrefs();
      say("已保存", true);
      return true;
    } catch (e) {
      say(String(e && e.message ? e.message : e));
      renderPrefs();
      return false;
    }
  }

  function muteUntil(kind) {
    const now = Date.now();
    if (kind === "30m") return now + 30 * 60000;
    if (kind === "1h") return now + 3600000;
    if (kind === "8h") return now + 8 * 3600000;
    if (kind === "forever") return -1;
    if (kind === "tomorrow") {
      const d = new Date(); d.setHours(8, 0, 0, 0);
      if (d.getTime() <= now) d.setDate(d.getDate() + 1);
      return d.getTime();
    }
    return 0;
  }

  // 按下的组合键 → 全局快捷键加速器字符串（如 "Control+Alt+KeyZ"）
  function accelOf(e) {
    const mods = [];
    if (e.ctrlKey) mods.push("Control");
    if (e.altKey) mods.push("Alt");
    if (e.shiftKey) mods.push("Shift");
    if (e.metaKey) mods.push(IS_MAC() ? "Command" : "Super");
    const c = e.code || "";
    if (!c || /^(Control|Alt|Shift|Meta|OS)(Left|Right)?$/.test(c)) return { mods, key: "" };
    const key = c.startsWith("Key") ? c.slice(3) : c.startsWith("Digit") ? c.slice(5) : c;
    return { mods, key };
  }

  function bindSettings() {
    const modal = $("tray-modal");
    if (!modal || modal._bound) return;
    modal._bound = true;
    modal.querySelectorAll("[data-pref]").forEach((el) => el.addEventListener("change", () => setPref({ [el.dataset.pref]: el.checked })));
    modal.querySelectorAll("[data-mute]").forEach((b) => b.addEventListener("click", async () => {
      const k = b.dataset.mute;
      if (await setPref({ muteUntil: muteUntil(k) })) say(k === "off" ? "已恢复通知" : "已开启免打扰", true);
    }));
    $("tp-test").addEventListener("click", async () => {
      try { await NM.inv("tray_test_notify"); say("已发送测试通知；没看到的话请检查系统通知权限", true); }
      catch (e) { say(String(e)); }
    });
    $("tp-hk-on").addEventListener("change", (e) => setPref({ hotkeyOn: e.target.checked }));
    $("tp-hk-reset").addEventListener("click", () => setPref({ hotkey: "CommandOrControl+Alt+Z", hotkeyOn: true }));
    const rec = $("tp-hk-rec");
    let recording = false;
    const stop = () => { recording = false; rec.classList.remove("rec"); window.removeEventListener("keydown", onKey, true); renderPrefs(); };
    const onKey = async (e) => {
      if (!recording) return;
      e.preventDefault(); e.stopPropagation(); e.stopImmediatePropagation();
      if (e.key === "Escape" && !e.ctrlKey && !e.altKey && !e.metaKey && !e.shiftKey) { stop(); say(""); return; }
      const { mods, key } = accelOf(e);
      if (!key) { rec.innerHTML = mods.length ? hkLabel(mods.join("+")) + "<i>…</i>" : "请按下组合键…"; return; }
      if (!mods.length || (mods.length === 1 && mods[0] === "Shift")) { say("需要包含 Ctrl、Alt 或 ⌘ 之一"); return; }
      const acc = [...mods, key].join("+");
      stop();
      if (await setPref({ hotkey: acc, hotkeyOn: true })) say("快捷键已更新", true);
    };
    rec.addEventListener("click", () => {
      if (recording) { stop(); return; }
      recording = true;
      rec.classList.add("rec");
      rec.textContent = "请按下组合键…";
      say("");
      window.addEventListener("keydown", onKey, true);
    });
    modal.addEventListener("click", (e) => { if (recording && !e.target.closest("#tp-hk-rec")) stop(); });
  }

  function applyPlatformText() {
    const mac = IS_MAC();
    const flash = $("tp-flash-row");
    if (flash) flash.hidden = mac;
    if (mac) {
      $("tp-badge-t").textContent = "Dock 未读角标";
      $("tp-badge-d").textContent = "在 Dock 图标上显示未读消息数";
      $("tp-tray-d").textContent = "程序可以常驻菜单栏，关掉窗口也不会错过消息。";
      $("tp-close-d").textContent = "关闭后可点菜单栏图标或 Dock 图标重新打开；关掉此项则关闭即退出";
    }
    const tips = $("tp-tray-tips");
    if (tips) {
      const rows = mac
        ? ["点击菜单栏图标：显示或隐藏窗口；有未读时直接打开最新会话", "右键菜单栏图标：状态、免打扰、未读会话等快捷菜单", "菜单栏图标旁的数字就是未读消息数"]
        : OS === "windows"
          ? ["单击托盘图标：显示或隐藏窗口；闪烁时直接打开最新会话", "右键托盘图标：状态、免打扰、未读会话等快捷菜单", "如果托盘图标被收进「^」里，可把它拖到任务栏上常驻显示"]
          : ["单击托盘图标：显示或隐藏窗口（部分桌面环境只支持菜单）", "托盘菜单：状态、免打扰、未读会话等", "GNOME 需要安装 AppIndicator 扩展才能显示托盘图标"];
      tips.innerHTML = `<div class="set-tips-t">使用提示</div>` + rows.map((r) => `<div class="set-tip">${r}</div>`).join("");
    }
  }

  async function openSettings(pane) {
    if (!available) return;
    bindSettings();
    applyPlatformText();
    try { PREFS = await NM.inv("tray_prefs_get"); } catch (_) {}
    renderPrefs();
    say("");
    const modal = $("tray-modal");
    if (window.dlgShowPane) dlgShowPane(modal.querySelector(".dlg"), pane || "notify");
    try {
      const v = window.__TAURI__.app ? await window.__TAURI__.app.getVersion() : "";
      $("tp-ver").textContent = v ? "版本 " + v : "";
    } catch (_) {}
    modal.classList.add("on");
  }

  async function init() {
    if (!has()) return;
    try { OS = await NM.inv("platform"); } catch (_) {}
    if (OS !== "macos" && OS !== "windows" && OS !== "linux") return;
    try { PREFS = await NM.inv("tray_prefs_get"); available = true; } catch (_) { return; }
    await NM.onEvent("tray://action", onAction);
    const w = win();
    if (w && w.onFocusChanged) {
      await w.onFocusChanged(({ payload }) => { if (payload) onFocus(); else focused = false; });
    } else {
      window.addEventListener("focus", onFocus);
      window.addEventListener("blur", () => { focused = false; });
    }
    document.addEventListener("visibilitychange", () => { if (!document.hidden && document.hasFocus()) onFocus(); });
    sync();
  }

  window.Tray = {
    sync, onMessage, openSettings,
    focused: isFocused,
    available: () => available,
  };
  init();
})();

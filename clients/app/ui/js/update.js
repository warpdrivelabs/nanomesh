// ── 应用内更新：启动静默检查 + 发现更新弹窗 + 设置 → 关于手动检查 ──
// 行为对齐 TrueMate：启动 5 秒后静默检查；有更新直接弹确认框；
// 同一版本 30 分钟内静默检查不重复弹（手动检查不受限）；force 更新无「稍后」且不可关闭。
(function () {
  const btn = document.getElementById("tp-update-btn");
  const status = document.getElementById("tp-update-status");
  if (!btn || !status) return;

  const SNOOZE_KEY = "nmspace-update-snooze";
  const SNOOZE_MS = 30 * 60 * 1000;

  let info = null;          // {version, notes, force}
  let busy = false;         // 下载安装进行中
  let listening = false;    // update-progress 只挂一次
  let modal = null;         // {root, notes, msg, installBtn, laterBtn, force, esc}
  let autoChecked = false;  // 启动自动检查只跑一次

  const hasTauri = () => !!(window.NM && window.NM.hasTauri && window.NM.hasTauri());
  const setStatus = (text) => { status.textContent = text; };
  const versionText = (v) => (v ? " " + v : "");

  function snoozed(version) {
    try {
      const s = JSON.parse(localStorage.getItem(SNOOZE_KEY) || "null");
      return !!(s && s.version === version && Date.now() - s.ts < SNOOZE_MS);
    } catch (_) {
      return false;
    }
  }

  function snooze(version) {
    try {
      localStorage.setItem(SNOOZE_KEY, JSON.stringify({ version: version, ts: Date.now() }));
    } catch (_) {}
  }

  function renderAvailable(update) {
    info = update;
    const ver = versionText(update.version);
    setStatus(`发现新版本${ver}${update.notes ? `：${update.notes}` : ""}`);
    btn.textContent = `下载并安装${ver}`;
  }

  function renderCurrent() {
    info = null;
    setStatus("当前已是最新版本");
    btn.textContent = "检查更新";
  }

  async function ensureProgressListener() {
    if (listening || !window.NM || !window.NM.onEvent) return;
    listening = true;
    try {
      await window.NM.onEvent("update-progress", (p) => {
        if (!p) return;
        const received = Number(p.received) || 0;
        const total = Number(p.total) || 0;
        let text;
        if (total > 0) {
          const pct = Math.min(100, Math.round((received / total) * 100));
          text = `下载中 ${pct}%（${Math.round(received / 1048576)} / ${Math.round(total / 1048576)} MB）`;
        } else {
          text = `下载中 ${Math.round(received / 1024)} KB`;
        }
        setStatus(text);
        if (modal && modal.msg) modal.msg.textContent = text;
      });
    } catch (_) {
      listening = false;
    }
  }

  function closeModal() {
    if (!modal) return;
    if (modal.force) return;
    if (modal.esc) document.removeEventListener("keydown", modal.esc);
    modal.root.remove();
    modal = null;
  }

  function showUpdateDialog(update) {
    if (modal) return;
    // 弹出即记已提示：30 分钟内静默检查不重复弹同版本；手动检查不受此限制。
    snooze(update.version);

    const force = !!update.force;
    const root = document.createElement("div");
    root.className = "sec-overlay on";
    root.setAttribute("role", "dialog");
    root.setAttribute("aria-modal", "true");

    const dlg = document.createElement("div");
    dlg.className = "dlg";
    dlg.style.cssText = "width:min(440px,92vw);aspect-ratio:auto;max-height:none";

    const head = document.createElement("div");
    head.className = "dlg-head";
    const title = document.createElement("span");
    title.className = "dlg-title";
    title.textContent = `发现新版本${versionText(update.version)}`;
    const sub = document.createElement("span");
    sub.className = "dlg-sub";
    sub.textContent = "NANO MESH 更新";
    head.appendChild(title);
    head.appendChild(sub);

    let closeBtn = null;
    if (!force) {
      closeBtn = document.createElement("button");
      closeBtn.type = "button";
      closeBtn.className = "sec-x";
      closeBtn.textContent = "×";
      closeBtn.title = "稍后";
      closeBtn.addEventListener("click", closeModal);
      head.appendChild(closeBtn);
    }

    const main = document.createElement("div");
    main.className = "dlg-main";
    main.style.cssText = "display:block;padding:18px 20px 8px";
    const notes = document.createElement("div");
    notes.className = "pane-d";
    notes.style.cssText = "white-space:pre-wrap;line-height:1.75";
    notes.textContent = update.notes ? update.notes : "暂无更新说明";
    const msg = document.createElement("div");
    msg.className = "sec-msg";
    msg.style.paddingTop = "10px";
    main.appendChild(notes);
    main.appendChild(msg);

    const foot = document.createElement("div");
    foot.className = "dlg-foot";
    const spacer = document.createElement("div");
    spacer.className = "sec-msg";
    foot.appendChild(spacer);

    let laterBtn = null;
    if (!force) {
      laterBtn = document.createElement("button");
      laterBtn.type = "button";
      laterBtn.className = "ns-btn";
      laterBtn.textContent = "稍后";
      laterBtn.addEventListener("click", closeModal);
      foot.appendChild(laterBtn);
    }

    const installBtn = document.createElement("button");
    installBtn.type = "button";
    installBtn.className = "ns-btn ns-primary";
    installBtn.textContent = "立即更新";
    installBtn.addEventListener("click", () => install(false));
    foot.appendChild(installBtn);

    dlg.appendChild(head);
    dlg.appendChild(main);
    dlg.appendChild(foot);
    root.appendChild(dlg);
    document.body.appendChild(root);

    const esc = (e) => {
      if (e.key === "Escape" && !force) closeModal();
    };
    document.addEventListener("keydown", esc);
    root.addEventListener("click", (e) => {
      if (!force && e.target === root) closeModal();
    });

    modal = { root, notes, msg, installBtn, laterBtn, force, esc };
  }

  async function check(silent) {
    if (!hasTauri()) {
      if (!silent) setStatus("仅桌面版支持检查更新");
      return;
    }
    if (busy) return;
    btn.disabled = true;
    if (!silent) setStatus("检查中…");
    try {
      const r = await window.NM.inv("app_check_update");
      const available = !!(r && (r.available || r.update_available));
      if (available) {
        const update = {
          version: r.version,
          notes: r.notes || "",
          force: !!r.force,
        };
        renderAvailable(update);
        if (!silent || update.force || !snoozed(update.version)) {
          showUpdateDialog(update);
        }
      } else {
        renderCurrent();
      }
    } catch (e) {
      info = null;
      btn.textContent = "检查更新";
      if (!silent) setStatus(`检查更新失败：${(e && e.message) || e}`);
    } finally {
      btn.disabled = false;
    }
  }

  async function install(auto) {
    if (!hasTauri() || busy || !info) return;
    busy = true;
    btn.disabled = true;
    if (modal) {
      modal.installBtn.disabled = true;
      if (modal.laterBtn) modal.laterBtn.disabled = true;
      modal.msg.textContent = "准备下载…";
    }
    setStatus("准备下载…");
    await ensureProgressListener();
    try {
      await window.NM.inv("app_download_install");
      // 安装成功后应用会重启，正常不会返回。
      busy = false;
      btn.disabled = false;
      renderCurrent();
      closeModal();
    } catch (e) {
      busy = false;
      btn.disabled = false;
      const text = `更新失败：${(e && e.message) || e}`;
      setStatus(text);
      if (modal) {
        modal.msg.textContent = text;
        modal.installBtn.disabled = false;
        modal.installBtn.textContent = "重试更新";
        if (modal.laterBtn) modal.laterBtn.disabled = false;
      }
      if (auto && window.toast) window.toast(text);
    }
  }

  btn.addEventListener("click", () => {
    if (info) install(false);
    else check(false);
  });

  function mainViewVisible() {
    const view = document.getElementById("main-view");
    if (!view) return true;
    return getComputedStyle(view).display !== "none";
  }

  function autoCheck() {
    if (autoChecked) return;
    if (!hasTauri()) return;
    if (document.visibilityState === "hidden") return;
    if (!mainViewVisible()) return;
    autoChecked = true;
    check(true);
  }
  setTimeout(autoCheck, 5000);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") autoCheck();
  });
  const mainView = document.getElementById("main-view");
  if (mainView && window.MutationObserver) {
    new MutationObserver(autoCheck).observe(mainView, {
      attributes: true,
      attributeFilter: ["style", "class"],
    });
  }
})();

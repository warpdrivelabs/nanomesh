// 手机壳：四项底栏、关系分段、我、页面栈。业务仍由 im/groups/channels 画进原有节点。
(function () {
  const TITLES = { chats: "消息", people: "关系", feed: "动态", me: "我" };
  const SEG_TITLE = { entities: "实体", groups: "群", channels: "频道" };
  let root = "chats";
  let depth = 0;

  function tablet() { return document.body.classList.contains("tablet"); }
  function applyTablet() {
    document.body.classList.toggle("tablet", Math.min(window.innerWidth, window.innerHeight) >= 700);
  }

  function segName() {
    const on = document.querySelector("#people-seg button.on");
    return (on && on.dataset.seg) || "entities";
  }

  function setTitle(text) {
    const el = document.getElementById("top-title");
    if (el) el.textContent = text || "";
  }

  function syncChrome() {
    const open = !!(window.Tabs && Tabs.active && Tabs.active());
    const sub = document.body.classList.contains("me-sub");
    document.body.classList.toggle("stacked", open);
    const back = document.getElementById("top-back");
    if (back) back.hidden = !open && !sub;
    if (open) setTitle("");
    else if (sub) setTitle("节点服务");
    else if (root === "people") setTitle(SEG_TITLE[segName()] || "关系");
    else setTitle(TITLES[root] || "");
    document.querySelectorAll("#phone-tabs .tab").forEach((b) => b.classList.toggle("on", b.dataset.root === root));
  }

  function showRoot(name) {
    root = name;
    if (window.Tabs && Tabs.active && Tabs.active()) Tabs.closeAll();
    document.body.classList.remove("me-sub");
    const nodes = document.getElementById("panel-nodesvc");
    if (nodes) nodes.classList.remove("on");
    const home = document.getElementById("me-home");
    if (home) home.hidden = false;
    if (history.state && history.state.nm) history.replaceState({}, "");
    depth = 0;
    document.querySelectorAll(".root").forEach((el) => el.classList.toggle("on", el.dataset.root === name));
    if (name === "chats" && typeof showPanel === "function") showPanel("im");
    if (name === "people" && typeof showPanel === "function") showPanel(segName());
    syncChrome();
    paintMe();
  }

  function showMeNodes() {
    const home = document.getElementById("me-home");
    const nodes = document.getElementById("panel-nodesvc");
    if (home) home.hidden = true;
    if (nodes) nodes.classList.add("on");
    document.body.classList.add("me-sub");
    if (typeof showPanel === "function") showPanel("nodesvc");
    if (!history.state || !history.state.nm) {
      depth += 1;
      history.pushState({ nm: depth }, "");
    }
    syncChrome();
  }

  function hideMeSub() {
    document.body.classList.remove("me-sub");
    const home = document.getElementById("me-home");
    const nodes = document.getElementById("panel-nodesvc");
    if (home) home.hidden = false;
    if (nodes) nodes.classList.remove("on");
    syncChrome();
  }

  function hookTabs() {
    if (!window.Tabs || Tabs._shell) return;
    ["open", "activate", "close", "closeAll"].forEach((name) => {
      const orig = Tabs[name];
      if (!orig) return;
      Tabs[name] = function () {
        const before = Tabs.active && Tabs.active();
        const out = orig.apply(this, arguments);
        const after = Tabs.active && Tabs.active();
        if (!before && after && (!history.state || !history.state.nm)) {
          depth += 1;
          history.pushState({ nm: depth }, "");
        }
        syncChrome();
        return out;
      };
    });
    Tabs._shell = true;
  }

  function paintMe() {
    const themeBtn = document.getElementById("me-theme");
    if (themeBtn) themeBtn.textContent = document.documentElement.getAttribute("data-theme") === "light" ? "外观 · 浅色" : "外观 · 深色";
    const langBtn = document.getElementById("me-lang");
    if (langBtn) langBtn.textContent = window.nmLocale && nmLocale() === "en" ? "语言 · English" : "语言 · 中文";
    const box = document.getElementById("me-status");
    const cur = window.Identity && Identity.current && Identity.current();
    if (!box || !cur || !window.Profile) return;
    const st = (Profile.get(cur).status || "online");
    box.innerHTML = ["online", "away", "busy", "dnd"].map((s) =>
      `<button type="button" class="me-st${s === st ? " on" : ""}" data-st="${s}"><span class="me-dot" style="background:${Profile.presenceColor(s)}"></span>${Profile.presenceLabel(s)}</button>`
    ).join("");
  }

  function undrill(dlg) {
    if (!dlg) return;
    dlg.classList.remove("drilled");
    const title = dlg.querySelector(".dlg-title");
    if (title && title.dataset.base) title.textContent = title.dataset.base;
  }
  function closeSheet(overlay) {
    if (!overlay) return;
    overlay.classList.remove("on");
    delete overlay.dataset.pushed;
    undrill(overlay.querySelector(".dlg"));
  }
  function watchSheets() {
    const mo = new MutationObserver(() => {
      document.querySelectorAll(".sec-overlay.on").forEach((m) => {
        if (m.dataset.pushed) return;
        m.dataset.pushed = "1";
        depth += 1;
        history.pushState({ nm: depth, nmKind: "sheet" }, "");
      });
    });
    mo.observe(document.body, { subtree: true, attributes: true, attributeFilter: ["class"] });
    document.addEventListener("click", (e) => {
      if (!document.body.classList.contains("mobile")) return;
      const tab = e.target.closest(".dlg-nav button[data-pane]");
      if (tab) {
        const dlg = tab.closest(".dlg");
        const title = dlg && dlg.querySelector(".dlg-title");
        if (title) {
          if (!title.dataset.base) title.dataset.base = title.textContent.trim();
          title.textContent = tab.innerText.replace(/\s+/g, " ").trim();
        }
        if (dlg) dlg.classList.add("drilled");
        if (!history.state || history.state.nmKind !== "set") {
          depth += 1;
          history.pushState({ nm: depth, nmKind: "set" }, "");
        }
        return;
      }
      const shut = e.target.closest("[data-close], .sec-overlay.on .sec-x");
      if (!shut) return;
      const overlay = shut.closest(".sec-overlay");
      const dlg = overlay && overlay.querySelector(".dlg");
      if (dlg && dlg.classList.contains("drilled")) {
        e.preventDefault();
        e.stopPropagation();
        if (history.state && history.state.nmKind === "set") history.back();
        else undrill(dlg);
      } else if (overlay && overlay.dataset.pushed && shut.hasAttribute("data-close")) {
        e.preventDefault();
        e.stopPropagation();
        history.back();
      }
    }, true);
  }
  function goBack() {
    if (depth > 0) { history.back(); return; }
    if (window.Tabs && Tabs.active && Tabs.active()) Tabs.close(Tabs.active());
    else hideMeSub();
  }

  function bind() {
    applyTablet();
    window.addEventListener("resize", applyTablet);
    document.getElementById("phone-tabs").addEventListener("click", (e) => {
      const b = e.target.closest("[data-root]");
      if (b) showRoot(b.dataset.root);
    });
    const seg = document.getElementById("people-seg");
    if (seg) seg.addEventListener("click", (e) => {
      const b = e.target.closest("[data-seg]");
      if (!b) return;
      seg.querySelectorAll("button").forEach((x) => x.classList.toggle("on", x === b));
      if (typeof showPanel === "function") showPanel(b.dataset.seg);
      syncChrome();
    });
    document.getElementById("top-back").addEventListener("click", goBack);
    const me = document.getElementById("me-home");
    if (me) me.addEventListener("click", async (e) => {
      const st = e.target.closest("[data-st]");
      if (st && window.setMyStatus) { await setMyStatus(st.dataset.st); paintMe(); return; }
      const id = e.target.closest("button") && e.target.closest("button").id;
      if (id === "me-profile" && typeof openProfileModal === "function") openProfileModal();
      else if (id === "me-nodes") showMeNodes();
      else if (id === "me-sec" && typeof openSecModal === "function") openSecModal();
      else if (id === "me-devices" && window.Devices) Devices.open();
      else if (id === "me-notify" && window.Tray && Tray.openSettings) Tray.openSettings();
      else if (id === "me-theme" && typeof toggleTheme === "function") { toggleTheme(); paintMe(); }
      else if (id === "me-lang" && window.nmSetLocale) { nmSetLocale(window.nmLocale && nmLocale() === "en" ? "zh-CN" : "en"); paintMe(); }
      else if (id === "me-logout" && typeof imDisconnect === "function") imDisconnect();
    });
    window.addEventListener("popstate", () => {
      depth = Math.max(0, depth - 1);
      const drilled = document.querySelector(".sec-overlay.on .dlg.drilled");
      if (drilled) { undrill(drilled); return; }
      const sheet = document.querySelector(".sec-overlay.on");
      if (sheet) { closeSheet(sheet); return; }
      if (window.Tabs && Tabs.active && Tabs.active()) Tabs.close(Tabs.active());
      else hideMeSub();
    });
    watchSheets();
    if (window.visualViewport) {
      const fit = () => {
        const inset = Math.max(0, window.innerHeight - visualViewport.height - visualViewport.offsetTop);
        document.documentElement.style.setProperty("--kb", inset + "px");
      };
      visualViewport.addEventListener("resize", fit);
      visualViewport.addEventListener("scroll", fit);
    }
    hookTabs();
    showRoot("chats");
    const prev = window.updateUserChip;
    window.updateUserChip = function () { if (prev) prev(); paintMe(); };
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", bind);
  else bind();
})();

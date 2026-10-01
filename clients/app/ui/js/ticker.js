// 标题栏跑马灯。勾选结果记在本机，数字和要闻只来自接口，失败的一路不显示。
(function () {
  const KEY = "nmspace-ticker";
  const DEFAULTS = { fx: true, crypto: true, news: true, weather: true, speed: 5 };

  function cfg() {
    try { return Object.assign({}, DEFAULTS, JSON.parse(localStorage.getItem(KEY) || "{}")); }
    catch (_) { return Object.assign({}, DEFAULTS); }
  }
  function save(c) {
    try { localStorage.setItem(KEY, JSON.stringify(c)); } catch (_) {}
  }
  function durationSec(speed) {
    const n = Math.min(10, Math.max(1, Number(speed) || 5));
    return 78 - n * 6;
  }
  function applySpeed(speed) {
    const track = document.getElementById("tb-marquee-track");
    if (!track) return;
    const sec = durationSec(speed) + "s";
    track.style.animationName = "none";
    void track.offsetWidth;
    track.style.animationName = "tb-marquee";
    track.style.animationDuration = sec;
    track.style.animationTimingFunction = "linear";
    track.style.animationIterationCount = "infinite";
  }
  function paintChecks() {
    const c = cfg();
    document.querySelectorAll("[data-ticker]").forEach((el) => { el.checked = !!c[el.dataset.ticker]; });
    const bar = document.getElementById("tb-ticker-speed");
    if (bar) bar.value = String(c.speed || 5);
    applySpeed(c.speed);
  }
  let ITEMS = [];
  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[ch]));
  }
  function setTrack(text) {
    const track = document.getElementById("tb-marquee-track");
    if (!track) return;
    const safe = esc(text);
    track.innerHTML = `<span>${safe}</span><span aria-hidden="true">${safe}</span>`;
  }
  function itemHtml(items) {
    return items.map((it, i) => {
      const sep = i ? `<span class="tb-tick-sep">·</span>` : "";
      return `${sep}<button type="button" class="tb-tick" data-tick="${esc(it.id)}">${esc(it.label)}</button>`;
    }).join("");
  }
  function setItems(items) {
    const track = document.getElementById("tb-marquee-track");
    if (!track) return;
    const html = itemHtml(items);
    track.innerHTML = `<span>${html}</span><span aria-hidden="true">${html}</span>`;
  }
  function paintPage(item) {
    const conv = document.getElementById("conv");
    if (!conv || !item) return;
    const rows = (item.rows || []).map((r) => `
      <div class="tick-row">
        <div><b>${esc(r.label)}</b>${r.hint ? `<small>${esc(r.hint)}</small>` : ""}</div>
        <div class="tick-row-side"><span>${esc(r.value)}</span>${r.url ? `<button type="button" class="tick-link" data-url="${esc(r.url)}">打开</button>` : ""}</div>
      </div>`).join("");
    conv.innerHTML = `<div class="tick-page">
      <h2>${esc(item.title || item.label)}</h2>
      <div class="tick-src">${esc(item.source || "")}${item.note ? " · " + esc(item.note) : ""}</div>
      <div class="tick-rows">${rows}</div>
      ${item.url ? `<button type="button" class="tick-open" data-url="${esc(item.url)}">在浏览器中打开来源</button>` : ""}
    </div>`;
    conv.querySelectorAll("[data-url]").forEach((btn) => btn.addEventListener("click", () => openUrl(btn.getAttribute("data-url"))));
  }
  async function openUrl(url) {
    if (!url || !window.NM) return;
    try { await NM.inv("open_https", { url }); }
    catch (e) { if (window.toast) toast("打不开链接：" + (e && e.message ? e.message : e)); }
  }
  function openItem(item) {
    if (!item || !window.Tabs) return;
    const id = item.id;
    Tabs.open({
      key: "tick:" + id,
      kind: "ticker",
      title: item.title || "详情",
      ico: "ticker",
      render: () => paintPage(ITEMS.find((x) => x.id === id) || item),
    });
  }
  async function refresh() {
    const track = document.getElementById("tb-marquee-track");
    if (!track || !window.NM) return;
    const c = cfg();
    if (!c.fx && !c.crypto && !c.news && !c.weather) {
      ITEMS = [];
      setTrack("标题栏内容已关闭");
      return;
    }
    try {
      const r = await NM.inv("ticker_feed", {
        fx: !!c.fx, crypto: !!c.crypto, news: !!c.news, weather: !!c.weather,
      });
      ITEMS = (r && r.items) || [];
      if (!ITEMS.length) setTrack("行情暂时不可用");
      else setItems(ITEMS);
      applySpeed(c.speed);
    } catch (_) {
      ITEMS = [];
      setTrack("行情暂时不可用");
    }
  }
  function init() {
    paintChecks();
    const btn = document.getElementById("tb-ticker-cfg");
    const pop = document.getElementById("tb-ticker-pop");
    if (btn && pop) {
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        pop.hidden = !pop.hidden;
      });
      pop.addEventListener("change", (e) => {
        if (e.target && e.target.id === "tb-ticker-speed") return;
        const next = cfg();
        document.querySelectorAll("[data-ticker]").forEach((el) => { next[el.dataset.ticker] = el.checked; });
        save(next);
        refresh();
      });
      const bar = document.getElementById("tb-ticker-speed");
      if (bar) bar.addEventListener("input", () => {
        const next = cfg();
        next.speed = Number(bar.value);
        save(next);
        applySpeed(next.speed);
      });
      document.addEventListener("click", (e) => {
        if (!pop.hidden && !pop.contains(e.target) && !btn.contains(e.target)) pop.hidden = true;
      });
    }
    const track = document.getElementById("tb-marquee-track");
    if (track) track.addEventListener("click", (e) => {
      const btn = e.target.closest("[data-tick]");
      if (!btn) return;
      e.preventDefault();
      e.stopPropagation();
      openItem(ITEMS.find((x) => x.id === btn.getAttribute("data-tick")));
    });
    refresh();
    setInterval(refresh, 10 * 60 * 1000);
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();

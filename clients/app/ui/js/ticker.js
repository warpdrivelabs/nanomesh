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
  function setTrack(text) {
    const track = document.getElementById("tb-marquee-track");
    if (!track) return;
    const safe = String(text || "").replace(/[&<>]/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" }[ch]));
    track.innerHTML = `<span>${safe}</span><span aria-hidden="true">${safe}</span>`;
  }
  async function refresh() {
    const track = document.getElementById("tb-marquee-track");
    if (!track || !window.NM) return;
    const c = cfg();
    if (!c.fx && !c.crypto && !c.news && !c.weather) {
      setTrack("标题栏内容已关闭");
      return;
    }
    try {
      const r = await NM.inv("ticker_feed", {
        fx: !!c.fx, crypto: !!c.crypto, news: !!c.news, weather: !!c.weather,
      });
      setTrack((r && r.text) || "行情暂时不可用");
      applySpeed(c.speed);
    } catch (_) {
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
    refresh();
    setInterval(refresh, 10 * 60 * 1000);
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();

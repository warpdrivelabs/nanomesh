// 展示格式化、复制按钮、图标和导航常量。

export const fmtBytes = (n) => {
  n = Number(n) || 0; const u = ["B", "KB", "MB", "GB", "TB"]; let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return `${n.toFixed(i ? 1 : 0)} ${u[i]}`;
};
export const fmtDur = (s) => {
  s = Math.floor(Number(s) || 0);
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d) return `${d} 天 ${h} 时`; if (h) return `${h} 时 ${m} 分`; if (m) return `${m} 分 ${s % 60} 秒`;
  return `${s} 秒`;
};
export const sinceMs = (ms) => (ms ? fmtDur((Date.now() - ms) / 1000) : "—");
export const fmtTime = (ms) => {
  if (!ms) return "—";
  const d = new Date(ms);
  return Number.isNaN(d.getTime()) ? "—" : d.toLocaleString();
};
export const fmtTimeSec = (sec) => (sec ? fmtTime(Number(sec) * 1000) : "—");
export const fmtRtt = (ms) => {
  if (ms == null || ms === "") return "—";
  const n = Number(ms);
  if (!Number.isFinite(n)) return "—";
  return n < 10 ? n.toFixed(1) + " ms" : Math.round(n) + " ms";
};
export const fmtRate = (bps) => {
  if (bps == null || !Number.isFinite(bps) || bps < 0) return "—";
  if (bps < 1024) return Math.round(bps) + " B/s";
  return fmtBytes(bps) + "/s";
};
const lossPct = (lost, dgrams) => {
  lost = Number(lost) || 0;
  dgrams = Number(dgrams) || 0;
  const base = dgrams + lost;
  if (!base) return null;
  return (lost / base) * 100;
};
export const fmtLoss = (lost, dgrams) => {
  const p = lossPct(lost, dgrams);
  if (p == null) return "—";
  return (p < 0.1 && p > 0 ? "<0.1" : p.toFixed(p < 1 ? 2 : 1)) + "%";
};
export const pathKindLabel = (k) => (k === "relay" ? "中继" : k === "direct" ? "直连" : "—");
/** 按 peer/connection id 采样瞬时吞吐（近几次刷新差分）。 */
const LINK_TRAF = new Map();
export function sampleRate(id, rx, tx) {
  const now = Date.now();
  const key = String(id || "");
  if (!key) return { down: null, up: null };
  const arr = LINK_TRAF.get(key) || [];
  arr.push({ t: now, rx: Number(rx) || 0, tx: Number(tx) || 0 });
  while (arr.length > 12) arr.shift();
  LINK_TRAF.set(key, arr);
  if (arr.length < 2) return { down: null, up: null };
  const a = arr[0], b = arr[arr.length - 1];
  const dt = (b.t - a.t) / 1000;
  if (dt < 0.8) return { down: null, up: null };
  return { down: Math.max(0, (b.rx - a.rx) / dt), up: Math.max(0, (b.tx - a.tx) / dt) };
}
export const shortId = (h) => (h && h.length > 16 ? `${h.slice(0, 10)}…${h.slice(-4)}` : h || "");
export const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
async function copyToClipboard(text) {
  const value = String(text ?? "");
  if (!value) throw new Error("没有可复制的内容");
  if (navigator.clipboard && window.isSecureContext) {
    await navigator.clipboard.writeText(value);
    return;
  }
  const ta = document.createElement("textarea");
  ta.value = value;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.top = "0";
  ta.style.left = "0";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.focus();
  ta.select();
  const ok = document.execCommand("copy");
  ta.remove();
  if (!ok) throw new Error("浏览器拒绝写入剪贴板");
}
const COPY_ICON = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>';
export function copyIconBtn(attr) {
  return `<button class="btn btn--icon" type="button" title="复制" aria-label="复制" ${attr}>${COPY_ICON}</button>`;
}
export function bindCopy(button, text) {
  button.onclick = async () => {
    try {
      await copyToClipboard(text);
      button.classList.add("is-copied");
      button.title = "已复制";
      setTimeout(() => {
        if (!button.isConnected) return;
        button.classList.remove("is-copied");
        button.title = "复制";
      }, 1200);
    } catch (e) { alert("复制失败：" + (e && e.message ? e.message : e)); }
  };
}

export const ICONS = {
  names: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c3 3 3 15 0 18M12 3c-3 3-3 15 0 18"/></svg>',
  overview: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="5" rx="1.5"/><rect x="14" y="12" width="7" height="9" rx="1.5"/><rect x="3" y="16" width="7" height="5" rx="1.5"/></svg>',
  connections: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M9 7 4 12l5 5M15 7l5 5-5 5"/></svg>',
  users: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="12" cy="8" r="3.2"/><path d="M5 20a7 7 0 0 1 14 0" stroke-linecap="round"/></svg>',
  storage: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/></svg>',
  traffic: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 17l5-6 4 3 4-7 5 5"/></svg>',
  system: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="4" y="4" width="16" height="12" rx="2"/><path d="M8 20h8M12 16v4" stroke-linecap="round"/></svg>',
  peers: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="6" cy="6" r="2.4"/><circle cx="18" cy="6" r="2.4"/><circle cx="12" cy="18" r="2.4"/><path d="M7.6 7.6 12 15.6 16.4 7.6M8.4 6h7.2" stroke-linecap="round"/></svg>',
  identity: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="3" y="5" width="18" height="14" rx="2"/><circle cx="8.5" cy="11" r="2"/><path d="M5.5 16c.6-1.6 4.2-1.6 6 0M14 9h4M14 12h4M14 15h2" stroke-linecap="round"/></svg>',
  password: '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8"><rect x="4" y="10" width="16" height="10" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3M12 14v2" stroke-linecap="round"/></svg>',
};
export const TABS = [
  { k: "overview", key: "tab.overview" },
  { k: "connections", key: "tab.connections" },
  { k: "users", key: "tab.users" },
  { k: "peers", key: "tab.peers" },
  { k: "names", key: "tab.names" },
  { k: "storage", key: "tab.storage" },
  { k: "traffic", key: "tab.traffic" },
  { k: "system", key: "tab.system" },
  { k: "identity", key: "tab.identity" },
  { k: "password", key: "tab.password" },
];
// 无需轮询的静态面板（表单/稳定信息）——渲染一次即可，避免定时重渲染打断输入。
export const STATIC_TABS = new Set(["identity", "password", "names"]);

export const metric = (label, val, sub) =>
  `<div class="metric"><div class="metric__label">${label}</div><div class="metric__val">${val ?? "—"}</div>${sub ? `<div class="metric__sub">${sub}</div>` : ""}</div>`;

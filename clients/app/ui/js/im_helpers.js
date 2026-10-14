// im.js 共享纯工具：实体类型、展示格式化与消息瘦身。

const ENTITY_TYPES = [
  { key: "person",  label: "朋友",   icon: "person", prefixes: ["person"] },
  { key: "device",  label: "设备",   icon: "device", prefixes: ["device"] },
  { key: "vehicle", label: "车辆",   icon: "vehicle", prefixes: ["vehicle"] },
  { key: "agent",   label: "智能体", icon: "agent", prefixes: ["agent"] },
  { key: "robot",   label: "机器人", icon: "robot",  prefixes: ["robot"] },
  { key: "compute", label: "算力体", icon: "compute", prefixes: ["compute"] },
];
function kindType(kind) {
  const k = (kind || "").toLowerCase();
  for (const t of ENTITY_TYPES) if (t.prefixes.some((p) => k === p || k.startsWith(p + "."))) return t.key;
  return "other";
}
function typeMeta(kind) { return ENTITY_TYPES.find((t) => t.key === kindType(kind)) || { key: "other", label: "其他", icon: "file" }; }

function slimMsg(m) {
  const o = {
    id: m.id, from: m.from, to: m.to, body: m.body || "", ts: m.ts || 0,
    group: !!m.group, channel: !!m.channel, typeUrl: m.typeUrl || "",
    kind: m.kind || "text", text: m.text || "",
    status: m.status || 0, gramId: m.gramId || 0,
  };
  const media = m.media;
  if (media && typeof media === "object") {
    o.media = {
      v: media.v || 1, kind: media.kind || "", text: media.text || "",
      mime: media.mime || "", name: media.name || "", size: media.size || 0,
      w: media.w || 0, h: media.h || 0, dur: media.dur || 0,
      parts: media.parts || [], home: media.home || "", thumb: media.thumb || "",
      card: media.card || "",
    };
  }
    if (m.seq) o.seq = m.seq;
    return o;
}
window.slimMsg = slimMsg;

function formatUnread(n) {
  n = Number(n) || 0;
  if (n <= 0) return "";
  if (n > 99) return "99+";
  return String(n);
}

function msgClock(ts) {
  const d = new Date(ts || 0);
  if (!ts || Number.isNaN(d.getTime())) return "";
  return String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
}
function dayLabel(ts) {
  const d = new Date(ts || 0);
  if (!ts || Number.isNaN(d.getTime())) return "";
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return window.t ? t("day.today") : "今天";
  const y = new Date(now); y.setDate(now.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return window.t ? t("day.yesterday") : "昨天";
  return d.getFullYear() + "年" + (d.getMonth() + 1) + "月" + d.getDate() + "日";
}
function listTime(ts) {
  if (!ts) return "";
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return "";
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return msgClock(ts);
  const y = new Date(now); y.setDate(now.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return window.t ? t("day.yesterday") : "昨天";
  if (d.getFullYear() === now.getFullYear()) return (d.getMonth() + 1) + "/" + d.getDate();
  return d.getFullYear() + "/" + (d.getMonth() + 1) + "/" + d.getDate();
}
function sameDay(a, b) { return new Date(a || 0).toDateString() === new Date(b || 0).toDateString(); }
function continues(prev, m) {
  return !!(prev && prev.from === m.from && sameDay(prev.ts, m.ts) && (m.ts || 0) - (prev.ts || 0) < 5 * 60 * 1000);
}

function shortId(id) { id = id || ""; return id.length <= 12 ? id : id.slice(0, 8) + "…"; }
function avatarColor(id) {
  const palette = ["#3987e5", "#1fb182", "#9085e9", "#d55181", "#eb6834", "#22c55e", "#e5b95f"];
  let h = 0; for (let i = 0; i < (id || "").length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return palette[h % palette.length];
}

// 选区截图遮罩页（shot-overlay 窗口本体）。
// 窗口由 Rust capture_screen 打开并铺满主显示器；快照 RGBA 存在 Rust SHOT_SNAP，
// 本页经 shotoverlay:// 协议拉 /meta（尺寸）与 /png（整屏 PNG，物理像素）。
// 遮罩是独立全屏窗口 → CSS 视口 = 屏幕逻辑尺寸，抓屏/视口的比例即系统 DPI，
// 选区坐标 ×(sx,sy) 直接换算物理像素，几何精确（修复主窗口内遮罩的「缩放」bug）。
// 快照以 PNG 文件经 asset 协议进来（Rust 落盘），像素由 drawImage 反推到 snapCanvas。

const COLORS = ["#ff4d4f", "#fadb14", "#2f9bff", "#3dd68c", "#b37feb", "#ff9f43", "#1a1a1a", "#ffffff"];
const SIZES = [2, 4, 7];

function shotStyle() {
  try {
    const v = JSON.parse(localStorage.getItem("shot.style") || "{}");
    return { color: COLORS.includes(v.color) ? v.color : "#ff4d4f", size: SIZES.includes(v.size) ? v.size : 4 };
  } catch (_) { return { color: "#ff4d4f", size: 4 }; }
}
function saveShotStyle(s) { try { localStorage.setItem("shot.style", JSON.stringify(s)); } catch (_) {} }

const $ = (id) => document.getElementById(id);
const mask = $("shot-mask"), sel = $("shot-sel"), bar = $("shot-bar"),
  hint = $("shot-hint"), dimtip = $("shot-dimtip"), styleBar = $("shot-stylebar"),
  toast = $("shot-toast");
// 8 个调整把手（选区未锁定时显示）。
for (const h of ["nw", "n", "ne", "e", "se", "s", "sw", "w"]) {
  const el = document.createElement("div");
  el.className = "shot-handle";
  el.dataset.h = h;
  sel.appendChild(el);
}

let W = 0, H = 0;        // 快照物理像素
let vw = 0, vh = 0;      // 本窗口 CSS 视口（= 屏幕逻辑尺寸）
let sx = 1, sy = 1;      // 物理/CSS 比例
let snapCanvas = null;   // 原始像素（马赛克取色/导出合成）
let snapUrl = "";
const sel2 = sel; // 语义别名
const ink = document.createElement("canvas");
ink.className = "shot-ink";
const inkCtx = ink.getContext("2d");
const tbox = document.createElement("textarea");
tbox.className = "shot-textbox";
sel.appendChild(ink);
sel.appendChild(tbox);

const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

async function boot() {
  const meta = await (await fetch("http://shotoverlay.local/meta")).json();
  W = meta.w; H = meta.h;
  // 快照：capture_screen 已落盘 PNG，经 asset 协议当普通图片加载（浏览器原生解码）。
  const pngUrl = meta.png
    ? window.__TAURI__.core.convertFileSrc(meta.png)
    : "http://shotoverlay.local/png";
  const img = new Image();
  img.onload = () => {
    // 原始像素反推：画进离屏 canvas，供马赛克取色与导出合成。
    snapCanvas = document.createElement("canvas");
    snapCanvas.width = W; snapCanvas.height = H;
    snapCanvas.getContext("2d").drawImage(img, 0, 0);
    snapUrl = pngUrl;
    mask.style.backgroundImage = `url(${snapUrl})`;
    sel.style.backgroundImage = `url(${snapUrl})`;
    mask.classList.add("shot-mask--ready");
  };
  img.onerror = () => { hint.textContent = "快照加载失败，按 Esc 退出"; };
  img.src = pngUrl;
  vw = innerWidth; vh = innerHeight;
  sx = W / vw; sy = H / vh;
  paint();
}

// ── 选区/标注状态 ─────────────────────────────────────────────
let drag = null, placed = false;
let tool = "select", style = shotStyle();
let strokes = [], redoStack = [], drawing = null, stepN = 0;
let exporting = false, textPos = null;
// 选区几何调整：moving=整体拖动 / resizing=把手 id。一旦画了标注 selLocked=true。
let moving = null, resizing = null, selLocked = false;

let toastTimer = 0;
function showToast(msg) {
  toast.textContent = msg;
  toast.classList.add("is-on");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toast.classList.remove("is-on"), 1600);
}

function setSelLocked(v) {
  selLocked = v;
  sel.classList.toggle("shot-sel--locked", v);
}

const rect = () => drag ? {
  x: Math.min(drag.x0, drag.x1), y: Math.min(drag.y0, drag.y1),
  w: Math.abs(drag.x1 - drag.x0), h: Math.abs(drag.y1 - drag.y0),
} : { x: 0, y: 0, w: 0, h: 0 };

function paint() {
  const r = rect();
  const on = r.w > 4 && r.h > 4;
  sel.style.display = on ? "block" : "none";
  sel.style.left = r.x + "px"; sel.style.top = r.y + "px";
  sel.style.width = r.w + "px"; sel.style.height = r.h + "px";
  if (on) {
    // 选区透亮:sel 平移同一张整屏背景(1:1 视口坐标系)
    sel.style.backgroundSize = `${vw}px ${vh}px`;
    sel.style.backgroundPosition = `${-r.x}px ${-r.y}px`;
  }
  if (on) {
    bar.style.visibility = "visible";
    const bw = bar.offsetWidth || 120, bh = bar.offsetHeight || 34;
    let bx = r.x + r.w - bw, by = r.y + r.h + 8;
    if (by + bh > innerHeight - 4) by = r.y - bh - 8;
    if (by < 4) by = innerHeight - bh - 4;
    bar.style.left = Math.max(4, Math.min(bx, innerWidth - bw - 4)) + "px";
    bar.style.top = by + "px";
    hint.style.visibility = "hidden";
    dimtip.style.visibility = "visible";
    dimtip.textContent = `${Math.round(r.w * sx)} × ${Math.round(r.h * sy)}`;
    dimtip.style.left = r.x + "px";
    dimtip.style.top = (r.y > 22 ? r.y - 22 : r.y + 4) + "px";
  } else {
    bar.style.visibility = "hidden";
    dimtip.style.visibility = "hidden";
    hint.textContent = "拖动框选区域，Enter 确认，Esc 取消";
    hint.style.visibility = "visible";
    hint.style.left = Math.max(8, (drag ? drag.x1 : innerWidth / 2) - 110) + "px";
    hint.style.top = ((drag ? drag.y1 : innerHeight / 2) + 16) + "px";
  }
}

// ── 标注绘制(与 composer.js 移植版一致) ───────────────────────
const rel = (e) => ({ x: e.clientX - rect().x, y: e.clientY - rect().y });
const clampRel = (p) => {
  const r = rect();
  return { x: Math.max(0, Math.min(r.w, p.x)), y: Math.max(0, Math.min(r.h, p.y)) };
};

function paintInk() {
  const r = rect();
  if (ink.width !== Math.round(r.w) || ink.height !== Math.round(r.h)) {
    ink.width = Math.round(r.w); ink.height = Math.round(r.h);
  }
  inkCtx.clearRect(0, 0, ink.width, ink.height);
  for (const s of strokes) drawStroke(inkCtx, s, 1);
  if (drawing) drawStroke(inkCtx, drawing, 1);
}

function drawStroke(ctx, s, fx, fy) {
  if (fx == null) fx = 1;
  if (fy == null) fy = fx;
  const P = (pt) => [pt.x * fx, pt.y * fy];
  ctx.save();
  ctx.lineCap = "round"; ctx.lineJoin = "round";
  if (s.tool === "mosaic") { drawMosaic(ctx, s, fx, fy); ctx.restore(); return; }
  ctx.strokeStyle = s.color; ctx.fillStyle = s.color;
  ctx.lineWidth = s.size * fx;
  if (s.tool === "marker") {
    ctx.restore();
    const off = document.createElement("canvas");
    off.width = Math.max(1, ctx.canvas.width); off.height = Math.max(1, ctx.canvas.height);
    const octx = off.getContext("2d");
    octx.lineCap = "square"; octx.lineJoin = "round";
    octx.strokeStyle = s.color; octx.lineWidth = s.size * 5 * fx;
    octx.beginPath();
    s.pts.forEach((p, i) => { const [x, y] = P(p); i ? octx.lineTo(x, y) : octx.moveTo(x, y); });
    octx.stroke();
    ctx.save(); ctx.globalAlpha = 0.45; ctx.drawImage(off, 0, 0); ctx.restore();
    return;
  }
  if (s.tool === "rect" || s.tool === "ellipse") {
    const [x0, y0] = P({ x: s.x0, y: s.y0 }), [x1, y1] = P({ x: s.x1, y: s.y1 });
    ctx.beginPath();
    if (s.tool === "rect") ctx.rect(x0, y0, x1 - x0, y1 - y0);
    else ctx.ellipse((x0 + x1) / 2, (y0 + y1) / 2, Math.abs(x1 - x0) / 2, Math.abs(y1 - y0) / 2, 0, 0, Math.PI * 2);
    ctx.stroke();
  } else if (s.tool === "arrow") {
    const [x0, y0] = P({ x: s.x0, y: s.y0 }), [x1, y1] = P({ x: s.x1, y: s.y1 });
    const ang = Math.atan2(y1 - y0, x1 - x0);
    const hl = Math.max(s.size * 3 * fx, 10 * fx);
    const back = Math.min(hl, Math.hypot(x1 - x0, y1 - y0) * 0.5);
    const ex = x1 - Math.cos(ang) * back, ey = y1 - Math.sin(ang) * back;
    ctx.beginPath(); ctx.moveTo(x0, y0); ctx.lineTo(ex, ey); ctx.stroke();
    ctx.beginPath();
    ctx.moveTo(x1, y1);
    ctx.lineTo(x1 - Math.cos(ang - 0.42) * hl, y1 - Math.sin(ang - 0.42) * hl);
    ctx.lineTo(x1 - Math.cos(ang + 0.42) * hl, y1 - Math.sin(ang + 0.42) * hl);
    ctx.closePath(); ctx.fill();
  } else if (s.tool === "pen") {
    ctx.beginPath();
    s.pts.forEach((p, i) => { const [x, y] = P(p); i ? ctx.lineTo(x, y) : ctx.moveTo(x, y); });
    ctx.stroke();
  } else if (s.tool === "step") {
    const [x, y] = P({ x: s.x, y: s.y });
    const rr = Math.max(s.size * 5 * fx, 12 * fx);
    ctx.beginPath(); ctx.arc(x, y, rr, 0, Math.PI * 2); ctx.fill();
    ctx.fillStyle = "#fff";
    ctx.font = `bold ${rr}px system-ui,sans-serif`;
    ctx.textAlign = "center"; ctx.textBaseline = "middle";
    ctx.fillText(String(s.n), x, y + rr * 0.05);
  } else if (s.tool === "text") {
    const [x, y] = P({ x: s.x, y: s.y });
    const fs = s.fontSize * fx;
    ctx.font = `${fs}px system-ui,"Segoe UI","Microsoft YaHei",sans-serif`;
    ctx.textAlign = "left"; ctx.textBaseline = "top";
    let ty = y;
    for (const line of String(s.text || "").split("\n")) {
      ctx.fillText(line, x, ty);
      ty += fs * 1.35;
    }
  }
  ctx.restore();
}

function drawMosaic(ctx, s, fx, fy) {
  if (fx == null) fx = 1;
  if (fy == null) fy = fx;
  const r0 = rect();
  const blockSize = Math.max(8, s.size * 6);
  const cells = new Set();
  // 网格始终用 CSS 相对坐标划分；采样快照时 ×(sx,sy)，绘制时 ×(fx,fy)。
  const addPt = (px, py) => {
    const bx = Math.floor(px / blockSize), by = Math.floor(py / blockSize);
    for (let dx = -1; dx <= 1; dx++) for (let dy = -1; dy <= 1; dy++) cells.add((bx + dx) + "," + (by + dy));
  };
  if (s.pts && s.pts.length) {
    for (const p of s.pts) addPt(p.x, p.y);
  } else {
    for (let px = s.x0; px <= s.x1; px += blockSize / 2)
      for (let py = s.y0; py <= s.y1; py += blockSize / 2) addPt(px, py);
    addPt(s.x1, s.y1);
  }
  for (const key of cells) {
    const [bx, by] = key.split(",").map(Number);
    const px0 = Math.round((r0.x + bx * blockSize) * sx), py0 = Math.round((r0.y + by * blockSize) * sy);
    const pw = Math.round(blockSize * sx), ph = Math.round(blockSize * sy);
    if (px0 < 0 || py0 < 0 || px0 >= W || py0 >= H) continue;
    const px1 = Math.min(W, px0 + pw), py1 = Math.min(H, py0 + ph);
    if (px1 <= px0 || py1 <= py0) continue;
    const d = snapCanvas.getContext("2d").getImageData(px0, py0, px1 - px0, py1 - py0).data;
    let rr = 0, gg = 0, bb = 0, n = 0;
    for (let i = 0; i < d.length; i += 16) { rr += d[i]; gg += d[i + 1]; bb += d[i + 2]; n++; }
    if (!n) continue;
    ctx.fillStyle = `rgb(${rr / n | 0},${gg / n | 0},${bb / n | 0})`;
    ctx.fillRect(bx * blockSize * fx, by * blockSize * fy, blockSize * fx + 1, blockSize * fy + 1);
  }
}

// ── 工具条 ────────────────────────────────────────────────────
function buildDragBar() {
  bar.innerHTML = `<button type="button" class="shot-ok" title="发送选区 (Enter)">${nmIcon("shot-check")}</button>
    <button type="button" class="shot-cancel" title="取消截图 (Esc)">${nmIcon("close")}</button>`;
  bar.querySelector(".shot-ok").addEventListener("click", confirm);
  bar.querySelector(".shot-cancel").addEventListener("click", cancel);
}
function buildAnnotBar() {
  const T = [
    ["select", "shot-rect", "移动/调整选区"], ["rect", "shot-rect", "矩形"], ["ellipse", "shot-ellipse", "椭圆"],
    ["arrow", "shot-arrow", "箭头"], ["pen", "shot-pen", "画笔"], ["marker", "shot-marker", "荧光"],
    ["step", "shot-step", "序号"], ["text", "shot-text", "文字"], ["mosaic", "shot-mosaic", "马赛克"],
  ];
  bar.innerHTML =
    T.map(([id, ico, ti]) =>
      `<span class="shot-split"${id === "select" ? ` data-select="1"` : ""}><button type="button" class="shot-tool${id === tool ? " is-on" : ""}" data-tool="${id}" title="${ti}">${nmIcon(id === "select" ? "move" : ico)}</button>` +
      (id === "mosaic" || id === "select" ? "" : `<button type="button" class="shot-caret" data-style="${id}" title="${ti}样式">${nmIcon("chevron")}</button>`) + `</span>`
    ).join("") +
    `<span class="shot-vline"></span>` +
    `<button type="button" class="shot-tool" data-undo="1" title="撤销 (Ctrl+Z)">${nmIcon("shot-undo")}</button>` +
    `<button type="button" class="shot-tool" data-redo="1" title="重做 (Ctrl+Y)">${nmIcon("shot-redo")}</button>` +
    `<span class="shot-vline"></span>` +
    `<button type="button" class="shot-ok" title="发送选区 (Enter)">${nmIcon("shot-check")}</button>` +
    `<button type="button" class="shot-tool" data-copy="1" title="复制到剪贴板">${nmIcon("copy")}</button>` +
    `<button type="button" class="shot-tool" data-save="1" title="保存到本地">${nmIcon("shot-save")}</button>` +
    `<button type="button" class="shot-cancel" title="取消截图 (Esc)">${nmIcon("close")}</button>`;
  bar.querySelectorAll("[data-tool]").forEach((b) => b.addEventListener("click", () => setTool(b.dataset.tool)));
  bar.querySelectorAll("[data-style]").forEach((b) => b.addEventListener("click", (e) => { e.stopPropagation(); openStyleBar(b, b.dataset.style); }));
  bar.querySelector("[data-undo]").addEventListener("click", undo);
  bar.querySelector("[data-redo]").addEventListener("click", redo);
  bar.querySelector(".shot-ok").addEventListener("click", confirm);
  bar.querySelector(".shot-cancel").addEventListener("click", cancel);
  bar.querySelector("[data-copy]").addEventListener("click", () => exportShot("copy"));
  bar.querySelector("[data-save]").addEventListener("click", () => exportShot("save"));
  paintInk();
}
function setTool(id) {
  commitText();
  tool = id;
  bar.querySelectorAll("[data-tool]").forEach((b) => b.classList.toggle("is-on", b.dataset.tool === id));
  closeStyleBar();
  // 光标：select 模式下选区是移动，其余是绘制。
  sel.classList.toggle("shot-sel--live", placed && id !== "select");
}
function openStyleBar(anchor, toolId) {
  closeStyleBar();
  const showColor = toolId !== "mosaic"; // 马赛克无颜色,仅粗细
  const colors = showColor ? COLORS.map((c) =>
    `<button type="button" class="shot-c${c === style.color ? " is-on" : ""}" data-c="${c}" style="background:${c}"></button>`).join("") : "";
  const sizes = SIZES.map((s) =>
    `<button type="button" class="shot-s${s === style.size ? " is-on" : ""}" data-s="${s}"><i style="width:${s + 2}px;height:${s + 2}px"></i></button>`).join("");
  styleBar.innerHTML = `<div class="shot-style-row">${colors}</div><div class="shot-style-row">${sizes}</div>`;  styleBar.style.visibility = "visible";
  const br = anchor.getBoundingClientRect();
  const sw = styleBar.offsetWidth || 200, sh = styleBar.offsetHeight || 70;
  let bx = br.left + br.width / 2 - sw / 2, by = br.bottom + 6;
  if (by + sh > innerHeight - 4) by = br.top - sh - 6;
  styleBar.style.left = Math.max(4, Math.min(bx, innerWidth - sw - 4)) + "px";
  styleBar.style.top = Math.max(4, by) + "px";
  styleBar.querySelectorAll("[data-c]").forEach((b) => b.addEventListener("click", () => {
    style.color = b.dataset.c; saveShotStyle(style);
    styleBar.querySelectorAll("[data-c]").forEach((x) => x.classList.toggle("is-on", x.dataset.c === style.color));
  }));
  styleBar.querySelectorAll("[data-s]").forEach((b) => b.addEventListener("click", () => {
    style.size = Number(b.dataset.s); saveShotStyle(style);
    styleBar.querySelectorAll("[data-s]").forEach((x) => x.classList.toggle("is-on", Number(x.dataset.s) === style.size));
  }));
}
function closeStyleBar() { styleBar.style.visibility = "hidden"; styleBar.innerHTML = ""; }

function undo() {
  commitText();
  const s = strokes.pop();
  if (!s) return;
  if (s.tool === "step") stepN = Math.max(0, s.n - 1);
  redoStack.push(s);
  if (!strokes.length) setSelLocked(false); // 撤到 0 条 → 解锁选区（§6）
  paintInk();
}
function redo() {
  const s = redoStack.pop();
  if (!s) return;
  strokes.push(s);
  if (s.tool === "step") stepN = Math.max(stepN, s.n);
  paintInk();
}

function commitText() {
  if (textPos === null) return;
  const val = tbox.value;
  const pos = textPos;
  textPos = null;
  tbox.style.display = "none";
  tbox.value = "";
  if (val && val.trim()) {
    strokes.push({ tool: "text", color: style.color, size: style.size, fontSize: style.size * 4, x: pos.x, y: pos.y, text: val });
    redoStack = [];
    paintInk();
  }
}

// ── 指针事件 ──────────────────────────────────────────────────
mask.addEventListener("mousedown", (e) => {
  if (e.button !== 0) return;
  if (placed) {
    // select 工具下点选区外部 = 重新框选（微信同款），已有标注作废；
    // 标注工具下点外部 = 忽略（防误触丢标注）。
    if (tool !== "select") return;
    strokes = []; redoStack = []; stepN = 0; placed = false; moving = null; resizing = null;
    tool = "select";
    setSelLocked(false);
    buildDragBar();
    sel.classList.remove("shot-sel--live");
    drag = { x0: e.clientX, y0: e.clientY, x1: e.clientX, y1: e.clientY };
    paint();
    return;
  }
  drag = { x0: e.clientX, y0: e.clientY, x1: e.clientX, y1: e.clientY };
  paint();
});
document.addEventListener("mousemove", (e) => {
  if (drag && !placed) {
    drag.x1 = e.clientX; drag.y1 = e.clientY;
    if (e.shiftKey) {
      const dw = drag.x1 - drag.x0, dh = drag.y1 - drag.y0;
      const m = Math.max(Math.abs(dw), Math.abs(dh));
      drag.x1 = drag.x0 + Math.sign(dw || 1) * m;
      drag.y1 = drag.y0 + Math.sign(dh || 1) * m;
    }
    paint();
  }
}, true);
document.addEventListener("mouseup", () => {
  if (drag && !placed) {
    const r = rect();
    if (r.w > 4 && r.h > 4) { placed = true; buildAnnotBar(); setSelLocked(false); }
    paint();
  }
}, true);

// ── 选区几何：把手调整 / 整体移动（select 工具下）──────────────
const HANDLE_TAP = 6; // 边缘命中半径（把手本身已占 5px）

function hitHandle(e) {
  const r = rect();
  const gx = e.clientX - r.x, gy = e.clientY - r.y;
  const corners = {
    nw: [0, 0], ne: [r.w, 0], se: [r.w, r.h], sw: [0, r.h],
    n: [r.w / 2, 0], s: [r.w / 2, r.h], e: [r.w, r.h / 2], w: [0, r.h / 2],
  };
  // 把手未隐藏（未锁定）时优先命中把手矩形；已锁定则按边缘命中仍允许移动外微调？
  // 设计：锁定后不显示把手、不允许 resize。仅 select 且未锁定可 resize。
  if (selLocked || tool !== "select") return null;
  for (const [h, [cx, cy]] of Object.entries(corners)) {
    if (Math.abs(gx - cx) <= HANDLE_TAP && Math.abs(gy - cy) <= HANDLE_TAP) return h;
  }
  return null;
}

function applyResize(h, e) {
  const x0 = resizing.sx, y0 = resizing.sy; // 原始选区
  let { x, y, w, h: hh } = x0;
  const dx = e.clientX - resizing.px, dy = e.clientY - resizing.py;
  if (h.includes("w")) { x = x0.x + dx; w = x0.w - dx; }
  if (h.includes("e")) { w = x0.w + dx; }
  if (h.includes("n")) { y = y0.y + dy; hh = x0.h - dy; }
  if (h.includes("s")) { hh = x0.h + dy; }
  if (w < 8 || hh < 8) return; // 太小不应用
  drag = { x0: x, y0: y, x1: x + w, y1: y + hh };
  paint();
  paintInk();
}

sel.addEventListener("mousedown", (e) => {
  if (e.button !== 0 || !placed) return;
  // select 工具：把手 → resize；选区内 → 整体移动。
  if (tool === "select") {
    const h = hitHandle(e);
    if (h) {
      e.preventDefault(); e.stopPropagation();
      resizing = { h, px: e.clientX, py: e.clientY, sx: { ...rect() } };
      return;
    }
    e.preventDefault(); e.stopPropagation();
    moving = { px: e.clientX, py: e.clientY, sx: { ...rect() } };
    return;
  }
  if (tool === "text") {
    commitText();
    const p = clampRel(rel(e));
    textPos = p;
    tbox.style.display = "block";
    tbox.style.left = p.x + "px";
    tbox.style.top = p.y + "px";
    tbox.style.color = style.color;
    tbox.style.fontSize = style.size * 4 + "px";
    tbox.value = "";
    setTimeout(() => tbox.focus(), 0);
    return;
  }
  const p = clampRel(rel(e));
  if (tool === "step") {
    stepN += 1;
    strokes.push({ tool: "step", color: style.color, size: style.size, x: p.x, y: p.y, n: stepN });
    redoStack = [];
    setSelLocked(true);
    paintInk();
    return;
  }
  e.preventDefault(); e.stopPropagation();
  drawing = {
    tool, color: style.color, size: style.size,
    x0: p.x, y0: p.y, x1: p.x, y1: p.y,
    pts: (tool === "pen" || tool === "marker" || tool === "mosaic") ? [p] : undefined,
  };
  paintInk();
});
// 移动/调整把手/画笔拖动都挂 document 捕获阶段：鼠标移出选区也持续（挂 sel 上会断线）。
document.addEventListener("mousemove", (e) => {
  if (moving) {
    const nx = moving.sx.x + (e.clientX - moving.px);
    const ny = moving.sx.y + (e.clientY - moving.py);
    drag = {
      x0: nx, y0: ny,
      x1: nx + moving.sx.w, y1: ny + moving.sx.h,
    };
    paint();
    return;
  }
  if (resizing) { applyResize(resizing.h, e); return; }
  if (!drawing || !placed) return;
  const p = clampRel(rel(e));
  if (e.shiftKey && (drawing.tool === "rect" || drawing.tool === "ellipse")) {
    const dw = p.x - drawing.x0, dh = p.y - drawing.y0;
    const m = Math.max(Math.abs(dw), Math.abs(dh));
    drawing.x1 = drawing.x0 + Math.sign(dw || 1) * m;
    drawing.y1 = drawing.y0 + Math.sign(dh || 1) * m;
  } else if (e.shiftKey && drawing.tool === "arrow") {
    // Shift：水平/垂直/45° 箭头。
    const dw = p.x - drawing.x0, dh = p.y - drawing.y0;
    const ang = Math.atan2(dh, dw);
    const snap = Math.round(ang / (Math.PI / 4)) * (Math.PI / 4);
    const len = Math.hypot(dw, dh);
    drawing.x1 = drawing.x0 + Math.cos(snap) * len;
    drawing.y1 = drawing.y0 + Math.sin(snap) * len;
  } else {
    drawing.x1 = p.x; drawing.y1 = p.y;
  }
  if (drawing.pts) drawing.pts.push(p);
  paintInk();
}, true);
window.addEventListener("mouseup", () => {
  if (moving || resizing) { moving = null; resizing = null; return; }
  if (!drawing) return;
  const s = drawing;
  drawing = null;
  const tiny = s.pts ? s.pts.length < 2 : Math.abs(s.x1 - s.x0) < 3 && Math.abs(s.y1 - s.y0) < 3;
  if (!tiny) {
    strokes.push(s);
    redoStack = [];
    setSelLocked(true); // 画了标注 → 选区锁定（§6）
  }
  paintInk();
}, true);
tbox.addEventListener("keydown", (e) => {
  e.stopPropagation();
  if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); commitText(); }
  if (e.key === "Escape") { textPos = null; tbox.style.display = "none"; tbox.value = ""; }
});

// ── 全局键盘 ──────────────────────────────────────────────────
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    e.stopPropagation();
    if (styleBar.style.visibility === "visible") { closeStyleBar(); return; }
    if (textPos !== null) { textPos = null; tbox.style.display = "none"; tbox.value = ""; return; }
    cancel();
    return;
  }
  if (!placed) return;
  if ((e.ctrlKey || e.metaKey) && (e.key === "z" || e.key === "Z")) { e.preventDefault(); undo(); return; }
  if ((e.ctrlKey || e.metaKey) && (e.key === "y" || e.key === "Y")) { e.preventDefault(); redo(); return; }
  if (e.key === "Enter" && textPos === null && !exporting) { e.preventDefault(); confirm(); }
}, true);

// ── 导出:合成快照选区 + 标注层 → JPEG → invoke shot_finish ────
async function exportShot(mode) {
  if (exporting) return;
  commitText();
  exporting = true;
  try {
    const r = rect();
    const cw = Math.round(r.w * sx), chh = Math.round(r.h * sy);
    const out = document.createElement("canvas");
    out.width = cw; out.height = chh;
    const octx = out.getContext("2d");
    octx.drawImage(snapCanvas, Math.round(r.x * sx), Math.round(r.y * sy), cw, chh, 0, 0, cw, chh);
    // 标注按物理像素重放一遍（不是先画逻辑尺寸再拉伸），文字/画笔导出清晰。
    const off = document.createElement("canvas");
    off.width = cw; off.height = chh;
    const oictx = off.getContext("2d");
    for (const s of strokes) drawStroke(oictx, s, sx, sy);
    octx.drawImage(off, 0, 0);
    const blob = await new Promise((res) => out.toBlob(res, "image/jpeg", 0.85));
    exporting = false;
    if (!blob) return;
    if (mode === "copy") {
      try {
        // 剪贴板要求 PNG；JPEG blob 冒充 image/png 会贴出坏图，重编码为真 PNG。
        const png = await new Promise((res) => {
          const c = document.createElement("canvas");
          c.width = out.width; c.height = out.height;
          c.getContext("2d").drawImage(out, 0, 0);
          c.toBlob(res, "image/png");
        });
        await navigator.clipboard.write([new ClipboardItem({ "image/png": png })]);
        showToast("已复制到剪贴板");
        setTimeout(() => invoke("shot_finish", { x: 0, y: 0, cw: 0, ch: 0 }), 350); // 关窗(复制已完成)
      } catch (_) { showToast("复制失败"); }
      return;
    }
    if (mode === "save") {
      const buf = new Uint8Array(await blob.arrayBuffer());
      let bin = "";
      for (let i = 0; i < buf.length; i += 0x8000) bin += String.fromCharCode.apply(null, buf.subarray(i, i + 0x8000));
      try {
        await invoke("shot_save", { dataB64: btoa(bin) });
        showToast("已保存到下载目录");
      } catch (_) { showToast("保存失败"); }
      setTimeout(() => invoke("shot_finish", { x: 0, y: 0, cw: 0, ch: 0 }), 350);
      return;
    }
    const buf = new Uint8Array(await blob.arrayBuffer());
    let bin = "";
    for (let i = 0; i < buf.length; i += 0x8000) bin += String.fromCharCode.apply(null, buf.subarray(i, i + 0x8000));
    await invoke("shot_finish", { x: 0, y: 0, cw: 0, ch: 0, jpegB64: btoa(bin) });
  } catch (e) {
    exporting = false;
  }
}

const confirm = () => {
  const r = rect();
  if (r.w < 4 || r.h < 4) return;
  exportShot("send");
};
const cancel = () => {
  invoke("shot_finish", { x: 0, y: 0, cw: 0, ch: 0 }).catch(() => {});
};

// 工具条上的按下不许冒泡成拖选。
bar.addEventListener("mousedown", (e) => e.stopPropagation());
styleBar.addEventListener("mousedown", (e) => e.stopPropagation());

buildDragBar();
boot();

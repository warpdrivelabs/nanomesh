// 会话输入区：表情、截图、图片、语音、视频、文件、名片 / @、会话记录。
// 纯文本仍走原来的发送。带附件的消息是 nmspace.v1/chat JSON，字节在 blob 分片里。
// 频道把同一段 JSON 放进 body，前面加 NMCHAT1 前缀。

const CHAT_PREFIX = "NMCHAT1 ";
const CHUNK = 768 * 1024;
const MEDIA_CAP = 50 * 1024 * 1024;
const VOICE_MS = 60 * 1000;
const EMOJI = {
  表情: ["😀","😁","😂","🤣","😊","😇","🙂","😉","😍","😘","😗","😙","😚","😋","😛","😜","🤪","😝","🤑","🤗","🤭","🤫","🤔","😐","😑","😶","😏","😒","🙄","😬","😌","😔","😪","🤤","😴","😷","🤒","🤕","🤢","🥵","🥶","🥴","😵","🤯","🤠","🥳","😎","🤓","🧐","😕","😟","🙁","☹️","😮","😯","😲","😳","🥺","😦","😧","😨","😰","😥","😢","😭","😱","😖","😣","😞","😓","😩","😫","🥱","😤","😡","😠","🤬"],
  手势: ["👍","👎","👌","✌️","🤞","🤟","🤘","🤙","👈","👉","👆","👇","☝️","✋","🤚","🖐️","🖖","👋","🤝","🙏","💪","👏","🙌","👐","🤲"],
  爱心: ["❤️","🧡","💛","💚","💙","💜","🖤","🤍","🤎","💔","❣️","💕","💞","💓","💗","💖","💘","💝"],
  动物: ["🐶","🐱","🐭","🐹","🐰","🦊","🐻","🐼","🐨","🐯","🦁","🐮","🐷","🐸","🐵","🐔","🐧","🐦","🦆","🦅","🦉","🐴","🦄","🐝","🐛"],
  食物: ["🍏","🍎","🍐","🍊","🍋","🍌","🍉","🍇","🍓","🍈","🍒","🍑","🥭","🍍","🥥","🥝","🍅","🥑","🍔","🍟","🍕","🍜","🍣","🍰","☕"],
  符号: ["⭐","🌟","✨","⚡","🔥","🌈","☀️","🌙","❄️","💧","🎉","🎊","🎁","🏆","📌","📎","✅","❌","❓","❗","💯","🔔","💡","📷","🎬"],
};

const drafts = new Map();
let viewId = "";
let viewRoom = false;
let api = null;
let popKind = "";
let emojiTab = "最近";
let rec = null;
let sending = false;
let installed = false;

function draft() {
  let d = drafts.get(viewId);
  if (!d) { d = { text: "", pending: [] }; drafts.set(viewId, d); }
  return d;
}

function snapshot() {
  const input = document.getElementById("conv-input");
  if (!viewId || !input) return;
  draft().text = input.value;
}

function prepare(id, room) {
  snapshot();
  viewId = id || "";
  viewRoom = !!room;
}

function markup() {
  const d = viewId ? draft() : { text: "", pending: [] };
  const who = viewRoom ? "提及成员" : "发送名片";
  return `<div class="im-composer">
    <div class="im-history" id="conv-history">
      <input id="conv-history-q" type="text" placeholder="搜索当前会话" autocomplete="off" />
      <div class="im-history-list" id="conv-history-list"></div>
    </div>
    <div class="im-tools">
      <button type="button" class="im-tool" data-act="emoji" title="表情">${nmIcon("smile")}</button>
      <span class="im-split">
        <button type="button" class="im-tool" data-act="shot" title="截图">${nmIcon("scissors")}</button>
        <button type="button" class="im-caret" data-menu="shot" title="截图方式">${nmIcon("chevron")}</button>
      </span>
      <span class="im-split">
        <button type="button" class="im-tool" data-act="windows" title="截取窗口">${nmIcon("crop")}</button>
        <button type="button" class="im-caret" data-menu="windows" title="选择窗口">${nmIcon("chevron")}</button>
      </span>
      <button type="button" class="im-tool" data-act="image" title="图片">${nmIcon("image")}</button>
      <button type="button" class="im-tool" data-act="video" title="视频">${nmIcon("video")}</button>
      <button type="button" class="im-tool" data-act="file" title="文件">${nmIcon("file")}</button>
      <button type="button" class="im-tool" id="conv-mic" data-act="mic" title="语音">${nmIcon("mic")}</button>
      <span class="im-split">
        <button type="button" class="im-tool" data-act="people" title="${who}">${nmIcon("at")}</button>
        <button type="button" class="im-caret" data-menu="people" title="${who}">${nmIcon("chevron")}</button>
      </span>
      <button type="button" class="im-tool im-tool--end" data-act="history" title="会话记录">${nmIcon("clock")}</button>
    </div>
    <div class="im-pending${d.pending.length ? " is-on" : ""}" id="conv-pending">${pendingHtml(d.pending)}</div>
    <div class="im-write">
      <textarea id="conv-input" rows="3" aria-label="消息">${escapeHtml(d.text || "")}</textarea>
      <button type="button" class="im-send" id="conv-send" data-act="send" title="发送">${nmIcon("send")}</button>
    </div>
    <div class="im-pop" id="conv-pop" hidden></div>
    <input type="file" id="conv-pick-image" accept="image/*" multiple hidden />
    <input type="file" id="conv-pick-video" accept="video/mp4,video/webm,video/quicktime,.mp4,.webm,.mov" hidden />
    <input type="file" id="conv-pick-file" hidden />
  </div>`;
}

function pendingHtml(items) {
  return (items || []).map((it) => {
    const thumb = it.previewUrl ? `<img src="${it.previewUrl}" alt="">` : nmIcon(it.kind === "voice" ? "mic" : it.kind === "video" ? "video" : "file");
    const label = it.kind === "voice" ? ("语音 " + fmtDur(it.dur)) : (it.name || kindLabel(it.kind));
    return `<span class="im-chip" data-chip="${escapeHtml(it.localId)}">${thumb}<b>${escapeHtml(label)}</b><button type="button" data-act="unchip" data-chip="${escapeHtml(it.localId)}" title="移除">×</button></span>`;
  }).join("");
}

function syncSend() {
  const btn = document.getElementById("conv-send");
  const input = document.getElementById("conv-input");
  if (!btn) return;
  const ready = !!((input && input.value.trim()) || ((viewId && drafts.get(viewId) && drafts.get(viewId).pending) || []).length);
  btn.classList.toggle("is-ready", ready);
}
function paintPending() {
  const box = document.getElementById("conv-pending");
  if (!box) return;
  const items = draft().pending;
  box.classList.toggle("is-on", items.length > 0);
  box.innerHTML = pendingHtml(items);
  syncSend();
}

function attach(next) {
  api = next || null;
  install();
  const input = document.getElementById("conv-input");
  if (!input) return;
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); doSend(); }
  });
  input.addEventListener("input", () => { draft().text = input.value; fit(input); syncSend(); });
  fit(input);
  syncSend();
  const img = document.getElementById("conv-pick-image");
  const vid = document.getElementById("conv-pick-video");
  const file = document.getElementById("conv-pick-file");
  if (img) img.addEventListener("change", () => { takeImages(img.files); img.value = ""; });
  if (vid) vid.addEventListener("change", () => { const f = vid.files && vid.files[0]; vid.value = ""; if (f) takeVideo(f); });
  if (file) file.addEventListener("change", () => { const f = file.files && file.files[0]; file.value = ""; if (f) takeFile(f); });
  const q = document.getElementById("conv-history-q");
  if (q) q.addEventListener("input", paintHistory);
  const mic = document.getElementById("conv-mic");
  if (mic && rec) mic.classList.add("is-rec");
  input.focus();
}

function install() {
  if (installed) return;
  installed = true;
  document.addEventListener("click", (e) => {
    const t = e.target;
    if (t.closest && (t.closest("#conv-pop") || t.closest(".im-tool") || t.closest(".im-caret"))) return;
    closePop();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") { closePop(); closeLight(); }
  });
}

function fit(input) {
  input.style.height = "auto";
  input.style.height = Math.min(180, Math.max(88, input.scrollHeight)) + "px";
}

function closePop() {
  popKind = "";
  const pop = document.getElementById("conv-pop");
  if (pop) { pop.hidden = true; pop.innerHTML = ""; pop.classList.remove("im-pop--wide"); }
}

function openPop(html, anchor) {
  const pop = document.getElementById("conv-pop");
  if (!pop) return;
  pop.innerHTML = html;
  pop.hidden = false;
  const tools = document.querySelector(".im-tools");
  if (anchor && tools) {
    const r = anchor.getBoundingClientRect();
    const tr = tools.getBoundingClientRect();
    pop.style.left = Math.max(8, r.left - tr.left) + "px";
  }
}

function onComposerClick(e) {
  const btn = e.target.closest("[data-act], [data-menu]");
  if (!btn || !btn.closest(".im-composer")) return;
  const act = btn.getAttribute("data-act") || "";
  const menu = btn.getAttribute("data-menu") || "";
  if (menu === "shot") { toggleShotMenu(btn); return; }
  if (menu === "windows" || act === "windows") { toggleWindows(btn); return; }
  if (menu === "people" || act === "people") { togglePeople(btn); return; }
  if (act === "unchip") {
    const id = btn.getAttribute("data-chip");
    const d = draft();
    const it = d.pending.find((x) => x.localId === id);
    if (it && it.previewUrl) URL.revokeObjectURL(it.previewUrl);
    d.pending = d.pending.filter((x) => x.localId !== id);
    paintPending();
    return;
  }
  if (act === "emoji") { toggleEmoji(btn); return; }
  if (act === "emoji-tab") { emojiTab = btn.getAttribute("data-tab") || "表情"; paintEmoji(btn); return; }
  if (act === "emoji-pick") { insertAt(btn.getAttribute("data-emoji") || ""); rememberEmoji(btn.getAttribute("data-emoji") || ""); closePop(); return; }
  if (act === "shot") { closePop(); capture(false); return; }
  if (act === "shot-hide") { closePop(); capture(true); return; }
  if (act === "win-pick") { closePop(); captureWindow(Number(btn.getAttribute("data-wid"))); return; }
  if (act === "image") { const el = document.getElementById("conv-pick-image"); if (el) el.click(); return; }
  if (act === "video") { const el = document.getElementById("conv-pick-video"); if (el) el.click(); return; }
  if (act === "file") { const el = document.getElementById("conv-pick-file"); if (el) el.click(); return; }
  if (act === "mic") { toggleMic(); return; }
  if (act === "mention") { insertAt("@" + (btn.getAttribute("data-name") || "") + " "); closePop(); return; }
  if (act === "card") { closePop(); sendCard(btn.getAttribute("data-id") || "", btn.getAttribute("data-name") || ""); return; }
  if (act === "send") { doSend(); return; }
  if (act === "history") { toggleHistory(); return; }
  if (act === "hist-jump") { jump(btn.getAttribute("data-mid") || ""); return; }
}

function toggleShotMenu(anchor) {
  if (popKind === "shot") { closePop(); return; }
  popKind = "shot";
  openPop(`<button type="button" data-act="shot">截取整个屏幕</button><button type="button" data-act="shot-hide">隐藏窗口后截图</button>`, anchor);
}

function toggleEmoji(anchor) {
  if (popKind === "emoji") { closePop(); return; }
  popKind = "emoji";
  openPop(`<div class="im-emoji-tabs" id="conv-emoji-tabs"></div><div class="im-emoji-grid" id="conv-emoji-grid"></div>`, anchor);
  const pop = document.getElementById("conv-pop");
  if (pop) pop.classList.add("im-pop--wide");
  paintEmoji(anchor);
}

function recentEmoji() {
  try { return JSON.parse(localStorage.getItem("nmspace-emoji-recent") || "[]"); } catch (_) { return []; }
}
function rememberEmoji(ch) {
  if (!ch) return;
  const list = [ch, ...recentEmoji().filter((x) => x !== ch)].slice(0, 24);
  try { localStorage.setItem("nmspace-emoji-recent", JSON.stringify(list)); } catch (_) {}
}
function paintEmoji() {
  const tabs = document.getElementById("conv-emoji-tabs");
  const grid = document.getElementById("conv-emoji-grid");
  if (!tabs || !grid) return;
  const names = ["最近", ...Object.keys(EMOJI)];
  if (!names.includes(emojiTab)) emojiTab = "表情";
  tabs.innerHTML = names.map((n) => `<button type="button" data-act="emoji-tab" data-tab="${n}" class="${n === emojiTab ? "is-on" : ""}">${n}</button>`).join("");
  const list = emojiTab === "最近" ? recentEmoji() : (EMOJI[emojiTab] || []);
  grid.innerHTML = list.length
    ? list.map((ch) => `<button type="button" data-act="emoji-pick" data-emoji="${ch}">${ch}</button>`).join("")
    : `<span class="im-history-empty">还没有用过的表情</span>`;
}

function insertAt(text) {
  const input = document.getElementById("conv-input");
  if (!input || !text) return;
  const a = input.selectionStart || 0;
  const b = input.selectionEnd || 0;
  input.value = input.value.slice(0, a) + text + input.value.slice(b);
  const n = a + text.length;
  input.setSelectionRange(n, n);
  draft().text = input.value;
  fit(input);
  input.focus();
}

async function capture(hide) {
  if (!window.NM || !NM.hasTauri || !NM.hasTauri()) { toast("截图只能在桌面端使用"); return; }
  try {
    const b64 = await NM.inv("capture_screen", { hide: !!hide });
    const bytes = b64ToBytes(b64);
    const url = URL.createObjectURL(new Blob([bytes], { type: "image/jpeg" }));
    const dim = await imageSize(url);
    pushPending({ kind: "image", name: "截图.jpg", mime: "image/jpeg", bytes, size: bytes.length, w: dim.w, h: dim.h, previewUrl: url });
  } catch (e) { toast("截图失败：" + errText(e)); }
}

async function toggleWindows(anchor) {
  if (popKind === "windows") { closePop(); return; }
  popKind = "windows";
  openPop(`<div class="im-history-empty">正在列出窗口…</div>`, anchor);
  try {
    const list = await NM.inv("list_windows");
    if (popKind !== "windows") return;
    if (!list || !list.length) { openPop(`<div class="im-history-empty">没有可截的窗口。若系统刚询问，允许录制屏幕后再试。</div>`, anchor); return; }
    openPop(list.map((w) => {
      const title = w.title || w.app || "窗口";
      const sub = w.title && w.app ? `<small>${escapeHtml(w.app)}</small>` : "";
      return `<button type="button" data-act="win-pick" data-wid="${Number(w.id) || 0}">${escapeHtml(title)}${sub}</button>`;
    }).join(""), anchor);
  } catch (e) {
    openPop(`<div class="im-history-empty">${escapeHtml(errText(e))}</div>`, anchor);
  }
}

async function captureWindow(id) {
  if (!id) return;
  try {
    const b64 = await NM.inv("capture_window", { id });
    const bytes = b64ToBytes(b64);
    const url = URL.createObjectURL(new Blob([bytes], { type: "image/jpeg" }));
    const dim = await imageSize(url);
    pushPending({ kind: "image", name: "窗口.jpg", mime: "image/jpeg", bytes, size: bytes.length, w: dim.w, h: dim.h, previewUrl: url });
  } catch (e) { toast("截取窗口失败：" + errText(e)); }
}

async function takeImages(files) {
  for (const file of files || []) {
    try {
      if (file.type === "image/gif" && file.size <= 1024 * 1024) {
        const bytes = new Uint8Array(await file.arrayBuffer());
        const url = URL.createObjectURL(file);
        const dim = await imageSize(url);
        pushPending({ kind: "image", name: file.name || "图片.gif", mime: "image/gif", bytes, size: bytes.length, w: dim.w, h: dim.h, previewUrl: url });
      } else {
        const img = await compressImage(file);
        pushPending({ kind: "image", name: (file.name || "图片").replace(/\.\w+$/, "") + ".jpg", mime: img.mime, bytes: img.bytes, size: img.bytes.length, w: img.w, h: img.h, previewUrl: img.url });
      }
    } catch (e) { toast("这张图没放进去：" + errText(e)); }
  }
}

function compressImage(file) {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      const max = 1920;
      const scale = Math.min(1, max / Math.max(img.width, img.height, 1));
      const w = Math.max(1, Math.round(img.width * scale));
      const h = Math.max(1, Math.round(img.height * scale));
      const canvas = document.createElement("canvas");
      canvas.width = w; canvas.height = h;
      canvas.getContext("2d").drawImage(img, 0, 0, w, h);
      const finish = (q) => canvas.toBlob(async (blob) => {
        if (!blob) { reject(new Error("压缩失败")); return; }
        if (blob.size > 900 * 1024 && q > 0.45) { finish(q - 0.12); return; }
        const bytes = new Uint8Array(await blob.arrayBuffer());
        resolve({ bytes, mime: "image/jpeg", w, h, url: URL.createObjectURL(blob) });
        URL.revokeObjectURL(url);
      }, "image/jpeg", q);
      finish(0.82);
    };
    img.onerror = () => { URL.revokeObjectURL(url); reject(new Error("读不到这张图")); };
    img.src = url;
  });
}

async function takeVideo(file) {
  if (file.size > MEDIA_CAP) { toast("视频超过 50MB"); return; }
  const bytes = new Uint8Array(await file.arrayBuffer());
  const url = URL.createObjectURL(file);
  let dur = 0;
  let thumb = null;
  try {
    const meta = await videoMeta(url);
    dur = meta.dur;
    thumb = meta.thumb;
  } catch (_) {}
  pushPending({ kind: "video", name: file.name || "视频", mime: file.type || "video/mp4", bytes, size: bytes.length, dur, previewUrl: thumb ? thumb.url : "", thumbBytes: thumb ? thumb.bytes : null });
}

function videoMeta(url) {
  return new Promise((resolve, reject) => {
    const v = document.createElement("video");
    v.preload = "auto";
    v.muted = true;
    v.onloadeddata = () => {
      const dur = Math.round((v.duration || 0) * 1000);
      const w = v.videoWidth || 320;
      const h = v.videoHeight || 180;
      let done = false;
      const draw = () => {
        if (done) return;
        done = true;
        const canvas = document.createElement("canvas");
        const scale = Math.min(1, 480 / Math.max(w, h, 1));
        canvas.width = Math.max(1, Math.round(w * scale));
        canvas.height = Math.max(1, Math.round(h * scale));
        canvas.getContext("2d").drawImage(v, 0, 0, canvas.width, canvas.height);
        canvas.toBlob(async (blob) => {
          if (!blob) { resolve({ dur, thumb: null }); return; }
          resolve({ dur, thumb: { bytes: new Uint8Array(await blob.arrayBuffer()), url: URL.createObjectURL(blob) } });
        }, "image/jpeg", 0.7);
      };
      v.onseeked = draw;
      try { v.currentTime = Math.min(0.2, (v.duration || 1) / 4); } catch (_) { draw(); }
      setTimeout(draw, 500);
    };
    v.onerror = () => reject(new Error("读不到视频"));
    v.src = url;
  });
}

async function takeFile(file) {
  if (file.size > MEDIA_CAP) { toast("文件超过 50MB"); return; }
  const bytes = new Uint8Array(await file.arrayBuffer());
  pushPending({ kind: "file", name: file.name || "文件", mime: file.type || "application/octet-stream", bytes, size: bytes.length });
}

function pushPending(item) {
  item.localId = "p" + Date.now() + Math.random().toString(16).slice(2, 6);
  draft().pending.push(item);
  paintPending();
}

async function toggleMic() {
  if (rec) { stopMic(); return; }
  if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) { toast("这里不能录音"); return; }
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    const mime = ["audio/webm;codecs=opus", "audio/webm", "audio/mp4"].find((t) => window.MediaRecorder && MediaRecorder.isTypeSupported(t)) || "";
    const recorder = new MediaRecorder(stream, mime ? { mimeType: mime } : undefined);
    const chunks = [];
    recorder.ondataavailable = (ev) => { if (ev.data && ev.data.size) chunks.push(ev.data); };
    const started = Date.now();
    rec = { recorder, stream, started, chunks, mime: mime || recorder.mimeType || "audio/webm", viewId };
    recorder.onstop = async () => {
      stream.getTracks().forEach((t) => t.stop());
      const hold = rec;
      rec = null;
      const mic = document.getElementById("conv-mic");
      if (mic) mic.classList.remove("is-rec");
      const blob = new Blob(chunks, { type: hold.mime });
      const bytes = new Uint8Array(await blob.arrayBuffer());
      const dur = Date.now() - started;
      if (bytes.length < 800 || dur < 400) { toast("录音太短"); return; }
      const item = { localId: "p" + Date.now(), kind: "voice", name: "语音", mime: hold.mime.split(";")[0], bytes, size: bytes.length, dur };
      const box = drafts.get(hold.viewId);
      if (box) box.pending.push(item);
      if (hold.viewId === viewId) paintPending();
    };
    recorder.start();
    const mic = document.getElementById("conv-mic");
    if (mic) mic.classList.add("is-rec");
    const timer = setInterval(() => {
      if (!rec) { clearInterval(timer); return; }
      if (Date.now() - rec.started >= VOICE_MS) stopMic();
    }, 300);
    rec.timer = timer;
  } catch (e) { toast("无法使用麦克风"); }
}

function stopMic() {
  if (!rec) return;
  clearInterval(rec.timer);
  try { rec.recorder.stop(); } catch (_) { rec = null; }
}

function togglePeople(anchor) {
  if (popKind === "people") { closePop(); return; }
  popKind = "people";
  if (viewRoom) {
    const list = (api && api.mentions ? api.mentions() : []);
    openPop(list.length ? list.map((m) => `<button type="button" data-act="mention" data-name="${escapeHtml(m.name)}">${escapeHtml(m.name)}</button>`).join("") : `<div class="im-history-empty">没有可提及的成员</div>`, anchor);
  } else {
    const list = (api && api.cards ? api.cards() : []);
    openPop(list.length ? list.map((m) => `<button type="button" data-act="card" data-id="${escapeHtml(m.id)}" data-name="${escapeHtml(m.name)}">${escapeHtml(m.name)}</button>`).join("") : `<div class="im-history-empty">没有可发送的名片</div>`, anchor);
  }
}

async function sendCard(id, name) {
  if (!id || !api || !api.sendRich) return;
  try {
    await api.sendRich({ v: 1, kind: "card", card: id, name, text: name });
  } catch (e) { toast("名片发送失败：" + errText(e)); }
}

function toggleHistory() {
  const box = document.getElementById("conv-history");
  if (!box) return;
  box.classList.toggle("is-on");
  if (box.classList.contains("is-on")) { paintHistory(); const q = document.getElementById("conv-history-q"); if (q) q.focus(); }
}

function paintHistory() {
  const list = document.getElementById("conv-history-list");
  if (!list || !api || !api.messages) return;
  const q = ((document.getElementById("conv-history-q") || {}).value || "").trim().toLowerCase();
  const rows = api.messages().filter((m) => {
    const text = (preview(m) + " " + (m.text || "") + " " + ((m.media && m.media.name) || "")).toLowerCase();
    return !q || text.includes(q);
  }).slice(-80).reverse();
  list.innerHTML = rows.length ? rows.map((m) => `<button type="button" data-act="hist-jump" data-mid="${escapeHtml(m.id)}"><span>${escapeHtml(msgClockSafe(m.ts))}</span><b>${escapeHtml(preview(m))}</b></button>`).join("") : `<div class="im-history-empty">没有匹配的记录</div>`;
}

function jump(id) {
  const el = document.querySelector(`.im-msg[data-mid="${CSS.escape(id)}"]`);
  if (!el) return;
  el.scrollIntoView({ block: "center" });
  el.classList.add("im-flash");
  setTimeout(() => el.classList.remove("im-flash"), 1200);
}

async function doSend() {
  if (sending || !api) return;
  const input = document.getElementById("conv-input");
  const text = (input && input.value || "").trim();
  const items = draft().pending.slice();
  if (!text && !items.length) return;
  sending = true;
  try {
    if (text && api.sendText) {
      await api.sendText(text);
      if (input) { input.value = ""; fit(input); }
      draft().text = "";
    }
    for (const item of items) {
      const rich = await upload(item);
      if (api.sendRich) await api.sendRich(rich);
      draft().pending = draft().pending.filter((x) => x.localId !== item.localId);
      if (item.previewUrl) URL.revokeObjectURL(item.previewUrl);
      paintPending();
    }
  } catch (e) {
    toast("发送失败：" + errText(e));
  } finally { sending = false; syncSend(); }
}

async function upload(item) {
  if (item.kind === "card") return item;
  const put = await putBytes(item.bytes, item.mime || "application/octet-stream");
  let thumb = "";
  if (item.thumbBytes) {
    try { const t = await putBytes(item.thumbBytes, "image/jpeg"); thumb = t.parts[0] || ""; } catch (_) {}
  }
  return {
    v: 1,
    kind: item.kind,
    text: "",
    mime: item.mime || "",
    name: item.name || "",
    size: item.size || item.bytes.length,
    w: item.w || 0,
    h: item.h || 0,
    dur: item.dur || 0,
    parts: put.parts,
    home: put.home,
    thumb,
  };
}

async function putBytes(bytes, mime) {
  const parts = [];
  let home = "";
  for (let i = 0; i < bytes.length; i += CHUNK) {
    const slice = bytes.subarray(i, Math.min(i + CHUNK, bytes.length));
    const r = await NM.inv("blob_put_ref", { dataB64: bytesToB64(slice), mime: i === 0 ? mime : "application/octet-stream" });
    if (!r || !r.ref) throw new Error("上传没有返回地址");
    parts.push(r.ref);
    home = r.home || home;
  }
  if (!parts.length) throw new Error("没有可上传的内容");
  return { parts, home };
}

function bytesToB64(bytes) {
  let bin = "";
  const step = 0x8000;
  for (let i = 0; i < bytes.length; i += step) {
    bin += String.fromCharCode.apply(null, Array.from(bytes.subarray(i, Math.min(i + step, bytes.length))));
  }
  return btoa(bin);
}
function b64ToBytes(b64) {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
function imageSize(url) {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve({ w: img.naturalWidth || 0, h: img.naturalHeight || 0 });
    img.onerror = () => resolve({ w: 0, h: 0 });
    img.src = url;
  });
}

const KIND_LABEL = { image: "[图片]", voice: "[语音]", video: "[视频]", file: "[文件]", card: "[名片]" };
function kindLabel(kind) { return KIND_LABEL[kind] || "[消息]"; }
function fmtDur(ms) {
  const s = Math.max(0, Math.round((ms || 0) / 1000));
  return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0");
}
function fmtSize(n) {
  n = n || 0;
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return (n / 1024).toFixed(n < 10240 ? 1 : 0) + " KB";
  return (n / 1024 / 1024).toFixed(1) + " MB";
}
function errText(e) { return (e && e.message) ? e.message : String(e || "失败"); }
function toast(s) { if (window.toast) window.toast(s); }
function msgClockSafe(ts) {
  const d = new Date(ts || 0);
  if (!ts || Number.isNaN(d.getTime())) return "";
  return String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
}

function parseChat(body, typeUrl) {
  const raw = body || "";
  const tagged = raw.startsWith(CHAT_PREFIX);
  if (typeUrl !== "nmspace.v1/chat" && !tagged) return null;
  try {
    const o = JSON.parse(tagged ? raw.slice(CHAT_PREFIX.length) : raw);
    if (!o || typeof o !== "object" || !KIND_LABEL[o.kind]) return null;
    return o;
  } catch (_) { return null; }
}

function preview(m) {
  if (!m) return "";
  const media = m.media || parseChat(m.body, m.typeUrl);
  if (!media) return m.body || "";
  const tag = kindLabel(media.kind);
  if (media.kind === "file" || media.kind === "card") return tag + (media.name ? " " + media.name : "");
  if (media.text) return tag + " " + media.text;
  return tag;
}

function absorb(m) {
  if (!m) return m;
  if (m.media && m.kind && KIND_LABEL[m.kind]) {
    return { ...m, body: preview(m) };
  }
  const media = parseChat(m.body, m.typeUrl);
  if (!media) return { ...m, kind: "text", text: m.body || "" };
  const next = { ...m, kind: media.kind, text: media.text || "", media };
  next.body = preview(next);
  return next;
}

function encode(media) { return JSON.stringify(media); }
function channelBody(media) { return CHAT_PREFIX + JSON.stringify(media); }

function bubble(m) {
  const media = m.media || parseChat(m.body, m.typeUrl);
  if (!media) return { cls: "", html: escapeHtml(m.text || m.body || "") };
  if (media.kind === "image" || media.kind === "voice" || media.kind === "video") {
    const parts = (media.parts || []).join(",");
    return {
      cls: "im-bubble--media",
      html: `<div class="im-media im-${media.kind === "image" ? "photo-wrap" : media.kind}" data-kind="${media.kind}" data-mime="${escapeHtml(media.mime || "")}" data-home="${escapeHtml(media.home || "")}" data-parts="${escapeHtml(parts)}" data-name="${escapeHtml(media.name || "")}" data-thumb="${escapeHtml(media.thumb || "")}" data-dur="${media.dur || 0}" data-size="${media.size || 0}">${fetchHtml(media.kind, media.name, media.size, media.dur)}</div>`,
    };
  }
  if (media.kind === "file") {
    const parts = (media.parts || []).join(",");
    return {
      cls: "im-bubble--media",
      html: `<button type="button" class="im-file" data-act="save-file" data-home="${escapeHtml(media.home || "")}" data-parts="${escapeHtml(parts)}" data-name="${escapeHtml(media.name || "文件")}" data-size="${media.size || 0}"><span>${nmIcon("file")}</span><span><b>${escapeHtml(media.name || "文件")}</b><small>${fmtSize(media.size)} · 点击下载</small></span></button>`,
    };
  }
  if (media.kind === "card") {
    const who = media.name || "名片";
    const face = window.Profile ? Profile.faceHtml(media.card || "", who, 32) : "";
    return {
      cls: "im-bubble--media",
      html: `<button type="button" class="im-card" data-act="open-card" data-id="${escapeHtml(media.card || "")}">${face}<span><b>${escapeHtml(who)}</b><small>名片</small></span></button>`,
    };
  }
  return { cls: "", html: escapeHtml(media.text || "不支持的消息") };
}

const urlCache = new Map();

function partsOf(el) {
  return (el.dataset.parts || "").split(",").filter(Boolean);
}
function fetchHtml(kind, name, size, dur) {
  const ico = kind === "voice" ? "mic" : kind === "video" ? "video" : kind === "file" ? "file" : "image";
  const title = kind === "voice" ? "语音 " + fmtDur(dur) : kind === "video" ? (name || "视频") : kind === "file" ? (name || "文件") : "图片";
  const extra = kind === "voice" ? "" : fmtSize(size);
  return `<button type="button" class="im-fetch" data-act="fetch-media"><span>${nmIcon(ico)}</span><span><b>点击下载</b><small>${escapeHtml(title)}${extra && extra !== "0 B" ? " · " + extra : ""}</small></span></button>`;
}
function urisToUrl(parts, uris, mime) {
  const key = parts.join("|");
  if (urlCache.has(key)) return urlCache.get(key);
  const chunks = uris.map((uri) => {
    const comma = String(uri).indexOf(",");
    return b64ToBytes(comma >= 0 ? uri.slice(comma + 1) : uri);
  });
  const url = URL.createObjectURL(new Blob(chunks, { type: mime || "application/octet-stream" }));
  urlCache.set(key, url);
  return url;
}
async function localUris(parts) {
  if (!parts.length || !window.NM) return null;
  const uris = await NM.inv("blob_cached", { references: parts });
  if (!Array.isArray(uris) || uris.length !== parts.length || uris.some((u) => !u)) return null;
  return uris;
}
async function loadParts(parts, home, mime) {
  const key = parts.join("|");
  if (urlCache.has(key)) return urlCache.get(key);
  const uris = await localUris(parts);
  if (uris) return urisToUrl(parts, uris, mime);
  const fetched = [];
  for (const ref of parts) {
    fetched.push(await NM.inv("blob_get", { reference: ref, homeNode: home || "" }));
  }
  return urisToUrl(parts, fetched, mime);
}
function playButton(el) {
  return `<button type="button" class="im-file" data-act="play-video" data-home="${escapeHtml(el.dataset.home || "")}" data-parts="${escapeHtml(el.dataset.parts || "")}" data-name="${escapeHtml(el.dataset.name || "视频")}"><span>${nmIcon("video")}</span><span><b>${escapeHtml(el.dataset.name || "视频")}</b><small>${fmtDur(Number(el.dataset.dur) || 0)} · 播放</small></span></button>`;
}
function paintMedia(el, url, poster) {
  const kind = el.dataset.kind;
  el.dataset.state = "shown";
  if (kind === "image") {
    el.innerHTML = `<img class="im-photo" data-act="zoom" src="${url}" alt="">`;
  } else if (kind === "voice") {
    el.innerHTML = `<audio controls src="${url}"></audio><small>${fmtDur(Number(el.dataset.dur) || 0)}</small>`;
  } else if (kind === "video") {
    const size = Number(el.dataset.size) || 0;
    if (size > 12 * 1024 * 1024) {
      el.innerHTML = playButton(el);
    } else {
      el.innerHTML = `<video controls playsinline src="${url}" ${poster ? `poster="${poster}"` : ""}></video>`;
    }
  }
}
async function localPoster(el) {
  if (!el.dataset.thumb) return "";
  const uris = await localUris([el.dataset.thumb]);
  return uris ? uris[0] : "";
}
async function hydrate(root) {
  if (!root) return;
  const nodes = root.matches && root.matches("[data-kind]") ? [root] : [...root.querySelectorAll("[data-kind]")];
  for (const el of nodes) {
    if (el.dataset.state === "shown" || el.dataset.state === "busy") continue;
    const parts = partsOf(el);
    const uris = await localUris(parts);
    if (!uris) continue;
    if (el.dataset.kind === "video" && Number(el.dataset.size) > 12 * 1024 * 1024) {
      el.dataset.state = "shown";
      el.innerHTML = playButton(el);
      continue;
    }
    const mime = el.dataset.mime || (el.dataset.kind === "voice" ? "audio/webm" : el.dataset.kind === "video" ? "video/mp4" : "image/jpeg");
    const url = urisToUrl(parts, uris, mime);
    paintMedia(el, url, await localPoster(el));
  }
  const files = root.matches && root.matches(".im-file[data-parts]") ? [root] : [...root.querySelectorAll(".im-file[data-parts]")];
  for (const el of files) {
    if (el.dataset.local === "1" || el.dataset.act === "play-video") continue;
    const uris = await localUris(partsOf(el));
    if (!uris) continue;
    el.dataset.local = "1";
    const small = el.querySelector("small");
    if (small) small.textContent = fmtSize(Number(el.dataset.size) || 0);
  }
}

async function onMediaClick(e) {
  const fetchBtn = e.target.closest("[data-act='fetch-media']");
  if (fetchBtn) {
    const el = fetchBtn.closest("[data-kind]");
    if (!el || el.dataset.state === "busy") return;
    el.dataset.state = "busy";
    const label = fetchBtn.querySelector("b");
    if (label) label.textContent = "下载中…";
    try {
      const kind = el.dataset.kind;
      const mime = el.dataset.mime || (kind === "voice" ? "audio/webm" : kind === "video" ? "video/mp4" : "image/jpeg");
      const size = Number(el.dataset.size) || 0;
      if (kind === "video" && size > 12 * 1024 * 1024) {
        await NM.inv("media_file", {
          references: partsOf(el),
          homeNode: el.dataset.home || "",
          ext: extOf(el.dataset.name, "mp4"),
        });
        el.dataset.state = "shown";
        el.innerHTML = playButton(el);
        return;
      }
      const url = await loadParts(partsOf(el), el.dataset.home, mime);
      let poster = await localPoster(el);
      if (!poster && el.dataset.thumb) {
        try { poster = await NM.inv("blob_get", { reference: el.dataset.thumb, homeNode: el.dataset.home || "" }); } catch (_) {}
      }
      paintMedia(el, url, poster);
    } catch (err) {
      el.dataset.state = "";
      el.innerHTML = fetchHtml(el.dataset.kind, el.dataset.name, Number(el.dataset.size) || 0, Number(el.dataset.dur) || 0);
      toast("下载失败：" + errText(err));
    }
    return;
  }
  const zoom = e.target.closest("[data-act='zoom']");
  if (zoom && zoom.src && !zoom.closest(".im-lightbox")) {
    closeLight();
    const box = document.createElement("div");
    box.className = "im-lightbox";
    box.innerHTML = `<img src="${zoom.src}" alt="">`;
    box.addEventListener("click", () => box.remove());
    document.body.appendChild(box);
    return;
  }
  const save = e.target.closest("[data-act='save-file']");
  if (save) {
    try {
      const path = await NM.inv("save_media", {
        references: (save.dataset.parts || "").split(",").filter(Boolean),
        homeNode: save.dataset.home || "",
        filename: save.dataset.name || "文件",
      });
      save.dataset.local = "1";
      const small = save.querySelector("small");
      if (small) small.textContent = fmtSize(Number(save.dataset.size) || 0);
      toast("已保存到 " + path);
    } catch (err) { toast("保存失败：" + errText(err)); }
    return;
  }
  const play = e.target.closest("[data-act='play-video']");
  if (play) {
    try {
      const ext = extOf(play.dataset.name, "mp4");
      const path = await NM.inv("media_file", {
        references: (play.dataset.parts || "").split(",").filter(Boolean),
        homeNode: play.dataset.home || "",
        ext,
      });
      await NM.inv("open_path", { path });
    } catch (err) { toast("播放失败：" + errText(err)); }
    return;
  }
  const card = e.target.closest("[data-act='open-card']");
  if (card && card.dataset.id && typeof window.selectContact === "function") {
    window.selectContact(card.dataset.id);
  }
}

function extOf(name, fallback) {
  const m = /\.([A-Za-z0-9]{1,8})$/.exec(name || "");
  return m ? m[1].toLowerCase() : fallback;
}
function closeLight() {
  document.querySelectorAll(".im-lightbox").forEach((el) => el.remove());
}

document.addEventListener("click", (e) => {
  if (e.target.closest && e.target.closest(".im-composer")) onComposerClick(e);
  else if (e.target.closest && (e.target.closest(".im-media") || e.target.closest(".im-file") || e.target.closest(".im-card") || e.target.closest(".im-lightbox"))) onMediaClick(e);
});

window.Composer = { prepare, markup, attach, snapshot, absorb, preview, bubble, hydrate, encode, channelBody };

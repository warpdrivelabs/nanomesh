// ── 用户资料（P0 富属性 + P1 内容寻址头像）：本地按身份持久（键 nmspace:profile:<pk>，经 store.js 写穿后端），
//    连接后经 update_profile 发布到家节点（目录 LWW 收敛 + 联邦扩散）。
//    头像（P1）：选图 → 缩放 → blob_put 存到家节点内容寻址 blob 库 → 档案只带 "b3:<hash>"（不再内联撑爆目录）；
//    展示端按 hash 经 blob_get 拉取 + 本地缓存。兼容 P0 遗留的内联 data:URI 头像。
(function () {
  const KEY = (pk) => "nmspace:profile:" + pk;
  const STATUS_OPTS = [["", "（不设置）"], ["online", "在线"], ["away", "离开"], ["busy", "忙碌"], ["dnd", "勿扰"]];
  const AV_MAX = 200 * 1024; // 缩放后原始图上限（内容寻址，节点侧约 1MiB 硬限）
  const avCache = new Map(); // "b3:<hash>" -> data:URI（会话内缓存，避免重复 blob_get）
  let pendingAvatar = null;  // 模态框内待保存头像：null=未改动 / ""=移除 / data:URI=新图

  function get(pk) { if (!pk) return {}; try { return JSON.parse(localStorage.getItem(KEY(pk)) || "{}"); } catch (_) { return {}; } }
  function save(pk, obj) { if (!pk) return; try { localStorage.setItem(KEY(pk), JSON.stringify(obj)); } catch (_) {} }
  function statusLabel(v) { const o = STATUS_OPTS.find((x) => x[0] === v); return o ? o[1] : v || ""; }
  function isImg(a) { return typeof a === "string" && /^data:image\//.test(a); }
  function isRef(a) { return typeof a === "string" && /^b3:[0-9a-fA-F]{64}$/.test(a); }

  // 某个用户当前能直接显示的头像：本人优先本地原图，其次目录里已解析的 data:URI。
  function displayAvatar(id, hint) {
    if (id && window.Identity && Identity.current && id === Identity.current()) {
      const own = ownAvatar(id);
      if (isImg(own)) return own;
    }
    if (isImg(hint)) return hint;
    if (id && typeof window.entityById === "function") {
      const c = entityById(id);
      if (c && isImg(c.avatar)) return c.avatar;
    }
    return "";
  }

  // 用户头像圆：有图就显示图，否则首字。extra 可塞在线状态点。
  function faceHtml(id, name, size, extra, hint, domId) {
    const s = size || 32;
    const avatar = displayAvatar(id, hint);
    const bg = (typeof avatarColor === "function" && id) ? avatarColor(id) : "#3987e5";
    const ch = (typeof escapeHtml === "function" ? escapeHtml : (x) => x)(String(name || "?").slice(0, 1) || "?");
    const idAttr = domId ? ` id="${domId}"` : "";
    const cls = s <= 28 ? "av av-sm" : "av";
    if (isImg(avatar)) {
      return `<span class="${cls} av-img"${idAttr} style="width:${s}px;height:${s}px;flex:0 0 ${s}px;padding:0;overflow:hidden;background:${bg}"><img src="${avatar}" alt="" style="width:100%;height:100%;object-fit:cover;border-radius:50%;display:block">${extra || ""}</span>`;
    }
    return `<span class="${cls}"${idAttr} style="width:${s}px;height:${s}px;flex:0 0 ${s}px;background:${bg}">${ch}${extra || ""}</span>`;
  }

  // 头像渲染：传入的应是可直接用的 data:URI（已解析）；否则回退首字母底色块。
  function avatarHtml(avatar, fallbackChar, size, bg) {
    const s = size || 40;
    if (isImg(avatar)) return `<img src="${avatar}" alt="" style="width:${s}px;height:${s}px;border-radius:50%;object-fit:cover;display:block">`;
    const ch = (typeof escapeHtml === "function") ? escapeHtml(fallbackChar || "?") : (fallbackChar || "?");
    return `<div style="width:${s}px;height:${s}px;border-radius:50%;display:flex;align-items:center;justify-content:center;background:${bg || "var(--accent-soft)"};color:#fff;font-weight:700;font-size:${Math.round(s * 0.42)}px">${ch}</div>`;
  }

  // 选图 → 居中裁剪缩放到 size → JPEG data:URI。
  function fileToAvatar(file, size, quality) {
    size = size || 256; quality = quality || 0.85;
    return new Promise((resolve, reject) => {
      const img = new Image();
      const url = URL.createObjectURL(file);
      img.onload = () => {
        URL.revokeObjectURL(url);
        const c = document.createElement("canvas"); c.width = size; c.height = size;
        const ctx = c.getContext("2d");
        const s = Math.min(img.width, img.height);
        ctx.drawImage(img, (img.width - s) / 2, (img.height - s) / 2, s, s, 0, 0, size, size);
        resolve(c.toDataURL("image/jpeg", quality));
      };
      img.onerror = () => { URL.revokeObjectURL(url); reject(new Error("图片加载失败")); };
      img.src = url;
    });
  }

  // data:URI → 上传为内容寻址 blob，返回 "b3:<hash>"（失败返回 null）。
  async function putBlob(dataUri) {
    const m = /^data:([^;]+);base64,(.+)$/.exec(dataUri || "");
    if (!m || !window.NM || !NM.hasTauri || !NM.hasTauri()) return null;
    try { return await NM.inv("blob_put", { dataB64: m[2], mime: m[1] }); }
    catch (e) { if (window.toast) toast("头像上传失败：" + (e && e.message ? e.message : e)); return null; }
  }

  // 把一个头像引用解析为可直接展示的 data:URI（data:→原样；b3:→缓存/blob_get；其它→空）。
  async function resolveAvatar(ref, home) {
    if (isImg(ref)) return ref;
    if (!isRef(ref)) return "";
    if (avCache.has(ref)) return avCache.get(ref);
    if (!window.NM || !NM.hasTauri || !NM.hasTauri()) return "";
    try {
      const uri = await NM.inv("blob_get", { reference: ref, homeNode: home || "" });
      if (isImg(uri)) { avCache.set(ref, uri); return uri; }
    } catch (_) {}
    return "";
  }

  // 批量解析一组联系人的 b3: 头像（就地把 c.avatar 换成 data:URI），完成后回调重渲染。
  async function resolveList(list, onDone) {
    const pend = Array.isArray(list) ? list.filter((c) => c && isRef(c.avatar)) : [];
    if (!pend.length) { if (onDone) onDone(); return; }
    await Promise.all(pend.map(async (c) => { const uri = await resolveAvatar(c.avatar, c.homeNode); if (uri) c.avatar = uri; }));
    if (onDone) onDone();
  }

  // 本人头像（用于 chip/下拉，免拉取）：优先本地 data 缓存，其次 P0 遗留内联。
  function ownAvatar(pk) {
    const p = get(pk);
    if (isImg(p.avatarData)) return p.avatarData;
    if (isImg(p.avatar)) return p.avatar;
    return "";
  }

  async function publish() {
    const pk = window.Identity && Identity.current();
    if (!pk || !window.NM || !NM.hasTauri || !NM.hasTauri()) return false;
    const p = get(pk);
    const displayName = (p.displayName || (window.Identity ? Identity.nameOf(pk) : "") || "").trim();
    const links = Array.isArray(p.links) ? p.links
      : (p.links ? String(p.links).split(/[,\n]/).map((s) => s.trim()).filter(Boolean) : []);
    let avatar = (isRef(p.avatar) || isImg(p.avatar)) ? p.avatar : ""; // 发布 b3:hash（或 P0 遗留内联）
    // 头像若为内容寻址引用：用本地原图幂等重传，确保 blob 在「当前家节点」。
    // 否则换机/重连/换节点后 blob 不在本家节点 → 他人 blob_get 拉不到 → 头像不显示，
    // 得「再改一次」才好（用户反馈的问题）；内容寻址 hash 恒定，重传不改变引用。
    if (isImg(p.avatarData)) {
      const ref = await putBlob(p.avatarData);
      if (ref) { avatar = ref; avCache.set(ref, p.avatarData); }
    }
    try {
      await NM.inv("update_profile", {
        displayName, bio: p.bio || "", statusText: p.statusText || "",
        links, locale: p.locale || "", status: p.status || "", avatar,
      });
      return true;
    } catch (e) { if (window.toast) toast("资料发布失败：" + (e && e.message ? e.message : e)); return false; }
  }

  // ── 通用「名称 + 简介 + 头像」编辑器（群 / 频道复用）。保存时上传头像 blob 后回调 onSave({name,bio,avatar})。
  async function metaEditor(opts) {
    opts = opts || {};
    const cur = opts.avatar || "";
    let pending = null; // null=未改动 / ""=移除 / data:URI=新图
    const m = document.createElement("div");
    m.className = "sec-overlay on";
    m.innerHTML = `
      <div class="sec-box"><div class="sec-head">${escapeHtml(opts.title || "编辑信息")}<button class="sec-x">${nmIcon("close")}</button></div>
        <div class="sec-body"><div class="sec-sec">
          <div class="sec-row" style="align-items:center"><label>头像</label>
            <div style="display:flex;align-items:center;gap:10px">
              <span class="me-prev"></span>
              <button class="ns-btn me-pick">选择图片</button>
              <button class="ns-btn me-clear">移除</button>
              <input type="file" class="me-file" accept="image/*" style="display:none">
            </div></div>
          <div class="sec-row"><label>名称</label><input class="me-name sec-input" placeholder="${escapeHtml(opts.namePh || "名称")}"></div>
          <div class="sec-row"><label>${escapeHtml(opts.bioLabel || "简介")}</label><input class="me-bio sec-input" placeholder="${escapeHtml(opts.bioPh || "一句话简介")}"></div>
          <div class="sec-actions"><button class="ns-btn me-cancel">取消</button><button class="ns-btn ns-primary me-save">保存</button></div>
        </div></div></div>`;
    document.body.appendChild(m);
    let previewData = isImg(cur) ? cur : (isRef(cur) ? await resolveAvatar(cur, opts.homeNode) : "");
    const paint = () => { m.querySelector(".me-prev").innerHTML = avatarHtml(pending === "" ? "" : (pending || previewData), (opts.name || "?").slice(0, 1), 48); };
    m.querySelector(".me-name").value = opts.name || "";
    m.querySelector(".me-bio").value = opts.bio || "";
    paint();
    const close = () => m.remove();
    m.querySelector(".sec-x").onclick = close;
    m.querySelector(".me-cancel").onclick = close;
    m.addEventListener("click", (e) => { if (e.target === m) close(); });
    m.querySelector(".me-pick").onclick = () => m.querySelector(".me-file").click();
    m.querySelector(".me-clear").onclick = () => { pending = ""; paint(); };
    m.querySelector(".me-file").onchange = async (e) => {
      const f = e.target.files && e.target.files[0]; e.target.value = "";
      if (!f) return;
      try {
        let data = await fileToAvatar(f, 256, 0.85);
        if (data.length > AV_MAX) data = await fileToAvatar(f, 192, 0.72);
        if (data.length > AV_MAX) { if (window.toast) toast("图片太大，请换更简单的图"); return; }
        pending = data; paint();
      } catch (err) { if (window.toast) toast("处理图片失败：" + (err && err.message ? err.message : err)); }
    };
    m.querySelector(".me-save").onclick = async () => {
      let avatar = (isRef(cur) || isImg(cur)) ? cur : "";
      if (pending === "") avatar = "";
      else if (pending) { const ref = await putBlob(pending); if (ref) { avatar = ref; avCache.set(ref, pending); } else avatar = pending; }
      const name = m.querySelector(".me-name").value.trim();
      const bio = m.querySelector(".me-bio").value.trim();
      close();
      try { if (opts.onSave) await opts.onSave({ name, bio, avatar }); }
      catch (e) { if (window.toast) toast("保存失败：" + (e && e.message ? e.message : e)); }
    };
  }

  function ensureModal() {
    let m = document.getElementById("profile-modal");
    if (m) return m;
    m = document.createElement("div");
    m.className = "sec-overlay"; m.id = "profile-modal";
    m.innerHTML = `
      <div class="sec-box">
        <div class="sec-head"><span class="ico">${nmIcon("contacts")}</span>编辑资料 <button class="sec-x" id="pf-close" title="关闭">${nmIcon("close")}</button></div>
        <div class="sec-body"><div class="sec-sec">
          <div class="sec-row" style="align-items:center">
            <label>头像</label>
            <div style="display:flex;align-items:center;gap:10px">
              <span id="pf-av-prev"></span>
              <button class="ns-btn" id="pf-av-pick">选择图片</button>
              <button class="ns-btn" id="pf-av-clear">移除</button>
              <input type="file" id="pf-av-file" accept="image/*" style="display:none">
            </div>
          </div>
          <div class="sec-row"><label>昵称</label><input id="pf-name" class="sec-input" placeholder="显示名"></div>
          <div class="sec-row"><label>状态</label><select id="pf-status" class="sec-input">${STATUS_OPTS.map(([v, l]) => `<option value="${v}">${l}</option>`).join("")}</select></div>
          <div class="sec-row"><label>状态文本</label><input id="pf-stext" class="sec-input" placeholder="如：在开会 / 摸鱼中"></div>
          <div class="sec-row"><label>简介</label><input id="pf-bio" class="sec-input" placeholder="一句话介绍自己"></div>
          <div class="sec-row"><label>链接</label><input id="pf-links" class="sec-input" placeholder="主页 / 社交，逗号分隔"></div>
          <div class="sec-row"><label>语言</label><input id="pf-locale" class="sec-input" placeholder="如 zh-CN"></div>
          <div class="sec-hint">头像以<b>内容寻址 blob</b>存到你的家节点，档案只带 <code>b3:&lt;hash&gt;</code>（不内联，联邦按需拉取+缓存）。其余资料经目录 <b>LWW</b> 收敛后其他人可见。</div>
          <div class="sec-actions"><button class="ns-btn" id="pf-cancel">取消</button><button class="ns-btn ns-primary" id="pf-save">保存并发布</button></div>
        </div></div>
      </div>`;
    document.body.appendChild(m);
    const close = () => m.classList.remove("on");
    m.querySelector("#pf-close").addEventListener("click", close);
    m.querySelector("#pf-cancel").addEventListener("click", close);
    m.addEventListener("click", (e) => { if (e.target === m) close(); });
    m.querySelector("#pf-save").addEventListener("click", onSave);
    m.querySelector("#pf-av-pick").addEventListener("click", () => m.querySelector("#pf-av-file").click());
    m.querySelector("#pf-av-clear").addEventListener("click", () => { pendingAvatar = ""; renderPreview(""); });
    m.querySelector("#pf-av-file").addEventListener("change", async (e) => {
      const f = e.target.files && e.target.files[0]; e.target.value = "";
      if (!f) return;
      try {
        let data = await fileToAvatar(f, 256, 0.85);
        if (data.length > AV_MAX) data = await fileToAvatar(f, 192, 0.72);
        if (data.length > AV_MAX) { if (window.toast) toast("图片太大，请换更简单的图"); return; }
        pendingAvatar = data; renderPreview(data);
      } catch (err) { if (window.toast) toast("处理图片失败：" + (err && err.message ? err.message : err)); }
    });
    return m;
  }

  function renderPreview(av) { const box = document.getElementById("pf-av-prev"); if (box) box.innerHTML = avatarHtml(av, "?", 48); }

  async function onSave() {
    const pk = window.Identity && Identity.current();
    if (!pk) { if (window.toast) toast("未登录"); return; }
    const q = (id) => document.getElementById(id);
    const prev = get(pk);
    let avatar = isRef(prev.avatar) || isImg(prev.avatar) ? prev.avatar : "";
    let avatarData = isImg(prev.avatarData) ? prev.avatarData : (isImg(prev.avatar) ? prev.avatar : "");
    if (pendingAvatar === "") { avatar = ""; avatarData = ""; }
    else if (pendingAvatar) {
      avatarData = pendingAvatar;
      const ref = await putBlob(pendingAvatar);           // 上传为内容寻址 blob
      if (ref) { avatar = ref; avCache.set(ref, pendingAvatar); }
      else { avatar = pendingAvatar; }                    // 上传失败：回退内联（仍可用）
    }
    const displayName = q("pf-name").value.trim();
    save(pk, {
      displayName, status: q("pf-status").value, statusText: q("pf-stext").value.trim(),
      bio: q("pf-bio").value.trim(),
      links: q("pf-links").value.split(/[,\n]/).map((s) => s.trim()).filter(Boolean),
      locale: q("pf-locale").value.trim(), avatar, avatarData,
    });
    if (displayName && window.Identity) Identity.setName(pk, displayName);
    if (typeof updateUserChip === "function") updateUserChip();
    document.getElementById("profile-modal").classList.remove("on");
    const ok = await publish();
    applyPresence(); // P2：把所选状态应用为 live presence
    if (window.toast) toast(ok ? "资料已保存并发布" : "资料已本地保存（连接后自动发布）");
  }

  function open() {
    const pk = window.Identity && Identity.current();
    const m = ensureModal();
    const p = get(pk);
    const q = (id) => document.getElementById(id);
    pendingAvatar = null;
    q("pf-name").value = p.displayName || (window.Identity ? Identity.nameOf(pk) : "") || "";
    q("pf-status").value = p.status || "";
    q("pf-stext").value = p.statusText || "";
    q("pf-bio").value = p.bio || "";
    q("pf-links").value = Array.isArray(p.links) ? p.links.join(", ") : (p.links || "");
    q("pf-locale").value = p.locale || "";
    renderPreview(ownAvatar(pk));
    m.classList.add("on");
  }

  window.Profile = { get, save, publish, open, statusLabel, avatarHtml, faceHtml, displayAvatar, isImg, isRef, resolveAvatar, resolveList, ownAvatar, presenceColor, presenceLabel, presenceDot, applyPresence, metaEditor };
  window.openProfileModal = open;

  // ── P2 在线状态 ──
  function presenceColor(p) {
    return p === "online" ? "#22c55e" : p === "away" ? "#f59e0b" : (p === "busy" || p === "dnd") ? "#ef4444" : "#94a3b8";
  }
  function presenceLabel(p) {
    return ({ online: "在线", away: "离开", busy: "忙碌", dnd: "勿扰" })[p] || "离线";
  }
  // 头像角标小圆点（容器需 position:relative）。size=直径像素。
  function presenceDot(p, size) {
    const s = size || 10;
    return `<span class="pdot" style="width:${s}px;height:${s}px;background:${presenceColor(p)}"></span>`;
  }
  // 把本地选择的状态应用为 live presence（连接后 / 改状态后调用）。
  async function applyPresence() {
    const pk = window.Identity && Identity.current();
    if (!pk || !window.NM || !NM.hasTauri || !NM.hasTauri()) return;
    try { await NM.inv("presence_set", { status: get(pk).status || "online" }); } catch (_) {}
  }
})();

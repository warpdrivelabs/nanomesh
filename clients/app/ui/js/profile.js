// ── 用户资料（P0 富属性 + P1 内容寻址头像）：按公钥存在本机 SQLite。
//    连接后经 update_profile 发布到家节点（目录 LWW 收敛 + 联邦扩散）。
//    头像（P1）：选图 → 缩放 → blob_put 存到家节点内容寻址 blob 库 → 档案只带 "b3:<hash>"（不再内联撑爆目录）；
//    展示端按 hash 经 blob_get 拉取 + 本地缓存。兼容 P0 遗留的内联 data:URI 头像。
(function () {
  const MEM = {};
  const STATUS_OPTS = [["", "（不设置）"], ["online", "在线"], ["away", "离开"], ["busy", "忙碌"], ["dnd", "勿扰"]];
  const AV_MAX = 200 * 1024; // 缩放后原始图上限（内容寻址，节点侧约 1MiB 硬限）
  const avCache = new Map(); // "b3:<hash>" -> data:URI（内存；磁盘另有 app_data/blobs）
  // 实体当前头像：id -> { ref, uri }。ref 未变则直接用 uri，不调网络；仅头像变更时重拉并更新。
  const entityAv = new Map();
  let pendingAvatar = null;  // 模态框内待保存头像：null=未改动 / ""=移除 / data:URI=新图

  async function load() {
    Object.keys(MEM).forEach((k) => delete MEM[k]);
    if (!window.NM) return MEM;
    try {
      const obj = JSON.parse((await NM.inv("profile_load")) || "{}");
      if (obj && typeof obj === "object") Object.assign(MEM, obj);
    } catch (_) {}
    // 解锁后资料进内存 → 同步当前用户扩展属性到 Identity.user，供全局随时访问。
    if (window.Identity && Identity.hydrateUser) Identity.hydrateUser();
    return MEM;
  }
  function get(pk) { if (!pk || !MEM[pk] || typeof MEM[pk] !== "object") return {}; return Object.assign({}, MEM[pk]); }
  function save(pk, obj) {
    if (!pk) return;
    MEM[pk] = obj;
    if (window.NM) NM.inv("profile_save", { id: pk, data: JSON.stringify(obj) }).catch(() => {});
    // 当前用户资料变更（含扩展属性）→ 刷新 Identity.user 内存镜像。
    if (window.Identity && Identity.current && Identity.current() === pk && Identity.hydrateUser) {
      Identity.hydrateUser(pk);
    }
  }
  /** 某用户扩展属性行（已规范化）；供 Identity.hydrateUser / 编辑器共用。 */
  function extAttrRows(pk) {
    return normalizeExtAttrs(get(pk).extAttrs);
  }
  function statusLabel(v) { const o = STATUS_OPTS.find((x) => x[0] === v); return o ? o[1] : v || ""; }
  function isImg(a) { return typeof a === "string" && /^data:image\//.test(a); }
  function isRef(a) { return typeof a === "string" && /^b3:[0-9a-fA-F]{64}$/.test(a); }

  // 记下实体头像（展示层与缓存层共用）。
  function rememberEntityAvatar(id, ref, uri) {
    if (!id || !isImg(uri)) return;
    const r = isRef(ref) ? ref : (isImg(ref) ? "" : String(ref || ""));
    entityAv.set(id, { ref: r || ref || "", uri });
    if (isRef(ref)) avCache.set(ref, uri);
  }

  // 某个用户当前能直接显示的头像：本人优先本地原图 → 实体缓存 → hint/目录里的 data:URI。
  function displayAvatar(id, hint) {
    if (id && window.Identity && Identity.current && id === Identity.current()) {
      const own = ownAvatar(id);
      if (isImg(own)) return own;
    }
    if (isImg(hint)) return hint;
    if (id && entityAv.has(id)) {
      const hit = entityAv.get(id);
      // hint 为新 b3: 且与缓存 ref 不同 → 视为已变更，不返回旧图（等 resolve 更新）。
      if (hit && isImg(hit.uri) && !(isRef(hint) && hint !== hit.ref)) return hit.uri;
    }
    if (id && typeof window.entityById === "function") {
      const c = entityById(id);
      if (c && isImg(c.avatar)) return c.avatar;
    }
    if (isRef(hint) && avCache.has(hint)) return avCache.get(hint);
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
      return `<span class="${cls} av-img"${idAttr} style="width:${s}px;height:${s}px;flex:0 0 ${s}px;padding:0;overflow:hidden;background:${bg}"><img src="${avatar}" alt="" style="width:100%;height:100%;object-fit:cover;border-radius:var(--avatar-radius);display:block">${extra || ""}</span>`;
    }
    return `<span class="${cls}"${idAttr} style="width:${s}px;height:${s}px;flex:0 0 ${s}px;background:${bg}">${ch}${extra || ""}</span>`;
  }

  // 头像渲染：传入的应是可直接用的 data:URI（已解析）；否则回退首字母底色块。
  function avatarHtml(avatar, fallbackChar, size, bg) {
    const s = size || 40;
    if (isImg(avatar)) return `<img src="${avatar}" alt="" style="width:${s}px;height:${s}px;border-radius:var(--avatar-radius);object-fit:cover;display:block">`;
    const ch = (typeof escapeHtml === "function") ? escapeHtml(fallbackChar || "?") : (fallbackChar || "?");
    return `<div class="av" style="width:${s}px;height:${s}px;flex:0 0 ${s}px;background:${bg || "var(--accent)"};font-size:${Math.round(s * 0.42)}px">${ch}</div>`;
  }

  // 选图 → 交互裁剪（拖动位置 + 缩放）→ 导出正方形 JPEG data:URI。
  // 用户 / 群 / 频道头像共用。file 为 File；返回 Promise<data:URI>，取消则 reject("cancelled")。
  // 通用裁剪：opts = { outW, outH, quality, title, minZoom, maxZoom }
  // z=1 为铺满视口（cover）；z<1 可缩小（留边填背景色）。
  function openImageCrop(file, opts) {
    opts = opts || {};
    const outW = opts.outW || 256;
    const outH = opts.outH || outW;
    const quality = opts.quality || 0.85;
    const title = opts.title || "调整图片";
    const minZoom = opts.minZoom != null ? opts.minZoom : 0.5;
    const maxZoom = opts.maxZoom != null ? opts.maxZoom : 3;
    const aspect = outW / outH;
    const isWide = aspect > 1.15;
    return new Promise((resolve, reject) => {
      const url = URL.createObjectURL(file);
      const img = new Image();
      img.onload = () => {
        const m = document.createElement("div");
        m.className = "sec-overlay on av-crop-overlay";
        m.innerHTML = `
          <div class="av-crop-box${isWide ? " av-crop-box--wide" : ""}" role="dialog" aria-label="${title}">
            <div class="av-crop-head"><span>${title}</span><button type="button" class="sec-x av-crop-x" title="关闭">${nmIcon("close")}</button></div>
            <div class="av-crop-stage">
              <div class="av-crop-view${isWide ? " av-crop-view--wide" : ""}" tabindex="0" style="aspect-ratio:${outW} / ${outH}">
                <img class="av-crop-img" alt="" draggable="false">
              </div>
            </div>
            <div class="av-crop-zoom">
              <span class="av-crop-zoom-lab">缩小</span>
              <input type="range" class="av-crop-range" min="${Math.round(minZoom * 100)}" max="${Math.round(maxZoom * 100)}" value="100" step="1">
              <span class="av-crop-zoom-lab">放大</span>
            </div>
            <div class="av-crop-hint">拖动移动位置，滑块或滚轮调整大小（可缩小）</div>
            <div class="av-crop-actions">
              <button type="button" class="ns-btn av-crop-cancel">取消</button>
              <button type="button" class="ns-btn ns-primary av-crop-ok">确定</button>
            </div>
          </div>`;
        document.body.appendChild(m);
        const view = m.querySelector(".av-crop-view");
        const imgEl = m.querySelector(".av-crop-img");
        const range = m.querySelector(".av-crop-range");
        imgEl.src = url;
        const br = view.getBoundingClientRect();
        const viewW = Math.max(160, Math.round(br.width) || (isWide ? 360 : 280));
        const viewH = Math.max(48, Math.round(br.height) || Math.round(viewW / aspect));

        // cover：铺满视口；z 相对 cover，可 <1 缩小。
        const cover = Math.max(viewW / img.naturalWidth, viewH / img.naturalHeight);
        let z = 1, ox = 0, oy = 0;

        function drawn() {
          const s = cover * z;
          return { s, w: img.naturalWidth * s, h: img.naturalHeight * s };
        }
        function clamp() {
          const { w, h } = drawn();
          // 图大于视口：限制不露边；图小于视口：限制不拖出框外。
          const maxX = Math.abs(w - viewW) / 2;
          const maxY = Math.abs(h - viewH) / 2;
          ox = Math.max(-maxX, Math.min(maxX, ox));
          oy = Math.max(-maxY, Math.min(maxY, oy));
        }
        function paint() {
          clamp();
          const { w, h } = drawn();
          imgEl.style.width = w + "px";
          imgEl.style.height = h + "px";
          imgEl.style.left = ((viewW - w) / 2 + ox) + "px";
          imgEl.style.top = ((viewH - h) / 2 + oy) + "px";
        }
        function setZoom(nz) {
          z = Math.max(minZoom, Math.min(maxZoom, nz));
          range.value = String(Math.round(z * 100));
          paint();
        }

        let drag = null;
        const onMove = (e) => {
          if (!drag) return;
          ox = drag.ox0 + (e.clientX - drag.x0);
          oy = drag.oy0 + (e.clientY - drag.y0);
          paint();
        };
        const onUp = () => {
          if (!drag) return;
          drag = null;
          view.classList.remove("dragging");
          window.removeEventListener("pointermove", onMove);
          window.removeEventListener("pointerup", onUp);
        };
        view.addEventListener("pointerdown", (e) => {
          if (e.button !== 0) return;
          e.preventDefault();
          drag = { x0: e.clientX, y0: e.clientY, ox0: ox, oy0: oy };
          view.classList.add("dragging");
          view.setPointerCapture?.(e.pointerId);
          window.addEventListener("pointermove", onMove);
          window.addEventListener("pointerup", onUp);
        });
        view.addEventListener("wheel", (e) => {
          e.preventDefault();
          setZoom(z * (e.deltaY < 0 ? 1.08 : 1 / 1.08));
        }, { passive: false });
        range.addEventListener("input", () => setZoom(Number(range.value) / 100));

        function finish(ok) {
          window.removeEventListener("pointermove", onMove);
          window.removeEventListener("pointerup", onUp);
          URL.revokeObjectURL(url);
          m.remove();
          if (!ok) { reject(new Error("cancelled")); return; }
          const d = drawn();
          const sx = ((viewW - d.w) / 2 + ox) * -1 / d.s;
          const sy = ((viewH - d.h) / 2 + oy) * -1 / d.s;
          const sw = viewW / d.s;
          const sh = viewH / d.s;
          const c = document.createElement("canvas");
          c.width = outW; c.height = outH;
          const ctx = c.getContext("2d");
          // 缩小后可能留边：先铺背景再画可见图区。
          ctx.fillStyle = "#1a1a1a";
          ctx.fillRect(0, 0, outW, outH);
          const srcL = Math.max(0, sx);
          const srcT = Math.max(0, sy);
          const srcR = Math.min(img.naturalWidth, sx + sw);
          const srcB = Math.min(img.naturalHeight, sy + sh);
          if (srcR > srcL && srcB > srcT) {
            const dw = outW / sw, dh = outH / sh;
            ctx.drawImage(
              img,
              srcL, srcT, srcR - srcL, srcB - srcT,
              (srcL - sx) * dw, (srcT - sy) * dh, (srcR - srcL) * dw, (srcB - srcT) * dh
            );
          }
          resolve(c.toDataURL("image/jpeg", quality));
        }
        m.querySelector(".av-crop-ok").onclick = () => finish(true);
        m.querySelector(".av-crop-cancel").onclick = () => finish(false);
        m.querySelector(".av-crop-x").onclick = () => finish(false);
        m.addEventListener("click", (e) => { if (e.target === m) finish(false); });
        paint();
      };
      img.onerror = () => { URL.revokeObjectURL(url); reject(new Error("图片加载失败")); };
      img.src = url;
    });
  }

  function openAvatarCrop(file, size, quality) {
    return openImageCrop(file, {
      outW: size || 256, outH: size || 256, quality: quality || 0.85, title: "调整头像",
    });
  }

  // 选图 → 打开裁剪器 → JPEG data:URI（取消时返回 null，不抛错，方便调用方静默）。
  async function fileToAvatar(file, size, quality) {
    try {
      return await openAvatarCrop(file, size, quality);
    } catch (e) {
      if (e && e.message === "cancelled") return null;
      throw e;
    }
  }

  // data:URI → 上传为内容寻址 blob，返回 "b3:<hash>"（失败返回 null）。
  async function putBlob(dataUri) {
    const m = /^data:([^;]+);base64,(.+)$/.exec(dataUri || "");
    if (!m || !window.NM || !NM.hasTauri || !NM.hasTauri()) return null;
    try { return await NM.inv("blob_put", { dataB64: m[2], mime: m[1] }); }
    catch (e) { if (window.toast) toast("头像上传失败：" + (e && e.message ? e.message : e)); return null; }
  }

  // 把一个头像引用解析为可直接展示的 data:URI。
  // 顺序：内存 → 本机磁盘(blob_cached，不联网) → blob_get(仅未命中时联网，并写磁盘)。
  // opts.cacheOnly=true 时不联网；opts.force=true 时跳过内存（仍可用磁盘）。
  async function resolveAvatar(ref, home, opts) {
    opts = opts || {};
    if (isImg(ref)) return ref;
    if (!isRef(ref)) return "";
    if (!opts.force && avCache.has(ref)) return avCache.get(ref);
    if (!window.NM || !NM.hasTauri || !NM.hasTauri()) return "";
    try {
      const disk = await NM.inv("blob_cached", { references: [ref] });
      if (disk && isImg(disk[0])) { avCache.set(ref, disk[0]); return disk[0]; }
    } catch (_) {}
    if (opts.cacheOnly) return "";
    try {
      const uri = await NM.inv("blob_get", { reference: ref, homeNode: home || "" });
      if (isImg(uri)) { avCache.set(ref, uri); return uri; }
    } catch (_) {}
    return "";
  }

  // 批量解析联系人/群/频道头像：
  // - ref 未变且已有缓存 → 直接套用，零 IO
  // - 仅对「新 ref / 缓存未命中」批量读盘，再对仍缺的联网 blob_get
  // - 头像变更（新 b3:）会替换 entityAv 并触发重绘
  async function resolveList(list, onDone) {
    const rows = Array.isArray(list) ? list.filter((c) => c && c.id) : [];
    if (!rows.length) { if (onDone) onDone(); return; }

    const needNet = [];
    const needDisk = [];
    let applied = false;

    rows.forEach((c) => {
      const ref = c.avatar;
      if (isImg(ref)) {
        rememberEntityAvatar(c.id, "", ref);
        return;
      }
      if (!isRef(ref)) return;
      const prev = entityAv.get(c.id);
      // 同 ref 已缓存 → 直接写回对象，不访问磁盘/网络
      if (prev && prev.ref === ref && isImg(prev.uri)) {
        c.avatar = prev.uri;
        applied = true;
        return;
      }
      if (avCache.has(ref)) {
        const uri = avCache.get(ref);
        c.avatar = uri;
        rememberEntityAvatar(c.id, ref, uri);
        applied = true;
        return;
      }
      needDisk.push(c);
    });

    if (needDisk.length && window.NM && NM.hasTauri && NM.hasTauri()) {
      try {
        const refs = needDisk.map((c) => c.avatar);
        const uris = await NM.inv("blob_cached", { references: refs });
        needDisk.forEach((c, i) => {
          const uri = uris && uris[i];
          if (isImg(uri)) {
            avCache.set(c.avatar, uri);
            rememberEntityAvatar(c.id, c.avatar, uri);
            c.avatar = uri;
            applied = true;
          } else {
            needNet.push(c);
          }
        });
      } catch (_) {
        needDisk.forEach((c) => needNet.push(c));
      }
    }

    if (needNet.length) {
      await Promise.all(needNet.map(async (c) => {
        const ref = c.avatar;
        const uri = await resolveAvatar(ref, c.homeNode);
        if (!uri) return;
        rememberEntityAvatar(c.id, ref, uri);
        c.avatar = uri;
        applied = true;
      }));
    }

    if (onDone && applied) onDone();
    else if (onDone && !applied) onDone();
  }

  /** 头像引用变更时调用：清掉该实体旧缓存记忆，下次 resolve 会按新 ref 拉（磁盘按 hash 命中则仍不联网）。 */
  function invalidateEntityAvatar(id) {
    if (id) entityAv.delete(id);
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
        attributes: normalizeExtAttrs(p.extAttrs),
      });
      return true;
    } catch (e) { if (window.toast) toast("资料发布失败：" + (e && e.message ? e.message : e)); return false; }
  }

  // 扩展属性规范化：去空 key、禁系统保留键；供本地存储与 update_profile 共用。
  const RESERVED_ATTR = { status: 1, presence: 1, name: 1 };
  function normalizeExtAttrs(rows) {
    const out = [];
    const seen = Object.create(null);
    (Array.isArray(rows) ? rows : []).forEach((r) => {
      if (!r || typeof r !== "object") return;
      const key = String(r.key || "").trim();
      if (!key || RESERVED_ATTR[key] || key.indexOf("__note:") === 0 || seen[key]) return;
      seen[key] = 1;
      out.push({
        key,
        value: String(r.value != null ? r.value : ""),
        note: String(r.note != null ? r.note : ""),
      });
    });
    return out;
  }

  // 扩展属性对话框：三列 key / value / note，增删改 + 保存/关闭。
  function openExtAttrsEditor() {
    const pk = window.Identity && Identity.current();
    if (!pk) { if (window.toast) toast("未登录"); return; }
    let old = document.getElementById("pf-attrs-modal");
    if (old) old.remove();
    const draft = normalizeExtAttrs(get(pk).extAttrs).map((r) => ({ key: r.key, value: r.value, note: r.note }));
    const m = document.createElement("div");
    m.className = "sec-overlay on";
    m.id = "pf-attrs-modal";
    m.innerHTML = `
      <div class="dlg pf-attrs-dlg" role="dialog" aria-label="扩展属性">
        <div class="dlg-head">
          <span class="ico">${nmIcon("list")}</span>
          <span class="dlg-title">扩展属性</span>
          <span class="dlg-sub">键 / 值 / 备注 · 保存后随资料发布</span>
          <button type="button" class="sec-x pf-attrs-x" title="关闭">${nmIcon("close")}</button>
        </div>
        <div class="dlg-body pf-attrs-body">
          <div class="pf-attrs-toolbar">
            <button type="button" class="ns-btn pf-attrs-add">${nmIcon("plus")} 添加</button>
            <span class="pf-attrs-hint">勿使用 status / presence / name（系统保留）</span>
          </div>
          <div class="pf-attrs-table-wrap">
            <table class="pf-attrs-table">
              <thead><tr><th class="c-key">Key</th><th class="c-val">Value</th><th class="c-note">Note</th><th class="c-act"></th></tr></thead>
              <tbody class="pf-attrs-tbody"></tbody>
            </table>
            <div class="pf-attrs-empty" hidden>暂无扩展属性，点击「添加」新建一行</div>
          </div>
        </div>
        <div class="dlg-foot">
          <div class="sec-msg"></div>
          <button type="button" class="ns-btn pf-attrs-close">关闭</button>
          <button type="button" class="ns-btn ns-primary pf-attrs-save">保存</button>
        </div>
      </div>`;
    document.body.appendChild(m);
    const tbody = m.querySelector(".pf-attrs-tbody");
    const empty = m.querySelector(".pf-attrs-empty");
    const esc = typeof escapeHtml === "function" ? escapeHtml : (s) => String(s)
      .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/"/g, "&quot;");

    function paint() {
      tbody.innerHTML = draft.map((r, i) => `
        <tr data-i="${i}">
          <td class="c-key"><input class="sec-input pf-a-key" data-f="key" value="${esc(r.key)}" placeholder="key" spellcheck="false"></td>
          <td class="c-val"><input class="sec-input pf-a-val" data-f="value" value="${esc(r.value)}" placeholder="value"></td>
          <td class="c-note"><input class="sec-input pf-a-note" data-f="note" value="${esc(r.note)}" placeholder="备注（可选）"></td>
          <td class="c-act"><button type="button" class="ns-btn ns-ico pf-a-del" title="删除">${nmIcon("trash")}</button></td>
        </tr>`).join("");
      empty.hidden = draft.length > 0;
      tbody.querySelectorAll("input").forEach((inp) => {
        inp.addEventListener("input", () => {
          const i = Number(inp.closest("tr").dataset.i);
          const f = inp.dataset.f;
          if (!draft[i] || !f) return;
          draft[i][f] = inp.value;
        });
      });
      tbody.querySelectorAll(".pf-a-del").forEach((btn) => {
        btn.onclick = () => {
          const i = Number(btn.closest("tr").dataset.i);
          draft.splice(i, 1);
          paint();
        };
      });
    }
    function syncFromDom() {
      tbody.querySelectorAll("tr").forEach((tr) => {
        const i = Number(tr.dataset.i);
        if (!draft[i]) return;
        draft[i].key = (tr.querySelector(".pf-a-key") || {}).value || "";
        draft[i].value = (tr.querySelector(".pf-a-val") || {}).value || "";
        draft[i].note = (tr.querySelector(".pf-a-note") || {}).value || "";
      });
    }
    function close() { m.remove(); }
    m.querySelector(".pf-attrs-add").onclick = () => {
      syncFromDom();
      draft.push({ key: "", value: "", note: "" });
      paint();
      const last = tbody.querySelector("tr:last-child .pf-a-key");
      if (last) last.focus();
    };
    m.querySelector(".pf-attrs-save").onclick = async () => {
      syncFromDom();
      const rows = normalizeExtAttrs(draft);
      const dup = draft.filter((r) => String(r.key || "").trim()).length !== rows.length;
      const bad = draft.some((r) => {
        const k = String(r.key || "").trim();
        return k && (RESERVED_ATTR[k] || k.indexOf("__note:") === 0);
      });
      if (bad) { if (window.toast) toast("含系统保留键，已跳过；请改名后再保存"); }
      else if (dup && window.toast) toast("已去除空键或重复键");
      const prev = get(pk);
      save(pk, Object.assign({}, prev, { extAttrs: rows }));
      refreshAttrsTip();
      close();
      const ok = await publish();
      if (window.toast) toast(ok ? "扩展属性已保存并发布" : "扩展属性已本地保存（连接后随资料发布）");
    };
    m.querySelector(".pf-attrs-close").onclick = close;
    m.querySelector(".pf-attrs-x").onclick = close;
    m.addEventListener("click", (e) => { if (e.target === m) close(); });
    paint();
  }

  // 横幅封面：交互裁剪 JPEG（频道 Hero 用，比例约 3:1）。
  async function fileToCover(file, w, h, quality) {
    try {
      return await openImageCrop(file, {
        outW: w || 960, outH: h || 320, quality: quality || 0.82, title: "调整封面",
      });
    } catch (e) {
      if (e && e.message === "cancelled") return null;
      throw e;
    }
  }

  // 已裁好的 data:URI 再缩一档（封面过大时用，避免重开裁剪丢位置）。
  function reencodeRect(dataUri, w, h, quality) {
    return new Promise((resolve, reject) => {
      const img = new Image();
      img.onload = () => {
        const c = document.createElement("canvas");
        c.width = w; c.height = h;
        c.getContext("2d").drawImage(img, 0, 0, w, h);
        resolve(c.toDataURL("image/jpeg", quality || 0.72));
      };
      img.onerror = () => reject(new Error("图片加载失败"));
      img.src = dataUri;
    });
  }

  // ── 通用「名称 + 简介 + 头像[+封面]」编辑器（群 / 频道复用）。
  //    opts.withCover=true 时多封面行；onSave({name,bio,avatar,cover})。
  async function metaEditor(opts) {
    opts = opts || {};
    const cur = opts.avatar || "";
    const curCover = opts.cover || "";
    const withCover = !!opts.withCover;
    let pending = null; // null=未改动 / ""=移除 / data:URI=新图
    let pendingCover = null;
    const m = document.createElement("div");
    m.className = "sec-overlay on";
    const ico = opts.ico || (withCover ? "channels" : "groups");
    const coverRows = withCover ? `
          <label>封面</label>
          <div class="me-cover-field">
            <div class="me-cover-prev"></div>
            <div class="me-field-ops">
              <button type="button" class="ns-btn ns-ico me-cover-pick" title="选择封面">${nmIcon("image")}</button>
              <button type="button" class="ns-btn ns-ico me-cover-clear" title="移除封面">${nmIcon("trash")}</button>
              <input type="file" class="me-cover-file" accept="image/*" hidden>
            </div>
            <div class="me-field-hint">建议宽幅约 3:1；未设置时用品牌色块</div>
          </div>` : "";
    m.innerHTML = `
      <div class="dlg me-dlg">
        <div class="dlg-head">
          <span class="ico">${nmIcon(ico)}</span>
          <span class="dlg-title">${escapeHtml(opts.title || "编辑信息")}</span>
          <span class="dlg-sub">${escapeHtml(opts.sub || "名称、简介与头像")}</span>
          <button type="button" class="sec-x me-x" title="关闭">${nmIcon("close")}</button>
        </div>
        <div class="dlg-main">
          <aside class="dlg-aside">
            <span class="me-prev pf-av"></span>
            <div class="aside-ops">
              <button type="button" class="ns-btn ns-ico me-pick" title="选择头像">${nmIcon("image")}</button>
              <button type="button" class="ns-btn ns-ico me-clear" title="移除头像">${nmIcon("trash")}</button>
            </div>
            <input type="file" class="me-file" accept="image/*" hidden>
            <div class="aside-d">建议正方形头像；保存后同步给成员。</div>
          </aside>
          <div class="dlg-form">
            <div class="fm-grid me-grid">
              ${coverRows}
              <label for="me-name-inp">名称</label>
              <input id="me-name-inp" class="me-name sec-input" type="text" autocomplete="off" placeholder="${escapeHtml(opts.namePh || "名称")}">
              <label for="me-bio-inp">${escapeHtml(opts.bioLabel || "简介")}</label>
              <textarea id="me-bio-inp" class="me-bio sec-input" rows="3" placeholder="${escapeHtml(opts.bioPh || "一句话简介")}"></textarea>
            </div>
          </div>
        </div>
        <div class="dlg-foot">
          <div class="sec-msg"></div>
          <button type="button" class="ns-btn me-cancel">取消</button>
          <button type="button" class="ns-btn ns-primary me-save">保存</button>
        </div>
      </div>`;
    document.body.appendChild(m);
    let previewData = isImg(cur) ? cur : (isRef(cur) ? await resolveAvatar(cur, opts.homeNode) : "");
    let coverPreview = isImg(curCover) ? curCover : (isRef(curCover) ? await resolveAvatar(curCover, opts.homeNode) : "");
    const paint = () => {
      m.querySelector(".me-prev").innerHTML = avatarHtml(pending === "" ? "" : (pending || previewData), (opts.name || "?").slice(0, 1), 72);
      const cp = m.querySelector(".me-cover-prev");
      if (!cp) return;
      const src = pendingCover === "" ? "" : (pendingCover || coverPreview);
      if (isImg(src)) {
        cp.classList.add("me-cover-tiled");
        cp.style.backgroundImage = "url(" + JSON.stringify(String(src)) + ")";
        cp.style.backgroundRepeat = "repeat";
        cp.style.backgroundPosition = "center center";
        cp.style.backgroundSize = "auto";
        cp.innerHTML = "";
      } else {
        cp.classList.remove("me-cover-tiled");
        cp.style.backgroundImage = "";
        cp.style.backgroundRepeat = "";
        cp.style.backgroundPosition = "";
        cp.style.backgroundSize = "";
        cp.innerHTML = `<span>未设置封面</span>`;
      }
    };
    m.querySelector(".me-name").value = opts.name || "";
    m.querySelector(".me-bio").value = opts.bio || "";
    paint();
    const close = () => m.remove();
    m.querySelector(".me-x").onclick = close;
    m.querySelector(".me-cancel").onclick = close;
    m.addEventListener("click", (e) => { if (e.target === m) close(); });
    m.querySelector(".me-pick").onclick = () => m.querySelector(".me-file").click();
    m.querySelector(".me-clear").onclick = () => { pending = ""; paint(); };
    m.querySelector(".me-file").onchange = async (e) => {
      const f = e.target.files && e.target.files[0]; e.target.value = "";
      if (!f) return;
      try {
        let data = await fileToAvatar(f, 256, 0.85);
        if (!data) return; // 用户取消裁剪
        if (data.length > AV_MAX) {
          data = await reencodeAvatar(data, 192, 0.72);
        }
        if (data.length > AV_MAX) { if (window.toast) toast("图片太大，请换更简单的图"); return; }
        pending = data; paint();
      } catch (err) { if (window.toast) toast("处理图片失败：" + (err && err.message ? err.message : err)); }
    };
    if (withCover) {
      m.querySelector(".me-cover-pick").onclick = () => m.querySelector(".me-cover-file").click();
      m.querySelector(".me-cover-clear").onclick = () => { pendingCover = ""; paint(); };
      m.querySelector(".me-cover-file").onchange = async (e) => {
        const f = e.target.files && e.target.files[0]; e.target.value = "";
        if (!f) return;
        try {
          let data = await fileToCover(f, 960, 320, 0.82);
          if (!data) return;
          if (data.length > 280 * 1024) data = await reencodeRect(data, 800, 268, 0.72);
          if (data.length > 280 * 1024) { if (window.toast) toast("封面太大，请换更简单的图"); return; }
          pendingCover = data; paint();
        } catch (err) { if (window.toast) toast("处理封面失败：" + (err && err.message ? err.message : err)); }
      };
    }
    m.querySelector(".me-save").onclick = async () => {
      let avatar = (isRef(cur) || isImg(cur)) ? cur : "";
      if (pending === "") avatar = "";
      else if (pending) { const ref = await putBlob(pending); if (ref) { avatar = ref; avCache.set(ref, pending); } else avatar = pending; }
      let cover = (isRef(curCover) || isImg(curCover)) ? curCover : "";
      if (withCover) {
        if (pendingCover === "") cover = "";
        else if (pendingCover) { const ref = await putBlob(pendingCover); if (ref) { cover = ref; avCache.set(ref, pendingCover); } else cover = pendingCover; }
      }
      const name = m.querySelector(".me-name").value.trim();
      const bio = m.querySelector(".me-bio").value.trim();
      close();
      try { if (opts.onSave) await opts.onSave({ name, bio, avatar, cover }); }
      catch (e) { if (window.toast) toast("保存失败：" + (e && e.message ? e.message : e)); }
    };
    setTimeout(() => { const inp = m.querySelector(".me-name"); if (inp) inp.focus(); }, 30);
  }

  function ensureModal() {
    let m = document.getElementById("profile-modal");
    if (m) return m;
    m = document.createElement("div");
    m.className = "sec-overlay"; m.id = "profile-modal";
    m.innerHTML = `
      <div class="dlg">
        <div class="dlg-head"><span class="ico">${nmIcon("contacts")}</span><span class="dlg-title">编辑资料</span><span class="dlg-sub">保存后同步给联系人</span><button class="sec-x" id="pf-close" title="关闭">${nmIcon("close")}</button></div>
        <div class="dlg-main">
          <aside class="dlg-aside">
            <span id="pf-av-prev" class="pf-av"></span>
            <div class="aside-ops">
              <button class="ns-btn" id="pf-av-pick">更换头像</button>
              <button class="ns-btn" id="pf-av-clear">移除</button>
            </div>
            <input type="file" id="pf-av-file" accept="image/*" style="display:none">
            <div class="aside-d">头像存放在你的 Home Node，资料里只带内容哈希，其他节点按需拉取并缓存。</div>
          </aside>
          <div class="dlg-form">
            <div class="fm-grid">
              <label for="pf-name">昵称</label><input id="pf-name" class="sec-input" placeholder="显示名">
              <label for="pf-status">状态</label>
              <div class="fm-pair">
                <select id="pf-status" class="sec-input">${STATUS_OPTS.map(([v, l]) => `<option value="${v}">${l}</option>`).join("")}</select>
                <input id="pf-stext" class="sec-input" placeholder="状态文本，如：在开会">
              </div>
              <label for="pf-bio">简介</label><input id="pf-bio" class="sec-input" placeholder="一句话介绍自己">
              <label for="pf-links">链接</label><input id="pf-links" class="sec-input" placeholder="主页 / 社交，逗号分隔">
              <label for="pf-locale">语言</label><input id="pf-locale" class="sec-input" placeholder="如 zh-CN">
              <label>扩展</label>
              <div class="fm-attrs-row">
                <button type="button" class="ns-btn" id="pf-attrs">${nmIcon("list")} 管理扩展属性</button>
                <span class="fm-attrs-tip" id="pf-attrs-tip"></span>
              </div>
            </div>
          </div>
        </div>
        <div class="dlg-foot"><div class="sec-msg"></div><button class="ns-btn" id="pf-cancel">取消</button><button class="ns-btn ns-primary" id="pf-save">保存并发布</button></div>
      </div>`;
    document.body.appendChild(m);
    const close = () => m.classList.remove("on");
    m.querySelector("#pf-close").addEventListener("click", close);
    m.querySelector("#pf-cancel").addEventListener("click", close);
    m.addEventListener("click", (e) => { if (e.target === m) close(); });
    m.querySelector("#pf-save").addEventListener("click", onSave);
    m.querySelector("#pf-attrs").addEventListener("click", () => openExtAttrsEditor());
    m.querySelector("#pf-av-pick").addEventListener("click", () => m.querySelector("#pf-av-file").click());
    m.querySelector("#pf-av-clear").addEventListener("click", () => { pendingAvatar = ""; renderPreview(""); });
    m.querySelector("#pf-av-file").addEventListener("change", async (e) => {
      const f = e.target.files && e.target.files[0]; e.target.value = "";
      if (!f) return;
      try {
        let data = await fileToAvatar(f, 256, 0.85);
        if (!data) return; // 用户取消裁剪
        if (data.length > AV_MAX) data = await reencodeAvatar(data, 192, 0.72);
        if (data.length > AV_MAX) { if (window.toast) toast("图片太大，请换更简单的图"); return; }
        pendingAvatar = data; renderPreview(data);
      } catch (err) { if (window.toast) toast("处理图片失败：" + (err && err.message ? err.message : err)); }
    });
    return m;
  }

  function renderPreview(av) {
    const box = document.getElementById("pf-av-prev");
    if (!box) return;
    const name = (document.getElementById("pf-name") || {}).value || "?";
    const pk = window.Identity && Identity.current && Identity.current();
    const bg = (typeof avatarColor === "function" && pk) ? avatarColor(pk) : "var(--accent)";
    box.innerHTML = avatarHtml(av, name.slice(0, 1), 96, bg);
  }

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
      extAttrs: normalizeExtAttrs(prev.extAttrs), // 保留已维护的扩展属性
    });
    if (displayName && window.Identity) Identity.setName(pk, displayName);
    if (typeof updateUserChip === "function") updateUserChip();
    document.getElementById("profile-modal").classList.remove("on");
    const ok = await publish();
    applyPresence(); // P2：把所选状态应用为 live presence
    if (window.toast) toast(ok ? "资料已保存并发布" : "资料已本地保存（连接后自动发布）");
  }

  function refreshAttrsTip() {
    const tip = document.getElementById("pf-attrs-tip");
    if (!tip) return;
    const pk = window.Identity && Identity.current();
    const n = normalizeExtAttrs(get(pk).extAttrs).length;
    tip.textContent = n ? `已配置 ${n} 项` : "未配置";
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
    refreshAttrsTip();
    m.classList.add("on");
  }

  // 已裁剪的 data:URI → 再缩到 size 并降质（体积超限时用，不再弹裁剪框）。
  function reencodeAvatar(dataUri, size, quality) {
    size = size || 192; quality = quality || 0.72;
    return new Promise((resolve, reject) => {
      const img = new Image();
      img.onload = () => {
        const c = document.createElement("canvas"); c.width = size; c.height = size;
        c.getContext("2d").drawImage(img, 0, 0, size, size);
        resolve(c.toDataURL("image/jpeg", quality));
      };
      img.onerror = () => reject(new Error("图片重编码失败"));
      img.src = dataUri;
    });
  }

  window.Profile = { load, get, save, publish, open, statusLabel, avatarHtml, faceHtml, displayAvatar, isImg, isRef, resolveAvatar, resolveList, ownAvatar, fileToAvatar, putBlob, presenceColor, presenceLabel, presenceDot, applyPresence, metaEditor, openAvatarCrop, openImageCrop, extAttrRows, normalizeExtAttrs, invalidateEntityAvatar, rememberEntityAvatar };
  window.openProfileModal = open;

  // ── P2 在线状态 ──
  function presenceColor(p) {
    return p === "online" ? "#22c55e" : p === "away" ? "#f59e0b" : (p === "busy" || p === "dnd") ? "#ef4444" : "#94a3b8";
  }
  function presenceLabel(p) {
    const key = { online: "presence.online", away: "presence.away", busy: "presence.busy", dnd: "presence.dnd" }[p] || "presence.offline";
    if (window.t) return t(key);
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

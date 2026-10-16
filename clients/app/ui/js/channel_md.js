// ── 频道 Markdown：marked(GFM) + 安全消毒 + 既有增强（HTML/data: 图、居中、放大）──
(function () {
  const ALLOWED = new Set([
    "P", "BR", "STRONG", "EM", "B", "I", "U", "S", "DEL", "CODE", "PRE", "A",
    "UL", "OL", "LI", "H1", "H2", "H3", "H4", "H5", "H6", "BLOCKQUOTE", "HR",
    "TABLE", "THEAD", "TBODY", "TFOOT", "TR", "TH", "TD", "IMG", "FIGURE",
    "FIGCAPTION", "SPAN", "SUP", "SUB", "INPUT",
  ]);

  function isSafeImgUrl(u) {
    u = String(u || "").trim();
    if (!u) return false;
    if (/^https?:\/\//i.test(u)) return !/[\s<>"']/.test(u);
    return /^data:image\/(?:png|jpe?g|gif|webp|svg\+xml|avif|bmp)(?:;charset=[-\w]+)?(?:;base64)?,/i.test(u);
  }
  function isSafeHref(u) {
    u = String(u || "").trim();
    if (!u) return false;
    if (/^https?:\/\//i.test(u) || /^mailto:/i.test(u)) return !/[\s<>"']/.test(u);
    if (u.startsWith("#")) return /^#[\w\-.:%]+$/.test(u);
    return false;
  }
  function readHtmlAttr(tag, name) {
    const re = new RegExp("\\b" + name + "\\s*=\\s*(\"([^\"]*)\"|'([^']*)'|([^\\s>]+))", "i");
    const m = String(tag || "").match(re);
    if (!m) return "";
    return m[2] != null ? m[2] : (m[3] != null ? m[3] : (m[4] || ""));
  }
  function escAttr(v) {
    return String(v || "").replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");
  }
  function escText(v) {
    return typeof escapeHtml === "function" ? escapeHtml(v || "") : escAttr(v);
  }

  function mdImgHtml(alt, url, opts) {
    opts = opts || {};
    if (!isSafeImgUrl(url)) return opts.keepText ? escText(alt || url || "") : "";
    const a = escText(alt || "");
    const src = escAttr(url);
    const cls = "chn-md-fig" + (opts.center ? " chn-md-fig--center" : "");
    let wattr = "";
    let style = "max-width:100%;height:auto";
    const w = String(opts.width || "").replace(/px$/i, "");
    if (/^\d{1,5}$/.test(w)) {
      wattr = ` width="${w}"`;
      style = `max-width:100%;width:min(100%,${w}px);height:auto`;
    }
    return `<figure class="${cls}"><img class="chn-md-img" data-act="zoom" src="${src}" alt="${a}"${wattr} style="${style}" loading="lazy" decoding="async">${a ? `<figcaption class="chn-md-cap">${a}</figcaption>` : ""}</figure>`;
  }

  function htmlImgToFigure(tag, center) {
    const src = readHtmlAttr(tag, "src");
    if (!isSafeImgUrl(src)) return "";
    return mdImgHtml(readHtmlAttr(tag, "alt"), src, {
      center: !!center,
      width: readHtmlAttr(tag, "width"),
    });
  }

  // 抽出安全 HTML 图片，避免 marked/escape 弄坏 data: URI
  function parkSafeImages(src) {
    const slots = [];
    const park = (html) => {
      const i = slots.length;
      slots.push(html);
      return "@@CHNMD" + i + "@@";
    };
    let s = String(src || "");
    s = s.replace(/<p\b([^>]*)>\s*(<img\b[^>]*\/?>)\s*<\/p>/gi, (all, pAttrs, img) => {
      const center = /align\s*=\s*(["']?)center\1/i.test(pAttrs) || /text-align\s*:\s*center/i.test(pAttrs);
      const fig = htmlImgToFigure(img, center);
      return fig ? park(fig) : all;
    });
    s = s.replace(/<div\b([^>]*)>\s*(<img\b[^>]*\/?>)\s*<\/div>/gi, (all, dAttrs, img) => {
      const center = /text-align\s*:\s*center/i.test(dAttrs) || /align\s*=\s*(["']?)center\1/i.test(dAttrs);
      const fig = htmlImgToFigure(img, center);
      return fig ? park(fig) : all;
    });
    s = s.replace(/<img\b[^>]*\/?>/gi, (img) => {
      const fig = htmlImgToFigure(img, false);
      return fig ? park(fig) : img;
    });
    return { text: s, slots };
  }

  function restoreSlots(html, slots) {
    return String(html || "").replace(/@@CHNMD(\d+)@@/g, (_, i) => slots[Number(i)] || "");
  }

  function enhanceImagesInDom(root) {
    root.querySelectorAll("img").forEach((img) => {
      const src = img.getAttribute("src") || "";
      if (!isSafeImgUrl(src)) { img.remove(); return; }
      img.classList.add("chn-md-img");
      img.setAttribute("data-act", "zoom");
      img.setAttribute("loading", "lazy");
      img.setAttribute("decoding", "async");
      if (!img.getAttribute("style")) img.setAttribute("style", "max-width:100%;height:auto");
      else if (!/max-width/i.test(img.getAttribute("style"))) {
        img.setAttribute("style", img.getAttribute("style") + ";max-width:100%;height:auto");
      }
      // 裸 img 包一层 figure，便于样式与居中
      if (!img.closest("figure")) {
        const fig = root.ownerDocument.createElement("figure");
        fig.className = "chn-md-fig";
        const parent = img.parentNode;
        const centerParent = parent && (
          (parent.getAttribute && /center/i.test(parent.getAttribute("align") || "")) ||
          /text-align\s*:\s*center/i.test((parent.getAttribute && parent.getAttribute("style")) || "")
        );
        if (centerParent) fig.classList.add("chn-md-fig--center");
        parent.insertBefore(fig, img);
        fig.appendChild(img);
        const alt = img.getAttribute("alt") || "";
        if (alt && !fig.querySelector("figcaption")) {
          const cap = root.ownerDocument.createElement("figcaption");
          cap.className = "chn-md-cap";
          cap.textContent = alt;
          fig.appendChild(cap);
        }
      }
    });
  }

  function sanitize(html) {
    const doc = new DOMParser().parseFromString("<div id='chn-md-root'>" + html + "</div>", "text/html");
    const root = doc.getElementById("chn-md-root");
    if (!root) return "";

    const walk = (node) => {
      const kids = [...node.childNodes];
      for (const child of kids) {
        if (child.nodeType === 8) { child.remove(); continue; } // comment
        if (child.nodeType === 3) continue; // text
        if (child.nodeType !== 1) { child.remove(); continue; }
        const tag = child.tagName;
        if (!ALLOWED.has(tag)) {
          // 保留文本子节点
          while (child.firstChild) node.insertBefore(child.firstChild, child);
          child.remove();
          continue;
        }
        // 清掉危险属性
        [...child.attributes].forEach((attr) => {
          const n = attr.name.toLowerCase();
          const v = attr.value || "";
          if (n.startsWith("on") || n === "srcdoc" || n === "xlink:href") { child.removeAttribute(attr.name); return; }
          if (n === "href") {
            if (!isSafeHref(v)) child.removeAttribute(attr.name);
            else {
              child.setAttribute("href", v);
              if (/^https?:/i.test(v)) {
                child.setAttribute("target", "_blank");
                child.setAttribute("rel", "noopener noreferrer");
              }
            }
            return;
          }
          if (n === "src") {
            if (!isSafeImgUrl(v)) child.removeAttribute(attr.name);
            return;
          }
          if (tag === "INPUT") {
            if (n === "type" && v.toLowerCase() === "checkbox") return;
            if (n === "checked" || n === "disabled") return;
            child.removeAttribute(attr.name);
            return;
          }
          // 白名单属性
          if (["class", "id", "alt", "title", "width", "height", "align", "style", "start", "colspan", "rowspan", "data-act", "loading", "decoding"].includes(n)) {
            if (n === "style") {
              // 仅允许排版相关声明
              const safe = v.split(";").filter((part) =>
                /^\s*(max-width|width|height|text-align|margin|margin-left|margin-right|display|object-fit)\s*:/i.test(part)
              ).join(";");
              if (safe) child.setAttribute("style", safe);
              else child.removeAttribute("style");
            }
            return;
          }
          child.removeAttribute(attr.name);
        });
        if (tag === "A") child.classList.add("chn-md-a");
        if (tag === "PRE") child.classList.add("chn-md-pre");
        if (tag === "CODE" && child.parentElement && child.parentElement.tagName !== "PRE") child.classList.add("chn-md-code");
        if (/^H[1-6]$/.test(tag)) child.classList.add("chn-md-h");
        if (tag === "P") child.classList.add("chn-md-p");
        if (tag === "UL" || tag === "OL") child.classList.add("chn-md-ul");
        if (tag === "BLOCKQUOTE") child.classList.add("chn-md-quote");
        if (tag === "TABLE") child.classList.add("chn-md-table");
        if (tag === "HR") child.classList.add("chn-md-hr");
        if (tag === "INPUT") {
          child.setAttribute("type", "checkbox");
          child.setAttribute("disabled", "");
        }
        walk(child);
      }
    };
    walk(root);
    enhanceImagesInDom(root);
    return root.innerHTML;
  }

  function configureMarked() {
    const m = window.marked;
    if (!m || !m.parse) return null;
    const renderer = new m.Renderer();
    const origImage = renderer.image.bind(renderer);
    renderer.image = function ({ href, title, text }) {
      if (isSafeImgUrl(href)) return mdImgHtml(text || title || "", href, {});
      return origImage.apply(this, arguments);
    };
    const origLink = renderer.link.bind(renderer);
    renderer.link = function (token) {
      const href = token.href || "";
      if (!isSafeHref(href)) return escText(token.text || href);
      return origLink.call(this, token);
    };
    m.setOptions({
      gfm: true,
      breaks: true,
      pedantic: false,
      renderer,
    });
    return m;
  }

  let configured = false;
  function ensureMarked() {
    if (configured) return window.marked;
    if (!window.marked || !window.marked.parse) return null;
    configureMarked();
    configured = true;
    return window.marked;
  }

  function renderMarkdown(src) {
    const raw = String(src || "");
    if (!raw.trim()) return "";
    const { text, slots } = parkSafeImages(raw);
    const m = ensureMarked();
    let html;
    if (m) {
      try { html = m.parse(text, { async: false }); }
      catch (_) { html = escText(text); }
    } else {
      // marked 未加载时的极简回退
      html = "<p class=\"chn-md-p\">" + escText(text).replace(/\n/g, "<br>") + "</p>";
    }
    html = restoreSlots(html, slots);
    // 占位符若被包进 code/转义，再扫一次纯文本还原
    html = restoreSlots(html, slots);
    return sanitize(html);
  }

  window.ChannelMd = { render: renderMarkdown, isSafeImgUrl, mdImgHtml };
})();

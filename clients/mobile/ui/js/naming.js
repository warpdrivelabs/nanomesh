// ── 去中心命名（N1）：认领 local@domain（home node 签发 + 联邦 gossip 复制）、解析、展示。
//    认领后你的实体在他人目录里带上 handle=local@domain；解析可按名找人。详见 docs/MESH_DNS_NAMING_DESIGN.md。
(function () {
  function claim() {
    if (!window.NM || !NM.hasTauri || !NM.hasTauri()) { if (window.toast) toast("请先连接节点"); return; }
    const box = document.createElement("div");
    box.className = "sec-overlay on";
    box.innerHTML = `<div class="sec-box"><div class="sec-head">认领名字（local@域名）<button class="sec-x">✕</button></div>
      <div class="sec-body"><div class="sec-sec">
        <div class="sec-hint">向你的 home node 认领一个本地名，形如 <code>你@域名</code>（域名由该节点声明）。
          认领后经联邦扩散，别人可用它找到你。<b>域内唯一</b>由节点保证。</div>
        <input class="sec-input nm-inp" placeholder="想要的名字（a-z 0-9 . - _，≤63）" autocomplete="off">
        <div class="sec-actions"><button class="ns-btn nm-c">取消</button><button class="ns-btn ns-primary nm-ok">认领</button></div>
        <div class="nm-msg" style="margin-top:8px;font-size:12px;color:var(--muted)"></div>
      </div></div></div>`;
    document.body.appendChild(box);
    const inp = box.querySelector(".nm-inp"); inp.focus();
    const msg = box.querySelector(".nm-msg");
    const close = () => box.remove();
    box.querySelector(".sec-x").onclick = close;
    box.querySelector(".nm-c").onclick = close;
    box.addEventListener("click", (e) => { if (e.target === box) close(); });
    const ok = async () => {
      const lp = inp.value.trim();
      if (!lp) return;
      msg.style.color = "var(--muted)"; msg.textContent = "认领中…";
      try {
        const full = await NM.inv("name_claim", { localPart: lp }); // 返回 local@domain
        try { const pk = await NM.inv("my_id"); localStorage.setItem("nmspace:handle:" + pk, full); } catch (_) {}
        if (window.toast) toast("已认领：" + full);
        if (typeof updateUserChip === "function") updateUserChip();
        if (window.imRefresh) imRefresh();
        close();
      } catch (e) {
        msg.style.color = "var(--red)"; msg.textContent = "认领失败：" + (e && e.message ? e.message : e);
      }
    };
    box.querySelector(".nm-ok").onclick = ok;
    inp.addEventListener("keydown", (e) => { if (e.key === "Enter") ok(); if (e.key === "Escape") close(); });
  }

  // 解析 name → 目标公钥 hex（无则 ""）。
  async function resolve(name) {
    if (!window.NM || !NM.hasTauri || !NM.hasTauri()) return "";
    try { return (await NM.inv("name_resolve", { name: (name || "").trim() })) || ""; } catch (_) { return ""; }
  }
  // 本人已认领的 handle（本地记忆，用于展示）。
  function myHandle(pk) { try { return localStorage.getItem("nmspace:handle:" + pk) || ""; } catch (_) { return ""; } }
  // 是否形如 local@domain（供“添加实体”时识别按名解析）。
  function looksLikeName(s) { return /^[a-z0-9._-]{1,63}@[a-z0-9.-]{1,253}$/i.test((s || "").trim()); }

  (function init() {
    const b = document.getElementById("name-claim-btn");
    if (b) b.addEventListener("click", claim);
  })();

  window.Naming = { claim, resolve, myHandle, looksLikeName };
  window.openNameClaim = claim;
})();

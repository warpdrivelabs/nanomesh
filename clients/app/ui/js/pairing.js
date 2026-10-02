// 旧设备一侧的迁移确认：收到推送的请求或粘贴迁移串后弹框，用户输入新设备上的核对码才放行。
(function () {
  const queue = [];
  let cur = null;

  function el(id) { return document.getElementById(id); }
  function msg(t, ok) {
    const m = el("pair-msg");
    m.textContent = t || "";
    m.className = "sec-msg" + (ok ? " ok" : "");
  }

  function show(req) {
    cur = req;
    el("pair-desc").textContent =
      "设备「" + (req.device || "未知设备") + "」请求把账号 " + (req.name || "（未注明）") + " 的私钥迁移过去。";
    el("pair-sas").value = "";
    el("pair-allow").disabled = false;
    el("pair-deny").disabled = false;
    msg("");
    el("pair-modal").classList.add("on");
    setTimeout(() => el("pair-sas").focus(), 50);
  }

  function next() {
    cur = null;
    el("pair-modal").classList.remove("on");
    if (queue.length) show(queue.shift());
  }

  function enqueue(req) {
    if (!req || !req.rid) return;
    if (cur && cur.rid === req.rid) return;
    if (queue.some((q) => q.rid === req.rid)) return;
    if (cur) queue.push(req); else show(req);
  }

  async function allow() {
    if (!cur) return;
    const sas = el("pair-sas").value.replace(/\D/g, "");
    if (sas.length !== 6) { msg("请输入 6 位核对码"); return; }
    el("pair-allow").disabled = true;
    try {
      await NM.inv("pair_approve", { rid: cur.rid, sas });
      msg("已发送，新设备即将完成登录。", true);
      el("pair-deny").disabled = true;
      setTimeout(next, 1200);
    } catch (e) {
      msg(String((e && e.message) || e));
      el("pair-allow").disabled = false;
      if (String(e).indexOf("核对码") < 0) setTimeout(next, 1800);
    }
  }

  async function deny() {
    if (!cur) return;
    const rid = cur.rid;
    next();
    try { await NM.inv("pair_reject", { rid }); } catch (_) {}
  }

  async function fromTicket() {
    const ta = el("sec-pair-in");
    const ticket = (ta.value || "").trim();
    const out = (t, ok) => (typeof secMsg === "function" ? secMsg(t, ok) : null);
    if (!ticket) { out("请先粘贴迁移串"); return; }
    try {
      const req = await NM.inv("pair_accept_ticket", { ticket });
      ta.value = "";
      out("");
      if (typeof closeSecModal === "function") closeSecModal();
      enqueue(req);
    } catch (e) {
      out(String((e && e.message) || e));
    }
  }

  function init() {
    if (!el("pair-modal")) return;
    el("pair-allow").addEventListener("click", allow);
    el("pair-deny").addEventListener("click", deny);
    el("pair-sas").addEventListener("keydown", (e) => { if (e.key === "Enter") allow(); });
    const go = el("sec-pair-go");
    if (go) go.addEventListener("click", fromTicket);
    NM.onEvent("pair://event", (ev) => { if (ev && ev.type === "request") enqueue(ev); });
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();

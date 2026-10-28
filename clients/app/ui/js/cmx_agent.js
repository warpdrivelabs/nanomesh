// 智能体工作台(cmx-agent 组件化嵌入)前端壳:点活动栏机器人图标 → 整个工作区交给 <truemate-app>
// (组件替代旧 iframe:见 docs/嵌入组件对接指南.md。组件脚本 /embed/truemate-app.js 由
//  cmx_host 的 axum 面同源静态服务;装配后组件检测到 window.__TAURI__ 即走 invoke 直连内核
//  ——invoke("agent")/invoke("send_stream")+session_event 事件,HTTP 面只剩附件与 SSE 兜底)。
// 设置入口在活动栏底部头像菜单(usermenu.js「智能体设置」);无左侧栏目(20261026 用户定稿)。
// 后端见 src-tauri/src/cmx_host.rs;配置 cmx-workbench.json,改动经「重启工作台」生效。
(function () {
  const WB_KEY = "cmx:workbench";
  let wbUrl = ""; // 工作台 HTTP 基址(含 ?t= token;取 origin 作组件 server/脚本源)
  let loading = null; // 组件脚本加载单飞

  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }
  function stageEl() { return document.getElementById("cmx-stage"); }
  function baseOf(url) {
    try { return new URL(url).origin; } catch (_) { return ""; }
  }

  // 活动栏高亮:进工作台点亮机器人图标;离开还原为当前侧栏面板对应图标。
  function actMark(cmxOn) {
    let name = "cmx";
    if (!cmxOn) {
      const p = document.querySelector("aside .panel.on");
      name = p && p.id.indexOf("panel-") === 0 ? p.id.slice(6) : "im";
    }
    document.querySelectorAll(".activitybar .act").forEach((a) => a.classList.toggle("active", a.dataset.panel === name));
  }

  // 组件脚本按需加载(同源 /embed/truemate-app.js;静态资产只回环校验,无需 token)。幂等单飞。
  function ensureComponent() {
    if (window.customElements && customElements.get("truemate-app")) return Promise.resolve();
    if (loading) return loading;
    loading = new Promise((resolve, reject) => {
      const s = document.createElement("script");
      s.src = baseOf(wbUrl) + "/embed/truemate-app.js";
      s.onload = () => resolve();
      s.onerror = () => { loading = null; reject(new Error("组件脚本加载失败")); };
      document.head.appendChild(s);
    });
    return loading;
  }

  // 建/取组件实例:常驻 #cmx-stage(重挂会整段重装配,故显隐而非重建)。
  function ensureInstance() {
    const st = stageEl();
    if (!st) return null;
    let el = st.querySelector("truemate-app");
    if (!el) {
      el = document.createElement("truemate-app");
      // Tauri 桥环境:server 仍指向本机 axum 面——附件上传/下载与 SSE 兜底按基址请求;
      // 命令总线/流式回合组件自动走 invoke(window.__TAURI__ 在,bridge.js 优先 invoke 分支)。
      el.setAttribute("server", baseOf(wbUrl));
      // token → 组件 HTTP 请求的 Authorization: Bearer(附件上传等过 cmx_host token 闸)。
      // shim_user 已让 invoke("agent") 拦掉 auth_token/current_user 类命令,不影响免登录态。
      const tok = (wbUrl.split("t=")[1] || "").split("&")[0];
      if (tok) el.setAttribute("token", tok);
      el.setAttribute("theme", document.documentElement.getAttribute("data-theme") === "light" ? "light" : "dark");
      // 应用级分区裁剪:账户(身份由 nanomesh 承担)/外观(随宿主主题)/关于(版本属宿主)/IM(nanomesh 自有 IM)。
      el.setAttribute("setting-hide", "account,appearance,about,im");
      el.style.cssText = "display:block;width:100%;height:100%";
      st.appendChild(el);
    }
    return el;
  }

  // 宿主主题切换联动组件(main.js applyTheme 改 data-theme;轮询开销可忽略,免改 main.js)。
  new MutationObserver(() => {
    const el = stageEl() && stageEl().querySelector("truemate-app");
    if (el) el.setAttribute("theme", document.documentElement.getAttribute("data-theme") === "light" ? "light" : "dark");
  }).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

  function showStage() {
    const st = stageEl();
    if (!st) return;
    if (wbUrl) {
      ensureComponent().then(() => ensureInstance()).catch((e) => {
        if (window.toast) toast(String(e && e.message ? e.message : e));
      });
    }
    st.hidden = false;
    document.body.classList.add("cmx-full");
    actMark(true);
  }
  function hideStage() {
    const st = stageEl();
    if (st) st.hidden = true;
    document.body.classList.remove("cmx-full");
    actMark(false);
  }
  // 工作台重启(端口/token 变化)后丢旧实例,按新地址重建;正显示时就地刷新。
  function resetStage(url) {
    if (url) wbUrl = url;
    const st = stageEl();
    if (!st) return;
    const el = st.querySelector("truemate-app");
    if (el) {
      try { if (window.TrueMate && TrueMate.destroy) TrueMate.destroy(); } catch (_) {}
      el.remove();
    }
    if (!st.hidden) showStage();
  }

  // 舞台显隐随活跃 tab 走(tabs.js 每次切换/关闭派发 nm-tab-changed)。
  document.addEventListener("nm-tab-changed", (e) => {
    if (e.detail && e.detail.key === WB_KEY) showStage();
    else hideStage();
  });

  // ── 打开工作台(活动栏图标点击;懒启动:cmx_start 幂等) ──
  async function openWorkbench() {
    let st;
    try { st = await NM.inv("cmx_start", {}); }
    catch (e) {
      if (window.toast) toast("启动失败:" + (e && e.message ? e.message : e));
      return;
    }
    if (st.url && st.url !== wbUrl) resetStage(st.url);
    if (!window.Tabs) return;
    Tabs.open({ key: WB_KEY, kind: "cmx", title: "智能体工作台", ico: "robot", render: showStage });
  }
  window.openCmxWorkbench = openWorkbench;

  // ── 设置表单(底部头像菜单入口;读写 cmx-workbench.json,保存后可一键重启工作台) ──
  async function openSettings() {
    let cfg;
    try { cfg = await NM.inv("cmx_cfg_get", {}); }
    catch (e) { if (window.toast) toast("读取配置失败:" + e); return; }
    if (!window.Tabs) return;
    Tabs.open({
      key: "cmx:settings",
      kind: "cmx",
      title: "智能体设置",
      ico: "settings",
      render: () => renderSettings(cfg),
    });
  }
  window.openCmxSettings = openSettings;

  function renderSettings(cfg) {
    const conv = document.getElementById("conv");
    if (!conv) return;
    const m = cfg.model || {};
    conv.innerHTML = `
      <div class="conv-head"><b>智能体设置</b></div>
      <div class="detail-scroll"><div class="detail" style="max-width:680px">
        <div class="detail-rows">
          <div class="detail-row"><span class="k">启用</span><span class="v"><label class="llm-check"><input type="checkbox" id="cx-enabled" ${cfg.enabled ? "checked" : ""}/> 显示工作台标签并允许启动</label></span></div>
          <div class="detail-row"><span class="k">自动启动</span><span class="v"><label class="llm-check"><input type="checkbox" id="cx-autostart" ${cfg.autostart ? "checked" : ""}/> 应用启动即拉起(默认点标签懒启动)</label></span></div>
          <div class="detail-row"><span class="k">登录</span><span class="v">
            <select class="ns-inp" id="cx-login"><option value="off" ${cfg.login !== "portal" ? "selected" : ""}>免登录(嵌入态,默认)</option><option value="portal" ${cfg.login === "portal" ? "selected" : ""}>门户登录(恢复原版)</option></select>
          </span></div>
          <div class="detail-row"><span class="k">门户地址</span><span class="v"><input class="ns-inp" id="cx-portal" style="width:100%" value="${esc(cfg.portalBaseUrl || "")}"/></span></div>
          <div class="detail-row"><span class="k">登录垫片</span><span class="v">
            <select class="ns-inp" id="cx-shim"><option value="auto" ${cfg.shim === "auto" || !cfg.shim ? "selected" : ""}>自动(随登录模式)</option><option value="on" ${cfg.shim === "on" ? "selected" : ""}>强制注入</option><option value="off" ${cfg.shim === "off" ? "selected" : ""}>不注入</option></select>
          </span></div>
          <div class="detail-row"><span class="k">UI 目录</span><span class="v"><input class="ns-inp" id="cx-uidir" style="width:100%" placeholder="空=自动(dev 源码路径)" value="${esc(cfg.uiDir || "")}"/></span></div>
          <div class="detail-row"><span class="k">数据目录</span><span class="v"><input class="ns-inp" id="cx-datadir" style="width:100%" placeholder="空=随 NanoMesh 隔离;~/.cmx-agent=共享原 TrueMate 数据(勿与独立版同时运行)" value="${esc(cfg.dataDir || "")}"/></span></div>
          <div class="detail-row"><span class="k">工作目录</span><span class="v"><input class="ns-inp" id="cx-workdir" style="width:100%" placeholder="空=$HOME" value="${esc(cfg.workdir || "")}"/></span></div>
          <div class="detail-row"><span class="k">端口</span><span class="v"><input class="ns-inp" id="cx-port" type="number" min="0" max="65535" value="${Number(cfg.port) || 0}"/> <small style="color:var(--muted)">0=随机</small></span></div>
          <div class="detail-row"><span class="k">访问令牌</span><span class="v"><input class="ns-inp" id="cx-token" style="width:100%" value="${esc(cfg.token || "auto")}"/> <small style="color:var(--muted)">auto=每次随机;off=关闭(仅本机回环,慎用)</small></span></div>
          <div class="detail-row"><span class="k">模型来源</span><span class="v">
            <select class="ns-inp" id="cx-mmode"><option value="cmx" ${m.mode !== "openai" && m.mode !== "nanomesh" ? "selected" : ""}>cmx 原配置链(env/providers.json)</option><option value="openai" ${m.mode === "openai" ? "selected" : ""}>显式 OpenAI 兼容端点</option><option value="nanomesh" ${m.mode === "nanomesh" ? "selected" : ""}>NanoMesh 模型栈(预留)</option></select>
          </span></div>
          <div class="detail-row"><span class="k">模型 BaseURL</span><span class="v"><input class="ns-inp" id="cx-mbase" style="width:100%" value="${esc(m.baseUrl || "")}"/></span></div>
          <div class="detail-row"><span class="k">模型 Key</span><span class="v"><input class="ns-inp" id="cx-mkey" style="width:100%" placeholder='支持 "${"${ENV:VAR}"}" 引用环境变量' value="${esc(m.apiKey || "")}"/></span></div>
          <div class="detail-row"><span class="k">模型名</span><span class="v"><input class="ns-inp" id="cx-mname" style="width:100%" value="${esc(m.name || "")}"/></span></div>
        </div>
        <div class="detail-actions" style="padding:14px 0">
          <button class="ns-btn ns-primary" id="cx-save">保存</button>
          <button class="ns-btn" id="cx-save-restart">保存并重启工作台</button>
        </div>
        <p style="font-size:12px;color:var(--muted)">恢复原版组合:登录=门户登录 + 数据目录=~/.cmx-agent + 模型来源=cmx 原配置链。配置改动需重启工作台生效。组件化嵌入后「隐藏边栏」由组件自身处理(嵌入态默认隐藏其活动栏)。</p>
      </div></div>`;
    const collect = () => ({
      enabled: !!document.getElementById("cx-enabled").checked,
      autostart: !!document.getElementById("cx-autostart").checked,
      login: document.getElementById("cx-login").value,
      portalBaseUrl: document.getElementById("cx-portal").value.trim(),
      shim: document.getElementById("cx-shim").value,
      hideRail: cfg.hideRail !== false,
      uiDir: document.getElementById("cx-uidir").value.trim(),
      dataDir: document.getElementById("cx-datadir").value.trim(),
      workdir: document.getElementById("cx-workdir").value.trim(),
      port: Number(document.getElementById("cx-port").value) || 0,
      token: document.getElementById("cx-token").value.trim() || "auto",
      model: {
        mode: document.getElementById("cx-mmode").value,
        baseUrl: document.getElementById("cx-mbase").value.trim(),
        apiKey: document.getElementById("cx-mkey").value.trim(),
        name: document.getElementById("cx-mname").value.trim(),
      },
      im: !!cfg.im,
    });
    const save = async (restart) => {
      try {
        await NM.inv("cmx_cfg_set", { cfg: collect() });
        if (restart) {
          const st = await NM.inv("cmx_restart", {});
          resetStage(st && st.url);
          if (window.toast) toast("已保存并重启工作台");
        } else if (window.toast) toast("已保存(重启工作台后生效)");
      } catch (e) {
        if (window.toast) toast("保存失败:" + (e && e.message ? e.message : e));
      }
    };
    document.getElementById("cx-save").onclick = () => save(false);
    document.getElementById("cx-save-restart").onclick = () => save(true);
  }
})();

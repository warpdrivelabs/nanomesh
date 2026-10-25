// 大模型实体详情：模型代理启停（消费方）；完整管理见「模型代理」面板
(function () {
  function showRunning(info) {
    const st = document.getElementById("ed-proxy-status");
    const hint = document.getElementById("ed-proxy-hint");
    const env = document.getElementById("ed-proxy-env");
    const start = document.getElementById("ed-proxy-start");
    const stop = document.getElementById("ed-proxy-stop");
    const copy = document.getElementById("ed-proxy-copy");
    const manage = document.getElementById("ed-proxy-manage");
    const listen = document.getElementById("ed-proxy-listen");
    const token = document.getElementById("ed-proxy-token");
    if (!st) return;
    if (info && info.running) {
      const extra =
        (info.requests != null ? " · 请求 " + info.requests : "") +
        (info.errors ? " · 错 " + info.errors : "") +
        (info.lastLatencyMs ? " · " + info.lastLatencyMs + "ms" : "");
      st.textContent = "运行中 · " + (info.listen || "") + extra;
      st.style.color = "var(--ok,#1fb182)";
      if (listen) listen.value = info.listen || "";
      if (token) token.value = info.localToken || "nm-local-token";
      if (hint) hint.style.display = "";
      if (env) {
        env.textContent =
          "OPENAI_BASE_URL=" + info.baseUrl + "\nOPENAI_API_KEY=" + (info.localToken || "nm-local-token");
      }
      if (start) start.style.display = "none";
      if (stop) stop.style.display = "";
      if (copy) copy.style.display = "";
      if (manage) manage.style.display = "";
    } else {
      st.textContent = "未启动";
      st.style.color = "";
      if (hint) hint.style.display = "none";
      if (start) start.style.display = "";
      if (stop) stop.style.display = "none";
      if (copy) copy.style.display = "none";
      if (manage) manage.style.display = "none";
    }
  }

  window.bindModelProxyUi = async function (providerId, entity) {
    const start = document.getElementById("ed-proxy-start");
    const stop = document.getElementById("ed-proxy-stop");
    const copy = document.getElementById("ed-proxy-copy");
    const manage = document.getElementById("ed-proxy-manage");
    if (!window.NM) {
      showRunning(null);
      return;
    }
    try {
      const st = await NM.inv("llm_proxy_status", { providerId });
      showRunning(st);
    } catch (_) {
      showRunning(null);
    }

    if (start) {
      start.onclick = async () => {
        const listen = (document.getElementById("ed-proxy-listen") || {}).value || "";
        const localToken = (document.getElementById("ed-proxy-token") || {}).value || "nm-local-token";
        const grantB64 = (document.getElementById("ed-proxy-grant") || {}).value || "";
        try {
          const info = await NM.inv("llm_proxy_start", {
            args: {
              providerId,
              displayName: (entity && entity.name) || "",
              model: (entity && entity.model) || "",
              listen: listen.trim(),
              localToken: localToken.trim() || "nm-local-token",
              grantB64: grantB64.trim(),
              timeoutSecs: 120,
            },
          });
          showRunning(info);
          if (window.toast) toast("代理已启动 " + info.listen);
          if (window.renderLlmProxies) renderLlmProxies();
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
    if (stop) {
      stop.onclick = async () => {
        try {
          await NM.inv("llm_proxy_stop", { providerId });
          showRunning(null);
          if (window.toast) toast("代理已停止");
          if (window.renderLlmProxies) renderLlmProxies();
        } catch (e) {
          if (window.toast) toast(String(e));
        }
      };
    }
    if (copy) {
      copy.onclick = async () => {
        const env = document.getElementById("ed-proxy-env");
        const text = env ? env.textContent : "";
        try {
          await navigator.clipboard.writeText(text);
          if (window.toast) toast("已复制环境变量");
        } catch (_) {
          if (window.toast) toast("复制失败");
        }
      };
    }
    if (manage) {
      manage.onclick = () => {
        if (window.openLlmProxyDetail) {
          openLlmProxyDetail(providerId, {
            displayName: (entity && entity.name) || "",
            model: (entity && entity.model) || "",
            localToken: (document.getElementById("ed-proxy-token") || {}).value || "nm-local-token",
            listen: (document.getElementById("ed-proxy-listen") || {}).value || "",
            grantB64: (document.getElementById("ed-proxy-grant") || {}).value || "",
          });
        } else if (window.showPanel) {
          showPanel("proxies");
        }
      };
    }
  };

  if (window.NM && NM.on) {
    NM.on("core://event", (ev) => {
      if (!ev || ev.type !== "llm_proxy") return;
      const box = document.getElementById("ed-proxy-box");
      if (!box) return;
      if (typeof DETAIL_ID !== "undefined" && DETAIL_ID && ev.providerId === DETAIL_ID) {
        if (ev.event === "started" || ev.event === "updated") {
          NM.inv("llm_proxy_status", { providerId: ev.providerId })
            .then(showRunning)
            .catch(() => {});
        } else if (ev.event === "stopped") {
          showRunning(null);
        }
      }
    });
  }
})();

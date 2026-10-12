// nm-admind 前端：纯 Web Components 标准（自定义元素 + 原生 DOM，无框架、无构建）。

import { AdminApp, AdminChange, AdminLogin, AdminShell } from "./js/admin/components.js";

customElements.define("admin-app", AdminApp);
customElements.define("admin-login", AdminLogin);
customElements.define("admin-change", AdminChange);
customElements.define("admin-shell", AdminShell);

// 国际化与语言切换。

export const I18N = {
  "zh-CN": {
    "login.title": "nmd 管理控制台", "login.lead": "请登录以继续", "login.user": "用户名", "login.pass": "密码", "login.submit": "登录",
    "change.title": "修改初始密码", "change.lead": "首次登录，请设置新密码后进入", "change.save": "保存并进入",
    "shell.brand": "nmd 控制台", "shell.logout": "退出登录", "shell.refreshed": "刷新于 ",
    "tab.overview": "总览", "tab.connections": "连接监控", "tab.users": "用户管理", "tab.peers": "对等节点",
    "tab.names": "域名管理", "tab.storage": "存储管理", "tab.traffic": "流量监控", "tab.system": "系统资源",
    "tab.identity": "服务标识", "tab.password": "修改密码", "lang": "English",
  },
  en: {
    "login.title": "nmd console", "login.lead": "Sign in to continue", "login.user": "Username", "login.pass": "Password", "login.submit": "Sign in",
    "change.title": "Change the initial password", "change.lead": "First sign-in. Set a new password to continue.", "change.save": "Save and continue",
    "shell.brand": "nmd console", "shell.logout": "Sign out", "shell.refreshed": "Updated ",
    "tab.overview": "Overview", "tab.connections": "Connections", "tab.users": "Users", "tab.peers": "Peers",
    "tab.names": "Names", "tab.storage": "Storage", "tab.traffic": "Traffic", "tab.system": "System",
    "tab.identity": "Identity", "tab.password": "Password", "lang": "中文",
  },
};
export function admLocale() {
  const saved = localStorage.getItem("admind-locale") || "";
  if (I18N[saved]) return saved;
  return (navigator.language || "").toLowerCase().startsWith("en") ? "en" : "zh-CN";
}
export let ADM_LOCALE = admLocale();
export function t(key) { return (I18N[ADM_LOCALE] && I18N[ADM_LOCALE][key]) || I18N["zh-CN"][key] || key; }
export function toggleAdmLocale() {
  ADM_LOCALE = ADM_LOCALE === "en" ? "zh-CN" : "en";
  localStorage.setItem("admind-locale", ADM_LOCALE);
  location.reload();
}
document.documentElement.lang = ADM_LOCALE === "en" ? "en" : "zh-CN";

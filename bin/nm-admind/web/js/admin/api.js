// 后台管理 API 客户端。

export const TOKEN_KEY = "admind-token";
export function authHeaders(extra) {
  const headers = { ...(extra || {}) };
  const token = sessionStorage.getItem(TOKEN_KEY);
  if (token) headers.Authorization = "Bearer " + token;
  return headers;
}
export async function api(path, opts = {}) {
  const res = await fetch(path, {
    method: opts.method || "GET",
    headers: authHeaders(opts.body ? { "Content-Type": "application/json" } : {}),
    body: opts.body ? JSON.stringify(opts.body) : undefined,
    credentials: "same-origin",
  });
  if (res.status === 401) {
    sessionStorage.removeItem(TOKEN_KEY);
    window.dispatchEvent(new CustomEvent("admind:logout"));
    throw new Error("会话已失效，请重新登录");
  }
  const text = await res.text();
  let data;
  try { data = text ? JSON.parse(text) : {}; } catch { data = { raw: text }; }
  if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
  return data;
}

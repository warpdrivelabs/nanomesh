import type { CoreTransport } from "./transport";
import { TauriTransport } from "./tauriTransport";
import { WebGatewayTransport } from "./webTransport";

// 运行时探测：Tauri 会注入 __TAURI_INTERNALS__，据此选择接入路径。
export function makeTransport(): CoreTransport {
  const isTauri = typeof (globalThis as unknown as { __TAURI_INTERNALS__?: unknown })
    .__TAURI_INTERNALS__ !== "undefined";
  return isTauri ? new TauriTransport() : new WebGatewayTransport();
}

export * from "./transport";

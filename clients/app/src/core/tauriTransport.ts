import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { CoreEvent, CoreTransport, Entity } from "./transport";

// 原生端：直接调用 Tauri 后端(src-tauri)里进程内运行的 Rust im-client + iroh。
export class TauriTransport implements CoreTransport {
  connect(nodeAddr: string, displayName: string): Promise<string> {
    return invoke<string>("connect", { nodeAddr, displayName });
  }
  sendTo(target: string, text: string): Promise<void> {
    return invoke("send_to", { target, text });
  }
  directoryQuery(kindPrefix: string): Promise<Entity[]> {
    return invoke<Entity[]>("directory_query", { kindPrefix });
  }
  onEvent(handler: (e: CoreEvent) => void): () => void {
    const un = listen<CoreEvent>("core://event", (ev) => handler(ev.payload));
    return () => {
      void un.then((f) => f());
    };
  }
}

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ConnectParams, CoreEvent, CoreTransport, Entity } from "./transport";

// 原生端：直接调用 Tauri 后端(src-tauri)里进程内运行的 Rust im-client + iroh。
export class TauriTransport implements CoreTransport {
  connect(p: ConnectParams): Promise<string> {
    return invoke<string>("connect", {
      mode: p.mode,
      node: p.node,
      displayName: p.displayName,
      relayUrls: p.relayUrls ?? [],
      pkarrUrl: p.pkarrUrl ?? null,
      dnsOrigin: p.dnsOrigin ?? null,
    });
  }
  sendTo(target: string, text: string): Promise<void> {
    return invoke("send_to", { target, text });
  }
  disconnect(): Promise<void> {
    return invoke("disconnect");
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

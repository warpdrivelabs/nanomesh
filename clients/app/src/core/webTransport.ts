import type { ConnectParams, CoreEvent, CoreTransport, Entity } from "./transport";

// Web 端：浏览器不能直跑 QUIC/iroh，经 im-gateway(WebSocket) 接入。
// 网关尚未实现（后续任务）；此适配器保持接口形状，调用时提示。
export class WebGatewayTransport implements CoreTransport {
  private notImplemented(): never {
    throw new Error("Web 网关(im-gateway)尚未实现；请用桌面端(Tauri)运行，或稍后接入网关。");
  }
  connect(_params: ConnectParams): Promise<string> {
    return this.notImplemented();
  }
  disconnect(): Promise<void> {
    return Promise.resolve();
  }
  sendTo(_target: string, _text: string): Promise<void> {
    return this.notImplemented();
  }
  directoryQuery(_kindPrefix: string): Promise<Entity[]> {
    return this.notImplemented();
  }
  onEvent(_handler: (e: CoreEvent) => void): () => void {
    return () => {};
  }
}

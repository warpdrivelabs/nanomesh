// 统一「核心通道」抽象：屏蔽「原生(Tauri IPC)」与「Web(网关)」两条接入路径。
// 界面与状态只依赖本接口 —— 写一次即可跑遍 web / desktop / iOS / android。

export interface Message {
  id: string;
  from: string; // 发送者 EntityId(hex)
  body: string;
  ts: number;
}

export interface Entity {
  id: string; // EntityId(hex)
  kind: string; // person / agent.assistant / compute.inference ...
  name: string;
}

export type CoreEvent = { type: "message"; msg: Message };

export interface CoreTransport {
  /** 连接节点并注册为 person；返回自己的 EntityId(hex)。 */
  connect(nodeAddr: string, displayName: string): Promise<string>;
  /** 向目标实体(hex id)发送一条文本消息。 */
  sendTo(target: string, text: string): Promise<void>;
  /** 按 kind 前缀查询目录（"" = 全部）。 */
  directoryQuery(kindPrefix: string): Promise<Entity[]>;
  /** 监听核心事件流；返回取消订阅函数。 */
  onEvent(handler: (e: CoreEvent) => void): () => void;
}

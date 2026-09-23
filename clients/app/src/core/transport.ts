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

// 连接模式（与后端 imd 对齐）。
export type ConnectMode = "nat" | "selfhost" | "lan";

export interface ConnectParams {
  mode: ConnectMode;
  /** nat/selfhost: 节点公钥(hex, 64位)；lan: 节点地址(JSON, IM_NODE_ADDR)。 */
  node: string;
  displayName: string;
  // 仅 selfhost：
  relayUrls?: string[];
  pkarrUrl?: string;
  dnsOrigin?: string;
}

export interface CoreTransport {
  /** 按模式连接节点并注册为 person；返回自己的 EntityId(hex)。 */
  connect(params: ConnectParams): Promise<string>;
  /** 向目标实体(hex id)发送一条文本消息。 */
  sendTo(target: string, text: string): Promise<void>;
  /** 按 kind 前缀查询目录（"" = 全部）。 */
  directoryQuery(kindPrefix: string): Promise<Entity[]>;
  /** 监听核心事件流；返回取消订阅函数。 */
  onEvent(handler: (e: CoreEvent) => void): () => void;
}

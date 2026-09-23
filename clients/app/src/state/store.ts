import { useEffect, useSyncExternalStore } from "react";
import { makeTransport } from "../core";
import type { ConnectParams, CoreEvent, Entity, Message } from "../core";

// 极简状态层。生产可替换为 zustand / jotai。
class ChatStore {
  private messages: Message[] = [];
  private entities: Entity[] = [];
  private myId = "";
  private connected = false;
  private serverLabel = "";
  private listeners = new Set<() => void>();
  readonly transport = makeTransport();
  private started = false;

  start(): void {
    if (this.started) return;
    this.started = true;
    this.transport.onEvent((e: CoreEvent) => {
      if (e.type === "message") {
        this.messages = [...this.messages, e.msg];
        this.emit();
      }
    });
  }

  async connect(params: ConnectParams, label = ""): Promise<void> {
    this.myId = await this.transport.connect(params);
    this.connected = true;
    this.serverLabel = label || shortLabel(params);
    this.emit();
  }

  async disconnect(): Promise<void> {
    try {
      await this.transport.disconnect();
    } catch {
      /* 忽略断开错误 */
    }
    this.connected = false;
    this.myId = "";
    this.serverLabel = "";
    this.messages = [];
    this.entities = [];
    this.emit();
  }

  async refreshDirectory(kindPrefix: string): Promise<void> {
    this.entities = await this.transport.directoryQuery(kindPrefix);
    this.emit();
  }
  async send(target: string, text: string): Promise<void> {
    // 先追加本地消息使 UI 立即可见，再 await 网络发送——
    // 避免对端（如 echo 机器人）的回复在自己消息前渲染。
    const ts = Date.now();
    this.messages = [
      ...this.messages,
      { id: `local-${ts}`, from: this.myId, body: text, ts },
    ];
    this.emit();
    await this.transport.sendTo(target, text);
  }

  getMessages = (): Message[] => this.messages;
  getEntities = (): Entity[] => this.entities;
  getMyId = (): string => this.myId;
  getServerLabel = (): string => this.serverLabel;
  isConnected = (): boolean => this.connected;

  subscribe = (cb: () => void): (() => void) => {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  };
  private emit(): void {
    this.listeners.forEach((l) => l());
  }
}

function shortLabel(p: ConnectParams): string {
  const n = p.node.trim();
  return n.length > 22 ? `${n.slice(0, 10)}…${n.slice(-4)}` : n;
}

export const chatStore = new ChatStore();

function useStore<T>(getter: () => T): T {
  return useSyncExternalStore(chatStore.subscribe, getter);
}
export const useMessages = () => useStore(chatStore.getMessages);
export const useEntities = () => useStore(chatStore.getEntities);
export const useMyId = () => useStore(chatStore.getMyId);
export const useServerLabel = () => useStore(chatStore.getServerLabel);
export const useConnected = () => useStore(chatStore.isConnected);

export function useStartOnce(): void {
  useEffect(() => {
    chatStore.start();
  }, []);
}

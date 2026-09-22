import { useEffect, useSyncExternalStore } from "react";
import { makeTransport } from "../core";
import type { CoreEvent, Entity, Message } from "../core";

// 极简状态层（占位）。生产可替换为 zustand / jotai。
class ChatStore {
  private messages: Message[] = [];
  private entities: Entity[] = [];
  private myId = "";
  private connected = false;
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

  async connect(nodeAddr: string, displayName: string): Promise<void> {
    this.myId = await this.transport.connect(nodeAddr, displayName);
    this.connected = true;
    this.emit();
  }
  async refreshDirectory(kindPrefix: string): Promise<void> {
    this.entities = await this.transport.directoryQuery(kindPrefix);
    this.emit();
  }
  async send(target: string, text: string): Promise<void> {
    await this.transport.sendTo(target, text);
    // 本地回显自己发出的消息
    this.messages = [
      ...this.messages,
      { id: `local-${Date.now()}`, from: this.myId, body: text, ts: Date.now() },
    ];
    this.emit();
  }

  getMessages = (): Message[] => this.messages;
  getEntities = (): Entity[] => this.entities;
  getMyId = (): string => this.myId;
  isConnected = (): boolean => this.connected;

  subscribe = (cb: () => void): (() => void) => {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  };
  private emit(): void {
    this.listeners.forEach((l) => l());
  }
}

export const chatStore = new ChatStore();

function useStore<T>(getter: () => T): T {
  return useSyncExternalStore(chatStore.subscribe, getter);
}
export const useMessages = () => useStore(chatStore.getMessages);
export const useEntities = () => useStore(chatStore.getEntities);
export const useMyId = () => useStore(chatStore.getMyId);
export const useConnected = () => useStore(chatStore.isConnected);

export function useStartOnce(): void {
  useEffect(() => {
    chatStore.start();
  }, []);
}

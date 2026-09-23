import { useSyncExternalStore } from "react";
import type { ConnectMode, ConnectParams } from "../core";

/** 一条保存的服务器记录（持久化到 localStorage）。 */
export interface SavedServer {
  id: string;
  label: string; // 用户起的名字，如 "公司 imd"
  mode: ConnectMode; // nat | selfhost | lan
  node: string; // node id(hex) 或 IM_NODE_ADDR(JSON)
  displayName: string; // 登录昵称（注册为 person 的展示名）
  relayUrls?: string[]; // 仅 selfhost
  pkarrUrl?: string; // 仅 selfhost
  dnsOrigin?: string; // 仅 selfhost
  lastConnectedAt?: number;
}

const KEY = "imspace.servers.v1";

function load(): SavedServer[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) || "[]");
    return Array.isArray(v) ? v : [];
  } catch {
    return [];
  }
}

function uuid(): string {
  const c = globalThis.crypto as Crypto | undefined;
  return c?.randomUUID ? c.randomUUID() : `s-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

class ServerStore {
  private items: SavedServer[] = load();
  private listeners = new Set<() => void>();

  private commit(next: SavedServer[]): void {
    this.items = next;
    localStorage.setItem(KEY, JSON.stringify(next));
    this.listeners.forEach((l) => l());
  }

  list = (): SavedServer[] => this.items;

  /** 新增或更新（无 id 则新增，置顶）。返回记录（含 id）。 */
  upsert(input: Omit<SavedServer, "id"> & { id?: string }): SavedServer {
    const id = input.id ?? uuid();
    const rec: SavedServer = { ...input, id };
    const i = this.items.findIndex((x) => x.id === id);
    if (i >= 0) {
      const next = this.items.slice();
      next[i] = rec;
      this.commit(next);
    } else {
      this.commit([rec, ...this.items]);
    }
    return rec;
  }

  remove(id: string): void {
    this.commit(this.items.filter((x) => x.id !== id));
  }

  /** 记一次成功连接的时间（用于排序/展示）。 */
  touch(id: string): void {
    this.commit(
      this.items.map((x) => (x.id === id ? { ...x, lastConnectedAt: Date.now() } : x)),
    );
  }

  toParams(s: SavedServer): ConnectParams {
    return {
      mode: s.mode,
      node: s.node,
      displayName: s.displayName,
      relayUrls: s.relayUrls,
      pkarrUrl: s.pkarrUrl,
      dnsOrigin: s.dnsOrigin,
    };
  }

  subscribe = (cb: () => void): (() => void) => {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  };
}

export const serverStore = new ServerStore();

export function useServers(): SavedServer[] {
  return useSyncExternalStore(serverStore.subscribe, serverStore.list);
}

import { useState } from "react";
import {
  chatStore,
  useEntities,
  useMessages,
  useMyId,
  useServerLabel,
} from "../state/store";

const IconLogout = (
  <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
    <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" /><path d="M16 17l5-5-5-5M21 12H9" />
  </svg>
);
const IconRefresh = (
  <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
    <path d="M21 2v6h-6" /><path d="M3 12a9 9 0 0 1 15-6.7L21 8" /><path d="M3 22v-6h6" /><path d="M21 12a9 9 0 0 1-15 6.7L3 16" />
  </svg>
);

export function ChatScreen() {
  const myId = useMyId();
  const serverLabel = useServerLabel();
  const messages = useMessages();
  const entities = useEntities();

  const [target, setTarget] = useState("");
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  const run = async (fn: () => Promise<void>) => {
    setBusy(true);
    setErr("");
    try {
      await fn();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const selected = entities.find((e) => e.id === target);

  return (
    <div className="chat">
      <header className="topbar">
        <div className="topbar__left">
          <span className="status-dot" title="已连接" />
          <div className="topbar__server">
            <span className="topbar__label">{serverLabel || "已连接"}</span>
            <span className="topbar__me">我 · <code className="mono">{myId.slice(0, 10)}…</code></span>
          </div>
        </div>
        <button className="btn btn--ghost btn--sm" onClick={() => run(() => chatStore.disconnect())}>
          {IconLogout}<span>切换服务器</span>
        </button>
      </header>

      <div className="chat__body">
        <aside className="dir">
          <div className="dir__head">
            <span>目录</span>
            <button className="iconbtn" title="刷新" disabled={busy} onClick={() => run(() => chatStore.refreshDirectory(""))}>
              {IconRefresh}
            </button>
          </div>
          <ul className="dir__list">
            {entities.map((e) => (
              <li
                key={e.id}
                className={`dir__item ${e.id === target ? "is-sel" : ""}`}
                onClick={() => setTarget(e.id)}
                title={e.id}
              >
                <span className="avatar">{(e.name || "?").slice(0, 1)}</span>
                <span className="dir__meta">
                  <b>{e.name || "(无名)"}</b>
                  <small>{e.kind}</small>
                </span>
              </li>
            ))}
            {entities.length === 0 && <li className="dir__empty">点右上「刷新」加载实体</li>}
          </ul>
        </aside>

        <section className="conv">
          <div className="conv__head">
            {selected ? (
              <>
                <span className="avatar avatar--sm">{(selected.name || "?").slice(0, 1)}</span>
                <b>{selected.name || "(无名)"}</b>
                <span className="badge badge--nat">{selected.kind}</span>
              </>
            ) : (
              <span className="muted">在左侧选择一个对象，或在下方填入其 EntityId</span>
            )}
          </div>

          <ul className="msgs">
            {messages.map((m) => (
              <li key={m.id} className={`msg ${m.from === myId ? "msg--me" : ""}`}>
                <span className="msg__bubble">{m.body}</span>
                <span className="msg__from">{m.from === myId ? "我" : `${m.from.slice(0, 6)}…`}</span>
              </li>
            ))}
            {messages.length === 0 && <li className="msgs__empty">暂无消息</li>}
          </ul>

          <form
            className="composer"
            onSubmit={(e) => {
              e.preventDefault();
              if (target && body.trim()) void run(() => chatStore.send(target, body)).then(() => setBody(""));
            }}
          >
            <input
              className="composer__target"
              placeholder="目标 EntityId(hex)"
              value={target}
              onChange={(e) => setTarget(e.target.value)}
            />
            <input
              className="composer__body"
              placeholder="输入消息…"
              value={body}
              onChange={(e) => setBody(e.target.value)}
            />
            <button className="btn btn--primary" type="submit" disabled={busy || !target || !body.trim()}>
              发送
            </button>
          </form>
          {err && <div className="alert alert--inline">{err}</div>}
        </section>
      </div>
    </div>
  );
}

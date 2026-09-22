import { useState } from "react";
import {
  chatStore,
  useConnected,
  useEntities,
  useMessages,
  useMyId,
  useStartOnce,
} from "../state/store";

export function ChatScreen() {
  useStartOnce();
  const connected = useConnected();
  const myId = useMyId();
  const messages = useMessages();
  const entities = useEntities();

  const [nodeAddr, setNodeAddr] = useState("");
  const [name, setName] = useState("我");
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

  if (!connected) {
    return (
      <div className="chat">
        <header className="chat__header">imspace · 连接节点</header>
        <div className="connect">
          <p className="hint">
            先运行 <code>cargo run -p imd</code>，把它打印的 <code>IM_NODE_ADDR=</code> 后面那段 JSON 粘到这里。
          </p>
          <textarea
            placeholder="节点地址(JSON)"
            value={nodeAddr}
            onChange={(e) => setNodeAddr(e.target.value)}
            rows={3}
          />
          <input placeholder="昵称" value={name} onChange={(e) => setName(e.target.value)} />
          <button
            disabled={busy || !nodeAddr.trim()}
            onClick={() => run(() => chatStore.connect(nodeAddr.trim(), name))}
          >
            {busy ? "连接中…" : "连接并注册为 person"}
          </button>
          {err && <p className="err">{err}</p>}
        </div>
      </div>
    );
  }

  return (
    <div className="chat">
      <header className="chat__header">
        imspace · 我是 <code>{myId.slice(0, 8)}…</code>
      </header>

      <div className="chat__body">
        <aside className="chat__dir">
          <div className="chat__dirhead">
            <span>目录</span>
            <button disabled={busy} onClick={() => run(() => chatStore.refreshDirectory(""))}>
              刷新
            </button>
          </div>
          <ul>
            {entities.map((e) => (
              <li key={e.id} onClick={() => setTarget(e.id)} title={e.id}>
                <b>{e.name || "(无名)"}</b>
                <small>{e.kind}</small>
              </li>
            ))}
            {entities.length === 0 && <li className="muted">点“刷新”加载实体</li>}
          </ul>
        </aside>

        <section className="chat__main">
          <ul className="chat__list">
            {messages.map((m) => (
              <li key={m.id} className={m.from === myId ? "mine" : ""}>
                <b>{m.from.slice(0, 6)}…</b> {m.body}
              </li>
            ))}
            {messages.length === 0 && <li className="muted">暂无消息</li>}
          </ul>
          <form
            className="chat__composer"
            onSubmit={(e) => {
              e.preventDefault();
              if (target && body.trim()) void run(() => chatStore.send(target, body)).then(() => setBody(""));
            }}
          >
            <input placeholder="目标 EntityId(hex，点左侧目录可填入)" value={target} onChange={(e) => setTarget(e.target.value)} />
            <input placeholder="消息…" value={body} onChange={(e) => setBody(e.target.value)} />
            <button type="submit" disabled={busy || !target || !body.trim()}>
              发送
            </button>
          </form>
          {err && <p className="err">{err}</p>}
        </section>
      </div>
    </div>
  );
}

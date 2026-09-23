import { useState } from "react";
import {
  chatStore,
  useConnected,
  useEntities,
  useMessages,
  useMyId,
  useStartOnce,
} from "../state/store";
import type { ConnectMode } from "../core";

export function ChatScreen() {
  useStartOnce();
  const connected = useConnected();
  const myId = useMyId();
  const messages = useMessages();
  const entities = useEntities();

  const [mode, setMode] = useState<ConnectMode>("nat");
  const [node, setNode] = useState("");
  const [name, setName] = useState("我");
  const [relayUrl, setRelayUrl] = useState("");
  const [pkarrUrl, setPkarrUrl] = useState("");
  const [dnsOrigin, setDnsOrigin] = useState("");
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

  const doConnect = () =>
    run(() =>
      chatStore.connect({
        mode,
        node: node.trim(),
        displayName: name,
        relayUrls: relayUrl.trim() ? [relayUrl.trim()] : [],
        pkarrUrl: pkarrUrl.trim() || undefined,
        dnsOrigin: dnsOrigin.trim() || undefined,
      }),
    );

  const isLan = mode === "lan";
  const nodeReady =
    node.trim().length > 0 && (mode !== "selfhost" || pkarrUrl.trim().length > 0);

  if (!connected) {
    return (
      <div className="chat">
        <header className="chat__header">imspace · 连接节点</header>
        <div className="connect">
          <label className="field">
            <span>连接模式</span>
            <select value={mode} onChange={(e) => setMode(e.target.value as ConnectMode)}>
              <option value="nat">nat · n0 公共设施（穿透 NAT，按公钥）</option>
              <option value="selfhost">selfhost · 自建 relay+dns（穿透 NAT，按公钥）</option>
              <option value="lan">lan · 仅同网（按地址）</option>
            </select>
          </label>

          {isLan ? (
            <>
              <p className="hint">
                本机跑 <code>cargo run -p imd</code>（lan 模式），把它打印的{" "}
                <code>IM_NODE_ADDR=</code> 后那段 JSON 粘到这里。
              </p>
              <textarea
                placeholder="节点地址(JSON)"
                value={node}
                onChange={(e) => setNode(e.target.value)}
                rows={3}
              />
            </>
          ) : (
            <>
              <p className="hint">
                填节点公钥 <code>IM_NODE_ID</code>（hex, 64 位），或直接粘完整地址{" "}
                <code>IM_NODE_ADDR</code>(JSON)。给地址时：<b>先试同网直连、失败再穿透 NAT</b>；
                只给公钥则走穿透（在线时也会优先同网路径）。
              </p>
              <textarea
                placeholder="节点公钥(hex, 64 位) 或 IM_NODE_ADDR(JSON)"
                value={node}
                onChange={(e) => setNode(e.target.value)}
                rows={3}
              />
            </>
          )}

          {mode === "selfhost" && (
            <div className="selfhost">
              <input
                placeholder="relay url（如 https://relay.example.com）"
                value={relayUrl}
                onChange={(e) => setRelayUrl(e.target.value)}
              />
              <input
                placeholder="pkarr 端点（如 https://dns.example.com/pkarr，必填）"
                value={pkarrUrl}
                onChange={(e) => setPkarrUrl(e.target.value)}
              />
              <input
                placeholder="dns origin（可选，如 dns.example.com.）"
                value={dnsOrigin}
                onChange={(e) => setDnsOrigin(e.target.value)}
              />
            </div>
          )}

          <input placeholder="昵称" value={name} onChange={(e) => setName(e.target.value)} />
          <button disabled={busy || !nodeReady} onClick={doConnect}>
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
              if (target && body.trim())
                void run(() => chatStore.send(target, body)).then(() => setBody(""));
            }}
          >
            <input
              placeholder="目标 EntityId(hex，点左侧目录可填入)"
              value={target}
              onChange={(e) => setTarget(e.target.value)}
            />
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

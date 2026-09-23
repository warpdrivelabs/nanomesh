import { useState } from "react";
import { chatStore } from "../state/store";
import { serverStore, useServers, type SavedServer } from "../state/servers";
import type { ConnectMode } from "../core";

/* ---------- 图标（内联 SVG，跟随 currentColor） ---------- */
const Icon = {
  plus: (
    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <path d="M12 5v14M5 12h14" />
    </svg>
  ),
  edit: (
    <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 20h9" /><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5z" />
    </svg>
  ),
  trash: (
    <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2m2 0v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" /><path d="M10 11v6M14 11v6" />
    </svg>
  ),
  server: (
    <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
      <rect x="3" y="4" width="18" height="7" rx="2" /><rect x="3" y="13" width="18" height="7" rx="2" /><path d="M7 7.5h.01M7 16.5h.01" />
    </svg>
  ),
  bolt: (
    <svg viewBox="0 0 24 24" width="15" height="15" fill="currentColor"><path d="M13 2 3 14h7l-1 8 10-12h-7l1-8z" /></svg>
  ),
  back: (
    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M15 18l-6-6 6-6" /></svg>
  ),
};

const MODES: { key: ConnectMode; label: string; hint: string }[] = [
  { key: "nat", label: "NAT 穿透", hint: "同网/跨网通用 · 按公钥" },
  { key: "selfhost", label: "自建设施", hint: "自建 relay + dns · 按公钥" },
  { key: "lan", label: "仅同网", hint: "按地址直连" },
];

function relTime(ts?: number): string {
  if (!ts) return "尚未连接";
  const s = Math.floor((Date.now() - ts) / 1000);
  if (s < 60) return "刚刚连接";
  if (s < 3600) return `${Math.floor(s / 60)} 分钟前`;
  if (s < 86400) return `${Math.floor(s / 3600)} 小时前`;
  return `${Math.floor(s / 86400)} 天前`;
}

function shortNode(node: string): string {
  const n = node.trim();
  if (n.startsWith("{")) return "地址(JSON)";
  return n.length > 24 ? `${n.slice(0, 12)}…${n.slice(-6)}` : n;
}

type Draft = {
  id?: string;
  label: string;
  mode: ConnectMode;
  node: string;
  displayName: string;
  relayUrl: string;
  pkarrUrl: string;
  dnsOrigin: string;
  remember: boolean;
};

const emptyDraft = (): Draft => ({
  id: undefined,
  label: "",
  mode: "nat",
  node: "",
  displayName: "我",
  relayUrl: "",
  pkarrUrl: "",
  dnsOrigin: "",
  remember: true,
});

function draftFrom(s: SavedServer): Draft {
  return {
    id: s.id,
    label: s.label,
    mode: s.mode,
    node: s.node,
    displayName: s.displayName || "我",
    relayUrl: (s.relayUrls && s.relayUrls[0]) || "",
    pkarrUrl: s.pkarrUrl || "",
    dnsOrigin: s.dnsOrigin || "",
    remember: true,
  };
}

export function ConnectScreen() {
  const servers = useServers();
  const [draft, setDraft] = useState<Draft | null>(null); // null = 列表视图
  const [busyId, setBusyId] = useState<string | null>(null); // 正在连接的记录 id（"__form__" 表示表单）
  const [err, setErr] = useState("");

  const patch = (p: Partial<Draft>) => setDraft((d) => (d ? { ...d, ...p } : d));

  async function connectSaved(s: SavedServer) {
    setErr("");
    setBusyId(s.id);
    try {
      await chatStore.connect(serverStore.toParams(s), s.label);
      serverStore.touch(s.id);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusyId(null);
    }
  }

  async function connectDraft() {
    if (!draft) return;
    setErr("");
    setBusyId("__form__");
    const params = {
      mode: draft.mode,
      node: draft.node.trim(),
      displayName: draft.displayName.trim() || "我",
      relayUrls: draft.relayUrl.trim() ? [draft.relayUrl.trim()] : [],
      pkarrUrl: draft.pkarrUrl.trim() || undefined,
      dnsOrigin: draft.dnsOrigin.trim() || undefined,
    };
    try {
      let label = draft.label.trim();
      if (draft.remember) {
        const rec = serverStore.upsert({
          id: draft.id,
          label: label || defaultLabel(draft),
          mode: draft.mode,
          node: params.node,
          displayName: params.displayName,
          relayUrls: params.relayUrls,
          pkarrUrl: params.pkarrUrl,
          dnsOrigin: params.dnsOrigin,
        });
        label = rec.label;
        await chatStore.connect(params, label);
        serverStore.touch(rec.id);
      } else {
        await chatStore.connect(params, label || defaultLabel(draft));
      }
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusyId(null);
    }
  }

  const canConnect =
    !!draft &&
    draft.node.trim().length > 0 &&
    (draft.mode !== "selfhost" || draft.pkarrUrl.trim().length > 0);

  return (
    <div className="screen">
      <div className="panel">
        <header className="panel__brand">
          <div className="logo">im</div>
          <div>
            <h1>imspace</h1>
            <p>去中心即时通讯 · 连接一个 imd 节点</p>
          </div>
        </header>

        {err && <div className="alert">{err}</div>}

        {draft === null ? (
          <>
            <div className="panel__head">
              <span className="panel__title">保存的服务器</span>
              <button className="btn btn--primary btn--sm" onClick={() => setDraft(emptyDraft())}>
                {Icon.plus}<span>添加服务器</span>
              </button>
            </div>

            {servers.length === 0 ? (
              <div className="empty">
                <div className="empty__icon">{Icon.server}</div>
                <p className="empty__title">还没有保存的服务器</p>
                <p className="empty__sub">添加一个 imd 节点，下次一键连接（同网/跨网自动切换）</p>
                <button className="btn btn--primary" onClick={() => setDraft(emptyDraft())}>
                  {Icon.plus}<span>添加第一个</span>
                </button>
              </div>
            ) : (
              <ul className="srv-list">
                {servers.map((s) => (
                  <li key={s.id} className="srv">
                    <div className="srv__main" onClick={() => busyId ? null : connectSaved(s)}>
                      <div className="srv__icon">{Icon.server}</div>
                      <div className="srv__text">
                        <div className="srv__row1">
                          <span className="srv__name">{s.label || "(未命名)"}</span>
                          <span className={`badge badge--${s.mode}`}>{modeLabel(s.mode)}</span>
                        </div>
                        <div className="srv__row2">
                          <code className="mono">{shortNode(s.node)}</code>
                          <span className="dot">·</span>
                          <span className="muted">{s.displayName || "我"}</span>
                          <span className="dot">·</span>
                          <span className="muted">{relTime(s.lastConnectedAt)}</span>
                        </div>
                      </div>
                    </div>
                    <div className="srv__actions">
                      <button
                        className="btn btn--connect btn--sm"
                        disabled={!!busyId}
                        onClick={() => connectSaved(s)}
                        title="连接"
                      >
                        {busyId === s.id ? <span className="spin" /> : Icon.bolt}
                        <span>{busyId === s.id ? "连接中" : "连接"}</span>
                      </button>
                      <button className="iconbtn" title="编辑" disabled={!!busyId} onClick={() => setDraft(draftFrom(s))}>
                        {Icon.edit}
                      </button>
                      <button
                        className="iconbtn iconbtn--danger"
                        title="删除"
                        disabled={!!busyId}
                        onClick={() => {
                          if (confirm(`删除服务器「${s.label || s.node}」？`)) serverStore.remove(s.id);
                        }}
                      >
                        {Icon.trash}
                      </button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </>
        ) : (
          <>
            <div className="panel__head">
              <button className="iconbtn" title="返回" onClick={() => { setDraft(null); setErr(""); }}>
                {Icon.back}
              </button>
              <span className="panel__title">{draft.id ? "编辑服务器" : "添加服务器"}</span>
            </div>

            <div className="form">
              <label className="field">
                <span className="field__label">名称</span>
                <input placeholder="例如：公司 imd / 家里的节点" value={draft.label} onChange={(e) => patch({ label: e.target.value })} />
              </label>

              <div className="field">
                <span className="field__label">连接模式</span>
                <div className="seg">
                  {MODES.map((m) => (
                    <button
                      key={m.key}
                      className={`seg__item ${draft.mode === m.key ? "is-active" : ""}`}
                      onClick={() => patch({ mode: m.key })}
                      type="button"
                    >
                      {m.label}
                    </button>
                  ))}
                </div>
                <span className="field__help">{MODES.find((m) => m.key === draft.mode)?.hint}</span>
              </div>

              <label className="field">
                <span className="field__label">
                  {draft.mode === "lan" ? "节点地址 (IM_NODE_ADDR / JSON)" : "节点公钥 (IM_NODE_ID) 或完整地址"}
                </span>
                <textarea
                  rows={draft.mode === "lan" ? 3 : 2}
                  placeholder={draft.mode === "lan" ? '{"id":"…","addrs":[…]}' : "64 位十六进制公钥，或粘贴完整 IM_NODE_ADDR"}
                  value={draft.node}
                  onChange={(e) => patch({ node: e.target.value })}
                />
                {draft.mode !== "lan" && (
                  <span className="field__help">填公钥即可：同网自动直连、跨网自动经中继打洞。</span>
                )}
              </label>

              {draft.mode === "selfhost" && (
                <div className="subform">
                  <label className="field">
                    <span className="field__label">relay 地址</span>
                    <input placeholder="https://relay.example.com" value={draft.relayUrl} onChange={(e) => patch({ relayUrl: e.target.value })} />
                  </label>
                  <label className="field">
                    <span className="field__label">pkarr 端点（必填）</span>
                    <input placeholder="https://dns.example.com/pkarr" value={draft.pkarrUrl} onChange={(e) => patch({ pkarrUrl: e.target.value })} />
                  </label>
                  <label className="field">
                    <span className="field__label">dns origin（可选）</span>
                    <input placeholder="dns.example.com." value={draft.dnsOrigin} onChange={(e) => patch({ dnsOrigin: e.target.value })} />
                  </label>
                </div>
              )}

              <label className="field">
                <span className="field__label">昵称</span>
                <input placeholder="在网络中显示的名字" value={draft.displayName} onChange={(e) => patch({ displayName: e.target.value })} />
              </label>

              <label className="check">
                <input type="checkbox" checked={draft.remember} onChange={(e) => patch({ remember: e.target.checked })} />
                <span>记住此服务器（保存到列表）</span>
              </label>

              <div className="form__actions">
                <button className="btn btn--ghost" onClick={() => { setDraft(null); setErr(""); }}>取消</button>
                <button className="btn btn--primary" disabled={!canConnect || busyId === "__form__"} onClick={connectDraft}>
                  {busyId === "__form__" ? <span className="spin" /> : Icon.bolt}
                  <span>{busyId === "__form__" ? "连接中…" : "连接"}</span>
                </button>
              </div>
            </div>
          </>
        )}
      </div>
      <p className="foot">身份即公钥 · 无需注册 · 数据端到端加密</p>
    </div>
  );
}

function modeLabel(m: ConnectMode): string {
  return m === "nat" ? "NAT 穿透" : m === "selfhost" ? "自建设施" : "仅同网";
}
function defaultLabel(d: Draft): string {
  const n = d.node.trim();
  if (n.startsWith("{")) return "同网节点";
  return n ? `${n.slice(0, 8)}…` : "新服务器";
}

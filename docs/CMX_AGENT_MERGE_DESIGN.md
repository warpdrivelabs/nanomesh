# cmx-agent 智能体能力接入 nanomesh 方案（引用式集成 · 模型走 P2P）

> 让 nanomesh 获得 `cmx-agent` 的**智能体能力**。策略分两层：
> **① Rust agent 内核 = 完整 Git 依赖引用**（`cmx-agent-core`/`-tools`/`-mcp`，单一代码源、随上游迭代，nanomesh 不复制、不 fork）；
> **② 前门 façade + 前端 UI = nanomesh 自写/复制后独立迭代**（上游前门层耦合 HTTP/门户/market 不宜引用；前端 JS 无法 Cargo 引用）。
> **IM（单聊/群聊/频道）用 nanomesh 自己已跑通的**；**唯一的模型改造**：接入从「OpenAI 兼容 HTTP 直连」改为 nanomesh 的 **P2P `model.*` 能力**
> （见 `COMPUTE_MODEL_P2P_DESIGN.md`），在 nanomesh 侧实现 cmx-agent 的 `ModelSeam` trait 即可，**不改 cmx-agent 一行**。
>
> 文档先行——审核通过后再落地。状态：草案 v3（2026-10-09：内核完整引用 + 前门/前端自写，依用户定）。
>
> **可行性结论：可行。** 精确边界（已核对两边）：
> - **可完整、干净引用的内核三件套** `cmx-agent-core/-tools/-mcp`：**零网络**（依赖仅 serde/tokio/async-trait/…；无 reqwest、无具体模型），Apache-2.0，`edition=2024`、工具链 1.97.1 与 nanomesh（rustc 1.99.0、edition 2024）兼容 → agent 的回合/工具/守卫/MCP **全在这里，100% 引用**。
> - **不宜引用的前门层** `cmx-agent-app`：耦合 `cmx-agent-model`(HTTP) + `cmx-agent-net` + `reqwest` + 门户/market → nanomesh **自写薄前门**（借其命令协议形状）。
> - 内核以 `trait ModelSeam` 把"具体大模型"与回合循环**解耦**，故 P2P 接入 = nanomesh 侧实现一个 `ModelSeam`，**内核零改**。

## 0. 前提澄清（已核对两边代码）

- **cmx-agent 目前没有真正的 IM**：`cmx-agent-im` 只是"把 IM 会话桥接到 agent 跑回合"的**遥控桩** + Telegram 参考 provider，不是单聊/群聊/频道实现。→ **IM 不从 cmx-agent 搬**。
- **nanomesh 已有并跑通 IM**：单聊、群聊、频道、离线收件箱（本轮刚修复离线补投）。→ **IM 用 nanomesh 的**。
- **要集成的是智能体那半边**：`cmx-agent-core`（内核）、`cmx-agent-tools`（内置工具）、`cmx-agent-mcp`（MCP host）——**以 Git 依赖完整引用**，不复制。它们是 agent 的"脑子"（回合循环/工具路由/守卫/会话事件/ModelSeam）。
- **前门层 `cmx-agent-app` 不引用（必须 nanomesh 自写）**：已核对——它虽是"同核多壳 façade + 命令协议"，但**耦合了 `cmx-agent-model`(HTTP 直连) + `cmx-agent-net` + 直接 `reqwest`**（market.rs/client.rs），且很重（app.rs 191KB、market 62KB、workspace 50KB、绑定 cmx 自己的会话 JSONL/门户登录/market）。我们**借它的命令协议形状做蓝本**，但在 nanomesh 侧重写薄前门（模型走 P2P、会话走 nanomesh、无 market/门户）。
- **cmx-agent 现在怎么接模型**：`cmx-agent-model::OpenAiCompatModel` —— `reqwest` POST `{base}/chat/completions`，直连 DeepSeek/OpenAI/Qwen/Ollama，靠本地 API Key（中心化、绕过 P2P）。**我们不引用这个 crate**，改为在 nanomesh 侧实现 `ModelSeam` 走 P2P。
- **前端 UI = 复制后独立迭代**：前端是 JS 静态资源、无法 Cargo 引用；**按用户决定：从 cmx-agent-shell 复制 agent 面板功能进 nanomesh 壳，之后独立演进**（不保持与上游同步）。

## 1. 合并总览

<img alt="cmx-agent 智能体能力并入 nanomesh 总览" width="860" src="data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA4NjAgNDE2IiB3aWR0aD0iODYwIiBoZWlnaHQ9IjQxNiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiI+PHJlY3Qgd2lkdGg9Ijg2MCIgaGVpZ2h0PSI0MTYiIGZpbGw9IiNmZmZmZmYiLz48ZGVmcz48bWFya2VyIGlkPSJhci1tIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiM2YjcyODAiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1hIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiMxZmIxODIiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1iIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiMyZjZmZGQiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1yIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiNkMTQ5NWIiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1wIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiM3YzZjZjAiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1vIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiNkOThhMWYiLz48L21hcmtlcj48L2RlZnM+PHRleHQgeD0iNDMwIiB5PSIzMCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxNS41IiBmaWxsPSIjMTQxYTFmIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIj7lm74gMSDCtyBjbXgtYWdlbnQg5pm66IO95L2T6IO95Yqb5bm25YWlIG5hbm9tZXNoIOKAlOKAlCBJTSDnlKggbmFub21lc2jvvIzmqKHlnovmlLnotbAgUDJQPC90ZXh0Pjx0ZXh0IHg9IjQzMCIgeT0iNTEiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTAuNiIgZmlsbD0iIzZiNzI4MCIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjQwMCI+5pCs44CM5pm66IO95L2T5YaF5qC4ICsg5bel5YW3ICsgTUNQICsgVUkg6Z2i5p2/44CN6L+bIG5hbm9tZXNo77yb5ZSv5LiA5pS56YCg77ya5qih5Z6L5o6l5YWlIEhUVFDnm7Tov54g4oaSIFAyUCBtb2RlbC4qIOiDveWKmzwvdGV4dD48cmVjdCB4PSIyOCIgeT0iNzIiIHdpZHRoPSIyMzYiIGhlaWdodD0iMzMwIiByeD0iMTIiIGZpbGw9IiNmZGVjZWUiIHN0cm9rZT0iI2QxNDk1YiIgc3Ryb2tlLXdpZHRoPSIxLjQiLz48dGV4dCB4PSIxNDYiIHk9IjkyIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjZDE0OTViIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIj5jbXgtYWdlbnTvvIjmnaXmupDvvIk8L3RleHQ+PHJlY3QgeD0iNDQiIHk9IjEwNCIgd2lkdGg9IjE2OCIgaGVpZ2h0PSI0MCIgcng9IjEwIiBmaWxsPSIjZmZmZmZmIiBzdHJva2U9IiMxZmIxODIiIHN0cm9rZS13aWR0aD0iMS41Ii8+PHRleHQgeD0iMTI4LjAiIHk9IjExOS4wIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjkuNiIgZmlsbD0iIzE0MWExZiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjcwMCIgZG9taW5hbnQtYmFzZWxpbmU9Im1pZGRsZSI+Y214LWFnZW50LWNvcmU8L3RleHQ+PHRleHQgeD0iMTI4LjAiIHk9IjEzNC4wIiBmb250LWZhbWlseT0idWktbW9ub3NwYWNlLE1lbmxvLENvbnNvbGFzLG1vbm9zcGFjZSIgZm9udC1zaXplPSI3LjQiIGZpbGw9IiM2YjcyODAiIHRleHQtYW5jaG9yPSJtaWRkbGUiPuWbnuWQiOW+queOr8K35bel5YW36Lev55SxwrflrojljavCt+S8muivneaXpeW/lzwvdGV4dD48cmVjdCB4PSIyMTgiIHk9IjExMiIgd2lkdGg9IjQwIiBoZWlnaHQ9IjI0IiByeD0iNiIgZmlsbD0iIzFmYjE4MiIgb3BhY2l0eT0iMC4xNCIvPjx0ZXh0IHg9IjIzOCIgeT0iMTI4IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjguNiIgZmlsbD0iIzFmYjE4MiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjcwMCI+5pCsPC90ZXh0PjxyZWN0IHg9IjQ0IiB5PSIxNTQiIHdpZHRoPSIxNjgiIGhlaWdodD0iNDAiIHJ4PSIxMCIgZmlsbD0iI2ZmZmZmZiIgc3Ryb2tlPSIjMWZiMTgyIiBzdHJva2Utd2lkdGg9IjEuNSIvPjx0ZXh0IHg9IjEyOC4wIiB5PSIxNjkuMCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI5LjYiIGZpbGw9IiMxNDFhMWYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI3MDAiIGRvbWluYW50LWJhc2VsaW5lPSJtaWRkbGUiPmNteC1hZ2VudC10b29sczwvdGV4dD48dGV4dCB4PSIxMjguMCIgeT0iMTg0LjAiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sQ29uc29sYXMsbW9ub3NwYWNlIiBmb250LXNpemU9IjcuNCIgZmlsbD0iIzZiNzI4MCIgdGV4dC1hbmNob3I9Im1pZGRsZSI+5YaF572u5bel5YW3IGVjaG8vY2xvY2svZnPigKY8L3RleHQ+PHJlY3QgeD0iMjE4IiB5PSIxNjIiIHdpZHRoPSI0MCIgaGVpZ2h0PSIyNCIgcng9IjYiIGZpbGw9IiMxZmIxODIiIG9wYWNpdHk9IjAuMTQiLz48dGV4dCB4PSIyMzgiIHk9IjE3OCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI4LjYiIGZpbGw9IiMxZmIxODIiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI3MDAiPuaQrDwvdGV4dD48cmVjdCB4PSI0NCIgeT0iMjA0IiB3aWR0aD0iMTY4IiBoZWlnaHQ9IjQwIiByeD0iMTAiIGZpbGw9IiNmZmZmZmYiIHN0cm9rZT0iIzFmYjE4MiIgc3Ryb2tlLXdpZHRoPSIxLjUiLz48dGV4dCB4PSIxMjguMCIgeT0iMjE5LjAiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iOS42IiBmaWxsPSIjMTQxYTFmIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIiBkb21pbmFudC1iYXNlbGluZT0ibWlkZGxlIj5jbXgtYWdlbnQtbWNwPC90ZXh0Pjx0ZXh0IHg9IjEyOC4wIiB5PSIyMzQuMCIgZm9udC1mYW1pbHk9InVpLW1vbm9zcGFjZSxNZW5sbyxDb25zb2xhcyxtb25vc3BhY2UiIGZvbnQtc2l6ZT0iNy40IiBmaWxsPSIjNmI3MjgwIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIj5NQ1AgaG9zdO+8iOWkluaOpeW3peWFt++8iTwvdGV4dD48cmVjdCB4PSIyMTgiIHk9IjIxMiIgd2lkdGg9IjQwIiBoZWlnaHQ9IjI0IiByeD0iNiIgZmlsbD0iIzFmYjE4MiIgb3BhY2l0eT0iMC4xNCIvPjx0ZXh0IHg9IjIzOCIgeT0iMjI4IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjguNiIgZmlsbD0iIzFmYjE4MiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjcwMCI+5pCsPC90ZXh0PjxyZWN0IHg9IjQ0IiB5PSIyNTQiIHdpZHRoPSIxNjgiIGhlaWdodD0iNDAiIHJ4PSIxMCIgZmlsbD0iI2ZmZmZmZiIgc3Ryb2tlPSIjZDE0OTViIiBzdHJva2Utd2lkdGg9IjEuNSIvPjx0ZXh0IHg9IjEyOC4wIiB5PSIyNjkuMCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI5LjYiIGZpbGw9IiMxNDFhMWYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI3MDAiIGRvbWluYW50LWJhc2VsaW5lPSJtaWRkbGUiPmNteC1hZ2VudC1tb2RlbDwvdGV4dD48dGV4dCB4PSIxMjguMCIgeT0iMjg0LjAiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sQ29uc29sYXMsbW9ub3NwYWNlIiBmb250LXNpemU9IjcuNCIgZmlsbD0iIzZiNzI4MCIgdGV4dC1hbmNob3I9Im1pZGRsZSI+T3BlbkFJIOWFvOWuuSBIVFRQIOebtOi/njwvdGV4dD48cmVjdCB4PSIyMTgiIHk9IjI2MiIgd2lkdGg9IjQwIiBoZWlnaHQ9IjI0IiByeD0iNiIgZmlsbD0iI2QxNDk1YiIgb3BhY2l0eT0iMC4xNCIvPjx0ZXh0IHg9IjIzOCIgeT0iMjc4IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjguNiIgZmlsbD0iI2QxNDk1YiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjcwMCI+5pu/5o2iPC90ZXh0PjxyZWN0IHg9IjQ0IiB5PSIzMDQiIHdpZHRoPSIxNjgiIGhlaWdodD0iNDAiIHJ4PSIxMCIgZmlsbD0iI2ZmZmZmZiIgc3Ryb2tlPSIjMmY2ZmRkIiBzdHJva2Utd2lkdGg9IjEuNSIvPjx0ZXh0IHg9IjEyOC4wIiB5PSIzMTkuMCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI5LjYiIGZpbGw9IiMxNDFhMWYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI3MDAiIGRvbWluYW50LWJhc2VsaW5lPSJtaWRkbGUiPnNoZWxsIFVJIOmdouadvzwvdGV4dD48dGV4dCB4PSIxMjguMCIgeT0iMzM0LjAiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sQ29uc29sYXMsbW9ub3NwYWNlIiBmb250LXNpemU9IjcuNCIgZmlsbD0iIzZiNzI4MCIgdGV4dC1hbmNob3I9Im1pZGRsZSI+YWdlbnRzL21jcC9tYXJrZXQuanM8L3RleHQ+PHJlY3QgeD0iMjE4IiB5PSIzMTIiIHdpZHRoPSI0MCIgaGVpZ2h0PSIyNCIgcng9IjYiIGZpbGw9IiMyZjZmZGQiIG9wYWNpdHk9IjAuMTQiLz48dGV4dCB4PSIyMzgiIHk9IjMyOCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI4LjYiIGZpbGw9IiMyZjZmZGQiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI3MDAiPuW5tuWFpeWjszwvdGV4dD48cmVjdCB4PSI0NCIgeT0iMzU0IiB3aWR0aD0iMTY4IiBoZWlnaHQ9IjQwIiByeD0iMTAiIGZpbGw9IiNmZmZmZmYiIHN0cm9rZT0iIzZiNzI4MCIgc3Ryb2tlLXdpZHRoPSIxLjUiLz48dGV4dCB4PSIxMjguMCIgeT0iMzY5LjAiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iOS42IiBmaWxsPSIjMTQxYTFmIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIiBkb21pbmFudC1iYXNlbGluZT0ibWlkZGxlIj5jbXgtYWdlbnQtaW08L3RleHQ+PHRleHQgeD0iMTI4LjAiIHk9IjM4NC4wIiBmb250LWZhbWlseT0idWktbW9ub3NwYWNlLE1lbmxvLENvbnNvbGFzLG1vbm9zcGFjZSIgZm9udC1zaXplPSI3LjQiIGZpbGw9IiM2YjcyODAiIHRleHQtYW5jaG9yPSJtaWRkbGUiPklNIOmBpeaOp+ahqe+8iOacquWunueOsCBJTe+8iTwvdGV4dD48cmVjdCB4PSIyMTgiIHk9IjM2MiIgd2lkdGg9IjQwIiBoZWlnaHQ9IjI0IiByeD0iNiIgZmlsbD0iIzZiNzI4MCIgb3BhY2l0eT0iMC4xNCIvPjx0ZXh0IHg9IjIzOCIgeT0iMzc4IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjguNiIgZmlsbD0iIzZiNzI4MCIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjcwMCI+5Lii5byDPC90ZXh0PjxyZWN0IHg9IjU5NiIgeT0iNzIiIHdpZHRoPSIyMzYiIGhlaWdodD0iMzMwIiByeD0iMTIiIGZpbGw9IiNlNmY1ZWYiIHN0cm9rZT0iIzFmYjE4MiIgc3Ryb2tlLXdpZHRoPSIxLjQiLz48dGV4dCB4PSI3MTQiIHk9IjkyIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjMWZiMTgyIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIj5uYW5vbWVzaO+8iOebruagh++8iTwvdGV4dD48cmVjdCB4PSI2MTIiIHk9IjEwOCIgd2lkdGg9IjIwNCIgaGVpZ2h0PSI0NCIgcng9IjEwIiBmaWxsPSIjZmZmZmZmIiBzdHJva2U9IiMxZmIxODIiIHN0cm9rZS13aWR0aD0iMS41Ii8+PHRleHQgeD0iNzE0LjAiIHk9IjEyNS4wIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEwIiBmaWxsPSIjMTQxYTFmIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIiBkb21pbmFudC1iYXNlbGluZT0ibWlkZGxlIj5JTe+8muWNleiBii/nvqTogYov6aKR6YGTPC90ZXh0Pjx0ZXh0IHg9IjcxNC4wIiB5PSIxNDAuMCIgZm9udC1mYW1pbHk9InVpLW1vbm9zcGFjZSxNZW5sbyxDb25zb2xhcyxtb25vc3BhY2UiIGZvbnQtc2l6ZT0iNy42IiBmaWxsPSIjNmI3MjgwIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIj7lt7Lot5HpgJog4pyTIOebtOaOpeeUqDwvdGV4dD48cmVjdCB4PSI2MTIiIHk9IjE2NCIgd2lkdGg9IjIwNCIgaGVpZ2h0PSI0NCIgcng9IjEwIiBmaWxsPSIjZmZmZmZmIiBzdHJva2U9IiMyZjZmZGQiIHN0cm9rZS13aWR0aD0iMS41Ii8+PHRleHQgeD0iNzE0LjAiIHk9IjE4MS4wIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEwIiBmaWxsPSIjMTQxYTFmIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIiBkb21pbmFudC1iYXNlbGluZT0ibWlkZGxlIj5ubS1hZ2VudO+8iOWGheaguO+8iTwvdGV4dD48dGV4dCB4PSI3MTQuMCIgeT0iMTk2LjAiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sQ29uc29sYXMsbW9ub3NwYWNlIiBmb250LXNpemU9IjcuNiIgZmlsbD0iIzZiNzI4MCIgdGV4dC1hbmNob3I9Im1pZGRsZSI+PSDmkKzlhaXnmoQgY214LWFnZW50LWNvcmU8L3RleHQ+PHJlY3QgeD0iNjEyIiB5PSIyMjAiIHdpZHRoPSIyMDQiIGhlaWdodD0iNDQiIHJ4PSIxMCIgZmlsbD0iI2ZmZmZmZiIgc3Ryb2tlPSIjMmY2ZmRkIiBzdHJva2Utd2lkdGg9IjEuNSIvPjx0ZXh0IHg9IjcxNC4wIiB5PSIyMzcuMCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMCIgZmlsbD0iIzE0MWExZiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjcwMCIgZG9taW5hbnQtYmFzZWxpbmU9Im1pZGRsZSI+bm0tYWdlbnQtdG9vbHMgLyBtY3A8L3RleHQ+PHRleHQgeD0iNzE0LjAiIHk9IjI1Mi4wIiBmb250LWZhbWlseT0idWktbW9ub3NwYWNlLE1lbmxvLENvbnNvbGFzLG1vbm9zcGFjZSIgZm9udC1zaXplPSI3LjYiIGZpbGw9IiM2YjcyODAiIHRleHQtYW5jaG9yPSJtaWRkbGUiPj0g5pCs5YWl55qE5bel5YW3ICsgTUNQPC90ZXh0PjxyZWN0IHg9IjYxMiIgeT0iMjc2IiB3aWR0aD0iMjA0IiBoZWlnaHQ9IjQ0IiByeD0iMTAiIGZpbGw9IiNmZmZmZmYiIHN0cm9rZT0iI2QxNDk1YiIgc3Ryb2tlLXdpZHRoPSIxLjUiLz48dGV4dCB4PSI3MTQuMCIgeT0iMjkzLjAiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTAiIGZpbGw9IiMxNDFhMWYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI3MDAiIGRvbWluYW50LWJhc2VsaW5lPSJtaWRkbGUiPlAycE1vZGVsU2VhbTwvdGV4dD48dGV4dCB4PSI3MTQuMCIgeT0iMzA4LjAiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sQ29uc29sYXMsbW9ub3NwYWNlIiBmb250LXNpemU9IjcuNiIgZmlsbD0iIzZiNzI4MCIgdGV4dC1hbmNob3I9Im1pZGRsZSI+5paw77ya5qih5Z6L6LWwIFAyUCBtb2RlbC4qPC90ZXh0PjxyZWN0IHg9IjYxMiIgeT0iMzMyIiB3aWR0aD0iMjA0IiBoZWlnaHQ9IjQ0IiByeD0iMTAiIGZpbGw9IiNmZmZmZmYiIHN0cm9rZT0iIzdjNmNmMCIgc3Ryb2tlLXdpZHRoPSIxLjUiLz48dGV4dCB4PSI3MTQuMCIgeT0iMzQ5LjAiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTAiIGZpbGw9IiMxNDFhMWYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI3MDAiIGRvbWluYW50LWJhc2VsaW5lPSJtaWRkbGUiPuahjOmdouWjsyB1aS9qczwvdGV4dD48dGV4dCB4PSI3MTQuMCIgeT0iMzY0LjAiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sQ29uc29sYXMsbW9ub3NwYWNlIiBmb250LXNpemU9IjcuNiIgZmlsbD0iIzZiNzI4MCIgdGV4dC1hbmNob3I9Im1pZGRsZSI+SU0g6Z2i5p2/ICsg5pm66IO95L2T6Z2i5p2/PC90ZXh0PjxsaW5lIHgxPSIyNjQiIHkxPSIxNTAiIHgyPSI2MTIiIHkyPSIxNTAiIHN0cm9rZT0iIzFmYjE4MiIgc3Ryb2tlLXdpZHRoPSIxLjgiIG1hcmtlci1lbmQ9InVybCgjYXItYSkiLz48dGV4dCB4PSI0MzgiIHk9IjE0MiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI4LjgiIGZpbGw9IiM2YjcyODAiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI0MDAiPuWkjeWItuWGheaguC/lt6XlhbcvTUNQPC90ZXh0PjxsaW5lIHgxPSIyNjQiIHkxPSIzMDAiIHgyPSI2MTIiIHkyPSIyNDAiIHN0cm9rZT0iI2QxNDk1YiIgc3Ryb2tlLXdpZHRoPSIyIiBtYXJrZXItZW5kPSJ1cmwoI2FyLXIpIi8+PHRleHQgeD0iNDMwIiB5PSIyOTAiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iOSIgZmlsbD0iI2QxNDk1YiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjcwMCI+5qih5Z6L5bGC5pu/5o2i5Li6IFAyUDwvdGV4dD48bGluZSB4MT0iMjY0IiB5MT0iMjUwIiB4Mj0iNjEyIiB5Mj0iMzIyIiBzdHJva2U9IiMyZjZmZGQiIHN0cm9rZS13aWR0aD0iMS42IiBtYXJrZXItZW5kPSJ1cmwoI2FyLWIpIi8+PHRleHQgeD0iNDMwIiB5PSIzMzIiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iOC44IiBmaWxsPSIjNmI3MjgwIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNDAwIj5VSSDpnaLmnb/lubblhaXlo7M8L3RleHQ+PC9zdmc+" />

- **完整引用内核（不复制）**：nanomesh 新建瘦前门 crate `nm-agent-bridge`，`[dependencies]` 以 **Git 依赖**完整引入 `cmx-agent-core` / `cmx-agent-tools` / `cmx-agent-mcp`（零网络、外部可直接编译）。agent 回合/工具/守卫/MCP 全部直接调用这些引用来的 crate。
- **自写薄前门（借上游蓝本）**：`nm-agent-bridge` 重写"前门命令 + 会话管理"——形状参照上游 `cmx-agent-app` 的 `AppRequest`/`AgentApp::send`，但实现里**模型走 `P2pModelSeam`、会话走 nanomesh（SQLite）、不含 market/门户/net**。
- **模型改造**：**不引用** `cmx-agent-model`（HTTP 直连）；新 `P2pModelSeam`（实现引用来的 `ModelSeam`）走 P2P。
- **前端复制后独立迭代**：从 cmx-agent-shell 复制 agent 面板功能进 nanomesh 壳 `clients/app/ui/`，**此后独立演进**，不与上游同步（两边壳同源同构，复制即可拼）。
- **不取**：`cmx-agent-app`（前门层，自写替代）、`cmx-agent-model`（HTTP）、`cmx-agent-net`（如需 web 工具另议）、`cmx-agent-im`（遥控桩；nanomesh 用自己的 IM）。

## 2. 唯一实质改造点：ModelSeam 从 HTTP 换成 P2P

<img alt="唯一改造点：ModelSeam 从 HTTP 换成 P2P" width="860" src="data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA4NjAgMzkyIiB3aWR0aD0iODYwIiBoZWlnaHQ9IjM5MiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiI+PHJlY3Qgd2lkdGg9Ijg2MCIgaGVpZ2h0PSIzOTIiIGZpbGw9IiNmZmZmZmYiLz48ZGVmcz48bWFya2VyIGlkPSJhci1tIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiM2YjcyODAiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1hIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiMxZmIxODIiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1iIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiMyZjZmZGQiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1yIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiNkMTQ5NWIiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1wIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiM3YzZjZjAiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1vIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiNkOThhMWYiLz48L21hcmtlcj48L2RlZnM+PHRleHQgeD0iNDMwIiB5PSIyOCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxNS41IiBmaWxsPSIjMTQxYTFmIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIj7lm74gMiDCtyDllK/kuIDmlLnpgKDngrnvvJrmjaLkuIDkuKogTW9kZWxTZWFtIOWunueOsO+8jOWbnuWQiOW+queOr+mbtuaUueWKqDwvdGV4dD48dGV4dCB4PSI0MzAiIHk9IjQ5IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEwLjQiIGZpbGw9IiM2YjcyODAiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI0MDAiPuWGheaguOWPquS+nei1liB0cmFpdCBNb2RlbFNlYW06OmNvbXBsZXRlKGN0eCktJmd0O3Jlc3DvvJvlupXlsYIgSFRUUCDov5jmmK8gUDJQIOWvueWGheaguOmAj+aYjjwvdGV4dD48cmVjdCB4PSIzMDAiIHk9Ijc0IiB3aWR0aD0iMjYwIiBoZWlnaHQ9IjcwIiByeD0iMTAiIGZpbGw9IiNlZWYyZjciIHN0cm9rZT0iIzE0MWExZiIgc3Ryb2tlLXdpZHRoPSIxLjUiLz48dGV4dCB4PSI0MzAuMCIgeT0iMTA0LjAiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTEuNSIgZmlsbD0iIzE0MWExZiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjcwMCIgZG9taW5hbnQtYmFzZWxpbmU9Im1pZGRsZSI+bm0tYWdlbnQg5YaF5qC477yI5Zue5ZCI5b6q546v77yJPC90ZXh0Pjx0ZXh0IHg9IjQzMC4wIiB5PSIxMTkuMCIgZm9udC1mYW1pbHk9InVpLW1vbm9zcGFjZSxNZW5sbyxDb25zb2xhcyxtb25vc3BhY2UiIGZvbnQtc2l6ZT0iOC42IiBmaWxsPSIjNmI3MjgwIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIj5BZ2VudCDlj6rmjIEgQXJjJmx0O2R5biBNb2RlbFNlYW0mZ3Q7PC90ZXh0Pjx0ZXh0IHg9IjQzMC4wIiB5PSIxMzEuMCIgZm9udC1mYW1pbHk9InVpLW1vbm9zcGFjZSxNZW5sbyxDb25zb2xhcyxtb25vc3BhY2UiIGZvbnQtc2l6ZT0iOC42IiBmaWxsPSIjNmI3MjgwIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIj7lt6Xlhbfot6/nlLEgwrcg5a6I5Y2rIMK3IOS8muivneaXpeW/lzwvdGV4dD48dGV4dCB4PSI0MzAiIHk9IjE2MiIgZm9udC1mYW1pbHk9InVpLW1vbm9zcGFjZSxNZW5sbyxDb25zb2xhcyxtb25vc3BhY2UiIGZvbnQtc2l6ZT0iOSIgZmlsbD0iIzdjNmNmMCIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjcwMCI+dHJhaXQgTW9kZWxTZWFtIHsgYXN5bmMgZm4gY29tcGxldGUoJmFtcDtjdHgpIC0mZ3Q7IE1vZGVsUmVzcG9uc2UgfTwvdGV4dD48bGluZSB4MT0iNDMwIiB5MT0iMTY4IiB4Mj0iNDMwIiB5Mj0iMTk2IiBzdHJva2U9IiM3YzZjZjAiIHN0cm9rZS13aWR0aD0iMS42IiBtYXJrZXItZW5kPSJ1cmwoI2FyLXApIi8+PHJlY3QgeD0iMTIwIiB5PSIyMDAiIHdpZHRoPSIzMDAiIGhlaWdodD0iOTIiIHJ4PSIxMCIgZmlsbD0iI2ZkZWNlZSIgc3Ryb2tlPSIjZDE0OTViIiBzdHJva2Utd2lkdGg9IjEuNSIvPjx0ZXh0IHg9IjI3MC4wIiB5PSIyNDEuMCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMC41IiBmaWxsPSIjZDE0OTViIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIiBkb21pbmFudC1iYXNlbGluZT0ibWlkZGxlIj7ml6fvvJpPcGVuQWlDb21wYXRNb2RlbO+8iOS4ou+8iTwvdGV4dD48dGV4dCB4PSIyNzAuMCIgeT0iMjU2LjAiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sQ29uc29sYXMsbW9ub3NwYWNlIiBmb250LXNpemU9IjgiIGZpbGw9IiM2YjcyODAiIHRleHQtYW5jaG9yPSJtaWRkbGUiPnJlcXdlc3QgUE9TVCB7YmFzZX0vY2hhdC9jb21wbGV0aW9uczwvdGV4dD48dGV4dCB4PSIyNzAuMCIgeT0iMjY4LjAiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sQ29uc29sYXMsbW9ub3NwYWNlIiBmb250LXNpemU9IjgiIGZpbGw9IiM2YjcyODAiIHRleHQtYW5jaG9yPSJtaWRkbGUiPuebtOi/niBEZWVwU2Vlay9PcGVuQUkvT2xsYW1hPC90ZXh0Pjx0ZXh0IHg9IjI3MC4wIiB5PSIyODAuMCIgZm9udC1mYW1pbHk9InVpLW1vbm9zcGFjZSxNZW5sbyxDb25zb2xhcyxtb25vc3BhY2UiIGZvbnQtc2l6ZT0iOCIgZmlsbD0iIzZiNzI4MCIgdGV4dC1hbmNob3I9Im1pZGRsZSI+PSDnu5Xov4cgUDJQ77yM5Lit5b+D5YyWIEtleTwvdGV4dD48cmVjdCB4PSI0NDAiIHk9IjIwMCIgd2lkdGg9IjMwMCIgaGVpZ2h0PSI5MiIgcng9IjEwIiBmaWxsPSIjZTZmNWVmIiBzdHJva2U9IiMxZmIxODIiIHN0cm9rZS13aWR0aD0iMS41Ii8+PHRleHQgeD0iNTkwLjAiIHk9IjI0MS4wIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEwLjUiIGZpbGw9IiMxZmIxODIiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI3MDAiIGRvbWluYW50LWJhc2VsaW5lPSJtaWRkbGUiPuaWsO+8mlAycE1vZGVsU2Vhbe+8iOaOpeWFpe+8iTwvdGV4dD48dGV4dCB4PSI1OTAuMCIgeT0iMjU2LjAiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sQ29uc29sYXMsbW9ub3NwYWNlIiBmb250LXNpemU9IjgiIGZpbGw9IiM2YjcyODAiIHRleHQtYW5jaG9yPSJtaWRkbGUiPm5tX2NsaWVudC5jYWxsKHByb3ZpZGVyLDwvdGV4dD48dGV4dCB4PSI1OTAuMCIgeT0iMjY4LjAiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sQ29uc29sYXMsbW9ub3NwYWNlIiBmb250LXNpemU9IjgiIGZpbGw9IiM2YjcyODAiIHRleHQtYW5jaG9yPSJtaWRkbGUiPiJtb2RlbC5pbmZlciIsIG9wZW5haV9qc29uKTwvdGV4dD48dGV4dCB4PSI1OTAuMCIgeT0iMjgwLjAiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sQ29uc29sYXMsbW9ub3NwYWNlIiBmb250LXNpemU9IjgiIGZpbGw9IiM2YjcyODAiIHRleHQtYW5jaG9yPSJtaWRkbGUiPj0g5aSN55SoIMKnMTMgT3BlbkFJIOWFvOWuuei9veiNtzwvdGV4dD48dGV4dCB4PSIyNzAiIHk9IjMxMCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI4LjgiIGZpbGw9IiNkMTQ5NWIiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI0MDAiPuKclyDkuK3lv4PljJbjgIHopoHphY0gQVBJIEtleTwvdGV4dD48dGV4dCB4PSI1OTAiIHk9IjMxMCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI4LjgiIGZpbGw9IiMxZmIxODIiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI0MDAiPuKckyDotbDnvZHnirbjgIHlj5HnjrAr5o6I5p2DK+iuoemHjzwvdGV4dD48cmVjdCB4PSIxMjAiIHk9IjMzMCIgd2lkdGg9IjYyMCIgaGVpZ2h0PSI0NCIgcng9IjEwIiBmaWxsPSIjZjRmN2Y2IiBzdHJva2U9IiNkNWRiZDgiIHN0cm9rZS13aWR0aD0iMS4zIi8+PHRleHQgeD0iNDMwIiB5PSIzNTAiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iOS40IiBmaWxsPSIjMTQxYTFmIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIj7lpI3nlKjnjrDmiJDnuq/lh73mlbDvvJpidWlsZF9yZXF1ZXN0X2JvZHkoY3R4KeKGkkpTT07jgIFwYXJzZV9yZXNwb25zZShKU09OKeKGkk1vZGVsUmVzcG9uc2U8L3RleHQ+PHRleHQgeD0iNDMwIiB5PSIzNjYiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iOC44IiBmaWxsPSIjNmI3MjgwIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNDAwIj5QMnBNb2RlbFNlYW0g5Y+q5piv5oqK44CMSFRUUCDlj5HpgIHjgI3mjaLmiJDjgIxubV9jbGllbnQuY2FsbCDlj5HpgIHjgI3vvIznvJYv6Kej56CB54Wn55So4oCU4oCU5pS55Yqo57qm5LiA5Liq5paH5Lu2PC90ZXh0Pjwvc3ZnPg==" />

**内核契约**（`cmx-agent-core/src/model.rs`，已核对）：
```
pub trait ModelSeam: Send + Sync {
    async fn complete(&self, ctx: &ModelContext) -> Result<ModelResponse, ModelError>;
}
// ModelContext { system, messages: Vec<ModelMessage>, tools: Vec<ToolSpec> }
// ModelResponse { content, tool_calls: Vec<ToolCall>, usage: Option<ModelUsage> }
```
回合循环（`agent.rs`）只持 `Arc<dyn ModelSeam>`，**不关心底层**。现有实现有 `OpenAiCompatModel`(HTTP)、`DemoModel`(离线)、`ModelSlot`(热插拔)、`MockModel`(测试)。

**新实现 `P2pModelSeam`**（约一个文件）：
```
impl ModelSeam for P2pModelSeam {
    async fn complete(&self, ctx: &ModelContext) -> Result<ModelResponse, ModelError> {
        let body = build_request_body(ctx);                  // 复用 cmx 现成纯函数 → OpenAI JSON
        let json = self.session
            .call(self.provider_id, "model.infer",           // nanomesh P2P 调用
                  Any{ type_url:"openai.chat.v1", value: body })
            .await?;                                          // → CommandResult
        parse_response(&json)                                 // 复用 cmx 现成纯函数 → ModelResponse
    }
}
```
- **复用**：`build_request_body(ctx)→JSON` 与 `parse_response(JSON)→ModelResponse` 是 cmx-agent-model 里的**纯函数**，照搬即可——P2pModelSeam 只把"HTTP 发送"换成"`nm_client.call` 发送"。
- **provider 选择**：`provider_id` 由 nanomesh 目录发现 + 撮合得到（`directory_query(kind_prefix="model.")`，见 P2P 方案 §3）；不再是本地配置的 base_url/API Key。
- **工具调用保真**：`ModelContext.tools` → OpenAI `tools` 字段；`ModelResponse.tool_calls` ← OpenAI `tool_calls`。OpenAI 兼容两端一致，工具调用链路不变。

> 结果：cmx-agent 的**工具路由、守卫、会话日志、MCP、回合循环**全部原样复用；换掉的只有"模型从哪来"。

## 3. 一个智能体回合（端到端）

<img alt="一个智能体回合端到端时序" width="860" src="data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA4NjAgMzg4IiB3aWR0aD0iODYwIiBoZWlnaHQ9IjM4OCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiI+PHJlY3Qgd2lkdGg9Ijg2MCIgaGVpZ2h0PSIzODgiIGZpbGw9IiNmZmZmZmYiLz48ZGVmcz48bWFya2VyIGlkPSJhci1tIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiM2YjcyODAiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1hIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiMxZmIxODIiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1iIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiMyZjZmZGQiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1yIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiNkMTQ5NWIiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1wIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiM3YzZjZjAiLz48L21hcmtlcj48bWFya2VyIGlkPSJhci1vIiB2aWV3Qm94PSIwIDAgMTAgMTAiIHJlZlg9IjkiIHJlZlk9IjUiIG1hcmtlcldpZHRoPSI3IiBtYXJrZXJIZWlnaHQ9IjciIG9yaWVudD0iYXV0by1zdGFydC1yZXZlcnNlIj48cGF0aCBkPSJNMCwwIEwxMCw1IEwwLDEwIHoiIGZpbGw9IiNkOThhMWYiLz48L21hcmtlcj48L2RlZnM+PHRleHQgeD0iNDMwIiB5PSIyOCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxNS41IiBmaWxsPSIjMTQxYTFmIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIj7lm74gMyDCtyDkuIDkuKrmmbrog73kvZPlm57lkIjvvJrop6blj5Eg4oaSIOWGheaguCDihpIgUDJQIOaooeWeiyDihpIg5bel5YW3IOKGkiDlm57lpI08L3RleHQ+PGxpbmUgeDE9IjEyMCIgeTE9IjY0IiB4Mj0iMTIwIiB5Mj0iMzcyIiBzdHJva2U9IiNkNWRiZDgiIHN0cm9rZS13aWR0aD0iMS4zIi8+PHRleHQgeD0iMTIwIiB5PSI1OCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMC4yIiBmaWxsPSIjMTQxYTFmIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIj7nlKjmiLcvSU08L3RleHQ+PGxpbmUgeDE9IjMzMCIgeTE9IjY0IiB4Mj0iMzMwIiB5Mj0iMzcyIiBzdHJva2U9IiNkNWRiZDgiIHN0cm9rZS13aWR0aD0iMS4zIi8+PHRleHQgeD0iMzMwIiB5PSI1OCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMC4yIiBmaWxsPSIjMTQxYTFmIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIj5ubS1hZ2VudCDlhoXmoLg8L3RleHQ+PGxpbmUgeDE9IjU2MCIgeTE9IjY0IiB4Mj0iNTYwIiB5Mj0iMzcyIiBzdHJva2U9IiNkNWRiZDgiIHN0cm9rZS13aWR0aD0iMS4zIi8+PHRleHQgeD0iNTYwIiB5PSI1OCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMC4yIiBmaWxsPSIjMTQxYTFmIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNzAwIj5QMlAg5qih5Z6LKHByb3ZpZGVyKTwvdGV4dD48bGluZSB4MT0iNzcwIiB5MT0iNjQiIHgyPSI3NzAiIHkyPSIzNzIiIHN0cm9rZT0iI2Q1ZGJkOCIgc3Ryb2tlLXdpZHRoPSIxLjMiLz48dGV4dCB4PSI3NzAiIHk9IjU4IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEwLjIiIGZpbGw9IiMxNDFhMWYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI3MDAiPuW3peWFty9NQ1A8L3RleHQ+PGxpbmUgeDE9IjEyMCIgeTE9IjkyIiB4Mj0iMzMwIiB5Mj0iOTIiIHN0cm9rZT0iIzJmNmZkZCIgc3Ryb2tlLXdpZHRoPSIxLjYiIG1hcmtlci1lbmQ9InVybCgjYXItbSkiLz48dGV4dCB4PSIyMjUuMCIgeT0iODYiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iOC40IiBmaWxsPSIjMmY2ZmRkIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNDAwIj7lj5Hmtojmga8gLyDmjIfku6Q8L3RleHQ+PHRleHQgeD0iMzMwIiB5PSIxMTIiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iNy44IiBmaWxsPSIjNmI3MjgwIiB0ZXh0LWFuY2hvcj0ic3RhcnQiIGZvbnQtd2VpZ2h0PSI0MDAiPu+8iElNIOaUtua2iOaBr+KGkui3keWbnuWQiO+8jOaIliBBZ2VudCDpnaLmnb/op6blj5HvvIk8L3RleHQ+PGxpbmUgeDE9IjMzMCIgeTE9IjEzMCIgeDI9IjU2MCIgeTI9IjEzMCIgc3Ryb2tlPSIjMmY2ZmRkIiBzdHJva2Utd2lkdGg9IjEuNiIgbWFya2VyLWVuZD0idXJsKCNhci1iKSIvPjx0ZXh0IHg9IjQ0NS4wIiB5PSIxMjQiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iOC40IiBmaWxsPSIjMmY2ZmRkIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNDAwIj5jb21wbGV0ZShjdHgpOiBPcGVuQUkgSlNPTjwvdGV4dD48dGV4dCB4PSI1NjAiIHk9IjE1MCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI3LjgiIGZpbGw9IiM2YjcyODAiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI0MDAiPnByb3ZpZGVyIOe7j+ebruW9leWPkeeOsCtHcmFudCDmjojmnYM8L3RleHQ+PGxpbmUgeDE9IjU2MCIgeTE9IjE3NiIgeDI9IjMzMCIgeTI9IjE3NiIgc3Ryb2tlPSIjMmY2ZmRkIiBzdHJva2Utd2lkdGg9IjEuNiIgbWFya2VyLWVuZD0idXJsKCNhci1iKSIvPjx0ZXh0IHg9IjQ0NS4wIiB5PSIxNzAiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLCdTZWdvZSBVSScsUm9ib3RvLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iOC40IiBmaWxsPSIjMmY2ZmRkIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXdlaWdodD0iNDAwIj5Nb2RlbFJlc3BvbnNle3Rvb2xfY2FsbHN9PC90ZXh0PjxsaW5lIHgxPSIzMzAiIHkxPSIyMTAiIHgyPSI3NzAiIHkyPSIyMTAiIHN0cm9rZT0iI2Q5OGExZiIgc3Ryb2tlLXdpZHRoPSIxLjYiIG1hcmtlci1lbmQ9InVybCgjYXItbykiLz48dGV4dCB4PSI1NTAuMCIgeT0iMjA0IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjguNCIgZmlsbD0iI2Q5OGExZiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjQwMCI+5omn6KGM5bel5YW36LCD55So77yI5YaF572uL01DUO+8iTwvdGV4dD48bGluZSB4MT0iNzcwIiB5MT0iMjQwIiB4Mj0iMzMwIiB5Mj0iMjQwIiBzdHJva2U9IiNkOThhMWYiIHN0cm9rZS13aWR0aD0iMS42IiBtYXJrZXItZW5kPSJ1cmwoI2FyLW8pIi8+PHRleHQgeD0iNTUwLjAiIHk9IjIzNCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI4LjQiIGZpbGw9IiNkOThhMWYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI0MDAiPuW3peWFt+e7k+aenDwvdGV4dD48bGluZSB4MT0iMzMwIiB5MT0iMjc2IiB4Mj0iNTYwIiB5Mj0iMjc2IiBzdHJva2U9IiMyZjZmZGQiIHN0cm9rZS13aWR0aD0iMS42IiBtYXJrZXItZW5kPSJ1cmwoI2FyLWIpIi8+PHRleHQgeD0iNDQ1LjAiIHk9IjI3MCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI4LjQiIGZpbGw9IiMyZjZmZGQiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI0MDAiPmNvbXBsZXRlKGN0eCvlt6Xlhbfnu5PmnpwpIOS6jOasoTwvdGV4dD48bGluZSB4MT0iNTYwIiB5MT0iMzEwIiB4Mj0iMzMwIiB5Mj0iMzEwIiBzdHJva2U9IiMyZjZmZGQiIHN0cm9rZS13aWR0aD0iMS42IiBtYXJrZXItZW5kPSJ1cmwoI2FyLWIpIi8+PHRleHQgeD0iNDQ1LjAiIHk9IjMwNCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sJ1NlZ29lIFVJJyxSb2JvdG8sQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSI4LjQiIGZpbGw9IiMyZjZmZGQiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI0MDAiPuacgOe7iCBjb250ZW50PC90ZXh0PjxsaW5lIHgxPSIzMzAiIHkxPSIzNDQiIHgyPSIxMjAiIHkyPSIzNDQiIHN0cm9rZT0iIzFmYjE4MiIgc3Ryb2tlLXdpZHRoPSIxLjYiIG1hcmtlci1lbmQ9InVybCgjYXItYSkiLz48dGV4dCB4PSIyMjUuMCIgeT0iMzM4IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSwnU2Vnb2UgVUknLFJvYm90byxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjguNCIgZmlsbD0iIzFmYjE4MiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjQwMCI+5Zue5aSN77yISU0g5raI5oGvIC8g5Lya6K+d5LqL5Lu277yJPC90ZXh0PjxyZWN0IHg9IjYwIiB5PSIzODQiIHdpZHRoPSI3NDAiIGhlaWdodD0iMCIgLz48L3N2Zz4=" />

1. **触发**：nanomesh IM 收到一条发给某 agent 的消息（或桌面壳 Agent 面板直接发指令）→ 驱动内核跑一个回合。
2. **模型调用**：内核 `complete(ctx)` → `P2pModelSeam` → `nm_client.call(provider, "model.infer", openai_json)`；provider 经目录发现 + Grant 授权。
3. **工具**：模型返回 `tool_calls` → 内核路由到内置工具 / MCP server 执行 → 工具结果回灌 → 二次 `complete`。
4. **回复**：最终 content 作为 **nanomesh IM 消息**回发（或写入会话事件供壳渲染）。

**要点**：整个回合中，模型那一步走 P2P，其余（工具/守卫/日志）是**被引用的** cmx 内核原样跑；"IM 触发 + IM 回复"用 nanomesh 的消息机制。

## 4. 内核引用机制（完整引用、随上游迭代）

**目标**：nanomesh 不 fork cmx-agent **内核**；上游持续迭代，nanomesh 升一个版本号即获新功能；内核只有**一份**代码（在 cmx-agent 仓库）。前门/前端是 nanomesh 自己的，不在此列（见 §4 末与 §6）。

**做法**：nanomesh 新建瘦前门 crate `crates/nm-agent-bridge`，以 **Git 依赖**完整引用 cmx-agent 的内核三件套（Cargo 当普通外部依赖拉取编译）：
```toml
# crates/nm-agent-bridge/Cargo.toml
[dependencies]
cmx-agent-core  = { git = "https://github.com/warpdrivelabs/cmx-agent.git", tag = "v0.1.0" }
cmx-agent-tools = { git = "https://github.com/warpdrivelabs/cmx-agent.git", tag = "v0.1.0" }
cmx-agent-mcp   = { git = "https://github.com/warpdrivelabs/cmx-agent.git", tag = "v0.1.0" }
# 不引用 cmx-agent-model（HTTP 直连）/ cmx-agent-im（遥控桩）
```
- **两个 workspace 不合并**：Cargo 不允许一个 crate 同属两个 workspace；cmx-agent 的 crate 用 `xxx.workspace = true` 继承其**自己**根的 `[workspace.package]`/`[workspace.dependencies]`。作为**外部 Git 依赖**引入时，它仍在自己的上下文里解析，互不干扰——这正是"引用而非并入"。
- **版本钉扎**：用 `tag`/`rev` 钉到确定提交（可复现构建）；上游发新版 → nanomesh 改 tag + `cargo update -p cmx-agent-core` 即升级。用 `branch` 可持续跟踪但牺牲可复现，**推荐 tag/rev**。
- **按 git 依赖编译一次即验证**：core 零网络 + 依赖通用库，`cargo build -p nm-agent-bridge` 能过即证明引用链成立。
- **升级面集中在前门**：只要上游**不改 `ModelSeam`/`Tool` 等 public trait 签名**，nanomesh 改 tag 即吸收新功能、零改；若上游改了 public API，只有 `nm-agent-bridge`（前门适配）按编译错误跟改——面小且集中，内核逻辑无须碰。
- **离线/内网**：Git 依赖需能访问 cmx-agent 仓库（GitHub/gitee）。离线构建环境可用 `[patch]` 指向本地 checkout，或配 Cargo 的 git 缓存/vendoring；不改方案本质。

> **双策略小结**：**内核（core/tools/mcp）= 完整引用**（一份代码、改 tag 升级、随上游迭代、不漂移）；**前门 façade（nm-agent-bridge）+ 前端 UI = nanomesh 自写/复制后独立迭代**（上游前门耦合 HTTP/门户/market 不宜引用，前端 JS 无法 Cargo 引用）。两者正好对应用户要求：Rust agent 功能完整引用、前端复制后独立发展。

## 5. 前门层为何不能引用：中心化依赖冲突（已核对代码）

**结论**：`cmx-agent-app`（前门 façade）及其门户/market 外壳**依赖一整套传统中心化后端服务**，与 nanomesh「去中心、无中心服务器」的立身之本**根本冲突**——这才是前门层必须 nanomesh 自写、绝不引用的真正原因（不是嫌它重）。可完整引用的 `core/tools/mcp` 则**不碰**这些，是干净的 agent 功能本体。

**求证（cmx-agent-app/src）**：

| 外壳组件 | 依赖的传统后端 | 代码证据 |
|---|---|---|
| 门户登录/鉴权 `auth.rs` | 中心化账号服务 `POST {base}/api/auth/login` → `access_token`/`refresh_token` → `/api/auth/me` | 默认 `https://cmx.pansoft.com`（本机开发 `:8091`/`:8080`） |
| 连接器 `client.rs` | 带 `Authorization: Bearer` + `X-Tenant` 调一批 cmx 服务 | `CmxServiceClient{base_url, token}`，多租户 |
| market `market.rs` | 远端市场/数据源 HTTP（拉技能/MCP 目录） | `<数据根>/market.json` 登记远端 URL 源（有 official.json 离线保底） |
| 模型 `cmx-agent-model` | 中心化模型 API + 本地 API Key | `reqwest` POST `{base}/chat/completions` |

**与 nanomesh 的冲突（为何不能拉进来）**：

| cmx 前门外壳 | 依赖 | nanomesh 的等价物 | 性质 |
|---|---|---|---|
| 门户登录 `/api/auth/login` | 中心化 HTTP 账号服务（cmx.pansoft.com） | **去中心身份**：Ed25519 公钥 + 本地保险库，无中心账号服务 | **根本冲突** |
| Bearer Token / X-Tenant | 中心化多租户鉴权 | **P2P `Grant`**（签名、离线可验证、委托授权） | 不兼容 |
| market 远端源 | 中心化市场 HTTP | （未来）P2P 发现 / `agent.tool` 实体 | 不兼容 |
| 模型 HTTP 直连 | 中心化模型 API + Key | **P2P `model.*` 能力** | 要替换 |

> 若把前门层引用进来，去中心的 nanomesh 会被塞进一个中心化依赖——连登录都要连 `cmx.pansoft.com`，本末倒置。

**nm-agent-bridge 的替换映射（前门自写要做的事）**：把上述每个「中心化接入点」换成 nanomesh 的去中心等价物——
- 门户登录 → **nanomesh 已有的身份/保险库**（复用现成，不连门户）；
- Bearer/租户鉴权 → **P2P `Grant`**；
- 模型 HTTP → **`P2pModelSeam`**（走 P2P `model.infer`）；
- market → **先不要**；未来用 P2P 发现 / `agent.tool` 实体替代。

**而被完整引用的 `core/tools/mcp` 为何干净**：已核对其依赖仅 serde/tokio/async-trait/chrono/uuid/tracing，**无 reqwest、无门户、无 market、无具体模型**——它们只做"回合循环 + 工具路由 + 守卫 + MCP"，即 agent 的功能本体，零中心化依赖，故可放心完整引用。

## 6. 智能体本身也是一个 P2P 实体（双向价值）

集成后 nanomesh 里的 agent 不只是"本地助手"——它天然可注册为 `kind=agent.*` 的 P2P 实体（P2P 方案 §2/§8）：
- **对内消费**：agent 用 `P2pModelSeam` 调网络里的 `model.llm`。
- **对外提供**：agent 自身以 `agent.tool`/`agent.assistant` 注册，**被别的实体 `call`**（成为网络里一个可被发现/调用的"技能"）。
- MCP 工具同理：接入的 MCP server 工具可（可选）作为 `agent.tool` 对联邦暴露。

这让"智能体 + 算力/模型网络"闭环：人/设备/机器人既用模型，也用别人的 agent，也贡献自己的 agent。

## 7. 目录结构（建议）

```
imspace/
  crates/
    nm-agent-bridge/   ← 新：瘦适配 crate
      Cargo.toml       ·   [dependencies] git 引用 cmx-agent-core/tools/mcp（不复制）
      src/seam.rs      ·   P2pModelSeam：impl cmx_agent_core::ModelSeam（走 P2P）
      src/turn.rs      ·   驱动 cmx 回合 + 把输入/输出接 nanomesh IM / 前门命令
  clients/app/
    ui/js/agent.js     ← 复制自 cmx-agent-shell 的 agent 面板，改写接 nanomesh 前门后独立迭代
    ui/js/mcp.js       ← 复制 MCP 管理面板（可选），独立迭代
    src-tauri/…        ← 新增 agent 前门命令（跑回合 / 列 agent / 配 MCP）
```
- **完整引用（不在本仓、单一源）**：`cmx-agent-core`/`-tools`/`-mcp` 以 Git 依赖拉取，源码仍在 cmx-agent 仓库，随上游迭代。
- **不引用**：`cmx-agent-app`（前门层，耦合 HTTP/门户/market → 自写替代）、`cmx-agent-model`（HTTP 直连，被 P2pModelSeam 取代）、`cmx-agent-im`（遥控桩）、`cmx-agent-net`（如需 web 工具，优先看能否单独引用，否则另议）。
- **前门自写（借蓝本）**：`nm-agent-bridge` 参照 `cmx-agent-app` 的命令协议形状（`AppRequest`/`AgentApp::send`）重写，模型走 P2P、会话走 nanomesh、无 market/门户。
- **前端复制后独立迭代**：从 cmx-agent-shell 复制 agent 面板功能进 nanomesh 壳，改写数据接口后**各自演进**（不保持与上游同步）。

## 8. 与现有代码集成点

| 用途 | 来源 | 落点（nanomesh 侧） |
|---|---|---|
| 智能体内核（回合/工具/守卫/日志） | **引用** cmx-agent-core（`trait ModelSeam` 解耦，git dep） | `nm-agent-bridge` 依赖它 |
| OpenAI 编/解码纯函数 | cmx-agent-model 的 `build_request_body`/`parse_response`（见下注） | `P2pModelSeam` 内复用 |
| P2P 模型调用 | nanomesh `Session::call(provider,"model.infer",Any)` | `P2pModelSeam::complete`（新代码） |
| provider 发现/撮合 | nanomesh `directory_query(kind_prefix="model.")` | agent 启动/每回合选 provider |
| 授权/计量 | nanomesh `Grant` + §13.6 计量记录 | 调用前出示 Grant |
| 工具 / MCP | **引用** cmx-agent-tools / cmx-agent-mcp（git dep） | `nm-agent-bridge` 依赖它们 |
| IM 触发 + 回复 | nanomesh 消息事件（已跑通） | agent 回合的输入/输出 |
| UI 面板 | **复制** cmx-agent-shell 的 agent 面板后独立迭代 | `clients/app/ui/js/agent.js`（改接 nanomesh 前门） |
| 前门 façade/命令协议 | 借 cmx-agent-app 的 `AppRequest`/`send` 蓝本（不引用） | `nm-agent-bridge`（自写，模型走 P2P、会话走 nanomesh） |

> **编解码函数的取用**：`build_request_body`/`parse_response` 在 `cmx-agent-model`（HTTP crate）里。为不引用该 HTTP crate，有两法：(i) 若上游把这两个**纯函数**提到 `cmx-agent-core` 或独立小 crate，则直接引用最干净（建议向上游提此拆分）；(ii) 否则 ModelContext↔OpenAI JSON 的编解码**在 `nm-agent-bridge` 侧按 §13 规范自写**（逻辑简单、与 OpenAI 格式一一对应）。二者都不复制 HTTP 代码。
>
> **依赖 P2P 方案落地**：本方案的"模型走 P2P"依赖 `COMPUTE_MODEL_P2P_DESIGN.md` 的 **C1（provider 运行时）+ C2（消费便捷层）** 先就位——`P2pModelSeam` 正是 C2 的一个消费者。

## 9. 分阶段里程碑

| 里程碑 | 内容 | 验证 |
|---|---|---|
| **（前置）** | P2P 模型 C1+C2：网络里有一个可调用的 `model.llm` provider | 一行 `call` 跨节点推理成功 |
| **A0 引用打通** | 建 `nm-agent-bridge`，git 依赖 cmx-agent-core/tools/mcp，编译通过 | `cargo build -p nm-agent-bridge` 绿 |
| **A1 离线回合** | 用 cmx 的 `DemoModel`（或自写 Mock）经引用内核跑通一个回合 | 一个离线回合通过 |
| **A2 P2pModelSeam** | 实现 `complete()` 走 `nm_client.call`；编解码按 §13（或复用上游纯函数） | agent 回合经 P2P 模型产出答复 |
| **A3 工具 + MCP** | 工具调用链路跑通（内置工具 + 一个 MCP server，均经引用 crate） | 带工具调用的回合端到端 |
| **A4 IM 驱动** | nanomesh IM 收消息 → 驱动 agent 回合 → IM 回复 | 给 agent 发消息得到智能回复 |
| **A5 UI 面板** | 从 cmx-agent-shell **复制** agent 面板进 nanomesh 壳、改接前门后独立迭代 | 壳内完成一次智能体对话 |
| **A6 agent 即实体** | agent 注册 `kind=agent.*`，可被别的实体 `call` | 跨节点调用一个 agent |
| 后续 | 流式回合 · agent 间协作 · 计量计费 · 升级演练（改 tag 吸收上游新功能） | — |

## 10. 风险与注意

1. **依赖 P2P 模型层先行**：无 C1/C2 则 agent 回合无模型可用（可先用 `DemoModel` 离线回退过渡）。
2. **上游 API 稳定性（引用式的核心风险）**：nanomesh 升级吸收上游新功能时，若上游改了 `ModelSeam`/`Tool` 等 **public trait 签名**，适配层需跟改。缓解：钉 `tag`/`rev`、升级走演练、必要时与上游约定公共 API 的兼容策略。
3. **编解码函数不在 core**：`build_request_body`/`parse_response` 目前在 HTTP crate；按 §7 注，优先推上游拆为可引用纯函数，否则 bridge 侧自写（不复制 HTTP 代码）。
4. **离线/内网构建**：Git 依赖需可访问 cmx-agent 仓库；受限环境用 `[patch]`/vendoring（§4）。
5. **流式**：cmx-agent 回合若用流式输出，需对接 P2P 方案 §13.5（uni 流）；首期可先非流式。
6. **工具安全**：引用来的工具（尤其 `fs_read`/MCP/未来 shell）在 nanomesh 语境下需复核沙箱与授权边界。
7. **许可证/来源**：cmx-agent 为 Apache-2.0，Git 依赖引用合规；仍在 nanomesh 注明依赖来源与许可。
8. **会话存储**：cmx-agent 用会话 JSONL；nanomesh 桌面已用 SQLite——agent 会话落库选一套（建议并入 SQLite，由 bridge 适配）。
9. **前端独立迭代的代价**：前端 agent 面板复制后与上游分叉——上游 UI 新功能不会自动流入，需人工挑拣回补；这是用户明确选择（前端独立发展）的预期代价，与"内核随上游迭代"相对。
10. **前门自写的覆盖面**：`nm-agent-bridge` 要覆盖 agent 所需的前门命令子集（CreateSession/Send/GetEvents/Cancel/TurnActive/ListToolsSpecs/ListAgents/SaveAgent/SetModel…），但**不做** market/workspace/门户那些上游外壳命令。

## 11. 待决策（评审定）

1. **引用钉扎策略**：`tag`（可复现，推荐）/ `rev` / `branch`（跟踪但漂移）？升级节奏？
2. **编解码来源**：推动上游把 `build_request_body`/`parse_response` 拆为可引用纯函数，还是 bridge 侧自写？
3. **最小闭环**：A0+A1+A2（引用打通→离线回合→P2P 模型）优先？
4. **agent 会话存储**：复用 cmx 的 JSONL，还是并入 nanomesh 的 SQLite？
5. **是否同时让 agent 作为 `agent.*` P2P 实体对外提供**（A6），还是先只做"本地助手消费模型"？
6. **web_fetch/search（cmx-agent-net）**：纳入首期工具集？（优先走引用而非复制）

---

**一句话评审结论**：可行，按双策略落地——**Rust agent 内核（core/tools/mcp）完整 Git 依赖引用**（零网络、Apache-2.0、工具链兼容，单一源、随上游迭代）；**前门 façade + 前端 UI 由 nanomesh 自写/复制后独立迭代**（上游前门耦合 HTTP/门户/market 不宜引用、前端 JS 无法 Cargo 引用）；**唯一模型改造是 `P2pModelSeam`** 把接入换成 P2P `model.infer`。建议以 **A0 引用打通 + A1 离线回合 + A2 P2pModelSeam** 做最小闭环，前置依赖是 P2P 模型方案的 C1+C2。

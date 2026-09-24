# nmspace 前端（clients/）

一套 **React + TypeScript** UI，覆盖 **web / desktop / iOS / android**。

## 关键设计：一个接口，两条接入路径

前端只依赖 `src/core/transport.ts` 的 `CoreTransport` 接口；`src/core/index.ts` 运行时探测环境选择适配器：

| 目标 | 渲染 | 接入核心的方式 |
|---|---|---|
| desktop / iOS / android | Tauri WebView | `TauriTransport` → Tauri IPC → 进程内 Rust `nm-client`(iroh) |
| web(浏览器) | 同一 React 产物(PWA) | `WebGatewayTransport` → WebSocket/WebTransport → `nm-gatewayd` |

因此**业务界面与状态只写一次**，四端复用。

## 目录

```
clients/app/
├── src/
│   ├── core/            # CoreTransport 抽象 + tauri/web 两适配器 + 运行时选择
│   ├── state/           # 极简状态层（可换 zustand/jotai）
│   ├── screens/         # 界面（ChatScreen 占位）
│   ├── App.tsx / main.tsx / styles.css
│   └── vite-env.d.ts
├── index.html · vite.config.ts · tsconfig*.json · package.json
└── src-tauri/           # Tauri 2 原生壳（独立 crate，内嵌 nm-client）
```

## 运行

```bash
npm install

# 1) Web（先在另一终端跑 `cargo run -p nm-gatewayd`）
npm run dev                 # http://localhost:1420
npm run build               # 产出 dist/ → 部署为 PWA

# 2) 桌面原生（内嵌 iroh，无需网关）
npm run tauri dev
npm run tauri build

# 3) 移动端（需 Xcode / Android SDK+NDK）
npm run tauri ios init && npm run tauri ios dev
npm run tauri android init && npm run tauri android dev
```

## 注意

- `src-tauri` 为最小骨架。若遇 Tauri 2.x 配置/能力(capabilities)细节，建议先用
  `npm create tauri-app@latest` 生成官方校验基线，再并入本仓前端与 `nm-client` 桥接命令。
- 应用图标：用 `npm run tauri icon <png>` 生成 `src-tauri/icons/`。
- Web 端网关地址通过环境变量 `VITE_GATEWAY_URL` 覆盖（默认 `ws://localhost:8088/ws`）。

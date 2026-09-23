# imspace

基于 **iroh** 的去中心化即时通讯系统（方案 B）。支持 **P2P 消息、群组、频道订阅、多服务器联邦**；
前端一套 UI 覆盖 **web / desktop / iOS / android**。

> 完整设计见 [`docs/PLAN_B_iroh_decentralized_im.md`](docs/PLAN_B_iroh_decentralized_im.md)；
> 旧工程分析见 [`docs/ANALYSIS.md`](docs/ANALYSIS.md)；旧代码归档在 `old_space/`。

## 目录结构

```
imspace/
├── crates/              # 后端 Rust 库（cargo workspace）
│   ├── im-proto         # 线协议：ID / gram / 编解码（prost 生成）
│   ├── im-core          # 领域模型 + 核心 trait（Deliver / Store）
│   ├── im-transport     # iroh 端点：dial-by-key / 流 / 身份
│   ├── im-store         # 存储抽象 + redb 后端
│   ├── im-gossip        # 频道 pub/sub（iroh-gossip）
│   ├── im-federation    # 跨节点状态（iroh-docs）+ 路由/重试/防环
│   ├── im-crypto        # 身份 / 校验 / 口令；可选 E2E(openmls)
│   ├── im-entity        # 可扩展实体类型体系（EntityKind trait + 内置类型）
│   ├── im-node          # 服务端节点（装配各服务）
│   ├── im-client        # 客户端 SDK（原生端内嵌 / 网关复用）
│   └── im-gateway       # 浏览器网关：WebTransport/WS ↔ iroh
├── bin/
│   ├── imd              # 节点守护进程
│   ├── im-gatewayd      # 网关守护进程
│   ├── im-echo          # 回声机器人（测试用：收到消息原样回发）
│   └── im-agent         # 无界面实体守护进程示例（compute.inference）
├── clients/app/         # 前端：React+TS（web PWA）+ Tauri(desktop/iOS/android)
├── xtask/               # 开发者任务编排
├── docs/                # 设计文档
└── old_space/           # 旧工程归档（勿动）
```

## 前置依赖

- **Rust** 1.85+（`rustup`）
- **protoc**（Protocol Buffers 编译器）：`im-proto` 构建时需要。
  - macOS：`brew install protobuf`
  - Debian/Ubuntu：`apt install protobuf-compiler`
  - Fedora：`dnf install protobuf-compiler`
  - 其他：从 [github.com/protocolbuffers/protobuf/releases](https://github.com/protocolbuffers/protobuf/releases) 下载二进制，加入 `PATH`
- **Node.js** 18+（前端，可选）

## 快速开始

```bash
# 后端（node + gateway）
cargo check --workspace
cargo run -p imd

# 测试辅助（与 imd 配合）
cargo run -p im-echo -- --node '<imd 打印的 IM_NODE_ADDR JSON>'
cargo run -p im-agent -- --node '<imd 打印的 IM_NODE_ADDR JSON>'

# 前端（详见 clients/README.md）
cd clients/app && npm install
npm run dev            # 浏览器（连 im-gatewayd）
npm run tauri dev      # 桌面原生（内嵌 iroh）
```

## 架构要点

- **原生端**(desktop/iOS/android)：Tauri 外壳 + 进程内 Rust `im-client`(iroh)，App 即真实节点。
- **Web 端**：同一套 React UI 构建为 PWA，经 `im-gatewayd`(WebTransport/WS) 接入（浏览器不能直跑 QUIC）。
- **接入抽象**：前端 `src/core` 里 `CoreTransport` 一个接口 + 两个适配器（Tauri IPC / 网关），界面写一次跑四端。
- **去中心**：节点按 Ed25519 公钥互联(dial-by-key)，无中心 S2S 网关；频道走 iroh-gossip，联邦状态走 iroh-docs。

## 状态

脚手架阶段：目录/crate 图/依赖/前端接入抽象已就位，业务逻辑为 TODO（对应 PLAN_B 各节 / 里程碑 M0–M3）。

# nmspace

基于 **iroh** 的去中心化网格系统（方案 B）。支持 **P2P 消息、群组、频道订阅、多服务器联邦**；
前端一套 UI 覆盖 **web / desktop / iOS / android**。

> 完整设计见 [`docs/PLAN_B_iroh_decentralized_im.md`](docs/PLAN_B_iroh_decentralized_im.md)；
> 旧工程分析见 [`docs/ANALYSIS.md`](docs/ANALYSIS.md)；旧代码归档在 `old_space/`。

## 目录结构

```
nmspace/
├── crates/              # 后端 Rust 库（cargo workspace）
│   ├── nm-proto         # 线协议：ID / gram / 编解码（prost 生成）
│   ├── nm-core          # 领域模型 + 核心 trait（Deliver / Store）
│   ├── nm-transport     # iroh 端点：dial-by-key / 流 / 身份
│   ├── nm-store         # 存储抽象 + redb 后端
│   ├── nm-gossip        # 频道 pub/sub（iroh-gossip）
│   ├── nm-federation    # 跨节点状态（iroh-docs）+ 路由/重试/防环
│   ├── nm-crypto        # 身份 / 校验 / 口令；可选 E2E(openmls)
│   ├── nm-entity        # 可扩展实体类型体系（EntityKind trait + 内置类型）
│   ├── nm-node          # 服务端节点（装配各服务）
│   ├── nm-client        # 客户端 SDK（原生端内嵌 / 网关复用）
│   └── nm-gateway       # 浏览器网关：WebTransport/WS ↔ iroh
├── bin/
│   ├── nmd              # 节点守护进程
│   ├── nm-gatewayd      # 网关守护进程
│   ├── nm-echo          # 回声机器人（测试用：收到消息原样回发）
│   └── nm-agent         # 无界面实体守护进程示例（compute.inference）
├── clients/app/         # 前端：React+TS（web PWA）+ Tauri(desktop/iOS/android)
├── xtask/               # 开发者任务编排
├── docs/                # 设计文档
└── old_space/           # 旧工程归档（勿动）
```

## 前置依赖

- **Rust** 1.85+（`rustup`）
- **protoc**（Protocol Buffers 编译器）：`nm-proto` 构建时需要。
  - macOS：`brew install protobuf`
  - Debian/Ubuntu：`apt install protobuf-compiler`
  - Fedora：`dnf install protobuf-compiler`
  - 其他：从 [github.com/protocolbuffers/protobuf/releases](https://github.com/protocolbuffers/protobuf/releases) 下载二进制，加入 `PATH`
- **Node.js** 18+（前端，可选）

## 快速开始

```bash
# 后端（node + gateway）
cargo check --workspace
cargo run -p nmd

# 测试辅助（与 nmd 配合）
cargo run -p nm-echo -- --node '<nmd 打印的 NM_NODE_ADDR JSON>'
cargo run -p nm-agent -- --node '<nmd 打印的 NM_NODE_ADDR JSON>'

# 前端（详见 clients/README.md）
cd clients/app && npm install
npm run dev            # 浏览器（连 nm-gatewayd）
npm run tauri dev      # 桌面原生（内嵌 iroh）
```

本机常驻运行：`./start-all.sh` / `./stop-all.sh` / `./restart-all.sh`（release 预编译 + nohup + PID + 日志；
配置在仓库根 `nmd.toml`，模板见 `bin/nmd/src/main.rs` 顶部文档）。

## 打包发布（mac / linux / windows，解压即可执行）

```bash
cargo xtask dist                                    # 当前主机平台
cargo xtask dist --all                              # 官方矩阵：macos-arm64/x64 + linux-x64(musl 静态) + windows-x64
cargo xtask dist --target x86_64-unknown-linux-musl # 指定 triple
cargo xtask dist --bins nmd,nm-admind --no-build    # 自定二进制集 / 只打包不编译
```

产物在 `dist/nanomesh-<版本>-<平台>.zip`：`bin/`（nmd + nm-admind + nm-echo）+ 启停脚本
（unix 为 `*.sh`，windows 为 `*.bat`/`*.ps1`）+ `nmd.toml.example` + README——解压后
`./start-all.sh`（或双击 `start-all.bat`）即运行，首启自动生成含随机管理令牌的 `nmd.toml`。

- mac 目标原生编译；linux(musl 静态)/windows(gnu) 交叉编译需 `brew install zig cargo-zigbuild`
- 模板与脚本资产在 `xtask/assets/`，打包逻辑在 `xtask/src/main.rs`（`cargo xtask dist` 即 `cargo run -p xtask -- dist`）

## 架构要点

- **原生端**(desktop/iOS/android)：Tauri 外壳 + 进程内 Rust `nm-client`(iroh)，App 即真实节点。
- **Web 端**：同一套 React UI 构建为 PWA，经 `nm-gatewayd`(WebTransport/WS) 接入（浏览器不能直跑 QUIC）。
- **接入抽象**：前端 `src/core` 里 `CoreTransport` 一个接口 + 两个适配器（Tauri IPC / 网关），界面写一次跑四端。
- **去中心**：节点按 Ed25519 公钥互联(dial-by-key)，无中心 S2S 网关；频道走 iroh-gossip，联邦状态走 iroh-docs。

## 状态

脚手架阶段：目录/crate 图/依赖/前端接入抽象已就位，业务逻辑为 TODO（对应 PLAN_B 各节 / 里程碑 M0–M3）。

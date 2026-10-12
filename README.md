# NANO MESH / nmspace

基于 **iroh** 的去中心化网格通信系统。当前后端核心、原生客户端、管理台、域名注册中心、联邦同步和 WebSocket 浏览器网关已经落地。

## 当前状态

- 已实现：P2P 私聊、离线信箱、群组、频道、presence、多设备、设备吊销、联系人册、内容寻址 blob、去中心命名、联邦成员发现、per-topic 联邦双写/退火管、selfhost relay/dns、管理台与域名注册中心。
- 原生客户端：`clients/mobile`，Tauri 2 + 静态 JS/CSS UI + 进程内 `nm-client`，桌面/移动共用。
- Web 端：`crates/nm-gateway` / `bin/nm-gatewayd` 提供最小可用 JSON WebSocket 桥 `/ws`，用于浏览器经 `nm-client` 接入 iroh；WebTransport 尚未实现。
- 安全现状：iroh 连接身份基于公钥认证；多设备证书、吊销、Grant、命名/presence/成员卡片等已有签名能力。消息内容的端到端加密尚未接入，联邦 gossip 中的部分业务消息仍是明文载荷。

## 目录结构

```text
.
├── crates/
│   ├── nm-proto         # prost 线协议
│   ├── nm-transport     # iroh endpoint、dial-by-key、gram 帧收发
│   ├── nm-store         # redb 持久化：inbox/entity/group/name/device/blob/roster
│   ├── nm-gossip        # iroh-gossip topic 封装
│   ├── nm-federation    # per-topic group/inbox/names 联邦封装
│   ├── nm-crypto        # 签名、Grant、多设备证书/吊销
│   ├── nm-entity        # 实体类型与 profile 打包
│   ├── nm-node          # 节点服务编排与路由核心
│   ├── nm-client        # 原生 SDK
│   └── nm-gateway       # 浏览器 WebSocket 网关：JSON /ws ↔ nm-client ↔ iroh
├── bin/
│   ├── nmd              # 网格节点守护进程
│   ├── nm-admind        # 本机管理台，反代 nmd 管理 API
│   ├── nm-domain        # 全球域名注册中心
│   ├── nm-compute       # compute.inference 示例服务
│   ├── nm-agent         # agent 示例
│   └── nm-echo          # echo 测试实体
├── clients/mobile       # Tauri 原生客户端与静态 UI
├── xtask                # 打包发布任务
├── src/index.ts         # Cloudflare Container 入口，仅包装 nm-domain
└── docs                 # 设计、部署、测试与安全文档
```

## 前置依赖

- Rust 1.85+。
- `protoc`，用于构建 `nm-proto`。
- Node.js 18+，仅在构建 Tauri 前端、Cloudflare 容器入口或运行 wrangler 时需要。

macOS:

```bash
brew install protobuf
```

## 快速开始

检查后端 workspace：

```bash
cargo check --workspace
cargo test --workspace --exclude xtask
```

启动一个节点：

```bash
cargo run -p nmd
```

`nmd` 会打印：

- `NM_NODE_ID`：稳定节点公钥，nat/selfhost 模式下对端优先用它拨号。
- `NM_NODE_ADDR`：完整地址 JSON，LAN/本机测试可直接使用。

启动辅助实体：

```bash
cargo run -p nm-echo -- --node '<NM_NODE_ADDR JSON>'
cargo run -p nm-agent -- --node '<NM_NODE_ADDR JSON>'
cargo run -p nm-compute -- --node '<NM_NODE_ADDR JSON>'
```

启动原生客户端：

```bash
cd clients/mobile
npm install
npm run tauri dev
```

启动浏览器网关：

```bash
cargo run -p nm-gatewayd
```

默认监听 `127.0.0.1:9620`，可用 `NM_GATEWAY_LISTEN=127.0.0.1:9700` 覆盖。`/health` 返回健康状态，`/ws` 接受 JSON 命令：`connect`、`disconnect`、`my_id`、`node_id`、`directory_query`、`send_to`、`send_group`、`group_list`；节点推送会转成 `{"type":"event","event":...}`。

## 运行与管理

本机常驻运行脚本：

```bash
./start-all.sh
./stop-all.sh
./restart-all.sh
```

运行配置为仓库根目录 `nmd.toml`，该文件包含本机 token/路径等敏感配置，已被 `.gitignore` 排除。

`nm-admind` 和 `nm-domain` 默认只监听 `127.0.0.1`。首次创建 state 文件时会生成随机初始密码并只在本次启动日志中打印，登录后需要立即修改。生产对外暴露时请显式传 `--listen 0.0.0.0:PORT` 并配置 TLS 或反向代理。

## 打包发布

```bash
cargo xtask dist
cargo xtask dist --all
cargo xtask dist --target x86_64-unknown-linux-musl
cargo xtask dist --bins nmd,nm-admind --no-build
```

产物在 `dist/nanomesh-<版本>-<平台>.zip`，包含二进制、启停脚本、`nmd.toml.example` 和 README。

Cloudflare Container 路径只构建和运行 `nm-domain`：

```bash
npm install
npx wrangler deploy
```

## 测试说明

默认测试套件中，非 ignored 用例应保持通过：

```bash
cargo test --workspace --exclude xtask
```

部分测试被标记为 `ignored`，原因通常是依赖 N0 公共网络、真实 iroh-dns-server、真实 seed，或 iroh Minimal-mode 跨节点 overlay 在本地/CI 中不稳定。这些用例用于手动验收真实网络路径，不适合作为默认 CI 阻塞项。

严格 lint 目标：

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

## 安全边界

- `nmd.identity`、`*.identity`、`*.redb`、`*.state.json`、`nmd.toml` 均视为本机敏感运行态，不应提交。
- 普通 gram 帧有固定大小上限；大媒体应走 `blob.put` 内容寻址路径。
- 当前联邦 gossip 仍有明文业务载荷，公开联邦前应完成 E2E 加密与更完整的应用层签名策略。

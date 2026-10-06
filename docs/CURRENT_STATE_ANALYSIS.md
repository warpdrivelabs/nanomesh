# nmspace 现状分析（大工程规划基线）

> 生成于 2026-10-04。全量功能地图 + 成熟度 + 缺口，作为后续工程规划依据。
> 结论性快照；细节以代码为准。**大工程方向：完成移动端（Android + iOS，手机 + 平板）——见文末「移动端」。**

## 1. 定位
基于 **iroh/QUIC** 的去中心化网格：节点 = Ed25519 公钥，按公钥直连（dial-by-key），无中心 S2S 网关。已从 IM 长成 **身份 + 命名 + 社交 + 多设备 + （规划中）算力** 平台。一套静态 UI（`clients/app/ui`）覆盖桌面/移动/Web：Tauri 进程内跑真实 iroh 节点；Web 经网关。

> ⚠️ 顶层 `README.md` 仍写「脚手架阶段、业务逻辑 TODO」——**已严重过时**，实际 P0–P4 + 命名 + 账号 + 多设备 + 全球域名注册中心均已落地。README 待重写。

## 2. 技术栈
iroh 1.2 · iroh-gossip/iroh-docs 0.101 · redb 4 · tokio · axum 0.8 · prost(protobuf) · Tauri v2 · rusqlite · Argon2id / XChaCha20Poly1305 / X25519 · （openmls E2E 预留未接）。

## 3. 后端 crate（workspace）
| crate | LoC | 职责 | 成熟度 |
|---|---|---|---|
| **nm-node** | 3438 | 会话表·实体目录·路由·群·频道·命名·blob·presence·联邦 gossip | 🟢 核心完备 |
| nm-client | 804 | 客户端 SDK（任意实体类型上线） | 🟢 |
| nm-store | 626 | redb：inbox/实体/群/blob/命名/域名 | 🟢 |
| nm-crypto | 305 | 完整性校验 + Grant(UCAN 风格授权) + 口令 | 🟢（E2E 未接）|
| nm-transport | 330 | iroh 端点：dial-by-key/流/relay·DNS 发现 | 🟢 |
| nm-entity | 230 | 可扩展实体类型（人/Agent/设备/车/算力/推理）| 🟢 |
| nm-proto | 41 消息 | 线协议（24 GramKind + 实体/群/频道/命名/设备证书）| 🟢 |
| nm-gossip | 108 | 频道 pub/sub（iroh-gossip 封装）| 🟢 |
| **nm-federation** | 16 | iroh-docs 封装 | 🔴 **桩**（联邦实际靠 gossip 全量广播）|
| **nm-gateway** | 16 | 浏览器 WebTransport/WS 网关 | 🔴 **桩**（Web 端未通）|

## 4. 可执行程序（bin）
- **nmd**(930)：节点守护；`nmd.toml` 配置；`/names/*` 管理 API + `domain.decision` gossip。
- **nm-admind**(889)：管理台（axum + Web Components）：总览/连接/用户/对等/存储/流量/系统/**域名管理**。
- **nm-domain**(1194)：**全球域名注册中心**（redb + axum，已接 Cloudflare Containers）。`domain→节点公钥`，**管理员审批制认领**（非共识/PoW/见证），冲突 409；批准后回 POST nmd `/names/domain-decision` → gossip 扩散 → 各节点把域名入自有集。**只管域名归属；`local@domain→用户公钥` 由 home node 自治。**
- nm-agent(75) 无头实体 · nm-echo(62) 测试回声 · nm-gatewayd(10) 网关桩 · xtask 多平台打包。

## 5. 核心数据流
1. **消息（私聊/群/频道）**：统一走**联邦 gossip**（跨 NAT/跨网已验证，弃用不通的 s2s 中继）；收件人所在/home 节点投递，离线入 redb 补投。
2. **命名解析**：`domain→node` 查 nm-domain（或 gossip 学到）；`local@domain→pubkey` 向 home node 查**自签名 NameRecord**（LWW + gossip 复制）。`name.*` RPC 已成**完整账号系统**：`claim/resolve/reverse/list/register/login/passwd/reset`——口令由 home node Argon2 校验。
3. **联邦收敛**：目录/群/命名/域名决定靠一张**联邦级 gossip 主题全量广播** + LWW/墓碑。

## 6. 身份与安全体系
- **本地保险库**（`src-tauri/src/auth.rs` / `docs/CLIENT_AUTH_SECURITY.md`）：主口令 —Argon2id→ MK → 包裹随机 VK → VK(XChaCha20Poly1305) 加密每个身份种子；私钥永不明文落盘；加密备份导入导出（独立口令）+ 24 词 BIP-39 恢复码 + 审计日志。**移动端已降低 Argon2 内存成本**。
- **多设备**（`devices.rs`）：每设备自有连接子密钥 + 账号签发设备证书；丢设备**只吊销该子密钥**不废账号；`device_revoke/freeze`。账号主种子**仅存 admin 设备**。
- **配对迁移**（`pair.rs`/`pairing.rs`）：X25519 ECDH + HKDF-SHA256 + **6 位 SAS 防中间人** + XChaCha20Poly1305 封装；`nmpair1:` 离线迁移串；默认只发设备证书，勾选才带主种子。

## 7. 前端（`clients/app/ui/js`，~5454 LoC，一套 UI 多端）
- 功能模块：`im/composer(富媒体 NMCHAT1)/groups/channels/naming/profile/usermenu/connect/account/nodesvc/services/identity/tabs/ticker` + **devices/pairing/tray/auth**。
- 基础设施：`bridge(Tauri 桥)`·`main(外壳内核)`·`platform(窗口三键)`·`store(localStorage 写穿后端)`·**i18n(`t()` 中英)**·**icons(`nmIcon`)**·**listcache(SQLite 列表缓存)**·**chatlog(SQLite 消息库)**；profile 亦按公钥存 SQLite。
- Tauri 原生模块：`lib.rs`(命令总线 + SQLite)·`auth/devices/pairing/tray/pair`。桌面已有系统托盘常驻、富媒体、本地历史、标签工作区。

## 8. 成熟度矩阵
- 🟢 **扎实**：P2P 消息/群/频道（gossip）、命名 N1 + 账号系统、多设备/配对/保险库、桌面客户端、管理台、域名注册中心、打包发布。
- 🟡 **弱/临时**：联邦靠 gossip 全量广播（**不扩展**：每节点看到全联邦所有群/频道/私聊/命名——隐私 + 规模隐患）；频道内存态（重启丢）；nm-domain 偏中心（单实例 + 人工审批）。
- 🔴 **缺口**：**Web 端未通**（nm-gateway/gatewayd 桩）；**E2E 加密未接**（openmls 预留，消息明文且广播全联邦）；实体注册/多数记录**未签名**（TODO）；命名**全局唯一非去中心**（审批制而非共识/见证）。

## 9. 部署/运维
节点守护 `start-all.sh`/`stop-all.sh`；`xtask dist` 多平台打包（mac 原生、linux-musl 静态、windows-gnu 交叉，需 zig/cargo-zigbuild）。生产种子：cmxdev18/46 + cmxlinux（systemd/nohup，详见 memory `seed-deploy-pipeline`）；nm-domain 跑 Cloudflare Containers。

## 10. 文档地图（`docs/`）
- **PLAN_B_iroh_decentralized_im.md** — 架构主干（iroh 去中心 IM）。
- **PLAN_C_extensible_entities.md** — 可扩展实体模型（人/Agent/IoT/车/算力）。
- **PLAN_D_public_internet.md** — 公网/跨 NAT 互通。
- **PLAN_E_decentralized_compute_network.md** — DePIN 去中心算力/推理网络 + 结算链。
- **MESH_DNS_NAMING_DESIGN.md / DECENTRALIZED_NAMING_DESIGN.md / PLAN_DNS_NAMING.md** — 命名架构（部分已由 nm-domain + name.* 实现）。
- **DECENTRALIZED_SOCIAL_DESIGN.md** — 富资料/群/频道设计。
- **CLIENT_AUTH_SECURITY.md** — 客户端接入与本地安全（auth.rs 实现之）。
- **CONFIG_MANAGEMENT.md** — 配置管理体系。
- **DEPLOY_selfhost_relay_dns.md / DEPLOY_two_nat_servers.md / TAURI_client_adjustments.md** — 部署与客户端适配。
- **ANALYSIS.md**(2026-09-21 旧分析)。

---

## 11. 大工程：移动端（Android + iOS · 手机 + 平板）
目标：把现有 Tauri v2 + 一套静态 UI 做成可发布的 **iOS / Android** 原生 App，适配**手机与平板**两类尺寸。关键评估维度（待细化为专项方案）：
- **可直接复用**：Rust 后端（nm-client 进程内 iroh）、整套 UI 与业务模块、命令总线、SQLite、i18n、保险库（已为移动降 Argon2 成本）、多设备/配对。
- **须重做/新增**：Tauri 移动工程脚手架（`tauri ios/android init`、签名、权限）；UI 从"桌面窗口外壳（自绘标题栏/活动栏/splitter）"改为**响应式 + 触摸 + 安全区 + 手机/平板双布局**；移动系统能力——iOS Keychain / Android Keystore（钥匙串已起步）、**后台收消息与推送（APNs/FCM，tray.rs 为桌面专属，需移动等价）**、权限（相机/麦克风/相册/通知）、QUIC/iroh 在移动网络与省电策略下的连通性与续连。

详见后续《移动端工程方案》。

<sub>Generated with dmxapi</sub>

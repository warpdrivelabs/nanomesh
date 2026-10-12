# 换网自愈设计：任意端点（nm-node 与客户端）换网后自动恢复通讯

状态：**草案 / 待评审**（评审通过后再落地）
日期：2026-10-12
适用：① **nm-node 节点**（服务器 nmd、本机 mac nmd）；② **嵌入 iroh 的客户端**（nm-compute、nm-agentd、desktop app、移动 app）。
**角色边界：移动端（Android/iOS）只做客户端，永不运行 nmd / 不作为 nm-node。**

---

## 1. 问题陈述

本机 mac（跑 `nmd` + Qwen 助手 `nm-compute`/`nm-agentd`）**换网后无法正常通讯**，需手动重启才恢复。

要解决的不是单点症状，而是**通用能力**：任何**端点**在网络环境变化后，都应**自动**恢复 P2P 通讯，无需人工干预。此处"端点"分两类角色，换网自愈对两者都要覆盖，但职责不同：

- **nm-node 节点**（服务器、本机 mac 上的 `nmd`）：被别人按公钥发现、承载路由/联邦/presence。
- **客户端**（nm-compute、nm-agentd、desktop/移动 app）：连到 home node 收发；**本身不是节点**。

> **范围澄清（2026-10-12）**：移动端（Android/iOS）**只做客户端，永不作为 nm-node**（不跑 nmd）。故"节点换网"仅指服务器/本机 mac 的 `nmd`。
> 但须注意一个现状事实：**原生移动客户端在进程内内嵌 `nm-client`(iroh) 直连 home node**（`clients/mobile/src-tauri/Cargo.toml:23` 依赖 nm-client/nm-transport；`lib.rs:2` 注释"原生端 iOS/android 运行本模块，Web 端走 nm-gateway"；`devices.rs:166 dial_account`→`connection.rs:129 client.online`）。因此移动端虽**非节点**，其 iroh 客户端端点换网后**仍需自愈**（含 Android 的 `network_change()`）。
> **已定决策（2026-10-12）**：移动端**维持原生内嵌 iroh 直连，不走瘦网关**。故移动客户端的换网自愈（§5.5 入站主动重连 + §5.6 Android `network_change()`）是**必做项**，纳入正式范围。

"换网"涵盖：
- Wi-Fi ↔ 蜂窝切换、换 Wi-Fi、DHCP 续租导致内网 IP 变化；
- 公网出口 IP 变化（NAT 重绑、换运营商）；
- 睡眠/唤醒、飞行模式开关、VPN 开关、网卡 up/down；
- home relay 可达性变化。

---

## 2. 现状（基于代码核实）

### 2.1 传输与基础设施
- iroh `1.3.0`（`Cargo.toml:63`），端点封装在 `crates/nm-transport/src/lib.rs::NodeEndpoint`。
- 三种 infra（`nm-transport` `Infra` 枚举，`lib.rs:42`）：
  - `N0`：n0 公共中继 + DNS/pkarr 发现（可穿 NAT）；
  - `Local`(Minimal)：无中继无发现，仅同网直连；
  - `SelfHosted`：Minimal + 自建 relay + 自建 pkarr dns。
- `presets::N0` 自带 `PkarrPublisher` + `PkarrResolver` + `DnsAddressLookup`（iroh `endpoint/presets.rs:125-133`），**地址变化时会自动重发布** endpoint info 到发现服务。

### 2.2 本机 nmd（`~/nmnode/nmd.toml`）
- `mode = "nat"` → `Infra::N0`；`bind_port = 9600`（固定端口）。
- 启动流程（`bin/nmd/src/main.rs`）：`bind_*_persistent_on` → `node.online()`（**仅一次**，10s 超时，`:431`）→ 打印 `NM_NODE_ADDR`（`:506`）→ `node.serve()`。
- **没有** `watch_addr` 监听、**没有** `network_change()` 调用、**没有**换网后重新 `online()`/重打印地址。
- 联邦同步/成员发现/presence 以 15s 间隔循环（`spawn_federation_sync(15s)` 等），对等掉线后有周期重试。

### 2.3 消费者侧
| 组件 | 寻址 | 断线重连 | 问题 |
|---|---|---|---|
| `nm-compute` | `online(addr)` 一次（`main.rs:67`） | **无**：主循环 `while sess.next_command()`（`:96`）在连接断开时 channel 关闭→返回 `None`→**退出进程**，靠 launchd 拉起 | 换网→整进程重启，恢复慢、依赖外部托管；若 iroh 本可自愈同一端点则属过度重启 |
| `nm-agentd` | `online(addr)` + serve 循环 | **有**：serve 返回后 `online`+重注册+重找 provider，指数退避（`nm-agentd.rs:160-195`） | 已是良好范式，可复用 |
| app/mobile | `dial_account(... node ...)` | **被动**：发送超时(5s)/判死→`reconnect()` 单飞重拨（`clients/app/src-tauri/src/connection.rs:197`） | 仅在"下一次主动发送"时才触发；**入站 DM 在换网后不会自动恢复**，直到用户主动发消息 |

### 2.4 寻址方式（决定能否靠发现自愈）
- 按**公钥 id** 拨号：`online_by_id` → `addr_from_id` 构造"仅身份"地址，由发现服务解析当前地址（`nm-client/src/lib.rs:189-194`）——**换网自愈的正道**，对端地址变了也能重新解析。
- 按**完整地址**拨号：`online(full_addr)` 会把 addrs 存进 `MemoryLookup`（`nm-transport:239-240`）。若缓存了旧 IP，可能**遮蔽发现**、拨到死地址。`add_peer_addr` 仅应在 lan/无发现时播种。

### 2.5 iroh 1.3 可用能力（已核实 API）
- `Endpoint::online()`：等待连上 home relay（`endpoint.rs:1374`）。
- `Endpoint::watch_addr()`：地址(IP/relay)变化的 Watcher（`:1289`）。
- `Endpoint::home_relay_status()` / `net_report()`：中继连通/网络报告 Watcher（`:1408/:1457`）。
- `Endpoint::network_change()`（`:1665`）：**关键**——文档明确：**macOS/Linux iroh 能自检换网；Android 不行**，必须由上层（Java ConnectivityManager 回调）主动调用；"即使网络没变或 iroh 已自检到，重复调用也无害"。
- 底层 `netwatch` / `portmapper` 依赖已在（`Cargo.lock`）。

---

## 3. 根因分析（分层）

换网后"通讯断"可能卡在任一层，通用方案须逐层覆盖：

- **L1 自身端点未重绑/重发布**：桌面端 iroh 多能自检，但睡眠/唤醒、VPN、相同网卡 DHCP 改 IP、Android 等场景可能漏检；我们从未调用 `network_change()` 兜底，也没有 `watch_addr` 驱动的"换网后重新 online"。
- **L2 寻址用了缓存旧地址**：凡按完整地址（含旧 IP）拨号、或 `add_peer_addr` 播了旧地址，会遮蔽发现、拨死地址。home node 应一律按 **id** 拨号。
- **L3 会话/连接未重建**：`nm-compute` 无进程内重连（退出靠托管）；app/mobile 只在主动发送失败时被动重连，**入站方向**换网后不自愈。
- **L4 平台差异**：Android（及 iOS 部分场景）需把系统网络回调接到 `network_change()`。
- **L5 不可观测**：无地址/relay 变化日志，换网后难判断卡在哪层。

---

## 4. 设计目标与非目标

**目标**
- 任意 nm-node 换网后，在有界时间内（目标 P50 < 10s、P99 < 60s，受 pkarr/DNS 传播限制）**自动**恢复双向通讯，无需手动重启。
- 覆盖出站与**入站**两个方向（入站是本次痛点：别人/服务器发给本机）。
- 跨平台统一机制（server/mac/android/ios）。
- 换网期间消息不静默丢失（最坏入离线库、重连补投）。

**非目标**
- 不改身份模型（seed/`--identity-file` 不变→node id 稳定→发现可解析，是自愈的前提）。
- 不在本文件实现代码（评审后分阶段落地）。
- 不改 nat/selfhost 基础设施选型（见开放问题）。

---

## 5. 方案

核心思想：**身份稳定 + 一律按 id 寻址 + 端点层换网事件驱动"重发布/重连" + 平台网络回调喂 `network_change()` + 入站侧主动重连**。

### 5.1 端点层：换网监督器（`nm-transport`，所有角色共享）
在 `NodeEndpoint` 增加一个可选后台任务 `spawn_roam_supervisor()`：
1. 订阅 `watch_addr().stream()`：地址变化时 → 结构化日志（新 addr/relay）+ 触发一个 `generation` 自增 + 通过 watch 通道通知订阅者（供上层重连）。
2. 订阅 `home_relay_status()`：relay 掉线→等待重连；用于可观测。
3. 暴露 `pub async fn notify_network_change(&self)` 直通 `endpoint.network_change()`，供平台钩子与"睡眠唤醒"调用。
4. 提供 `wait_online()`（封装现有 `online()`）与 `addr_changed()` 订阅接口。

> 说明：N0 preset 地址变化会自动重发布 pkarr，这里主要补"事件通知上层重连"+"兜底 network_change"+"可观测"，不替 iroh 做重发布。

### 5.2 nmd（`bin/nmd`，服务器 + 本机 mac）
- bind 后 `spawn_roam_supervisor()`；订阅地址变化：变化时**重新打印 `NM_NODE_ADDR`**（运维/日志可见新地址）并 `online()` 再断言一次。
- 周期性（如 60s）或事件驱动 `network_change()` 兜底（桌面无害、移动必需）。
- 对等/gossip 连接依赖既有 15s 重试；确认换网后这些循环确实重连（测试项）。
- 不改默认行为的前提下，加日志即可显著改善可诊断性。

### 5.3 nm-compute：改为进程内重连（对齐 agentd）
把 `main.rs:96` 的"断线即退出"改为**进程内 serve-with-reconnect**：
```
loop {
    serve_commands(&mut sess, ...).await;   // next_command()==None 返回
    // 断线：按 id 重新 online + 重注册（register_as ModelLlm），指数退避
    sess = reconnect_by_id(&client, provider_id/seed, ...).await;
}
```
- 与 `nm-agentd.rs:160-195` 的重连范式**抽成共享 helper**：**在 `nm-client` 内新建一个 `mod`**（如 `nm_client::serve_loop`/`reconnect`，不新建 crate），compute/agentd/echo/agent 经其公开 API 复用，避免各写一份。
- 保留 launchd 作为"进程真崩溃"的最后兜底（崩溃≠换网）。

### 5.4 一律按 id 寻址 home node
- compute/agentd 连 home node 改用 `online_by_id(home_id)`（或 id-only addr，`addrs:[]`），已在用（run 脚本写的 `{"id":...,"addrs":[]}`）——确认重连路径也走 id、不缓存旧完整地址。
- 审查 `add_peer_addr` 调用点（`bin/nmd/src/main.rs:410`、`membership.rs:260`、`nm-gateway:173`）：nat/selfhost 下不要播种会过期的完整地址以免遮蔽发现；lan 才播种。

### 5.5 入站方向主动重连（app/mobile + 任何 serve 角色）
当前 app 只在"主动发送失败"时重连 → 换网后**别人发来的消息收不到**。改为：
- 订阅 §5.1 的 `addr_changed()` 与 `conn.closed()`：一旦检测到换网/连接断，**主动**重建到 home node 的持久会话（不等用户发消息），恢复入站推送通道。
- 保留既有"发送失败→重连"作为二次兜底。

### 5.6 平台网络回调（移动端**客户端**，L4）
> 范围：此节针对移动端**客户端**内嵌的 iroh 端点（非节点）。**已定**：移动端维持原生内嵌 iroh（不走瘦网关），故本节为**必做项**。
- **Android**：注册 `ConnectivityManager.NetworkCallback`（`onAvailable`/`onLost`/`onCapabilitiesChanged`）→ **经 tauri 插件桥**（Kotlin 回调 → tauri 命令/事件 → Rust）调用端点 `notify_network_change()` + 触发 §5.5 主动重连。**不走 JNI 直连**（已定 2026-10-12）。这是原生移动客户端换网自愈的必备项（iroh 在 Android 无法自检）。
- **iOS**：`NWPathMonitor` 监听 → 同样经 tauri 插件桥回到 Rust 调用 `notify_network_change()`；桌面端 iroh 多能自检，仍加 `network_change()` 兜底（无害）。
- 复用现有 mobile tauri 结构（`clients/mobile/src-tauri`，见 [[MOBILE_PLAN]]）。

### 5.7 可观测（L5）✅ 已完成（2026-10-12）
- 地址/relay 变化结构化日志（`tracing`，`spawn_addr_watch`）；
- admin API `/system` 端点暴露"当前 node_addr / relays / ip_addrs / 换网次数 roam_changes / 最近换网 roam_last_ms"（`nm-node::RoamStat` 原子计数，换网哨兵触发时累加；`bin/nmd/src/admin.rs::system` 读出），nm-admind 系统面板展示换网次数、最近换网、relay/IP 列表（`bin/nm-admind/web/js/admin/panels.js`）。

---

## 6. 分阶段落地（评审后按优先级）

- **P0（最小、最高 ROI）✅ 已完成（2026-10-12）**
  - nm-compute 进程内重连（对齐 agentd）+ 抽共享 serve-with-reconnect helper（`nm-client::reconnect` 新 mod：`online_by_id_retry` / `serve_with_reconnect`）；
  - nm-transport 换网监督器（`watch_addr` 日志 `spawn_addr_watch` + `notify_network_change` 直通）；
  - compute/agentd 加换网日志、home node 一律按 id 拨号。（nmd 自身的换网日志归 P2 一并做。）
- **P1（部分完成）**
  - ✅ **app 入站方向主动重连（§5.5）已完成（2026-10-12）**：`nm-transport::spawn_addr_change` → `nm-client::spawn_addr_change_task` → app 换网哨兵发信号 → setup 里 `roam_reconnect_driver` 收信号（300ms 节流）→ 复用既有 `reconnect`（单飞锁 + gen 判重）主动重连，恢复入站推送；重连 future 含 tauri `State`（非 Send）故哨兵只发信号、不进裸 spawn。
  - ⏸ **mobile 入站主动重连：暂缓**（用户 2026-10-12 指示"移动端暂缓，不急"）。现状 mobile 连接层无重连地基（无 `conn_gen`/`last_connect`/`app_handle`/`send_with_retry`/网络回调），需先移植 app 的重连骨架，单列后续。
  - ⏸ **Android `network_change()` 回调接线（§5.6，走 tauri 插件桥）：暂缓**（随 mobile 一起）。基础能力已就位：`Client::notify_network_change()` 已提供，插件桥只需接系统网络回调调用它。
- **P2（部分完成）**
  - ✅ **nmd 换网后重新 online + 重打印 NM_NODE_ADDR + 周期 `network_change()` 兜底，已完成（2026-10-12）**：nmd（nat/selfhost）启动 `spawn_addr_watch("nmd")` 日志 + 换网哨兵（`Node::spawn_addr_change`）——地址变化即 `notify_network_change()` 兜底 + 限时重 `online()` + 重打印 `NM_NODE_ADDR`；lan 模式无中继/发现跳过。为此 `Node` 新增 `notify_network_change`/`spawn_addr_watch`/`spawn_addr_change` 封装（转调 `NodeEndpoint`）。
  - ⏸ **iOS 回调：暂缓**（随 mobile 一起）。
  - ⏳ **可观测看板（§5.7）：✅ 已完成（2026-10-12）**——admin `/system` 吐换网/地址指标，nm-admind 系统面板展示。
- **P3**
  - 换网集成测试 + 文档；selfhost 基础设施评估（开放问题）。

---

## 7. 测试计划

模拟换网并验证自动恢复（无需手动重启）：
1. **内网 IP 变化**：改 Wi-Fi/DHCP 或 `sudo ifconfig en0 ...`，观察 compute/agentd 日志出现"地址变化→重连成功"，且 Qwen 助手 DM 往返恢复。
2. **Wi-Fi ↔ 热点/蜂窝切换**（mac + 手机）。
3. **睡眠/唤醒**：合盖再开，验证恢复时间。
4. **VPN 开关 / 飞行模式开关**（移动端重点）。
5. **公网出口变化**：切运营商/重拨，验证 pkarr 重发布 + 对端按 id 重解析。
- 指标：恢复时延（P50/P99）、换网期间消息零静默丢失（最坏离线补投）。
- 验收脚本：换网后向 agent id `f0c65069…` 发消息应正常回（见 [[qwen-agent-launchd-supervision]]）。

---

## 8. 风险与权衡
- **身份稳定是前提**：compute 用 `--seed 160`、agentd 用 `--identity-file`，id 不变→发现可解析。若改随机身份则自愈失效。
- **重连重复会话**：节点按 conn `stable_id` 清理旧会话（`crates/nm-node/src/protocol.rs:74-82`），重连不会误删新会话。
- **pkarr/DNS 传播延迟**：决定恢复时延下界；P99 目标放宽到 60s。
- **退避抖动**：统一 helper 用 2s→30s 指数退避 + 抖动，避免换网瞬间风暴。
- **relay 依赖**：N0 走 n0 公共 relay；大规模可评估 selfhost（见开放问题）。

---

## 9. 决策与开放问题

### 已定决策（2026-10-12 评审）
1. **nm-compute 重连**：采用**进程内重连**（对齐 agentd）+ launchd 仅作"真崩溃"兜底。恢复快、不丢重连周期。
2. **基础设施**：**先维持 nat（n0 公共设施）**；selfhost（自建 relay+dns）**将来再考虑**，本轮不做。
3. **移动客户端形态**：移动端"只做客户端、非节点"，且**维持原生内嵌 iroh 直连，不走瘦网关**。故移动端换网自愈（§5.5 + §5.6）为必做项；瘦网关方案**已排除**。
4. **共享 helper 落点**：在 **`nm-client` 内新建一个 `mod`**（如 `nm_client::serve_loop` / `reconnect`）承载 serve-with-reconnect 与按-id 重连逻辑，**不新建 crate**；compute/agentd/echo/agent 复用。（`nm-agent-bridge` 仍保持排除出 workspace，仅通过 `nm-client` 这个 mod 的公开 API 复用，见 [[p2p-compute-agent-plan]]）

5. **移动端回调实现**：Android/iOS 的系统网络回调一律**经既有 tauri 插件桥**接回 Rust 调用 `notify_network_change()`，**不用 JNI 直连**（已定 2026-10-12）。
6. **恢复 SLA**：**P50 < 10s，P99 < 60s**（已确认 2026-10-12）。
   - 含义：恢复时延 = 从**换网发生**到**双向通讯重新可用**（能发也能收）的耗时；每次换网不同，取其分布的百分位。**P50<10s** = 一半的换网 10s 内恢复（典型体验）；**P99<60s** = 99% 的换网 60s 内恢复，仅 1% 落入长尾（如撞上 DNS TTL 传播/relay 重连慢）——保证"最坏有上界、不卡死"。用双百分位而非平均值：P50 管常态够快、P99 管最坏不失控，直接作为测试验收项（§7）。

### 待确认 / 待定
（无——方向项已全部敲定；落地中的实现细节在各 §与 §6 阶段计划内处理。）

---

## 10. 相关
- [[COMPUTE_MODEL_P2P_DESIGN]]、[[FEDERATION_SCALING_DESIGN]]、[[DEPLOY_two_nat_servers]]、[[MOBILE_PLAN]]
- 记忆：[[qwen-agent-launchd-supervision]]、[[federation-delivery-mechanisms]]、[[presence-and-message-loss-architecture]]

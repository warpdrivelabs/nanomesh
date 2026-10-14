# nm-node 全面评估报告：海量用户支撑能力、鲁棒性与架构

**日期**: 2026-10-14
**评估对象**: `crates/nm-node`(5104 行 / 22 模块)及其依赖 `nm-transport`、`nm-store`、`nm-federation`、`nm-gossip`、`nm-core`、`nm-proto`,以及 `bin/nmd`
**评估方法**: 源码普查 + 并发/存储专项审查 + 运行时事实采集(非臆测)
**一句话结论**: 架构清晰、模块化良好、并发安全纪律到位,**适合中小规模(单节点数千至约 1–2 万在线)稳定运行**;但要支撑"海量用户 + 严格 7×24",在 **DoS 防护、优雅关闭、存储写吞吐、内存上界、水平扩展** 五个方面存在明确短板,需按本报告改进。

---

## 0. 评级总览

| 维度 | 评级 | 关键依据 |
|---|---|---|
| 系统架构清晰度 | 🟢 良好 | 分层明确:transport / store / federation / node / nmd;职责单一 |
| 模块化 | 🟢 良好 | 22 文件,最大 632 行,无超大文件(详见 §6) |
| 并发安全 | 🟢 良好 | 43 处 unwrap 无 panic 风险;std Mutex guard **零跨-await**(专项审查确认) |
| 单节点海量用户 | 🟡 中等 | 内存表无显式上界、redb 写吞吐受限、无连接数上限 |
| 鲁棒性(容错) | 🟡 中等 | 无 panic/todo、错误多为 Result;但缺 DoS 防护与背压 |
| 7×24 无间断 | 🟡 中等偏下 | **无优雅关闭/信号处理**、无自监控自愈(靠外部 systemd/launchd) |
| 水平扩展 | 🔴 弱 | 单节点全内存状态 + 单机 redb,无分片/复制;扩展靠联邦(gossip)而非节点内扩容 |
| 可观测性 | 🟡 中等 | 有 rss/cpu/traffic/overview,但无标准 metrics(Prometheus)、无结构化告警 |

图例:🟢 可投产 / 🟡 可用但需改进 / 🔴 明显短板

---

## 1. 系统架构

### 1.1 分层

```
┌─────────────────────────────────────────────┐
│ bin/nmd            进程/配置/admin HTTP API    │
├─────────────────────────────────────────────┤
│ nm-node (5104)     节点核心：会话/投递/联邦/命名  │
│   protocol  delivery  federation_runtime      │
│   membership channels presence devices …      │
├──────────────┬──────────────┬────────────────┤
│ nm-federation│ nm-gossip    │ nm-store (redb) │
│ 每群独立主题   │ 频道 pub/sub │ 15 表持久化      │
├──────────────┴──────────────┴────────────────┤
│ nm-transport (iroh QUIC)  端点/发现/中继/ALPN   │
├─────────────────────────────────────────────┤
│ nm-proto / nm-core        协议(prost) / 目录    │
└─────────────────────────────────────────────┘
```

### 1.2 并发模型

- **运行时**: `#[tokio::main]` 默认多线程运行时(`bin/nmd/src/main.rs:316`),未自定义 `worker_threads`。
- **accept**: iroh `Router` 统一分流(`node_runtime.rs:290`),`nmspace/0` 走单播/命令/联邦,`iroh-gossip/1` 走频道。每入站连接由 iroh 内部托管,应用层每连接一个 `handle_conn` 循环。
- **共享状态**: `Ctx` 约 25 个字段,核心是一组 `Arc<DashMap>`(分片并发哈希表)——`sessions / principals / devices / revoked / groups / names / presence / …`,外加 redb 持久化副本。
- **后台任务**: 8 个命名常驻任务(`spawn_federation_sync / spawn_group_sync / spawn_pending_flush / spawn_membership / spawn_member_index / spawn_presence / spawn_addr_watch / spawn_addr_change`)+ inbox prune(每 6 小时)。

### 1.3 数据流(一条私聊)

```
client → QUIC(nmspace/0) → handle_conn → handle_gram
  → 本地在线? deliver_account → try_push(uni stream)
  → home 在本节点离线? → redb push_inbox（重连 drain）
  → 远端? → 每群/收件箱 gossip 主题 publish + 火管兜底
```

**架构评价(🟢)**: 分层干净、依赖方向正确(经上一轮 nm-compute 依赖倒置修复后)。联邦用 gossip 做去中心扩展,避免了中心节点瓶颈。问题不在架构本身,而在"单节点承载力"与"运维成熟度"。

---

## 2. 海量用户支撑能力

### 2.1 内存模型 — 🟡 关键风险:核心表无显式上界

随用户/设备/群**线性增长且无上界**的内存表:

| 表 | 增长维度 | 上界 |
|---|---|---|
| `sessions` / `sessions_meta` | 在线连接数 | ❌ 无 |
| `principals` | 在线设备数 | ❌ 无 |
| `devices` | 历史登记设备总数(持久+内存) | ❌ 无 |
| `dir.entities` | 账号/实体总数 | ❌ 无 |
| `names` | 命名记录总数 | ❌ 无 |
| `groups` / `channels` | 群/频道总数 | ❌ 无 |

有界的集合(设计良好,作为对照):`SeenGrams` FIFO CAP=8192(`dedup.rs:12`)、channel log CAP=300(`channels.rs:31`)、blob ≤1 MiB(`blobs.rs:8`)、`pending_fwd` CAP + 60s TTL(`lib.rs:85`)。

**影响**: `devices` 和 `entities` 是**持久累积**(节点启动时全量 load 进内存,`node_runtime.rs:185`)。十万级账号 × 每账号多设备,启动即全量驻留内存;`local_accounts()` / `online_rids()` 等还对这些表做 `O(n)` 全表遍历(`delivery.rs`),海量用户下遍历成本显著。

### 2.2 存储写吞吐 — 🟡 真实瓶颈(专项审查确认)

- **每操作一事务**: 每个 `put_/del_/ban_` 各自 `begin_write()` + `commit()`(证据:`nm-store/src/lib.rs` 的 push_inbox:86/91、put_device:158/163、put_entity:204/209 等十余处)。
- **默认 `Durability::Immediate`**: 无 `set_durability` 调用,**每次 commit 都 fsync**(redb 4.3.0 默认)。
- **热路径逐条写**: 群/多设备离线投递在调用方循环逐条 `push_inbox`(`delivery.rs:207`)—— **一条群消息 → N 个离线设备 → N 次独立事务 + N 次 fsync**,放大效应明显。
- `inbox_len`(`lib.rs:129`)为取长度把该用户全部离线消息解码一遍,`O(n)`。

**量级判断**: 单盘 fsync 典型 ~1–10ms,即单节点离线写入上限约 **数百–千 TPS 级**。在线直投(走 QUIC uni stream,不落盘)不受此限,吞吐高得多;瓶颈集中在**离线消息入库**与**实体/设备变更持久化**。

### 2.3 无连接数上限 / 无限流 — 🔴 DoS 风险

专项检索:**无任何** rate-limit / throttle / semaphore / max-conn / quota / 背压(`§G` 检索全空)。每个入站连接无条件 accept 并 spawn 处理循环。海量/恶意连接下:连接数无上限 → `sessions` 表 + 每连接任务无限增长 → 内存/FD 耗尽。黑名单(`blacklist`)是事后拉黑,非事前限流。

### 2.4 水平扩展 — 🔴 弱

- 单节点状态**全在内存 + 单机 redb**,无分片、无副本、无共识。
- 扩展路径是**联邦(多节点 gossip)**而非节点内扩容:加节点 = 加一个独立 home,用户分散到不同 home node。
- 但跨节点互联本身不稳(见 `docs/CROSSNODE_DELIVERY_DIAGNOSIS_AND_RELAY.md`),联邦扩展的可靠性尚未达生产级。
- 单个 home node 一旦宕机,其名下用户的离线消息/目录**不可用且无接管**(redb 单副本)。

---

## 3. 鲁棒性(容错)

### 3.1 panic 面 — 🟢 良好

- **0 处** `panic!` / `unreachable!` / `todo!` / `unimplemented!`(非测试)。
- 43 处 `unwrap()` / 4 处 `expect()`,专项审查确认**均非危险**:绝大多数是 `lock().unwrap()`(std Mutex 仅在持锁线程 panic 时才中毒,本代码无此路径)和启动期 `expect`。
- 错误传播以 `Result<_, NodeError>` 为主,网络/存储错误多为 `warn!` + 降级,不 panic。

### 3.2 并发安全 — 🟢 良好(专项审查重点确认)

- 所有 `std::sync::Mutex` guard **零跨-await**:代码刻意采用"持锁只做同步操作(clone/drain/push)→ 释放 → 再 await"模式(如 `channels.rs:163-171` 内层块、`delivery.rs:269` 锁后无 await、`federation_runtime.rs:408-411` drain 后释放再 await)。这是很好的工程纪律,避免了 async 死锁/executor 阻塞这一类最隐蔽的生产事故。
- **2 处需预防性加固**(当前安全,易被未来改动破坏):`federation_runtime.rs:225` 和 `:236` 的 `self.xxx_rx.lock().unwrap().take()` 写在 `match`/`if-let` scrutinee 里,guard 存活到块体结束且跨越 `tokio::spawn()`。建议改为 `let x = { let mut g = ...lock(); g.take() };` 先释放再用。

### 3.3 错误恢复 — 🟡 中等

- 客户端侧有换网自愈 + 重连(上一轮已修复自激)。
- agentd 有 serve-loop 断线重连。
- 节点间 s2s 失败是 `warn!` + 下轮重试(`fed sync failed`),不崩溃,但也**不保证最终一致**(持续失败只会刷日志)。
- **缺**: 存储写失败(`push_inbox failed`)仅 `warn!`,消息静默丢失,无重试/落盘补偿队列。

---

## 4. 7×24 持续运行能力

### 4.1 优雅关闭 — 🔴 缺失(最大运维短板)

- `serve()` 用 `std::future::pending::<()>().await` **永久阻塞**(`node_runtime.rs:296`),无 shutdown 分支。
- **无 SIGTERM / SIGINT / ctrl_c 处理**(`§H` 检索确认)。进程被杀时:
  - 正在写的 redb 事务——redb 有崩溃一致性(事务型),数据不会损坏 ✓
  - 但在线 QUIC 连接**硬断**,未发完的 uni stream 消息丢失;客户端需重连 + drain。
  - 8 个后台任务无有序收尾。
- **后果**: 升级/重启 = 粗暴 kill,每次都造成一次全量客户端重连风暴 + 可能的瞬时消息丢失。本会话中我们已多次目睹"重启 nmd → 所有客户端重连"的代价。

### 4.2 进程级自愈 — 🟡 依赖外部

- 节点自身无 watchdog / 健康自检 / 自动重启。
- 靠外部 systemd(robot.link)/ launchd(mac)的 `KeepAlive` + `Restart=on-failure` 托管。这**可用但不完整**:进程假死(未崩溃但不响应)时外部托管不会重启(无健康探针)。

### 4.3 资源泄漏面 — 🟡

- 每连接一个 `handle_conn` 任务 + watch-closed 子任务;连接正常关闭时清理 `sessions`/`principals`(`protocol.rs:88`)。本会话验证过 churn 场景下清理是生效的。
- 但**无连接数上限**(见 §2.3),异常场景下任务/内存可无界增长。
- `pending_fwd` 有 CAP+TTL,`SeenGrams` 有 FIFO——这些有界,好。

### 4.4 可观测性 — 🟡 中等

- 有:`/overview` `/system`(rss/cpu/uptime/online)、`/traffic`(grams/bytes)、`/connections`(每连接 rtt/丢包/path_kind)。本会话排障时这些 admin API 很有用。
- 缺:标准 metrics 导出(Prometheus `/metrics`)、结构化告警、分布式追踪、慢查询/慢投递统计、队列深度(inbox 积压)指标。

---

## 5. 调优建议(按投入产出排序)

### P0 — 低成本高收益(建议优先)

1. **优雅关闭**(修 §4.1): `serve()` 接入 `tokio::signal::ctrl_c()` / SIGTERM,收到后:停止 accept → 广播 shutdown 给后台任务(`tokio_util::sync::CancellationToken`)→ 等 in-flight 投递 flush → 关闭 redb。**直接消除每次重启的消息丢失与重连风暴**。
2. **连接数上限 + 基础限流**(修 §2.3): accept 前用 `Semaphore` 限并发连接数;对单 IP/单公钥的新建连接速率加令牌桶。防 DoS 与资源耗尽。
3. **redb 批量写 + Eventual durability**(修 §2.2): 群/多设备 `push_inbox` 合并为单事务批量 insert;离线队列写入改 `Durability::Eventual`(容忍崩溃丢最近数条,换数量级吞吐)。**离线写吞吐可提升 1–2 个量级**。
4. **预防性锁加固**(修 §3.2): `federation_runtime.rs:225/236` 先 take 再用。

### P1 — 中等成本

5. **内存上界**(修 §2.1): `devices` 对已吊销/长期离线设备做 LRU 淘汰(持久层保留,内存层按需加载);`presence` 已有 TTL,确认 sweep 真删。给热点表加 size 指标。
6. **自监控 + 健康探针**: 加 `/healthz`(存活)、`/readyz`(就绪:redb 可写 + endpoint online),供外部托管做健康重启;加队列深度/投递失败率指标。
7. **tokio 运行时调优**: 显式设 `worker_threads` 匹配 CPU;重的 redb 写放 `spawn_blocking` 或专用线程池,避免阻塞异步 executor。
8. **投递可靠性**: `push_inbox failed` 不能只 `warn!`——加本地补偿队列或至少回推发送方"投递失败",避免静默丢消息。

### P2 — 架构级(长期)

9. **水平扩展 / 高可用**(修 §2.4): home node 的 redb 状态做主从复制或迁移到可复制存储,支持 home 故障接管;否则单 home 宕机 = 其用户不可用。
10. **节点间可靠互联**: 落地自建 relay(`selfhost`),根治跨节点 s2s 不稳(已有设计文档)。

---

## 6. 大文件拆分评估

**结论:当前模块化良好,无"必须拆"的失控大文件。** nm-node 已是 22 个单一职责模块,最大文件 632 行,在健康区间(Rust 单文件 <800 行普遍可接受)。

| 文件 | 行数 | 是否建议拆分 |
|---|---|---|
| `protocol.rs` | 632 | 🟡 可选。它混了 `handle_conn`(连接循环)、`handle_gram`(路由分发)、`handle_command`(命令处理,多方法 match)。**若继续膨胀**,可把 `handle_command` 的大 match 按命令域拆到 `commands/` 子模块(device./channel./name./roster. 等)。现状不紧迫。 |
| `federation_runtime.rs` | 458 | 🟢 保留。三个 spawn_* 任务内聚,职责一致。 |
| `delivery.rs` | 430 | 🟢 保留。投递逻辑高内聚,是核心路径,集中反而利于维护。 |
| `lib.rs` | 382 | 🟡 可选。`Ctx`/`Node` 的 25 字段定义 + 多个 `bind_*` 构造。可把类型别名与常量抽到 `types.rs`,构造抽到 `builder.rs`,让 lib.rs 回归模块声明与导出。 |
| 其余 | <380 | 🟢 均健康 |

**优先级**: 拆分不是当前痛点,P0/P1 的运维与性能改进远比拆文件重要。若要动,只建议 `protocol.rs` 的 `handle_command` → `commands/` 子模块(随命令增多收益递增)。

---

## 7. 结论

**nm-node 是一个架构清晰、代码纪律良好的节点实现**——分层合理、模块化到位、并发安全(std Mutex 零跨-await 是加分项)、无 panic 面。对于**中小规模、单节点数千至万级在线用户、配合外部进程托管**的场景,它已经可用且本会话验证了其在真实多节点环境的工作能力。

**但"海量用户 + 严格 7×24"还有明确距离**,集中在五个短板:
1. 🔴 无优雅关闭 → 每次重启丢消息 + 重连风暴(P0 必修)
2. 🔴 无 DoS 防护/连接上限(P0 必修)
3. 🟡 redb 每操作一 fsync + 逐条写 → 离线写吞吐受限(P0 优化)
4. 🟡 核心内存表无上界 → 超大规模内存/遍历成本(P1)
5. 🔴 单节点单副本,无水平扩展/高可用(P2 架构级)

**建议路线**: 先做 P0 四项(优雅关闭、限流、redb 批量+Eventual、锁加固)——投入小、直接提升 7×24 稳定性与写吞吐;再按用户增长推进 P1(内存上界、健康探针、运行时调优);当单节点触顶或需高可用时,才投入 P2(复制/分片/自建 relay)。

---

## 附:本报告的事实采集点(可复核)

- 文件规模: `find crates/nm-node/src -name '*.rs' | xargs wc -l`
- 并发原语: `grep -rn 'std::sync::Mutex\|DashMap\|lock()'`
- panic 面: `grep -rn 'unwrap()\|expect(\|panic!\|todo!'`
- 资源上界: `grep -rn 'CAP\|TTL\|LRU\|retain\|max_'`
- DoS 防护: `grep -rniE 'rate.?limit|semaphore|max.?conn|throttle'`(全空)
- 优雅关闭: `grep -rniE 'shutdown|SIGTERM|ctrl_c|signal'`(仅 JoinHandle)
- 存储事务: 专项审查 `nm-store/src/lib.rs` 各 put_/get_ 方法事务粒度与 durability
- async 锁跨 await: 专项审查 channels/delivery/federation_runtime 每个 lock 站点

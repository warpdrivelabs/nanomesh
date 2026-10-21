# nmd P2P 节点服务 — 能力缺口分析与建议方案

- **日期**：2026-10-20
- **状态**：方案待审核（只出方案，审核通过后再逐项出详细设计并落地；**在此之前不改代码**）
- **范围**：`bin/nmd` 及其依赖的 `crates/nm-node`、`nm-store`、`nm-crypto`、`nm-federation`、`nm-transport`、`nm-proto`、`nm-gossip`，以及管理台 `bin/nm-admind`
- **方法**：对当前代码树做了四维勘察（安全/密码学、投递可靠性/数据、运维/可观测、联邦/扩展），结论均附 `file:line` 证据。本文"必要"的定义 = **生产会出事 / 信任根基 / 规模天花板**三类，不含锦上添花。

> **实施状态（2026-10-20）**：**P0 四项已全部落地并部署到 5 节点**（见附录 B）。**P1 运维三件套（P1-8/9/10）已落地、`cargo check --workspace` 通过 + 相关测试通过，未提交/未部署**（见附录 C）。E2E（P1-5）与密钥轮换（P1-7）按用户要求暂缓；P1-6 多设备已读待排期。

---

## 0. 一句话结论

nmd 的**可靠投递机制已相当扎实**（持久 outbox + ack 重传、频道全局定序回填、双层去重、30 天离线信箱、多设备投递扇出、换网自愈）。真正的缺口集中在**数据生命周期安全**（无界表/无备份/无迁移）、**投递失败的静默性**、**应用层反滥用**、**内容机密性（E2E）**、**多设备已读态**、**运维可观测/审计**，以及**联邦的 O(N) firehose 扩展天花板**。下文按 P0/P1/P2 分级给出方案。

---

## 1. 现状基线（已具备，不在本方案范围）

### 1.1 身份与信任
- 节点身份 = iroh Ed25519；QUIC/TLS 握手已在传输层证明"对端持有其公钥"，连接身份被密码学认证（`nm-crypto/src/lib.rs:1-7`）。
- gossip 投递的消息（presence/membership/group）带**应用层 Ed25519 签名**，可按声明的 node id 验作者（`nm-crypto/src/lib.rs:108-120`）。
- UCAN 式 **Grant** 能力授权（issuer→audience 在 expires 前对 resource 做 action，可验可过期，`nm-crypto/src/lib.rs:51-106`）。
- 多设备**证书**（账号私钥签设备公钥）+ **吊销**（账号或 home 节点"冻结"，`nm-crypto/src/lib.rs:122-212`）。
- blake3 逐 gram 完整性（`Gram.crc = blake3(payload.value)`，`nm-proto/proto/nm.proto:102`）。

### 1.2 投递可靠性
- **DM**：离线 `INBOX`（30 天，`nm-node/src/lib.rs:71`）+ 持久 `OUTBOX`（24h horizon，指数退避，`lib.rs:90-91`）+ `DirectAck` 销账 + 每设备 `Receipt` 清信箱。
- **群**：`GROUP_OUTBOX` **按成员 home 节点**（非按成员）ack；`GroupAck`/`GroupAckBatch`（250ms 批，`federation_runtime.rs:720-792`）；24h horizon。
- **频道**：home **全局定序**（`CHANNEL_SEQ` 单调、永不复用）+ **连续前缀回填**（`channel_log_contiguous_upto`）+ 发布者 outbox 重传。三类中最强。
- **多设备投递扇出**：发给账号的消息会到达其**全部**设备（在线 `online_rids`、离线每设备一份，`delivery.rs:128-181`）；自己发的消息也同步到自己其它设备。
- **去重**：内存 `SeenGrams`（CAP 8192）+ 持久 `DEDUP_DIRECT`（24h 裁剪），投递后才标记避免"标记但未投递"空洞。

### 1.3 运维
- 丰富 admin API（`/overview /connections /users /devices /storage /traffic /system /identity /peers /ban /unban /kick /add-peer /remove-peer /names/*`，`bin/nmd/src/admin.rs:94-125`）。
- `/healthz`（存活）+ `/readyz`（就绪：redb 可达 + 有地址/relay），**免鉴权**、适配编排探针（`admin.rs:154-159`）。
- 优雅关闭 drain + 换网**自重启哨兵**（30s settle→exec 自身保留身份，`main.rs:598-632`）。
- 多 relay（iroh 层故障转移）、`max_connections`（默认 10000）、单公钥重连速率门（10/10s）、黑名单/吊销准入门。

### 1.4 联邦/扩展
- **per-topic gossip 已实现且 nmd 默认开**（`main.rs:148,520`；`nmd.toml:17`）；`nm-federation` 已是真实 303 行协调器（非文档所称"stub"）。
- 群消息/DM 的 firehose **happy-path 退火**已上线（全部远端成员 home 在 per-topic overlay 新鲜且发布成功→抑制 firehose 副本，`protocol.rs:378-395`、`delivery.rs:310-323`、`retire.rs`）。
- **能力位协商**：`supports_per_topic` / `supports_ack_batch` / `supports_direct_ack`（announce/inbox 心跳广告，混版网各自回退、无丢失）。
- pkarr 自建中继发现（群/频道 pubkey→seeds、锚点 member-index）、`[[peers]]` 种子/锚点引导。

---

## 2. 缺口总览（按优先级）

| # | 缺口 | 层面 | 严重性 | 工作量 |
|---|---|---|---|---|
| **P0-1** | 多张 redb 表无界增长、无 GC（CHANNEL_INTAKE 文档称裁剪却从未实现；BLOBS/GROUP_SECRETS 泄漏） | 存储 | 🔴 迟早撑爆磁盘 | M |
| **P0-2** | 无 DB 模式版本化/迁移 + 无备份/恢复（破坏性改键布局会**静默丢行**；单 `nmd.redb` 无快照） | 存储 | 🔴 每次升级都在赌 | M |
| **P0-3** | DM 超 24h 投不达即**静默丢弃**，发送方无感知 | 投递 | 🔴 休眠/换网节点常触发 | S–M |
| **P0-4** | 仅连接级限流，**无消息级限流/配额**，可被灌爆 outbox/inbox/群 | 反滥用 | 🔴 DoS 面 | M |
| **P1-5** | 内容无端到端加密：home 节点存/转**明文** | 机密性 | 🟠 去中心化信任根基 | XL |
| **P1-6** | 多设备**已读状态**不同步（A 已读 B 仍未读） | 产品 | 🟠 多设备体验 | M |
| **P1-7** | 账号密钥无恢复、无轮换（丢私钥=丢身份） | 身份 | 🟠 | L |
| **P1-8** | 无审计日志；admin token 匿名单一明文、无身份/权限/轮换；控制台登录零日志 | 运维安全 | 🟠 | M |
| **P1-9** | 无 Prometheus/时延/错误率指标（仅 ad-hoc JSON） | 可观测 | 🟠 无法上监控/SLA | M |
| **P1-10** | 无配置热加载：relay/limits/federation 改动全需重启 | 运维 | 🟠 | M |
| **P2-11** | presence/membership 仍是全联邦 O(N) firehose | 扩展 | 🟡 规模天花板 | L |
| **P2-12** | 发现仍依赖单中继（非真·去中心 DHT，设计 F2 未完成） | 扩展 | 🟡 单点 | L |
| **P2-13** | 跨联邦路由仅文档未落地；`PROTOCOL_VERSION` 空转无握手；后端无邀请链接；成员无准入（anti-Sybil） | 扩展/入网 | 🟡 | S–XL |

---

## 3. P0 详述（correctness / 安全时炸弹——会直接出事）

### P0-1 存储生命周期治理（GC + 配额）
**现状**：22 张表中，**无任何删除路径**的有 `ENTITIES`、`BLOBS`、`NAME_SECRETS`、`GROUP_SECRETS`（群解散 `del_group` 不清→泄漏）、`DEVICES`（吊销不删行）、`REVOKED`（设计只增不删）、`CHANNELS`、`CHANNEL_SECRETS`、`CHANNEL_INTAKE`。其中 **CHANNEL_INTAKE 注释写"按龄裁剪"但无任何 prune 实现**，每条频道消息 +1 行永久驻留。无 redb cache/size/compaction 配置。
**为什么必要**：磁盘终将被喂满；且 redb 单文件膨胀影响全表性能。
**方案要点**：
1. 给每张无界表补裁剪/GC：CHANNEL_INTAKE 按龄（兑现其注释）、BLOBS **引用计数 GC**、GROUP_SECRETS 随 `del_group` 清、ENTITIES/DEVICES 软 TTL 或 LRU 上限。
2. 引入**每账号/每表配额**与入库背压（与 P0-4 共用计量）。
3. `inbox` 裁剪改用**入库时间**而非发送方 `gram.timestamp_ms`（现 `nm-store/src/lib.rs:857` 用发送方时钟，伪钟可改保留期）。
**工作量/风险**：M / 中（不得误删在用数据；BLOBS 需引用计数）。

### P0-2 DB 模式版本化 + 迁移 + 备份/恢复
**现状**：`open()` 直接打开 22 张固定表，**无 version/magic 记录**；手工字节键布局（如 OUTBOX 值 `first_ms++retries++next_due_ms++bytes`）解码失配时只 `continue`/返回 `None`——**破坏性改布局会静默跳过旧行 = 悄悄丢数据**，无检测无转换。无备份/导出/快照工具，仅靠单个 `nmd.redb`。
**为什么必要**：`bin/` 下已有几十个 nmd 历史二进制，升级频繁；任何键布局演进都在赌不丢数据；一次磁盘损坏=全量丢失。
**方案要点**：
1. 新增 `META` 表写 `schema_version` + magic；`open()` 校验→跑已注册迁移链或**拒绝启动**（杜绝静默丢行）。
2. 在线快照（redb savepoint 或加锁文件拷贝）+ 轮转 + `nmd backup` / `nmd restore` 子命令。
3. 手工字节键/值处加长度/版本前缀，解码失配**显式报错**而非吞掉。
**工作量/风险**：M / 中。

### P0-3 投递失败可见化 + 死信
**现状**：`now - first_ms > OUTBOX_MAX_HORIZON_MS(24h)` 即删 outbox 行并 `warn!`，**消息被丢且发送方完全无感知**（`federation_runtime.rs:644-648`）。非跟踪/未知收件人的 DM 走 `pending_fwd`（cap 4096 / TTL 60s）**无持久**，TTL 到即丢。
**为什么必要**：与刚修复的休眠/换网强相关——收件人 home 掉线 >24h 现在就是**无声丢信**；用户以为发出去了。
**方案要点**：
1. horizon 可配（按部署容忍度）。
2. 放弃时写**死信**并回发一个 delivery-failed `Receipt` 给发送方 → 客户端显示"!"未送达，而非静默。
3. 失败计数入指标（接 P1-9）。
**工作量/风险**：S–M / 低。

### P0-4 消息级限流 / 配额（应用层反滥用）
**现状**：仅有**连接级**防护（`max_connections`、单公钥重连速率门），**无消息级限流**：已连上的对端可灌爆 inbox/outbox/群扇出。
**为什么必要**：P2P 节点对外接受任意对端连接，消息级洪泛是现实 DoS 面。
**方案要点**：对入站 gram（DM/群/频道 publish）、outbox 入队、presence/announce 速率做**按发送方/账号令牌桶**，超限背压/拒绝；与 P0-1 配额打通；拒绝计数入指标。
**工作量/风险**：M / 中（阈值需调参）。

---

## 4. P1 详述（一个消息节点应有的核心能力）

### P1-5 端到端内容加密（最大架构缺口）
**现状**：全工作区**零加密原语**（grep `encrypt/aead/xchacha/x25519/ratchet/cipher` = 0 命中）；`nm-crypto` 只有签名/哈希/Grant/设备证书。`Gram.payload` 注释"启用 E2E 时 value 为密文"、crc 已覆盖 `payload.value`——**协议就绪但未实现**；`openmls` 在 `nm-crypto/Cargo.toml:13-14` 注释掉。故 DM/群/频道正文在 home 节点 redb 里是**明文**，仅靠逐跳 QUIC/TLS，home/中转节点可读全文。
**为什么必要**：这是"去中心化/自主可控"价值主张的信任根基；否则 home 节点是全知可信方。
**方案要点（分期）**：
1. **DM 先行**：设备 prekey 经 inbox 主题/pkarr 分发，X25519 + 双棘轮（前向保密 + 后向安全）。
2. **群/频道**：MLS（openmls，对应设计 F5），群密钥随成员增删 rekey。
3. **多设备**：每设备独立叶/设备密钥，发送方同步自有设备也走密文。
**工作量/风险**：XL / 高（密钥管理、多设备、历史消息、丢密钥不可读）。**须单独出设计文档评审后再动。**

### P1-6 多设备已读状态同步
**现状**：投递扇出到每台设备**存在**，但**无已读态**：grep `read_state/unread/last_read/mark_read` = 无。`Receipt` 是**投递**回执（清信箱 + 双勾），非已读标记，且不同步到同账号其它设备。A 读过，B 仍显示未读并保留自己那份信箱副本。
**方案要点**：新增 read-state 表 + 签名"已读标记"gram，经**账号自有设备扇出路径**（已存在）同步；未读数派生。
**工作量/风险**：M / 中。

### P1-7 账号密钥恢复 + 轮换
**现状**：账号私钥即身份根，**丢失=身份不可恢复**；无轮换协议（节点身份轮换会直接换 node-id 断裂，如本月实况）。
**方案要点**：
1. **恢复**：助记词 / 秘密分享（Shamir）备份账号私钥。
2. **轮换**：签名"轮换声明"（旧→新公钥）经目录/membership 传播，保持身份连续。
**工作量/风险**：L / 高（涉及信任模型）。

### P1-8 审计日志 + 管理身份/权限
**现状**：安全事件（ban/kick/revoke/限流拒绝）只作普通 tracing 行，**无审计流、无操作者身份**（admin token 匿名）；控制台 `nm-admind` 登录/失败/改密**零日志**（暴力破解不可见）；单一明文 token（`nmd.toml [admin] api_token`）无作用域无轮换无管理口限流。
**方案要点**：① 管理与安全事件入**审计流**并带操作者身份；② 单 token → 每操作员凭据 + 作用域 + 轮换；③ 控制台登录事件记日志 + 登录限流。
**工作量/风险**：M / 低–中。

### P1-9 可观测性：Prometheus + 时延/错误率
**现状**：有 GroupMetrics / RoamStat / table_sizes / TrafficStat / 每会话链路质量 / ProcMetrics / outbox-pending gauge，但**全是拉取式 ad-hoc JSON**；无 Prometheus/OpenMetrics 导出；**无逐消息投递时延、无错误率**（仅 giveups 与 backlog 作间接代理）。
**方案要点**：`/metrics`（OpenMetrics）导出；新增**投递时延直方图**（send→ack）与**错误率计数**（投递失败/拒连/认证失败/限流拒绝）。
**工作量/风险**：M / 低。

### P1-10 配置热加载
**现状**：配置 boot 时读一次，**无 SIGHUP/无文件监听/无 reload 端点**（grep 证实）。relay URL、`max_connections`、`per_topic`、membership 全**仅重启生效**；仅 peers/blacklist 可经 API 热改。唯一"重配"路径是控制台整进程重启。
**为什么必要**：直接回应早先"relay.robot.link 有变动其他节点的 nm 都要重启吗"的诉求——relay 重指应能进程内重建端点。
**方案要点**：SIGHUP 或 admin 端点，热加载 relay URL（进程内重建/切换端点）、limits、federation 开关、peers/blacklist；可安全热改的先做，需重绑的明确标注并给出受控重启。
**工作量/风险**：M / 中。

---

## 5. P2 详述（战略性扩展，规模变大才咬人）

### P2-11 退火 O(N) firehose：presence / membership 作用域化
**现状**：每个节点常驻**三条全联邦 overlay**——`nmspace-groups`（firehose）、`nmspace-federation`（membership）、`nmspace-presence`，三者**每个节点都是成员**。presence 每 10s 为每个本地在线实体广播、全网接收；membership 每个节点持有每个节点的卡片 → `peer_info` O(N)。控制面（announce/names/revoke）仍恒走 firehose。
**方案要点**：presence 作用域化到**联系人/roster**（而非全联邦广播）；membership 走**部分视图/分片**或完成真 DHT 以退掉控制面 firehose。
**工作量/风险**：L / 高（架构）。

### P2-12 真·去中心发现（完成设计 F2）
**现状**：发现是**自建中继 HTTP PUT/GET**（pkarr 群 seeds、锚点 member-index）——弱锚点/witness 模型，**非 trustless DHT**；单中继即单点。
**方案要点**：以 Mainline-DHT 或多锚点法定数替换单中继 witness。
**工作量/风险**：L / 高。

### P2-13 跨联邦 / 协议握手 / 入网 / 准入
- **跨联邦路由**：仅 `docs/FEDERATION_CROSS_*` 设计，代码里不同 federation 名=完全隔离 overlay，无跨见；需落地 gateway + 名字路由。
- **协议版本握手**：`PROTOCOL_VERSION=1`（`nm-proto/src/lib.rs:24`）被写入/回显但**从不比较**——无最小版本协商；唯一硬门是 ALPN（`nmspace/0`）。应让版本真正参与握手，安全拒绝/提示升级。
- **邀请链接**：后端**无** invite 符号；应铸造/核销签名 token（含 seed+relay+fed）做零配置入网。
- **成员准入（anti-Sybil）**：当前任意节点可加入联邦并广播成员/presence 卡片；可选锚点签发的 membership 令牌 / 白名单做准入。
**工作量/风险**：S–XL / 中–高（分项差异大）。

---

## 6. 建议落地顺序

1. **先 P0 存储与投递安全**（P0-1/2/3/4）——真正会出事的运维炸弹，彼此独立、工作量中等，性价比最高。
2. **再 P1 运维/产品闭环**：低风险快赢（P1-8 审计/身份、P1-9 指标、P1-10 热加载）→ P1-6 多设备已读 → P1-7 密钥恢复。
3. **E2E（P1-5）单列**：架构级，先出独立设计文档评审再动手。
4. **P2 扩展**按用户规模增长再排期。

---

## 7. 待拍板的取舍点

1. **范围认定**：本文把"必要"定义为"生产会出事 / 信任根基 / 规模天花板"三类。是否认可？还是要把某块（如 E2E）提到最前？
2. **E2E 路线**：DM 走双棘轮、群走 MLS——还是统一 MLS？决定设计文档怎么写。
3. **优先级确认**：保留/删除/调序哪些项；先深入哪一项出详细设计。

> 审核后请在本文逐条批注或直接告知结论；在你拍板前**不动代码**。

---

## 附录 B：P0 实施记录（2026-10-20，已落地，未提交/未部署）

全部 `cargo check --workspace` 通过；nm-store 8 项、nm-node 库 9 项 + group_outbox 8 项单测通过。（既有 `app_flow`/`real_seed` 测试编译失败系 nm-client 旧漂移，与本次无关。）

**P0-1 存储 GC + 配额**
- `nm-store`：新增 `channel_intake_prune`（CHANNEL_INTAKE 值改 `seq(8)++ts(8)`，兑现"按龄裁剪"）、`prune_blobs`（新增 BLOB_META 旁表记入库时间，按龄 GC）、`del_group` 解散时连带删 `GROUP_SECRETS`（+`del_group_secret`）；`prune_inbox` 改按**入库时间**（INBOX 值改 `recv_ms(8)++Gram`，不信发送方时钟）。
- `nm-node`：`node_runtime.rs` 周期 GC 任务（6h）新增 intake/blob/dead_letter 三类裁剪；`delivery.rs` 离线入库加**单设备信箱配额**（`INBOX_MAX_PER_ENTITY=50000`）。

**P0-2 模式版本化 + 迁移 + 备份/恢复**
- `nm-store`：新增 `META` 表（`schema_version`+`magic`）；`open()` 校验版本——新库戳当前、旧库跑迁移链、**版本高于本二进制即拒绝启动**（防降级静默丢行）；`migrate_v0_to_v1` 一次性重写旧 INBOX/INTAKE 行；`backup()` 在线一致快照。
- `nmd`：admin `POST /backup {dest}`（在线备份）；`nmd --restore <备份>`（离线恢复，覆盖前自动另存）。

**P0-3 死信 + 投递失败回执**
- `nm-store`：`DEAD_LETTER` 表 + `dead_letter_put/count/prune`。
- `nm-node`：`federation_runtime.rs` DM/群 give-up 从"静默丢"改为**落死信 + 计数**；DM 另向本地发送方推 `nmspace.v1/delivery.failed` 系统 gram（带 `ref_gram_id`，客户端据此标记"!"）。

**P0-4 消息级限流**
- `nm-node`：`protocol.rs` `handle_gram` 入站链路加**单发送方（设备连接）滑动窗口限流**（`300 条/10s`，远超正常客户端，仅掐洪泛），超限丢弃并计数。

**可观测**：admin `/system` 新增 `dead_letters` / `msgs_rate_limited` / `dead_letter_pending`。

**改动文件**：`crates/nm-store/src/lib.rs`(+Cargo.toml tracing)、`crates/nm-node/src/{lib,node_runtime,delivery,federation_runtime,protocol,admin_api}.rs`、`bin/nmd/src/{admin,main}.rs`。

---

## 附录 C：P1 运维三件套实施记录（2026-10-20，已落地，未提交/未部署）

E2E（P1-5）与账号密钥恢复/轮换（P1-7）按用户要求暂缓；本批只做 P1-8/9/10。`cargo check --workspace` 通过；nm-store 8 + nm-node lib 9 + group_outbox 8 + cross_dm_outbox 6 测试通过；nmd + nm-admind release 构建链接通过。

**P1-8 审计日志 + 管理身份 + 登录限流**
- `bin/nmd/src/admin.rs`：新增 `audit(actor,action,target,ok,detail)`（结构化 `tracing` 事件，带 `audit=true` 字段，走既有订阅器，无新依赖）；10 个管理变更处理器（ban/unban/kick/add-peer/remove-peer/backup/domain-decision/domain-add/name-set/name-del）全部记审计，成功与失败都记。actor 取自 nm-admind 转发的 `X-Admin-Operator` 头（nmd token 本身匿名）。
- `bin/nm-admind/src/auth.rs`：登录**失败滑动窗口限流**（5 分钟内 5 次失败即锁定，挡暴力破解；成功清零）；login/logout/change_password 成功失败全部记审计（带来源）。`login()` 增 `src` 参数。
- `bin/nm-admind/src/main.rs`：`login` 取 `X-Forwarded-For` 作来源（直连记 `direct`）；`proxy_post` 转发 `X-Admin-Operator: admin` 给 nmd（审计归因到具名操作员）。

**P1-9 Prometheus/OpenMetrics + 送达时延 + 错误率**
- `crates/nm-node/src/lib.rs`：新增 `LatencyHist`（固定 11 个 le 毫秒桶 + sum + count，`record`/`snapshot` 累积语义）、`ConnRejects`（banned/revoked/server_busy/rate_limited 四计数）；两者作 `Arc` 字段挂 `Node`+`Ctx`。
- `crates/nm-store/src/lib.rs`：新增只读点查 `outbox_first_ms` / `group_outbox_first_ms`（ack 到达时算 send→ack 时延，不改既有 `outbox_del`/`ack_node`/`ack_batch` 签名，零破坏）。
- `crates/nm-node/src/federation_runtime.rs`：DirectAck 清条目时记 DM 时延；GroupAck 清条目（末个成员 home ack）时记群扇出 **tail** 时延。
- `crates/nm-node/src/protocol.rs`：accept 门四处拒绝各自计数。
- `crates/nm-node/src/admin_api.rs`：`delivery_hist_snapshot` / `conn_rejects_snapshot` 访问器。
- `bin/nmd/src/admin.rs`：新增 `GET /metrics`（OpenMetrics 文本，**免鉴权**——admin 口默认绑 127.0.0.1，Prometheus 抓取无需 token），导出到达率/时延直方图/连接拒绝/死信/限流/换网/流量/各表 gauge/进程。
- **设计取舍**：群消息多成员时，清条目=末个 ack → 记的是 send→**last**-ack 的扇出 tail 时延（SLA 有意义的那个）；批量 ack 臂不逐条记时延（清后 first_ms 不易取），只走权威的单 GroupAck 臂。

**P1-10 配置热加载（SIGHUP + POST /reload）**
- `bin/nmd/src/main.rs`：新增 `reload_config_from(cfg_path,node)` 共享应用函数——**热应用**黑名单(diff ban/unban)、per_topic、pkarr/dns url、域名；SIGHUP 哨兵（`SignalKind::hangup()`）调之并记审计。
- `bin/nmd/src/admin.rs`：`AppState` 加 `config_path`；`serve()` 加参数；`main.rs` 调用处传 `args.config`；新增 `POST /reload`（鉴权）。
- **诚实边界**：relay URL / bind_port / identity / db / mode / max_connections / membership.* 因 iroh `Endpoint` 为构建期绑定、或值经 `Ctx` 快照进 accept 环，**无法热改**——reload 对这些只在摘要里提示"需重启"，不假装生效。真要换 relay 仍走重启/自重启。

**改动文件**：`crates/nm-node/src/{lib,node_runtime,federation_runtime,protocol,admin_api}.rs`、`crates/nm-store/src/lib.rs`、`bin/nmd/src/{admin,main}.rs`、`bin/nm-admind/src/{auth,main}.rs`。

**待验证（部署后）**：`curl http://127.0.0.1:9611/metrics` 看 OpenMetrics 输出；`curl -XPOST .../reload` + `kill -HUP <nmd>` 看热加载审计日志；连错密码 5 次看 nm-admind 登录锁定。

---

## 附录 D：P1-6 多设备已读状态同步实施记录（2026-10-20，已落地）

后端机制完成；客户端 UI（未读角标归并）为消费方后续接入。`cargo check --workspace` 通过；nm-store 新增 `read_state_monotonic_and_scan` 测试通过（nm-store 共 9 项）。

**模型**：后端只做「账号→会话→已读时刻」的**单调 KV + 自有设备扇出**，对 `conversation` 视作 32 字节不透明键，语义由客户端定义（单聊=对端 EntityId，群=group_id）。未读数由客户端据已读位派生。

- **proto**（`crates/nm-proto/proto/nm.proto` + `src/lib.rs` 导出）：`ReadMarker{conversation, read_up_to_ms}`、`ReadMarkerList{repeated markers}`。
- **store**（`crates/nm-store/src/lib.rs`）：新表 `READ_STATE`（key=account(32)++conversation(32)，value=last_read_ms(8BE)）；`read_state_set`（单事务 RMW，**仅当新值>旧值才写**，返回是否推进）；`read_state_all`（按账号前缀扫）。
- **node**（新模块 `crates/nm-node/src/read_sync.rs` + `protocol.rs` 两个 dispatch 臂）：
  - `read.mark`：持久化（单调）→ 若推进则构造 `kind=StateSync`（=41，复用既有空枚举值）、`payload.type_url="nmspace.v1/read.marker"` 的系统 gram，经 `deliver_account(caller, …, store_offline=false, skip=本设备)` 扇到账号**其它在线设备**（离线不入库，避免信箱膨胀）。
  - `read.list`：返回账号全部已读位，供客户端登录后追平离线期间别设备的已读变化。
- **nm-client**（新 `crates/nm-client/src/session_read.rs`）：`Session::read_mark(conversation, read_up_to_ms)` / `Session::read_list() -> Vec<([u8;32], u64)>`，使协议可被客户端调用。

**安全**：`caller` 为已鉴权账号（device.hello 后 principal=账号），只能改自己的已读态、只扇给自己的设备；跨账号不可达。

**消费方待接入（Tauri app / clients）**：① 用户读某会话时调 `read.mark`；② 处理入站 `StateSync` gram（`type_url=nmspace.v1/read.marker`）→ 推进本地未读；③ 登录后调 `read.list` 追平；④ 由已读位派生未读角标（替换现有纯本机 per-device `unread`）。

**改动文件**：`crates/nm-proto/proto/nm.proto`、`crates/nm-proto/src/lib.rs`、`crates/nm-store/src/lib.rs`、`crates/nm-node/src/{read_sync.rs(新),lib.rs,protocol.rs}`、`crates/nm-client/src/{session_read.rs(新),lib.rs}`。

### 附录 D 续：桌面 app 消费端已接入（2026-10-20）

P1-6 后端机制的桌面消费端（`clients/app`）已接上，已读状态真正跨设备可见。`cargo check`(nmspace-app) 通过；4 个 UI js `node --check` 通过。

- **src-tauri**：`social.rs` 加 `read_mark(conv, read_up_to_ms)` / `read_list()` 两个 tauri 命令（调 `Session::read_mark/read_list`，`send_with_retry`/`session_of`）；`runtime.rs` 注册；`connection.rs` 入站 gram 泵在通用消息分支**之前**加 `type_url=="nmspace.v1/read.marker"` 分支 → `ReadMarker::decode` → emit `core://event {type:"read", conversation, readUpToMs}`（需 `use prost::Message;`）。
- **ui/js**：`im.js` 加 `READ_UPTO` map + `convLastTs`/`syncRead`(去抖 400ms)/`applyReadMarker` helpers；`imStart` 重置 READ_UPTO + 登录后 `read_list` 追平；开会话三处(selectContact/openGroupChat/openChannel)调 `syncRead`；`im_events.js` onCoreEvent 加 `read` 事件分支→applyReadMarker，并把未读自增改为「正在看→syncRead；没看且 `m.ts>READ_UPTO` 才++」；`im_tray_bridge.js` imMarkRead/imMarkAllRead 调 syncRead。
- **链路**：开会话→syncRead→read_mark→节点单调写+扇 StateSync 给在线他设备→他设备 pump emit read 事件→applyReadMarker 清角标；离线设备登录 read_list 追平。
- **未做**：`clients/mobile`（镜像结构，独立项目，另行接入）；端到端双设备实测需手动 QA（编译+语法+逻辑已校验）。

---

## 附录 E：P2 自包含快赢实施记录（2026-10-20，已落地）

P2 架构级项（presence 作用域化 / 真 DHT / 跨联邦）仍为待设计文档；本批只做两个自包含、不动现网拓扑的快赢。`cargo check --workspace` 过；nm-proto `version_compat` + nm-crypto `invite_sign_verify_tamper_expiry` 测试过；nm-node 回归 + app 编译过。

**协议版本握手（让 PROTOCOL_VERSION 真参与）**
- `crates/nm-proto/src/lib.rs`：新增 `MIN_SUPPORTED_VERSION` + `version_compatible(peer)`（`0`=旧端未声明容忍；否则须 ∈ `[MIN, PROTOCOL]`，过旧/过新都判不兼容）+ 单测。
- `crates/nm-node/src/protocol.rs`：`handle_conn` 回包路径加版本门——不兼容即回明确 `nmspace.v1/error` 回包（客户端 `route_send` 已会把它转成可读 toast），不再静默乱处理。现网全 v1 → 不受影响；未来升 v2 破坏性变更时 v1↔v2 互相明确拒绝。
- `bin/nmd/src/admin.rs`：`/identity` 吐 `protocol_version`/`min_supported_version` 供自检。

**邀请链接（后端铸造签名入网凭证）**
- `crates/nm-proto/proto/nm.proto`：`Invite{federation, seed_node, relay_urls, pkarr_url, group_id?, channel_id?, expires_ms, nonce, issuer, sig}` + lib.rs 导出。
- `crates/nm-crypto/src/lib.rs`：`sign_invite`(节点私钥签，填 issuer+sig)/`verify_invite`(有效期+issuer 验签)——镜像设备证书长度前缀域分隔签名；+ 篡改/过期/冒充单测。
- `crates/nm-node/src/invite.rs`(新)+protocol dispatch：`invite.create` 命令——客户端填联邦/relay/pkarr/群频道/有效期，节点盖 `seed_node=本节点`+nonce+签名后返回。节点只对"自己作种子"签名背书；其余字段客户端自备并一并纳入签名防篡改。
- `crates/nm-client/src/session_invite.rs`(新)：`Session::invite_create(fed, relays, pkarr, group?, channel?, expires_ms) -> Invite`。
- **消费方待接入**：客户端生成链接（base64(Invite) 嵌 `nmspace://invite/…`）；打开链接 → `verify_invite` → 配 relay/pkarr/种子 → 连接 →（可选）join 群/订阅频道。零配置入网链路。

**改动文件**：`crates/nm-proto/{proto/nm.proto,src/lib.rs}`、`crates/nm-crypto/src/lib.rs`、`crates/nm-node/src/{invite.rs(新),lib.rs,protocol.rs}`、`crates/nm-client/src/{session_invite.rs(新),lib.rs}`、`bin/nmd/src/admin.rs`。

### 附录 E 续：邀请链接桌面消费端已接入（2026-10-20）

P2 邀请链接的桌面 app 消费端（生成 + 打开）已接上。`cargo check`(nmspace-app) 过；4 js `node --check` 过。

- **src-tauri**（`social.rs` + `runtime.rs` 注册）：`invite_create(group_id?, channel_id?, ttl_secs)` → 从 `last_connect` 取 selfhost 配置 + 调 `Session::invite_create` + URL-safe-base64 → 返回 `nmspace://invite/<b64>`；`invite_parse(link)` → base64 decode + `nm_crypto::verify_invite`（离线验签，无需连接）→ 返回校验过的字段 JSON；`group_join(group_id)` → `Session::group_join`（补齐"自己加群"命令，原来只有管理员 `group_add`）。
- **ui/js**（新 `invite.js` + 4 处接线）：`window.Invite = {applyLink, generate, generateAndCopy, openFromPaste, consumePending}`。打开流程(登录前)：粘贴 → `invite_parse` 验签 → 写 `nm-relay-urls`/`nm-pkarr-url`（自动持久化）+ `NodeSvc.put` 种子为 selfhost 服务 → 暂存群/频道待连后加入 → 提示照常登录（login 即 connect，`infra()` 读到刚写的配置）。连接就绪(`imStart` 末)→ `consumePending` 自动 `channel_sub` / `group_join`。生成流程(已登录)：群详情"邀请链接"按钮 → `generateAndCopy({groupId})` → 复制 `nmspace://invite/…`。登录屏加"打开邀请链接"入口。
- **设计取舍**：遵 recon 的 option (a)——打开邀请只落地配置+种子，不自驱连接（新用户无本地身份，须走登录表单）；群现在可经 `group_join` 自助加入（邀请自动加入其 group_id）。
- **未做**：OS deep-link（`nmspace://` 系统级唤起，需 tauri onOpenUrl 插件）——目前靠手动粘贴；`clients/mobile` 镜像；双机端到端实测需手动 QA。

**改动文件**：`clients/app/src-tauri/src/{social.rs,runtime.rs}`、`clients/app/ui/index.html`、`clients/app/ui/js/{invite.js(新),account.js,im.js,groups.js}`。

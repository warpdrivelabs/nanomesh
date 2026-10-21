# Presence 去火管化（P2-11）：从全联邦 O(N) 广播到按 home 拉取

> 方案稿（评审后动手）。对齐 [`FEDERATION_SCALING_DESIGN.md`](./FEDERATION_SCALING_DESIGN.md) §9 灰度迁移习惯。
> 范围：**presence**。membership 的 O(N) 退火与发现强耦合，归 P2-12（真 DHT / 部分视图），本文只给它加一个**节点级能力位**、不动其 overlay。

## 0. 一句话

presence 现在是**三条常驻全联邦 overlay 之一**：每节点每 10s 把本地每个在线账号的卡片广播给**全联邦每个节点**，人人收人人存。但 `presence.query` 本就是个**拉取**命令，客户端本就每 15s 只查自己 roster 的那批 id。火管缓存的是「全网」，消费的只是「我的联系人」——**纯浪费**。方案：**查询时按 home 直拉**（复用现有 `presence.query` + dial-by-key），退掉 presence overlay。

## 1. 现状（含 file:line 证据）

| 环节 | 位置 | 机制 |
|---|---|---|
| 发布 | `crates/nm-node/src/presence.rs:143` `spawn_presence` | 10s tick（`PRESENCE_INTERVAL_SECS=10` `:7`），遍历**本地所有在线 session→账号**，每个发一张 `PresenceAnnounce{entity,status,ts,home_node,sig}`（JSON，`:23-30`），home 私钥签名（`:195`），publish 到单一主题 |
| 主题 | `presence.rs:55` | `"nmspace-presence:{federation}"` → `content_hash` 32B，**整个联邦一个 overlay** |
| 接收 | `presence.rs:210-234` → `on_presence_msg` `:97-140` | JSON 解 → 黑名单 → `verify_bytes` 对 `home_node` 验签（`:116`）→ ±300s 窗 → 本地权威覆盖 → LWW(ts) → 写内存 |
| 存储 | `presence.rs:19` `PresenceMap=DashMap`，`lib.rs:191` | **无 redb 表，纯内存**。读 TTL 35s（`:8`、`presence_status:33-50`），retain 350s 硬扫（`:176`） |
| 查询 | `protocol.rs:538-556` `"presence.query"` | 入 `{ids:[hex]}`（≤1024），**只读本地缓存** `presence_status`，无跨节点 RPC；出 `{hex:status}` |
| 客户端 | `clients/app/ui/js/im.js:228 refreshPresence` → 每 15s | 对 `mergedEntities()`（roster+directory）批量 `presence_query({ids})`——**本就只关心自己的联系人** |
| 启动 | `bin/nmd/src/main.rs:536`（`if cfg.membership.enabled`）| 与 membership(`:530`)/group_sync(`:540`) 一起**无条件 join**三条 overlay |

**代价**：`O(节点数 × 本地在线账号)` 条广播 / 10s，全联邦每节点都收都存。5 节点现网持续在跑。

## 2. 关键洞察（为什么能便宜地退掉）

1. **消费侧本就是拉**：`presence.query` 读缓存，客户端 15s 轮询自己 roster。火管只是「替缓存预热全网」。
2. **home 本就权威**：`presence_status` 先查**本地 `account_online`**（session/principals/devices）。→ **任意节点被问到「它自己的本地账号」都能不依赖火管直接答对**。这正是灰度安全的根：拉取对**老节点**也成立（老 home 收到 `presence.query` 照样本地权威作答）。
3. **无存储迁移**：presence 纯内存，退火不碰 redb、无 schema 风险。
4. **account→home 可解析**：directory 实体带 `home_node`（fed.sync 复制，`directory.rs`/`protocol.rs:495`），presence 缓存 rec 也带 home。DM 投递早已做 account→home 路由。
5. **状态意图（away/busy/dnd）天然对**：intent 存在 home 的 `status_intent`，`presence_status` 在 home 侧合成最终态——拉取拿到的就是终态。

## 3. 方案选型

| 方案 | 机制 | 取舍 |
|---|---|---|
| **① 按 home 拉取（推荐）** | 查询时把非本地 id 按 home 分组，dial-by-key 对每个 home 发 `presence.query`，合并+短缓存；退掉 overlay | ✅ 最自包含（复用 `presence.query`+dial+directory，零新主题/新 proto）✅ 不触 §11 多主题开销 ✅ 升级完 5 节点即可彻底关火管。△ 拉模型（延迟=轮询间隔，但现火管也是 10s，无回归）△ 热门 home 承压→靠按 home 批量+缓存化解 |
| ② 每主体一主题 | `nmspace-presence-id:<subject>`，home 发布，联系人 join（镜像 §6 inbox） | ✅ 推送低延迟 ✅ 贴 F 系列架构。❌ **主题爆炸**（§11 风险①）：节点 join 数=roster 并集，几百联系人=几百 overlay，presence 低价值高频，性价比差 |
| ③ 兴趣注册推送 | 联系人向 home 注册「订阅 S」，home 维护订阅集、变更时直推 | ✅ 推送、按真实兴趣伸缩、无主题爆炸。❌ 代码最多（注册协议/订阅集状态/TTL/换网重注册/事件化 presence 变更）——不符「自包含快赢」 |

**选 ①**。② 作为未来若需「实时推送」再议；③ 是 ① 的推送式升级，可后置。与我选 P2-11 时的定性一致：最自包含、无新基建、直接惠及 5 节点。

## 4. 详细设计（方案①）

### 4.1 拉取路径（核心，`protocol.rs "presence.query"`）
入参扩展：`{ids:[hex], local_only?:bool}`。
- `local_only=true`（**节点→home 的内部调用**才带）：维持现状——只读本地 `presence_status`，**绝不再向外拉**（防递归）。
- 缺省（客户端/账号会话调用）：**scatter-gather**
  1. 分流 ids：`home == self` → 本地 `presence_status` 直答；其余按 `resolve_home(id)` 分组。
  2. 对每个远端 home H：先查本地 presence 缓存，命中且新鲜（`now-last_seen ≤ PULL_TTL`，取 ~20s，介于客户端 15s 轮询与现 35s 读 TTL 之间）→ 用缓存；否则把「homed 于 H 的那批 id」**一次** `rpc_over(H, "presence.query", {ids:[...], local_only:true})`，**每 home 超时独立**（如 2s），失败不拖垮整查询。
  3. 回填缓存：把拉回的 `{id:status}` 写进现有 `presence` DashMap（`PresenceRec{status,last_seen:now,home_node:H}`），**复用** `presence_status`/TTL/retain，无需新表；随后统一按 `presence_status` 产出，老缓存/离线兜底。
  4. 合并返回 `{hex:status}`，形状不变——**客户端零改动**。

`resolve_home(ctx, id) -> Option<[u8;32]>`：① MemDirectory/`ENTITIES` 实体 `home_node`（主）；② presence 缓存 rec.home_node（次）；③ 未知 → 迁移期回落火管缓存，否则 `offline`。

> 不新增 proto/method：节点→home 复用 `presence.query` 自身（`local_only` 守卫递归）。唯一新逻辑是**查询端的 scatter-gather + resolve_home + 缓存**。

### 4.2 退掉 overlay（`presence.rs` / `main.rs`）
`[federation] presence_scoped`（`bool`，`AtomicBool`，**热加载**，镜像 `set_per_topic`）：
- `false`（迁移期缺省）：`spawn_presence` 照常 join+发+收（老消费者仍靠火管）。拉取路径**始终开**（更准、全网安全）。
- `true`：`spawn_presence` **不 join overlay**（不发不收），presence 全走拉取。`main.rs:536` 处据此决定是否 spawn。

### 4.3 能力位（Phase B，面向混版/公网自动退火）
节点级能力，挂 **membership announce**（每节点无条件 announce，是比 group-announce 更普适的载体；group-announce 对无群节点漏报）：
- `MembershipAnnounce`/`PeerInfo`（`membership.rs:8-56`）加 `presence_scoped:bool`（**不入签名字节**：非安全关键，谎报的最坏后果是你多留一会儿火管或照常拉取，均安全）。
- `on_membership_msg`（`:201-234`）镜像 `per_topic_nodes` 的 `insert/else remove`（`federation_runtime.rs:162`）填 `presence_scoped_nodes: DashSet`（`lib.rs` 新字段 + `node_runtime.rs` init）。
- **自动退火**：当 `peer_info` 中**所有** peer 都 `presence_scoped=true` 时，节点自动离开 presence overlay；出现老 peer 则自动重 join（`else remove` 自愈）。`[federation] presence_scoped` 作强制覆盖闸（运维免等）。

### 4.4 观测（`admin.rs /system`）
`presence_pulls`、`presence_pull_cache_hits`、`presence_pull_homes`（本轮触达 home 数）、`presence_overlay_joined:bool`、`presence_pull_errors`。

## 5. 迁移与兼容（§9 灰度，不停机）

**混版正确性**（关键论证）：拉取永远问「账号的 home」，而**任何节点对自己本地账号都权威作答**（§2.2，老节点亦然）→ 拉取对 old/new 都成立。火管仅剩一个消费者：**老节点**对非本地账号的查询读火管缓存。故：
- 新节点：拉取开 → 自身查询不再依赖火管；可随时停**收**。
- `presence_scoped=true`（停**发+收**）**只在无老消费者时安全**：Phase A 由运维在「5 节点全升级后」手动翻；Phase B 由能力位自动判定。
- 5 节点现网（全自有）：升级 5 台 → 翻 flag（或 Phase B 自动）→ 火管即刻消失，纯拉取。

## 6. 分阶段落地

- **Phase A（现网拿分）**：§4.1 拉取路径 + §4.2 `presence_scoped` 热 flag + §4.4 观测。升级 5 台、翻 flag → 退火完成。*（strictly 更准、全网安全，可单独上。）*
- **Phase B（公网安全自动）**：§4.3 membership 能力位 + `presence_scoped_nodes` + 全员自动退火/重 join。
- **Phase C（收尾）**：可选把 `directory.query` 的 presence 盖戳（`protocol.rs:495`）也走拉取解析器，避免非本地实体显示 offline；文档与 e2e 固化。

## 7. 风险与边界（诚实）

| 项 | 处置 |
|---|---|
| 热门 home 承压（N 节点各 15s 拉） | 按 home 批量（一 RPC 覆盖该 home 全部 id）+ 查询端 `PULL_TTL≈20s` 缓存，塌缩 N 客户端×轮询→~1 RPC/home/TTL；home 侧 `presence.query` 本就 O(ids) 极廉 |
| home 不可达 | 每 home 独立超时；回落上次缓存或 `offline`；不拖垮整查询 |
| account→home 解不出 | directory 主 + presence 缓存次 + 迁移期火管兜底；roster 联系人几乎必在 directory |
| 换网/漫游 | home 经 relay 稳定可达（selfhost relay 前提）；dial-by-key 复用 [[network-roaming-recovery]] 重连 |
| `directory.query` presence 盖戳退化 | 非本地实体本地缓存可能空→显示 offline；客户端另有 presence_query 批量兜底；Phase C 可修 |
| **不解决** | membership 仍 O(N)（=P2-12 DHT/部分视图）；directory 的 fed.sync 复制（另一机制，不在 P2-11）；presence 仍非 E2E（元数据对 home 可见，P1-5） |

## 8. 验证

- **单测**：`resolve_home` 优先级；scatter 按 home 分组 + `local_only` 防递归；`PULL_TTL` 缓存命中/过期；能力位 `insert/else remove`。
- **`#[ignore]` e2e**（镜像 `cross_group_ack_e2e.rs`，2–3 节点）：A 查 B 上在线账号的 presence，在 **overlay 关闭**（`presence_scoped=true`）下仍得 `online`（证不依赖火管）；B 下线后 A 于 `PULL_TTL` 内转 `offline`；混版：老节点 overlay 开着仍能见新节点 presence。
- **观测**：`/system` `presence_pulls` 增长、`presence_overlay_joined` 可翻、`presence_pull_errors` 不飙。
- **不提交**（遵循「改代码不提交」约定，落地后待你指示再提交）。

## 9. 编辑点

| 文件 | 改动 |
|---|---|
| `crates/nm-node/src/protocol.rs` | `"presence.query"`：加 `local_only` 守卫 + scatter-gather（分流/按 home 批量 rpc/缓存回填/合并） |
| `crates/nm-node/src/presence.rs` | `resolve_home` 辅助；拉取回填复用 `presence` DashMap；常量 `PULL_TTL`、`PULL_HOME_TIMEOUT` |
| `crates/nm-node/src/presence.rs` + `bin/nmd/src/main.rs:536` | `presence_scoped` 为真时 `spawn_presence` 不 join overlay |
| `crates/nm-node/src/lib.rs` + `node_runtime.rs` | `presence_scoped: AtomicBool`、`presence_scoped_nodes: DashSet`（Phase B）+ init |
| `crates/nm-node/src/membership.rs` + 相关 proto/JSON | （Phase B）`MembershipAnnounce`/`PeerInfo` 加 `presence_scoped`；`on_membership_msg` track |
| `bin/nmd`（config + `reload_config_from`）| `[federation] presence_scoped` 读取 + 热加载（镜像 per_topic）|
| `bin/nmd/src/admin.rs` | `/system` presence 拉取指标 |

## 附录 A：Phase A 实施记录（2026-10-21，已落地，未提交/未部署）

`cargo check --workspace` **0 errors**；`cargo test -p nm-node --lib` **10/10**（含新增 `presence::tests::parse_params_local_only_and_ids` + 既有 `group_fully_per_topic_predicate` 等未回归）；`cargo test -p nm-store` **9/9**。拉取路径**始终开**（更准、全网安全、火管在时缓存热→几乎不拨）；`[federation] presence_scoped` 默认 **false**（迁移安全）。

- **nm-node/src/presence.rs**：常量 `PRESENCE_PULL_TTL_SECS=20`、`PRESENCE_PULL_HOME_TIMEOUT_MS=2500`；`PresenceStat{pulls,cache_hits,errors}`；`Node::set_presence_scoped/presence_scoped_on/presence_stat_snapshot`；`spawn_presence` 重写为 `Option<ChannelTopic>`——退火开关热生效（开→`topic.take()` 不 join/发/收、2s 空转待关回/关闭；关→join+广播+收播+换网 epoch/0 邻居自愈如旧）；`parse_presence_params`（+单测）；`presence_query_handle`（`local_only` 守卫递归 / scatter：本地在线账号直答、远端按 home 分组、全缓存新鲜免拨、否则 `s2s_presence_query` 拉取并回填 presence 缓存、无 addr/失败回落缓存/离线）；`resolve_home`（directory `home_node` 主、presence 缓存 home 辅）。
- **nm-node/src/s2s.rs**：`s2s_presence_query(ep,addr,ids)`——dial home → `presence.query {ids,local_only:true}` → `CommandResult`→`HashMap<hex,status>`（镜像 `s2s_blob_get`；复用私有 `s2s_request` 的 10s 兜底，外层另有 2.5s/home）。
- **nm-node/src/protocol.rs**：`"presence.query"` 臂改为 `presence_query_handle(ctx,&cmd).await`；删除失效的 `hex_decode_n` import。
- **nm-node/src/lib.rs**：`Node` +`presence_scoped:Arc<AtomicBool>`+`presence_stat`；`Ctx` +`presence_stat`；`use presence::PresenceStat`。
- **nm-node/src/node_runtime.rs**：init `presence_scoped=false`+`presence_stat`；`ctx()` clone `presence_stat`。
- **bin/nmd/src/main.rs**：`FederationCfg` +`presence_scoped:bool`（`#[serde(default)]`=false）+ `Default`；spawn 前 `node.set_presence_scoped(...)`；`reload_config_from` 热应用 + changes 记录（镜像 `per_topic`）。
- **bin/nmd/src/admin.rs**：`/system` +`presence_scoped`/`presence_pulls`/`presence_pull_cache_hits`/`presence_pull_errors`。

**混版安全性复核（已编码对齐）**：任意节点对「自身本地在线账号」`presence_status` 本地权威作答（`account_online` 优先）——新查询端永远问账号 home、home（新旧皆然）本地权威应答；`local_only` 由 `s2s_presence_query` 恒置 true，home 侧 `presence_query_handle` 短路为纯本地读，无递归。火管在 `presence_scoped=false` 期间照常双写，老消费者无感。

**未做（后续阶段）**：Phase B（membership announce 加 `presence_scoped` 能力位 + `presence_scoped_nodes` 镜像 `per_topic_nodes` 的 insert/else-remove + 全员自动退火/重 join）；Phase C（`directory.query` 的 presence 盖戳改走同一拉取解析器；2–3 节点 `#[ignore]` e2e——overlay 关下拉取得真态）。行为级 e2e 留部署前手动 QA（`cargo test -p nm-node` 全量因既有 `app_flow`/`real_seed` 漂移不编译，故用 `--lib`）。**未提交**（遵循「改代码不提交」约定）。

## 附录 B：Phase B 实施记录（2026-10-21，已落地，未提交/未部署）

`cargo check --workspace` **0 errors**；`cargo test -p nm-node --lib` **11/11**（新增 `presence::tests::auto_retire_rules`）。自动退火落地：节点在 membership announce 广告 `supports_presence_scoped`（编译期能力位），收端按能力追踪，近期活跃的 peer **全部**支持时自动退 overlay、出现活跃老 peer 自动回退。

- **nm-node/src/membership.rs**：`MembershipAnnounce` +`supports_presence_scoped`（`#[serde(default)]`，双向兼容：老节点缺字段=false、忽略我方多余字段；**不入签名**——非安全关键）；`membership_card` 置 `true`（编译期能力）；`on_membership_msg` 验签+反重放后按能力 `insert`/`else remove` `presence_scoped_nodes`（镜像 `per_topic_nodes`）。
- **nm-node/src/presence.rs**：常量 `PRESENCE_PEER_ACTIVE_SECS=180`；`presence_retired()`=`presence_scoped`（强制）`|| compute_presence_retired(...)`（纯内核，单测）；`presence_scoped_peer_count()`；`spawn_presence` 退火判据由 `presence_scoped.load()` 改 `presence_retired()`（Phase A 热开关降为强制覆盖闸）。
- **nm-node/src/lib.rs** + **node_runtime.rs**：`Node` +`presence_scoped_nodes: Arc<DashSet<Vec<u8>>>` + init（镜像 `per_topic_nodes`）。
- **bin/nmd/src/admin.rs**：`/system` `presence_scoped` 改为**实际退火态**（`presence_retired()`），新增 `presence_scoped_forced`（配置强制闸）、`presence_scoped_peers`（已广告支持的 peer 数）。

**判据** `compute_presence_retired = forced || (∃ 近期活跃 peer 且其全部 ∈ capable)`。近期活跃=`now-last_seen ≤ 180s`：离线的手工/种子 peer（`last_seen` 陈旧）**不阻塞**退火（避免一台 seed 掉线即全网无法退火）；无任何活跃 peer → 不退（安全默认）。**灰度**：A/B 混版期 Phase A 节点不广告 → B 节点视其不支持 → 维持火管双写、全无感；全员升 B 后各自见「活跃 peer 全支持」→ 同步自动退火。`[federation] presence_scoped=true` 仍可强制提前退火。

**取舍（诚实）**：自动退火**无热「强制关」闸**——全员 B 后若要保留火管（调试/回滚）需降级到 Phase A 二进制（或令某 peer 不广告）；如需热 `presence_scope=off`，可把 Phase A 的 `presence_scoped:bool` 扩成三态（off/auto/on），已记在下一步。`presence_scoped_nodes` 同 `per_topic_nodes` 不随 TTL 清扫（churned peer 微量残留；⊆ 判定用近期活跃过滤已规避影响）。**未做**：Phase C（`directory.query` 盖戳走拉取 + 2–3 节点 `#[ignore]` e2e）。**未提交**。

## 附录 C：Phase C 实施记录（2026-10-21，已落地，未提交/未部署）

`cargo check --workspace` **0 errors**；`nm-node --lib` 11/11；**e2e `presence_pull_e2e` 实跑通过（0.78s）**——两端 overlay 退火下 A 按 home 拉取到 B 上 bob 的 `online`。P2-11 三阶段（A 拉取路径 + B 自动退火 + C 收尾）全落地。

- **按 home 拉取抽成并发 `presence_resolve(ctx, ids) -> HashMap<id,status>`**（presence.rs）：本地在线账号本地权威；其余按 home 分组——缓存新鲜免拨，否则 `tokio::task::JoinSet` **并发**向各 home `s2s_presence_query`（各自 2.5s 有界超时）→ 整体时延 ≈ 最慢 home 而非各 home 之和（directory.query 可能跨多 home 时的关键）→ 回填缓存。`presence_query_handle` 改薄封装（保留客户端 hex 原样键）；`directory.query`（protocol.rs）presence 盖戳改走 `presence_resolve`。
- **为何必需（correctness，非可选）**：桌面 `im.js mergedEntities` 对目录实体的 presence **优先于** presence_query 结果（`if(!c.presence)` 才用后者）。故 overlay 退火后若 directory.query 仍只读本地冷缓存 → 盖 `offline` → 盖过 presence_query 的拉取结果 → UI 误显离线。走 presence_resolve 后 directory 实体也得真态。overlay 在时缓存热 → 全命中、零拨号，行为同旧。
- **e2e** `crates/nm-node/tests/presence_pull_e2e.rs`（镜像 cross_group_ack_e2e，`#[ignore]`）：A/B 两持久节点互对等、**均 `set_presence_scoped(true)`**（overlay 退火）、`spawn_presence`（空转）+`spawn_federation_sync`（目录复制）；owner@A 查 bob@B → resolve_home=B → s2s 拉取 → `online`。`cargo test -p nm-node --test presence_pull_e2e -- --ignored` 通过。

**P2-11 收官**：presence 去火管化三阶段全落地+验证，**未提交/未部署**。剩余边界同前：presence 非 E2E（P1-5）、membership O(N)（P2-12）、无热 force-off（可三态化）。

<sub>Generated with dmxapi</sub>

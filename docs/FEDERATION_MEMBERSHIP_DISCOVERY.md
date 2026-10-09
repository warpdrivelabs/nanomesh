# 联邦成员发现去中心化方案（F6：成员发现 relay/锚点化，收敛成员频道 O(N²)）

> 背景：去火管（F0–F5，`per_topic` 默认开）已把**消息流量**从 `O(节点数 × 全量消息)` 收敛到「按参与的群/频道计」。规模天花板因此从消息层移到了**成员发现层**——所有节点仍 join 一条联邦级成员频道 `nmspace-federation:<fed>`、每 30s 全量广播签名卡片，应用层成本 **Θ(N)/节点、Θ(N²)/联邦**。本稿评估并给出去中心化成员发现的落地路线。
> 决策：**先做路线 R（relay/锚点，复用 F2 管线，~7–9 人日）**；真 DHT（路线 D，net-new ~12–17 人日）作后续、仅在要求「无任何弱中心」时才做。
> 这是一条**纯后端 crate 线**（`crates/nm-node`/`nm-store`/`nm-transport` + `bin/nmd`），不碰 `clients/app` 与 `clients/mobile`。承 [`FEDERATION_SCALING_DESIGN.md`](./FEDERATION_SCALING_DESIGN.md) §3（A+B 混合）、§7。本文为方案稿（评审后动手）。

## 0. 现状（代码实证）

成员发现 = 每联邦一条 gossip 主题 `nmspace-federation:<fed>`（`membership_channel`，`crates/nm-node/src/lib.rs:1028`），与群火管 `nmspace-groups:<fed>`、presence `nmspace-presence:<fed>` 并列但独立。

- **广播**：`spawn_membership`（`lib.rs:1156`）每 `announce_interval_secs`（默认 30s，floor 5s）`topic.publish(membership_card)` 一张签名卡片 `MembershipAnnounce{node_id, ts, info(name/address/email/mobile/gps), addr, sig}`（`lib.rs:116`、签名 `membership_signing_bytes:238`）。
- **接收**：`on_membership_msg`（`lib.rs:1051`）对每张卡片做 JSON parse + **ed25519 verify** + 防重放 + `learn_peer` upsert；每节点每周期收 **~N 张**。
- **成本**：每节点每周期 **Θ(N) 收 + Θ(N) 验签**；全联邦 **Θ(N²) 投递+验签**。`peers`/`peer_info`（`lib.rs:395/399`）无上限，靠 `sweep_discovered`（TTL 90s，`lib.rs:1135`）回收离线 discovered 节点。
- **消费契约（替换方案必须继续满足）**：
  1. **gossip bootstrap**：`peers_list()`（`lib.rs:900`）是 join 每个 overlay（成员/群/presence/F1 各 per-topic）的 bootstrap 种子集。
  2. **目录拉取 dial 列表**：`spawn_federation_sync`（`lib.rs:703`，每 15s）遍历全部 `peers` 做 s2s `fed.sync`（`fed_pull:3795`）。
  3. **admin/持久化**：`/peers`（`admin.rs:234`）读 `peers_detail`（`lib.rs:948`）；`persist_peer`（`lib.rs:1123`）落 redb，重启 `lib.rs:537` 回填。

## 1. 用词澄清：当前「DHT」= HTTP pkarr relay，非真 DHT

F2 群发现**不是** mainline/BitTorrent DHT。`Cargo.lock` 无 `pkarr`/`mainline` crate，只有 `iroh-dns 1.3`——其 `pkarr::SignedPacket` 仅作**编解码**（`<32 pubkey><64 sig><8 ts><DNS packet>`）。传输是手写 `reqwest` PUT/GET 打一个 relay URL（`[dns] url`）。即「群公钥 → seed 列表」记录由 home 节点签名 PUT、成员按群公钥 GET。本方案沿用此 relay 模型（路线 R）；真 DHT 见 §6。

## 2. F2 可复用件（高复用）

| 件 | 位置 | 复用方式 |
|---|---|---|
| 记录 codec | `pkarr_rec::build/parse`（`lib.rs:210-227`） | near-copy → `member_rec`（TXT 名 `_nmmember`，值=node_id 列表；`SignedPacket` 自带 ts/TTL） |
| 发布传输 | `pkarr_publish_group`（`lib.rs:817`） | PUT `{url}/{pubkey_z32}` 骨架照搬 |
| 解析传输 | `pkarr_resolve_group`（`lib.rs:848`） | GET + verify 照搬 |
| relay 配置 | `pkarr_url`/`set_pkarr`/`pkarr_endpoint`（`lib.rs:410,803-809`）+ nmd `[dns] url`（`main.rs:451`） | 记录无关，直接用 |
| cache+bootstrap 合并 | `pkarr_seeds` + `group_bootstrap`（`lib.rs:411,2308`） | 模板照搬为 `member_seeds` |
| 签名密钥 | **节点自身 identity**（`self.ep` secret） | 比群的 `GROUP_SECRETS` 更简单——跳过 redb 查 |

额外利好：iroh 的 `PkarrPublisher` 已把 `node_id → addr` 发到同一 relay（`crates/nm-transport/src/lib.rs:91-96`），故成员记录**只需带 `{federation, ts, 可选 status}`**，地址交给 iroh 解析。

## 3. 关键设计决策（评审重点，每条给推荐 + ❓待拍板）

**D1 · 枚举问题怎么破**　relay/DHT 都是纯 KV，无法「列出某联邦所有节点」。F2 的解法是「home 在**已知群公钥**下发 seed 列表」。成员发现等价解：**每联邦一条索引记录**，key = `blake3("nmspace-federation:<fed>")` 的 z32，value = `[近期活跃成员 node_id...]`，由**锚定节点**发布。新节点按联邦名 GET 即得 bootstrap 集。**推荐**：锚点发布联邦索引（对应 `FEDERATION_SCALING_DESIGN.md` §3 方案 B）。❓确认。

**D2 · 谁当锚点**　**推荐**：配置项 `[membership] anchor = true`（默认 false）；锚点常驻、周期发布联邦索引。少量锚点即可（可多副本，弱锚）。非锚点节点不发索引，只 GET。❓确认锚点由配置指定（非自动选举，M1 不做轮换/选举）。

**D3 · 成员频道是否保留**　两步走。**M1**：成员频道**照旧**，仅**叠加** relay 索引做冷启动 bootstrap（低风险、可灰度）。**M2**：把成员频道从「全网广播」收敛为「锚点维护成员目录 + 普通节点向锚点/按群弱扇出」，真正砍掉 Θ(N²)。**推荐**：M1 先上、灰度开关 `[membership] relay_index`，M2 再收敛。❓确认分两步。

**D4 · 索引记录的大小/分片**　DNS 包上限 1000B（`MAX_DNS_PACKET_SIZE`），一条记录装不下大联邦全部 id。**推荐**：索引只放「bootstrap 用的少量代表节点」（如锚点自己 + 最近活跃 N≤8，与 F2 `MAX_SEEDS=8` 一致），不追求全量；全量成员仍由 gossip 在 bootstrap 后收敛。❓确认索引是 bootstrap 种子而非全量名册。

**D5 · relay 单点**　单 relay = 单点故障。**推荐**：`[dns] url` 支持**多 relay**（逗号分隔或数组），发布写多个、解析轮询（对应文档 §3「弱锚多副本」）。❓确认多 relay。

**D6 · 回滚**　`[membership] relay_index = false` 即回到纯 gossip 成员频道（现状）。M1 叠加式、M2 收敛式都须保留此开关，回滚无损。

## 4. 落地分阶段（每阶段：交付 / 文件 / 测试 / 完成判据）

### M1 · relay 索引做冷启动 bootstrap（叠加，不改成员频道）
- **交付**：`member_rec` codec；锚点发布联邦索引（`publish_member_index`）；新节点解析索引 → 回填 `peers`（满足契约①）；灰度开关 `[membership] relay_index` + `anchor`。成员频道照旧并行。
- **文件**：`crates/nm-node/src/lib.rs`（新 `member_rec` 模块仿 `pkarr_rec`；`publish_member_index`/`resolve_member_index` 仿 `pkarr_publish_group`/`resolve`；驱动循环仿 `lib.rs:1585`；`member_seeds` cache + 并入 `peers_list`/各 `*_bootstrap`）；`bin/nmd/src/main.rs`（`[membership] anchor`/`relay_index` 配置 + 多 relay 的 `[dns] url`）。
- **测试**：`crates/nm-node/tests/member_relay.rs`——mock relay（复用 F2 的 axum mock 模式）：锚点 PUT 索引、新节点 GET 到 bootstrap 集、无 gossip 预种子也能 join；签名校验（篡改索引→拒）；多 relay 轮询。
- **完成判据**：一个**无任何 `[[peers]]` 预配**的新节点，仅凭 `[dns] url` + 联邦名，能经 relay 索引发现 bootstrap 并加入联邦收发；`cargo test --workspace` 绿；`relay_index=false` 行为 == 现状。
- **估时**：**~4.5 人日**（codec 0.5 / 发布 1 / 解析+引导 1 / 契约②③回填 0.5 / 多 relay 1 / 测试 0.5+）。

### M2 · 收敛成员频道（砍 Θ(N²)）
- **交付**：成员频道不再「每节点全网广播」；改为**锚点维护成员目录**（锚点订阅频道、聚合成员表、经 relay 索引或按请求下发），普通节点**只向锚点/按所在群**弱扇出心跳，不再 N 对 N 广播。`sweep`/TTL/presence 逻辑保留。
- **文件**：`crates/nm-node/src/lib.rs`（`spawn_membership` 广播循环改造：非锚点节点降低广播面；`on_membership_msg` 保留；新增锚点聚合路径）。
- **测试**：扩 `member_relay.rs` + 新 `member_scale.rs`——N(=5) 节点，断言非锚点节点**不再收到全部 N 张卡片**（收敛证明），但成员可见性/目录拉取/admin 计数仍正确。
- **完成判据**：N 节点下单节点每周期收到的成员卡片数 **≪ N**（上界与锚点数相关，非 N）；契约①②③ 全部仍满足；回滚开关有效。
- **估时**：**~3 人日**（收敛改造 1.5–2 / 测试 1）。

### 合计 **~7.5 人日**（M1 可独立交付、独立见效：解冷启动；M2 才真正降 O(N²)）

> 更省的**最小中间量（~2.5 人日）**：只做 M1 的前 3 块（codec + 锚点发布 + 解析引导），不做多 relay、不做 M2——先让「新节点零配置冷启动找到联邦入口」，成员频道暂留。适合先验证 relay 索引链路通不通，再决定是否投入 M2 收敛。

## 5. 迁移与兼容（灰度，不停机）
- `[membership] relay_index` 默认 **false** = 纯现状（gossip 成员频道），零行为变化。
- 置 true（M1）= **叠加** relay 索引;成员频道仍在，双路发现，回滚无损。
- M2 的成员频道收敛单独开关门控;未开则保持 M1 的双路。
- 锚点身份经配置显式指定;无锚点时 relay 索引为空，自动退化为纯 gossip（不劣于现状）。

## 6. 路线 D（真 DHT，net-new，后续）
仅当要求「无任何弱中心（无 relay、无锚）、全球无锚发现」时才做。接 mainline DHT（`mainline`/`pkarr` crate 带 DHT feature），`federation_pubkey`/`node_id → 记录` 发全球 DHT。
- **主要风险**：依赖冲突——`pkarr 8.x` 要 `ed25519-dalek ^3`，与 iroh 1.3 的 dalek 2 并行，F2 当初正是为避此才改用 iroh-dns 自带 codec。接真 DHT 须先排雷。
- **枚举问题仍在**：DHT 同样不能枚举联邦成员，仍需「按已知 key（联邦公钥）查」——即 §3 D1 的索引思路，只是传输从 relay 换成 DHT。
- **估时**：**~12–17 人日**（选型+依赖排雷 1–2 / DHT 集成 3–4 / 记录策略 2 / 契约回填 2 / 隐私女巫可用性 2–3 / 测试 2–3）。CI 难跑真 DHT。

## 7. 风险与回滚
1. **relay 单点/可用性**：D5 多 relay 缓解;全挂则退化为纯 gossip（M1 双路兜底）。
2. **锚点信任/下线/轮换/防女巫**（文档 §9 待决）：M1 配置指定、不选举;轮换/激励留到后续。
3. **索引新鲜度**：`SignedPacket` 自带 ts + TTL（300s），锚点周期重发;过期索引 GET 到旧 bootstrap 仍可用（gossip 会纠偏）。
4. **回滚**：任一开关置 false 即回现状，双路/叠加期无损。

## 8. 审核清单（需确认后再写码）
1. **路线**：先 R（relay/锚点），D（真 DHT）作后续 —— 已定。
2. **D1**：枚举问题用「锚点在联邦 key 下发 bootstrap 索引」解?（推荐是）
3. **D2**：锚点由 `[membership] anchor=true` 配置指定、M1 不做选举/轮换?（推荐是）
4. **D3**：分 M1（叠加 relay 索引）→ M2（收敛成员频道）两步?（推荐是）
5. **D4**：索引只放少量 bootstrap 种子（≤8），非全量名册?（推荐是）
6. **D5**：`[dns] url` 支持多 relay?（推荐是）
7. **起步范围**：全量 M1（~4.5d）还是先最小中间量（~2.5d，仅 codec+发布+解析引导）?

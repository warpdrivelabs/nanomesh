# 去中心发现（P2-12）：退「单中继」到多锚 + 真 Mainline DHT

> 方案稿（评审后动手）。对齐 [`FEDERATION_SCALING_DESIGN.md`](./FEDERATION_SCALING_DESIGN.md) §4（发现与引导：A=DHT/pkarr、B=见证/锚定，推荐 A+B 混合）。
> 范围：**发现/引导的去单点**。presence 去火管=P2-11（已落）；跨联邦/准入=P2-13。

## 0. 一句话

现在「找到某群/某联邦的第一个 peer」全靠**一个**配置的中继（`[dns] url`）做 pkarr HTTP PUT/GET——**单点**。方案分两阶段：**A** 把现有记录层从单中继扩成**多中继法定数**（零新依赖，惠及现网 5 节点）；**B** 加**真 Mainline DHT**（新依赖 `pkarr` crate），把本就是 `SignedPacket` 的记录发到公共 DHT，实现**无锚、trustless、群公钥不可枚举**的冷启动。

## 1. 现状（file:line 证据）

**依赖**：iroh 1.3.0 / iroh-dns 1.3.0 / iroh-gossip 0.101。**无 `pkarr` 独立 crate、无 `mainline`、无任何 DHT**（Cargo.lock 零命中）。唯一 pkarr 面是 `iroh_dns::pkarr::SignedPacket`——只是 relay-payload 编解码（BEP44 签名包），**不含 DHT 客户端**。

| 层 | 位置 | 机制 |
|---|---|---|
| **F2 群种子** | `nm-node/src/pkarr_runtime.rs` + `records.rs:21-47`(`pkarr_rec`) | home 把群 seed 节点集签成 `SignedPacket`（TXT `_nmspace`，group 私钥签，≤8 seed）`PUT {pkarr_url}/{group_pub.z32}`；成员 GET+按 group 公钥验。**`pkarr_endpoint()` 不拆逗号→实际假设单中继**（`pkarr_runtime.rs:12`）=单点 |
| **F6 成员索引** | `nm-node/src/membership.rs:97-179` + `records.rs:49-75`(`member_rec`) | anchor 把成员集签成 `SignedPacket`（TXT `_nmmember`，anchor 私钥签，`MAX_MEMBERS=8`）；`pkarr_endpoints()` **已拆逗号多中继**（publish 全发、resolve 首成功）`membership.rs:85-92` |
| **iroh 传输发现** | `nm-transport/src/lib.rs:82-133` | nat=`presets::N0`（n0 公共 relay+DNS/pkarr）；selfhost=`PkarrPublisher/Resolver`+`DnsAddressLookup`+自建 relay（**均指向单个 `[dns] url`/relay**）；永挂 `MemoryLookup` 做 dial-by-key |
| **gossip 引导** | `nm-federation/src/lib.rs:143-189`、`delivery.rs:11-92`、`admin_api.rs:255-260` | join 主题的 bootstrap ids = 群 `home_node` ∪ `ctx.peers`(手工/已学) ∪ F2 `pkarr_seeds[gid]`；0 邻居自愈重注入 |
| **配置** | `bin/nmd/src/main.rs` | `[dns] url: Option<String>`（**单个**,`:259-267`）；`[relay] url+urls`（selfhost,`:236-256`）；`[membership] anchor/anchors/relay_index`（`:188-194`,**非热加载**）；`set_pkarr` 热加载（`:720`） |

**根问题（§4 引导悖论）**：所有 bootstrap 源最终都归约为**已知 id**（home/手工 `[[peers]]`/已学）。一个**全新非成员**只知群/联邦公钥时，找第一个 peer 的**唯一非手工桥**是 F2/F6 记录——而它们都**只过那一个 `[dns] url`**。中继挂=新节点/新成员无法冷启动加入。

## 2. 关键洞察（为何 DHT 是自然延伸）

1. **记录天生 DHT-ready**：`_nmspace`/`_nmmember` 已是 `SignedPacket`（pkarr=「公钥可寻址资源记录」over Mainline，BEP44 ed25519 签名）。发到 Mainline DHT 与发到 relay 是**同一个包**换传输，无需改记录格式。
2. **key 即公钥、不可枚举**：group_id==群公钥==DHT key（z32）；anchor==节点公钥。不知公钥无法枚举/遍历群——正是 §4「群公钥不可枚举」目标，trustless 下天然成立。
3. **验证已 trustless**：resolve 后按**声明公钥**验 ed25519（`from_relay_payload`/`txt_records`），与来源无关——DHT 节点作恶无法伪造，与中继同等安全。
4. **invite 协同**：P2 邀请链接已带 `seed_node/relay_urls/pkarr_url`；DHT 落地后邀请可只带**群公钥**，收方从公共 DHT 解析一切——更强去中心（见 §5）。
5. **能力位先例**：`supports_presence_scoped`（JSON MembershipAnnounce，`serde(default)`、不签名、`presence_scoped_nodes` DashSet insert/else-remove）可镜像为发现能力位。

## 3. 方案选型

| 方案 | 机制 | 取舍 |
|---|---|---|
| **A 多中继法定数（推荐先行）** | 把 F2 群种子也走现有 `pkarr_endpoints()` 多中继（publish 全发、resolve 首成功/法定数）；`[dns]` 收 list；selfhost iroh 层也配多 relay/dns-origin | ✅ **零新依赖**，复用 member-index 已验证的多中继码 ✅ 现网跑 2–3 relay 即消单点 ✅ 可立即部署到 5 节点。△ 仍是「弱锚/见证」非 trustless；需运维多配几台 relay |
| **B 真 Mainline DHT** | 加 `pkarr` crate（带 `mainline` DHT client）；把现有 `SignedPacket` 额外 publish/resolve 到公共 Mainline DHT；解析源 = DHT ∪ relays ∪ 手工，合并 seed | ✅ **无锚 trustless**：只知群公钥即可冷启动，不依赖任何配置 relay ✅ 群公钥不可枚举 ✅ 与 invite 协同最强。❌ **新依赖 + 传递依赖**（mainline/udp）❌ DHT-behind-NAT：多数节点在 NAT 后**难作 DHT 服务端**→只做 resolve 源、publish 仍靠 relay 兜底 ❌ **兼容待验**：`iroh_dns::pkarr::SignedPacket` ↔ 独立 `pkarr` crate 的包是否 wire 兼容（都是 BEP44，大概率兼容但**必须实测** key 派生/bencode）❌ 延迟高于 relay |
| **C 只上 B** | 跳过 A 直接 DHT | ❌ 风险集中、无法增量；A 的多中继无论如何都该有（DHT 不可用时的兜底） |

**选 A→B 分阶段**（镜像 P2-11 的「自包含快赢先行，再深入架构」）。A 立即落地消单点、可部署；B 作 trustless 层叠加，灰度 + 实测兼容后再上。

## 4. 详细设计

### Phase A — 多中继法定数（自包含，零新依赖）
1. **F2 群种子多中继**（`pkarr_runtime.rs`）：`pkarr_endpoint()`→`pkarr_endpoints()`（复用 `membership.rs:85-92` 的逗号拆分，或抽到公共 helper）。`pkarr_publish_group` **发到每个** relay；`pkarr_resolve_group` **逐 relay 首成功**（可选法定数：≥⌈N/2⌉ 一致才采信，防单 relay 投毒——但记录已签名，投毒至多拒服务，故首成功足够 + 记录来源多样化）。channel 同理。
2. **`[dns]` 收 list**（`bin/nmd/src/main.rs`）：`DnsCfg` 加 `urls: Vec<String>`（保留 `url` 兼容），`all()` 合并去重（镜像 `RelayCfg`）；`set_pkarr` 传合并串（现有逗号协议）。热加载照常。
3. **iroh 传输层多 relay/dns**（`nm-transport/src/lib.rs:88-113`，selfhost 分支）：`PkarrResolver`/`DnsAddressLookup` 按多 origin 注册（iroh builder 支持多 discovery）；`RelayMode::custom` 已收 `relay_urls` 列表（`RelayCfg::all()`，已多）。
4. **观测**：admin `/system` 加 `pkarr_relays`（配置数）、`pkarr_resolve_ok/fail` 按源计数。
> 落地即**现网消单点**：部署 A + 运维加 1–2 台 relay（已有 relay.robot.link；再起 1 台独立网络）→ 任一 relay 挂不影响发现。

### Phase B — 真 Mainline DHT（trustless，叠加）
1. **依赖**：加 `pkarr` crate（启 `dht`/`mainline` feature）。**前置实测**：用 `pkarr::SignedPacket` 解析 `iroh_dns::pkarr::SignedPacket::to_relay_payload()` 产物（反之亦然），确认 wire 兼容；不兼容则统一改用独立 `pkarr` crate 产/验包（records.rs 换底，记录语义不变）。
2. **发布**：`pkarr::Client` 把 `_nmspace`/`_nmmember` 包 `publish` 到 Mainline（home/anchor，与 relay 双发）。**NAT 现实**：DHT 存储节点需 UDP 可达；home 多在 NAT 后→publish 经 pkarr 的 relay-to-DHT 桥或仅作 best-effort，relay PUT 仍为主路径。
3. **解析**：`pkarr_resolve_group`/`resolve_member_index` 的源扩为 **DHT ∪ relays ∪ 手工**，并发查、合并 seed、去重；任一命中即可引导 gossip join。
4. **无锚冷启动**：只知群公钥的新节点，无需任何 `[dns] url` 即可从公共 DHT 取 seed → join。与 **invite 链接**协同：邀请可降级为「仅群公钥 + 联邦名」。
5. **门控**：`[discovery] dht = false`（默认关，`AtomicBool` 热加载，镜像 `per_topic`/`presence_scoped`）；开则叠加 DHT 源。可选能力位 `supports_dht` 走 MembershipAnnounce（镜像 `supports_presence_scoped`）仅作观测/路由提示（发现无需协商——多源合并天然兼容）。
6. **观测**：`/system` 加 `dht_enabled`/`dht_publishes`/`dht_resolves_ok`。

## 5. 迁移与兼容（§9 灰度）
- **多源合并天然兼容**：解析永远是 DHT ∪ relays ∪ 手工的并集，任一源命中即引导；新增 DHT 源不改旧行为，旧节点只是没 DHT 源。无需严格协商。
- Phase A：部署即生效（多中继），`[dns]` 旧单 `url` 继续工作。
- Phase B：`dht=false` 默认→二进制行为中性；运维逐台开 `dht=true`，先 resolve 后 publish；全开后邀请可去中继。
- **invite 协同**：Phase B 后 `Invite` 可省略 `relay_urls/pkarr_url`（留空=走 DHT）；`verify_invite` 不变（签名自证）。

## 6. 风险与边界（诚实）
| 项 | 处置 |
|---|---|
| 新依赖 `pkarr`/`mainline`（传递依赖、审计面） | Phase B 才引入；先在 branch 验编译 + wire 兼容再并 |
| `SignedPacket` 两实现 wire 兼容未知 | **Phase B 前置实测**；不兼容则 records.rs 统一换独立 `pkarr` crate（语义不变） |
| DHT-behind-NAT：home 难作 DHT 服务端 | DHT 只做 **resolve 源**；publish 仍 relay 为主、DHT best-effort；不假设 home UDP 可达 |
| DHT 延迟/可用性 | 并发多源、relay 先回即用；DHT 慢到无妨（后台刷新 `pkarr_seeds` 缓存） |
| 单 relay 投毒 | 记录 ed25519 自验→投毒至多拒服务；多源 + 首成功已缓解；法定数可选 |
| 群枚举隐私 | 群公钥不可枚举（DHT key=公钥）；不泄露群列表 |
| **不解决** | iroh 传输层 node-addr 发现在 nat 模式仍依赖 n0 公共 infra（selfhost 才自托管）——另议；跨联邦=P2-13；成员准入=P2-13 |

## 7. 验证
- **Phase A 单测**：`pkarr_endpoints()` 多中继拆分；`pkarr_resolve_group` 首成功/全失败回落；`DnsCfg::all()` 合并去重（镜像 RelayCfg 测试）。
- **Phase A `#[ignore]` e2e**（镜像 `pkarr_mock.rs`）：双 mock relay，一个 503，resolve 仍从另一个取到 seed → 群 join 成功。
- **Phase B 前置**：一次性兼容实测脚本（iroh_dns 产包 ↔ pkarr crate 验包）。
- **Phase B `#[ignore]` e2e**：本地 mainline testnet（pkarr 提供 `Testnet`）两节点，无 relay 下 publish→resolve→join 成功。
- 现网：部署 A + 2–3 relay，admin `/system` `pkarr_resolve_ok` 多源增长、停一台 relay 不致发现失败。
- **不提交**（遵循「改代码不提交」约定），落地按阶段来。

## 8. 编辑点（预估）
| 文件 | Phase | 改动 |
|---|---|---|
| `nm-node/src/pkarr_runtime.rs` | A | `pkarr_endpoint`→多中继 publish/resolve |
| `nm-node/src/records.rs` | A/B | 抽公共多值 helper；B 若不兼容则换 `pkarr` crate 产/验 |
| `nm-node/src/membership.rs` | A | `pkarr_endpoints()` 抽公共（已多中继，复用） |
| `nm-transport/src/lib.rs` | A | selfhost 多 PkarrResolver/DnsAddressLookup origin |
| `bin/nmd/src/main.rs` | A/B | `DnsCfg.urls` list+`all()`；`[discovery] dht` flag+热加载 |
| `crates/nm-node/Cargo.toml` + 新 `dht.rs` | B | 加 `pkarr`(dht)；DHT publish/resolve 封装 |
| `bin/nmd/src/admin.rs` | A/B | `/system` 发现观测 |

<sub>Generated with dmxapi</sub>

## 附录 A：Phase A 实施记录（2026-10-21，已落地，未提交/未部署）

`cargo check --workspace` **0 errors**；`nm-node --lib` 11/11；**`pkarr_mock` 2/2（新增 `pkarr_multi_relay_failover` 实跑过）**——证 publish 写全部中继、首个中继宕机 resolve 仍 failover 成功（退单点），计数器正确。**part 3（iroh 传输层 node-addr 多 pkarr）亦已补**（见下），`selfhost_smoke` 单 URL 传输路径未回归。

- **`nm-node/src/pkarr_runtime.rs`**（重写）：`PkarrStat{resolve_ok,resolve_fail}`；`pkarr_endpoints()`（逗号多中继，pub(crate)，从 membership.rs 迁来统一）；`pkarr_put_all`（PUT **每个**中继）；`pkarr_resolve_key`（逐中继 GET+验签，**首个非空即返回**，ok/fail 计数）；`pkarr_publish_group`/`pkarr_resolve_group`/`pkarr_publish_channel` 改走多中继；`pkarr_relay_count`/`pkarr_stat_snapshot` 观测。首成功足够（记录 ed25519 自验，多中继仅做可用性冗余）。
- **`nm-node/src/membership.rs`**：删重复 `pkarr_endpoints`（迁至 pkarr_runtime，成员索引 3 处调用方自动复用统一实现）。
- **`nm-node/src/lib.rs` + `node_runtime.rs`**：`Node` +`pkarr_stat: Arc<PkarrStat>` + init。
- **`bin/nmd/src/main.rs`**：`DnsCfg` +`urls: Vec<String>` +`all()`（合并 url+urls 去重保序）/`joined()`；selfhost 传输用 `joined()`（part 3：逗号多中继透传传输层），`set_pkarr(joined())`（逗号多中继喂既有协议，群/频道/成员发现统一多中继），relay_index 门改 `all().is_empty()`，reload 同步。
- **`bin/nmd/src/admin.rs`**：`/system` +`pkarr_relays`（中继数,>1=已退单点）/`pkarr_resolve_ok`/`pkarr_resolve_fail`。
- **`crates/nm-transport/src/lib.rs`**（part 3）：`bind_infra` selfhost 分支把 `pkarr_url` **按逗号拆分**，逐个注册 `PkarrPublisher`+`PkarrResolver`（node-addr 发布/解析也多副本）。**零签名改动**（`bind_*_selfhosted(pkarr_url: String)` 全不变→nm-client/gateway/compute/**excluded nm-agent-bridge**/测试全不受影响；单 URL=原行为）。`selfhost_smoke` 过。

**部署即消单点**：运维在 `[dns]` 配 2–3 台独立中继（现有 relay.robot.link + 再起 1–2）→ 任一中继挂不影响群/频道/成员发现**与** node-addr 解析。向后兼容：单 `url` 行为不变。**未做**：Phase B（真 Mainline DHT，新依赖）。**未提交、未部署**。

<sub>Generated with dmxapi</sub>
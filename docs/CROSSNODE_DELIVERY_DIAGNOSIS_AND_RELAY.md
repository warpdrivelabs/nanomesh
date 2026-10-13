# 跨节点消息投递：系统性诊断与自建 Relay 方案评估

**日期**: 2026-10-13
**背景**: 连续多轮排查"跨节点私聊单向失效/收不到消息",每次定位到一个具体点、修一个又冒一个。本文把所有失效点串成一张完整的"消息从 A 到 B 必经的关卡"图,给出每关的失败模式,并论证根因是**节点间可靠互联**这一架构前提在当前部署下不成立,最后评估根治方案(自建 relay)。

---

## 1. 现状拓扑(事实)

| 节点 | 网络属性 | node id 前缀 | 角色 |
|---|---|---|---|
| mac 本机 | **NAT**,动态地址,Wi-Fi | `a78000cc` | 客户端 home + 跑 agent |
| cmxlinux | **NAT**(与 mac 同局域网) | `89e182a8` | 客户端 home |
| cmxdev18 | 公网出口 `183.242.167.134`(**与 46 共享出口 IP**) | `018640c4` | seed |
| cmxdev46 | 公网出口 `183.242.167.134`(**与 18 共享出口 IP**) | `d19e4931` | seed |
| robot.link | 阿里云公网 `8.152.101.120` | `e2432b` | seed + 客户端 home |

federation 统一为 `nmspace`,`per_topic = true`,全部 `mode = "nat"`(N0 公共 relay + DNS/pkarr 发现)。

**拓扑的三个先天难点**:
1. mac / cmxlinux 在 NAT 后(入站需打洞)。
2. cmxdev18 与 cmxdev46 **共享同一公网出口 IP**(`183.242.167.134`)——外部无法用 IP+端口区分两者,NAT 端口映射/回环(hairpin)复杂。
3. robot.link 在**另一个云网络**(阿里云),与前面两组之间要跨公网互联。

---

## 2. 消息从 A 到 B 必经的关卡(完整链路)

一条跨节点私聊 `用户X@节点A → 用户Y@节点B` 要成功,必须**逐关通过**。任一关失败 → 某个方向"收不到"。

```
①  X 的客户端 → 节点A         (客户端↔home 的会话)
②  节点A: is_home_here(Y)?    (判定 Y 是否本地；误判则投本地离线库并 return)
③  节点A → 节点B 的传输       (s2s 直连 / inbox gossip overlay / 火管)
④  节点B: 订阅了 Y 的 inbox 主题? (local_accounts → join_inbox)
⑤  节点B: deliver_account(Y)  (投 Y 的在线设备 / 存离线库)
⑥  节点B → Y 的客户端         (uni 流推送；会话须稳定)
```

### 每关的失败模式(本轮排查全部实锤过)

| 关 | 失败模式 | 现象 | 已确认实例 |
|---|---|---|---|
| ① | 客户端↔home 的 relay 会话不稳(NAT 地址抖动触发"换网哨兵"重连风暴) | 会话短时间内反复 up/down,消息投递窗口几乎不存在 | mac tauri↔cmxdev18:`spawn_addr_change_task` 对 NAT 地址抖动过度敏感 |
| ② | `is_home_here` 把"本地残留的旧设备 cert"误判为"账号 home 在本节点" | 节点收到消息却投本地离线库 + return,不转发 | cmxdev18 收 gram 8/9/10 无转发日志;**已修**(entity.home_node 为单一权威),未全量部署验证 |
| ③ | 节点间 QUIC 直连建不起来(NAT 穿透失败 / 共享出口 IP / 跨云) | `fed sync failed: s2s request timed out` 持续 | robot.link↔cmxdev18:`s2s timed out`;cmxdev18↔cmxdev46 曾因 **9600/udp 防火墙未放行**单向阻断(**已修**) |
| ③ | inbox gossip overlay 在某方向 0 邻居 | `pending-forward enqueued (cold overlay)`,publish 扩散不到对端 | robot.link→cmxdev18:overlay 未与 cmxdev18 成邻居 |
| ④ | 节点B 未订阅 Y 的 inbox 主题(Y 的 entity.home_node 数据不一致) | 即使消息到了 overlay,节点B 没订阅就收不到 | 疑似(未 100% 验证):依赖 `local_accounts` 对 home_node 的解读 |
| ⑤ | `try_push` 对"半死会话"假成功(QUIC idle 未触发) | 消息 fire-and-forget 进黑洞,不转离线库 | bug4,已把 idle 30s→10s 收窄,未根治 |
| ⑥ | 同 ①,会话抖断 | 显示在线但收不到,退出重进才 drain 离线库 | 同 ① |

### 为什么表现为"单向"

每一关大多是**方向相关**的:
- ③ 的 QUIC 直连:A→B 能打通不代表 B→A 能(NAT 类型、谁主动拨号、防火墙入站规则不对称)。
- ③ 的 9600/udp 防火墙:cmxdev18 放行了、cmxdev46 曾没放行 → 只有发往 46 的方向被挡。
- presence 走**全联邦广播主题 + relay 中转**,不依赖点对点直连 → 所以"**互相看到在线,但收不到消息**"反复出现:presence 的路通、DM 的点对点路不通。

---

## 3. 根因:一个架构前提在当前部署下不成立

把 §2 的失效点归纳,它们**不是七八个独立 bug**,而是**同一个架构前提的反复破裂**:

> **nmspace 的跨节点投递,默认依赖"任意两个相关节点之间能建立可靠的点对点 QUIC 直连"(经 N0 公共 relay 打洞)。**

而当前部署让这个前提大面积不成立:
- **NAT 节点**(mac/cmxlinux)入站打洞成功率低且地址抖动。
- **共享出口 IP**(cmxdev18/46)让外部无法区分、NAT 回环不可靠。
- **跨云**(robot.link ↔ 其他)直连路径长、易被安全组/防火墙拦。

N0 公共 relay **只在打洞失败时兜底部分流量**(presence 这类广播能过),但 nmspace 的 **DM inbox 是按需的小 overlay**,它在"两节点无法直接成为 gossip 邻居"时就建不起来——于是消息在该方向静默丢失或困在发送节点。

**结论:继续逐个修关卡(防火墙、is_home_here、订阅、pending-forward……)是打地鼠,治标不治本。** 根因是"节点间互联不可靠",必须从传输层根治。

---

## 4. 方案评估

### 方案 A:自建 Relay(推荐)

所有节点配置**同一个自建 iroh-relay**(放在一台对所有节点都公网可达的机器,**robot.link 阿里云正合适**),外加一个自建 iroh-dns-server(pkarr)做按公钥发现。节点全部切到 `mode = "selfhost"`,指向这套自建设施。

**为什么根治**:节点间不再依赖两两 NAT 直连——**所有流量经 relay 中转**。relay 对每个节点都可达(节点主动外连 relay 的 443,出站几乎总是通的,不受入站 NAT/防火墙影响)。A→B 的消息:A 发给 relay,relay 转给 B。只要 A、B 各自能连上 relay(出站),双向就通。单向失效、NAT 打洞失败、共享出口 IP、跨云——这些问题**整类消失**。

**代码基础已就绪**(无需开发):
- `nm-transport` 的 `Infra::SelfHosted` / `Client::bind_selfhosted` 已实现(relay_urls + pkarr_url)。
- `nmd` 已支持 `mode = "selfhost"` + `[relay] url` + `[dns] url`(见 `bin/nmd/src/main.rs` 文档头)。
- 客户端(tauri)`dial` 已支持 selfhost 模式。

**要做的是部署,不是写代码**:
1. 在 robot.link(公网可达)部署 **iroh-relay**(iroh 1.3 官方独立二进制)+ **iroh-dns-server**(pkarr 端点)。给它们配 TLS(relay 要 https/wss)。
2. 各节点 nmd.toml 改 `mode = "selfhost"` + `[relay] url = "https://relay.robot.link"` + `[dns] url = "https://dns.robot.link/pkarr"`。
3. 客户端(app)登录时选 selfhost 模式 + 填同一套 relay/pkarr。
4. **所有节点和客户端必须指向同一套 relay/pkarr**(`bind_selfhosted` 的硬要求:同 relay/dns 才能按公钥发现+穿透)。

**成本/风险**:
- 运维一套 relay + dns-server(需 TLS 证书、监控、它本身的高可用)。relay 成为**单点**——它挂了全网断。生产需冗余(配多个 `urls`)。
- 全链路多一跳(A→relay→B),延迟略增(通常可忽略,relay 选就近)。
- 迁移期:所有节点 + 客户端要同步切 selfhost,过程中混用(部分 nat 部分 selfhost)会连不上(不同 relay/发现域)。需一次性切换。
- 工程量:**部署约 1–2 天**(iroh-relay/dns-server 搭建 + TLS + 各节点配置 + 验证),零代码改动。

### 方案 B:所有节点放到能两两直连的网络

把所有节点放公网、或都接入一个 VPN/WireGuard overlay 网络,消除 NAT 穿透不确定性。

- 优点:不引入 relay 单点;节点间真·直连,延迟最低。
- 缺点:NAT 后的客户端(mac/cmxlinux 上的 tauri)仍然是问题——它们是终端设备,不可能都上公网/VPN。只解决 seed 间,不解决客户端↔home。**不彻底**。

### 方案 C:接受现状 + 文档化限制

明确"哪些节点对能通",用户只在能互通的节点登录。

- 优点:零成本。
- 缺点:治标不治本,用户体验差(要记哪些能通),且"能通的节点对"会随网络变化漂移。不推荐作为长期方案。

---

## 5. 推荐与路线

**推荐方案 A(自建 relay),用 robot.link 兼做 relay 宿主。**

理由:
1. **根治**——从传输层消除"节点间互联不可靠"这个总根因,§2 的大部分关卡(③④⑤⑥ 的连通性部分)一次性解决。
2. **零代码**——`selfhost` 模式及其全链路支持已实现,只需部署 + 配置。
3. robot.link 是现成的公网阿里云机,直接兼做 relay/dns-server 宿主,不用新采购。

**仍需单独保留的修复**(relay 解决连通性,但这些是逻辑正确性,与传输无关,应一并落地):
- ② `is_home_here` 用 entity.home_node 为单一权威(**已改,待全量部署**)——relay 不能修这个逻辑误判。
- ① tauri"换网哨兵"对 NAT 地址抖动过度敏感的重连风暴——即使走 relay,地址抖动仍可能触发;应加约束(只在 relay/关键地址真变时重连,或探测现连接是否真死再重连)。
- ⑤ `try_push` 假成功——relay 下连接更稳,此问题缓解,但彻底根治仍需应用层投递 ACK。

**落地顺序**:
1. 先把已改的 `is_home_here` 修复全量部署到所有 seed(低风险、已测)。
2. 搭建 robot.link 上的 iroh-relay + iroh-dns-server + TLS。
3. 小范围验证:两个 seed 切 selfhost,确认双向互通 + `fed sync` 不再 timed out。
4. 全量切换所有节点 + 客户端到 selfhost。
5. 修 ① 的换网哨兵(减少客户端会话抖动)。

---

## 6. 验证清单(切换后逐项确认)

- [ ] 每个节点 `fed sync failed` 归零(`journalctl -u nmd | grep -c "fed sync failed"`)。
- [ ] 任意两节点双向私聊往返通(含之前失效的 robot.link↔cmxdev18)。
- [ ] presence 与 DM 一致(不再"显示在线却收不到")。
- [ ] 客户端会话稳定(cmxdev18 的 `session up/down` 频率从频繁抖动降到个位数/小时)。
- [ ] NAT 客户端(mac/cmxlinux tauri)入站消息可靠(退出重进不再堆积大量离线消息)。

---

## 附:涉及的关键代码位置(供实施参考)

- 自建设施绑定:`crates/nm-transport/src/lib.rs` `Infra::SelfHosted` / `bind_selfhosted`
- nmd selfhost 配置:`bin/nmd/src/main.rs`(`mode` / `[relay]` / `[dns]`)
- 客户端拨号:`clients/app/src-tauri/src/connection.rs` `dial`(selfhost 分支)
- is_home_here(已修):`crates/nm-node/src/delivery.rs`
- 换网哨兵(待修):`clients/app/src-tauri/src/connection.rs` `spawn_addr_change_task` / `roam_reconnect_driver`;底层 `crates/nm-transport/src/lib.rs` `spawn_addr_change`
- 跨节点投递:`crates/nm-node/src/delivery.rs` `deliver_direct` / `deliver_account`;`crates/nm-node/src/federation_runtime.rs` `spawn_pending_flush`

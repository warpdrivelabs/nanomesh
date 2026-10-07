# 联邦真实测试清单（单聊 / 群聊 / 频道 / 收件箱）

> 对象：`work/f0-mobile-docs-20261007` 的去火管/每主题联邦工作（F0–F5 + F2 pkarr + F3/F5 退火管）。
> 目标：在真实网络环境验证四大消息功能，先验证现状（火管），再验证新特性（每主题）。

## 0. 范围与重要前提

- **覆盖功能**：单聊（DM）、群聊、频道、收件箱（离线补投 + F3 每收件人主题）。
- **两种模式**：
  - **默认（火管）** —— `[federation] per_topic` 不配或 `false`，即当前生产行为。
  - **每主题（新特性）** —— 两端都设 `[federation] per_topic = true`。
- **关于连通性**：真实测试请用 `mode = "nat"`（N0 公共中继 + 打洞 + 按公钥发现），**跨节点连通可靠**。
  自动化 bash 测试里被 `#[ignore]` 的跨节点用例，是因为测试用的是 `bind_local`（Minimal 模式，无中继、本机叠加网不稳），**不代表生产路径有问题**——真实 nat 模式正好补上这部分验证。
- **前置**：外网可达 N0（nat 模式）；`cargo build --release` 通过。

---

## A. 自动化基线（动手前先确认）

```bash
cargo test --workspace            # 期望：46 passed; 0 failed（确定性全绿）
```

可选（环境依赖、本机 Minimal 模式偶发不过属正常）：

```bash
cargo test -p nm-node -- --ignored    # 跨节点/每主题 e2e：连通时通过
```

- [ ] `cargo test --workspace` 全绿（46/0）

---

## B. 单节点冒烟（最快打通一条链路）

根目录 `nmd.toml`（现成）：`mode="nat"`、`[admin]`、`[membership] federation="nmspace"`。

```bash
./start-all.sh                    # 起 nmd + nm-admind
tail -f logs/nmd.log              # 记下启动打印的两行：NM_NODE_ADDR=<json>  和  NM_NODE_ID=<hex>
# 另开终端起回声机器人（把 NM_NODE_ADDR 的值填进 --node，整段 json 用单引号括住）：
cargo run -p nm-echo -- --node '<NM_NODE_ADDR 的值>' --name 回声机器人 --seed 7
```

- [ ] 用桌面/移动客户端连到该节点（连接页填 `NM_NODE_ADDR` 的值）→ 目录里出现「回声机器人」
- [ ] 给回声机器人发一条消息 → 收到回声（✅ 单聊·同节点 + 在线投递）

---

## C. 双节点 · 默认模式（火管）—— 四功能验收

**两个节点（可同机两目录，或两台机）**，各自独立 `identity / db / bind_port`，**同一 federation 名**，互相把对方节点 id 配成种子。

`node-a/nmd.toml`：
```toml
mode = "nat"
identity = "node-a.identity"
db = "node-a.redb"
bind_port = 9600
[membership]
federation = "nmspace"
[[peers]]
id = "<node-b 的 NM_NODE_ID（B 启动日志打印的那行 hex）>"
```
`node-b/nmd.toml`：同上，改 `bind_port=9601`、identity/db 换名、`[[peers]].id` 填 A 的 `NM_NODE_ID`。

> 取 id：先各启动一次 `./target/release/nmd --config node-X/nmd.toml`，启动日志会打印本节点 `NM_NODE_ID=<64 位 hex>`，把它回填到**对方**的 `[[peers]]`。nat 模式按 id 经 N0 发现对端地址，无需手填地址。

```bash
cargo build --release -p nmd
./target/release/nmd --config node-a/nmd.toml   # 终端1
./target/release/nmd --config node-b/nmd.toml   # 终端2
```

客户端：Alice 连 A、Bob 连 B（桌面/移动 app 的连接页各填对应节点的 `NM_NODE_ADDR`）。

| # | 功能 | 操作 | 预期 | 勾选 |
|---|---|---|---|---|
| C1 | 单聊·跨节点 | Alice→Bob 发 DM | Bob 实时收到 | ☐ |
| C2 | 收件箱·离线补投 | Bob 下线 → Alice 再发 → Bob 上线 | Bob 上线后收到离线消息 | ☐ |
| C3 | 群聊·建群加成员 | Alice 建群，加 Bob（跨节点成员） | 两端都显示该群与成员 | ☐ |
| C4 | 群聊·扇出 | 群内发消息 | A、B 两端成员都收到 | ☐ |
| C5 | 群聊·离线补投 | 某成员离线→发群消息→上线 | 上线后补收 | ☐ |
| C6 | 频道·订阅发布 | 两端订阅同一频道，一端发布 | 另一端收到 | ☐ |

---

## D. 双节点 · 每主题模式（F1–F5 新特性）

两个节点 `nmd.toml` 各加：

```toml
[federation]
per_topic = true
```

重启两节点，并开调试日志以观察新行为：

```bash
RUST_LOG=nm_node=debug,info ./target/release/nmd --config node-a/nmd.toml
RUST_LOG=nm_node=debug,info ./target/release/nmd --config node-b/nmd.toml
```

**先重跑 C1–C6，确认开 per_topic 后四功能仍全部送达**（现在额外/改走每主题）。然后验证新行为：

| # | 验证点 | 怎么看 | 勾选 |
|---|---|---|---|
| D1 | 每主题泵启动 | 日志出现 `per-topic group pump started` | ☐ |
| D2 | 群消息退火管（F5） | 成员稳定后日志出现 `firehose suppressed for group message (all remote homes live on topic)` | ☐ |
| D3 | 私聊退火管（F3） | 稳定后日志出现 `firehose Direct suppressed (recipient home live on inbox topic)` | ☐ |
| D4 | 退火管后仍送达 | D2/D3 出现后继续发群消息/私聊 → 对端仍实时收到（不漏投） | ☐ |
| D5 | 自愈 | 让一端离线 >12s 再恢复 → 日志恢复火管、消息不丢 | ☐ |

**隐私/仅成员可达（可选，需第三个节点 C）**：C 加入同 federation 但**不是**某群成员。
- [ ] D6：该群发消息时，C 节点不向任何本地用户投递该群消息（每主题仅成员订阅）。
  > 说明：火管尚未对「群公告 announce」退役，故 C 仍能"知道群存在"；但**群消息内容**在退火管生效后只到成员节点。

---

## E. F2 pkarr 群发现（可选 · 需真实 pkarr 服务）

仅当你有自建 `iroh-dns-server`（`mode="selfhost"` + `[dns] url=...`）或可用的 pkarr relay 时：

- [ ] E1：客户端用「带密钥群」接口建群（`group_create_keyed`）→ home 节点把 `群公钥→seed` 记录 PUT 到 pkarr relay。
- [ ] E2：另一节点在**无 home 锚点**信息时，经 pkarr 解析到 seed 并加入该群主题。

> 无 pkarr 服务则跳过。本机 bash 环境无法验证（仅 `pkarr_mock` 单测覆盖编解码 + HTTP）。

---

## F. 排查速查

- **连不通**：确认 `mode=nat` 且外网能到 N0；两端 `[membership] federation` 同名；`[[peers]].id` 填的是**对方**节点 id；防火墙放行 `bind_port`（同网直连）。
- **看日志**：`logs/nmd.log`（start-all 方式）或前台 `RUST_LOG=nm_node=debug` 输出。关键字：`group gossip joined` / `per-topic group pump started` / `firehose suppressed` / `pkarr`。
- **per_topic 不生效**：确认**两端**都 `=true` 且已重启；退火管需成员/收件人 home 稳定存活数秒（心跳周期 ~3s，TTL 12s）后才触发。
- **停止/重启**：`./stop-all.sh` / `./restart-all.sh`（单节点 start-all 方式）；手动双节点直接 kill 对应进程。

---

## G. 结论记录

- [ ] C（火管）四功能全部通过 → 现状生产行为 OK
- [ ] D（每主题）四功能仍全部通过 + D1–D4 观察到退火管行为 → 新特性 OK
- [ ] 客户端 UI（桌面/移动）侧的单聊/群聊/频道交互走查通过

> 建议顺序：先过 C（低风险、验证现状），再开 per_topic 过 D（验证新特性）。两者都 OK 再考虑合并 `work/f0-mobile-docs-20261007` → `main`。

# nmspace 邮件网关方案（POP3/SMTP ↔ P2P）

> 让任意标准邮件客户端（Thunderbird / Outlook / Apple Mail / mutt …）通过一个**本地网关**，
> 用 nmspace 的 P2P 机制在联邦内 `user@domain.com` 之间收发"邮件"。
> 文档先行——审核通过后再落地（与 `FEDERATION_SCALING_DESIGN.md` 同节奏）。
>
> 状态：草案 v1（2026-10-08）。本文所引 API 均已在现有代码核对（见 §7）。

## 0. 目标 / 非目标

**目标**
- 任意标准邮件客户端，指向本地网关即可收发 nmspace 邮件，无需改客户端。
- 复用现有身份（`user@domain.com`）、收件箱（home node 离线存储）、附件（内容寻址 blob）、离线补投。
- 发送方**密码学可验证**（天然反伪造 / 反钓鱼）。

**非目标（本期不做，列为后续）**
- 不接入全球邮件系统（gmail/outlook.com 等外部地址）——需要 SMTP 出口网关，会重新引入中心化 + 滥用治理，单列。
- 不做 IMAP（先 POP3；IMAP 需服务端邮箱状态）。
- 不做端到端加密（依赖规划中的 MLS；首期静态加密 = 标准邮件同级）。
- 不做多账户/多身份网关（首期一网关一身份）。

## 1. 为什么天然契合（三处对应）

| 邮件模型 | nmspace 现有 | 结论 |
|---|---|---|
| 地址 `user@domain.com` | `NameRecord`：`local@domain → pubkey + home_node`（home 签名） | **零地址翻译** |
| 服务器暂存、客户端拉取（POP3） | home node 离线入库 `push_inbox`/上线补投 `drain_inbox` | **收件模型一致** |
| 客户端不关心底层传输 | 网关对客户端说 POP3/SMTP，对内走 P2P | **传输被封装** |

先例：**Proton Mail Bridge**——本地跑 IMAP/SMTP、对内走私有加密后端，任意邮件客户端照用。本方案是同一模式。

## 2. 总体架构（本地网关）

```
  [邮件客户端]                      [邮件客户端]
  Thunderbird                        Outlook
     │  POP3/SMTP (127.0.0.1)           │  POP3/SMTP (127.0.0.1)
     ▼                                  ▼
 ┌───────────────┐                 ┌───────────────┐
 │ nm-mailbridge │  持用户私钥      │ nm-mailbridge │
 │ (alice 身份)  │  online          │ (bob 身份)    │
 └──────┬────────┘                 └──────┬────────┘
        │ iroh P2P (nm-client)            │
        ▼                                 ▼
   [alice 的 home node] ◄── 联邦 gossip ──► [bob 的 home node]
        收件箱(redb) + blob                收件箱(redb) + blob
```

- **部署形态**：独立 bin `nm-mailbridge`，持用户身份（seed/vault），`online` 到自己的 home node。
  - 优点：headless 可跑在 VPS / 常驻；与 GUI 解耦；也能内嵌桌面 App（二期，复用 App 已解锁身份）。
- **关键原则**：
  1. **发件在用户侧**——网关持用户私钥，以用户身份 `send_typed`。**绝不**让 home node 代发（否则破坏 P2P 信任模型）。
  2. **收件走 home node 收件箱**——网关作为一个普通客户端 `online`，自动收到实时 + 离线补投的消息。
  3. **只绑 loopback**——不对网络暴露 POP3/SMTP（或仅加本地自签 TLS）。

## 3. 身份与认证映射

- **地址解析（收件人）**：`To/Cc: alice@a.nm` → `client.name_resolve("alice@a.nm")` → `NameRecord{ client_pubkey, home_node }` → 用 `pubkey` 作 `send_typed` 的 target。
- **发件人渲染/验证（收件侧）**：收到 Gram 后 `client.name_reverse(gram.sender)` → `alice@a.nm`，注入可信头（见 §4）。
- **客户端认证**：SMTP/POP3 的 `AUTH`
  - 用户名 = nmspace 名 `user@domain`
  - 口令 = 解锁本地 vault 的口令（复用 App 的 `auth.rs` 保险库；网关启动时或首次 AUTH 时解锁出 seed）
  - 仅接受 loopback 连接；口令错即拒。

## 4. 消息模型：RFC822 整封隧道

**核心设计：把整封 RFC822 当作不透明载荷在 P2P 上隧道传输。** 收端原样还原 → Subject / Date / Message-ID / In-Reply-To/References（线程）/ MIME / 附件 **全部保真**，无需在 Gram 里逐字段建模。

- **载荷**：`Any { type_url = "message/rfc822", value = <raw RFC822 bytes> }`，经 `Session::send_typed(pubkey, "message/rfc822", bytes)` 发送。
  - **不需要改 proto**（复用 `send_typed` + `Any`）。可选：新增 `GramKind::Mail` 便于收端快速过滤（否则用 `kind=Message` + `type_url` 判定）。
- **发件人验证头（反伪造，优势项）**：收件侧网关在交给 POP3 前，用 `name_reverse(gram.sender)` 核对信中 `From:`：
  - 一致 → 注入 `X-Nmspace-From: alice@a.nm (verified)` + `Authentication-Results: nmspace; auth=pass`。
  - 不一致/解析失败 → 注入 `X-Nmspace-From: <pubkey> (unverified)` 并可改写/标注 `From`，客户端据此可过滤。
  - 因为 `gram.sender` 是**连接身份、密码学绑定**，联邦内从根上杜绝发件人伪造。
- **大附件外置（二期优化）**：RFC822 中 >阈值（如 256 KB）的 MIME part 抽出 → `blob_put` → 载荷里替换为 `BlobRef{hash,home_node}` 占位；收端网关 `blob_get` 还原后再拼回 RFC822 交付。首期可先限制单信大小 / 全内联。

## 5. 收件路径（POP3）

```
home node 收件箱 ──(补投/实时)──► Session.recv() ──► 网关本地邮箱队列(持久化) ──► POP3 ──► 客户端
```

- 网关 `online` 后，`Session::recv()`（或 `take_inbox()`）持续收 Gram（**含上线时 home node 补投的离线信**——即刚修复的那条路径）。
- 过滤 `type_url=="message/rfc822"`（或 `GramKind::Mail`）→ 转成本地邮箱条目，持久化（sqlite/文件），**按 `(sender, gram_id)` 去重**并作为 **POP3 UIDL**。
- POP3 命令映射：
  - `USER/PASS` → 校验 + 解锁身份
  - `STAT` → 封数 + 总字节
  - `LIST` / `UIDL` → 列表 + 稳定唯一 id
  - `RETR n` → 输出该条 RFC822（注入验证头后）
  - `DELE n` / `QUIT` → 标记删除、`QUIT` 时提交（从本地队列移除）
- **与 home node 收件箱的关系**：home node 的 inbox 仍是权威离线存储；网关只是"把 session 流（含补投）变成一个 POP3 邮箱"。home node 的补投是一次性 drain（删除式），所以网关需**本地持久化**收到的信，避免网关重启丢信。

## 6. 发件路径（SMTP）

```
客户端 ──SMTP──► 网关 ──解析信封──► name_resolve 每个 RCPT ──► send_typed(pubkey, rfc822) (逐收件人 fan-out)
```

- SMTP 命令：`EHLO/AUTH/MAIL FROM/RCPT TO/DATA/QUIT`。
- `RCPT TO` 逐个 `name_resolve`：
  - 解析成功 → 记下 `pubkey`。
  - 解析失败（联邦内无此名）→ 立即 `550 5.1.1 user unknown in nmspace`（即时拒收代替异步退信）。
  - 外部真实邮箱（非 nmspace 名）→ `551`/拒收（本期不出联邦）。
- `DATA` 收完整 RFC822 → 对每个解析到的收件人 `send_typed(pubkey, "message/rfc822", raw)`（多收件人 = fan-out 多份；Bcc 由信封处理，不入可见头）。
- **Sent 副本**：POP3 模式下由客户端本地留存（标准 POP3 行为）；IMAP 二期再在服务端留 Sent。
- **投递语义（诚实说明）**：尽力而为。网关本地**排队 + 重试**（对端 home node 暂不可达时）。目前**无 DSN/硬退信**（弱于传统 MTA）——可在 N 次重试失败后，由网关给发件人本地投一封"投递失败通知"信（伪 NDR）兜底。

## 7. 与现有系统集成点（已核对的 API）

| 用途 | 现有 API（nm-client / 节点） | 位置 |
|---|---|---|
| 连接 home node | `Client::online(addr)` / `online_by_id(node_id)` → `Session` | nm-client:100/155 |
| 收消息（含离线补投） | `Session::recv()` / `take_inbox()` | nm-client:293/299 |
| 发"邮件"Gram | `Session::send_typed(target:[u8;32], type_url:&str, body:&[u8])` | nm-client:619 |
| 名字→pubkey | `Session::name_resolve(name) -> Option<NameRecord>` | nm-client:577 |
| pubkey→名字（验证/From） | `Session::name_reverse(pubkey) -> Option<NameRecord>` | nm-client:585 |
| 附件外置/还原 | `blob_put(data,mime)` / `blob_get(hash,home_node)` | nm-client:470/485 |
| 身份/解锁 | App `auth.rs` 保险库（seed/VK） | clients/app |

- **无需改后端协议**：RFC822 走 `Any(type_url="message/rfc822")`。唯一**可选**的 proto 改动：`GramKind::Mail`（纯为收端过滤方便）。
- **无需改投递/收件箱**：网关作为普通客户端 `online`，离线补投自动生效（复用本次已修复的补投路径）。

## 8. 安全

- **绑定**：默认仅 `127.0.0.1`；如需跨机，必须本地/自签 TLS + 强认证，不裸奔。
- **密钥托管**：私钥只在用户侧网关；home node 不代发。
- **发件人验证**：`gram.sender` 密码学绑定 → 反伪造（§4）。
- **静态加密**：首期信件在 home node 以明文入库（与标准邮件"服务器可读"同级，不更差）。主打"私密邮件"差异化时上 **E2E（MLS）**——届时收件人之外（含 home node）不可读。
- **滥用/配额**：联邦内发件人可验证 → 天然抑制匿名垃圾；仍建议加每身份发信频率/配额。

## 9. 边界与风险（诚实清单）

1. **仅联邦内互通**——不接全球邮件（除非另建出口网关）。定位："邮件客户端 UX 跑在 P2P 上"。
2. **投递保证弱于 SMTP**——尽力而为、无标准 DSN；靠本地重试 + 伪 NDR 兜底。
3. **消息大小**——gossip/gram 有上限，大附件需 blob 外置或分片（二期）。
4. **POP3 删除 vs 多设备**——POP3 下载即删与多客户端/多设备冲突；首期单网关单客户端，多设备留 IMAP（服务端状态）。
5. **网关需本地持久化**——home node 补投是一次性 drain，网关重启前必须先把收到的信落本地库，否则丢信。
6. **域名唯一性**——`domain` 现为 TOFU 自声明（命名 N1），尚无全局唯一（N2 见证注册表）。"像不像真邮箱"随命名层成熟度提升。

## 10. MVP 里程碑

| 里程碑 | 内容 | 验证 |
|---|---|---|
| **M0 骨架** | `nm-mailbridge` bin：持身份 + `online` home node + 起 loopback SMTP(2525)/POP3(1100) 监听 | 端口可连、AUTH 通过 |
| **M1 发件** | SMTP：`RCPT` resolve + `send_typed`；单收件人纯文本 | Thunderbird 发出 → 对端网关 session 收到 |
| **M2 收件** | POP3：session→本地库→`LIST/UIDL/RETR/DELE`；纯文本往返 | 两个 Thunderbird 互发互收一封纯文本 |
| **M3 保真** | 完整 MIME：多收件人 fan-out、附件内联、`Subject`/线程头保真 + 发件人验证头 | 带附件 + 回复（threading）往返一致 |
| **M4 健壮** | 大附件 blob 外置、本地队列持久化、发件重试 + 伪 NDR | 重启网关不丢信；大附件可达；对端离线→补投 |
| 后续 | IMAP（服务端邮箱状态，可复用 App SQLite）、E2E(MLS)、出口网关（如决定） | — |

## 11. 工作量 / 依赖

- 新建 `bin/nm-mailbridge`（或 `crates/nm-mailbridge` + 瘦 bin），复用 `nm-client`。
- Rust 依赖：
  - SMTP server：`samotop` 或 `mailin`（成熟）
  - POP3 server：轻量自写（命令集很小）或现成 crate
  - MIME：`mail-parser`（解析）+ `mail-builder`（构造/注入头）
  - 本地库：复用 `rusqlite`（与 App 一致）
- **不动后端协议**（除可选 `GramKind::Mail`），风险可控。中等工作量。

## 12. 待决策（开评审时定）

1. **形态**：独立 daemon（headless 友好）优先，还是先内嵌桌面 App（身份已解锁、最省事）？
2. **`GramKind::Mail`**：新增一个 kind 便于过滤，还是复用 `Message` + `type_url` 判定？
3. **IMAP 是否进路线**：决定要不要服务端邮箱状态（文件夹/已读/多设备）。
4. **附件阈值 / 单信大小上限**：首期全内联 + 限大小，还是直接上 blob 外置？
5. **多账户网关**：首期一身份；是否要一个网关服务多个本地用户？
6. **出口网关**：是否纳入路线（接全球邮件）——若纳入，另开设计（中心化/反滥用是大头）。

---

**一句话评审结论**：架构契合度高、可落地、风险可控；建议按"本地 bridge + 先 POP3/SMTP + RFC822 隧道"做 MVP（M0–M3），把"密码学发件人验证"作为亮点，E2E/IMAP/出口网关列后续。

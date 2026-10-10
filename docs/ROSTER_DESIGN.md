# Roster Design（联系人册）

**状态**: 待审核（2026-10-10）
**相关文档**: `docs/MESSAGE_LOSS_DIAGNOSIS.md`、`docs/FEDERATION_MEMBERSHIP_DISCOVERY.md`

---

## 问题陈述

当前实体列表的两个问题：

1. **全局共享 directory**：`MemDirectory` 是节点级内存，任何人连上节点注册都进同一张表，`directory.query` 对所有登录用户返回相同结果。节点上的所有用户自动成为彼此的"实体列表"，没有隐私边界，也无法表达「A 加了 B 但 B 没加 A」的单向关系。

2. **联系人仅存客户端本地**：当前前端的手动添加（`ADDED`）通过 `catalog.db`（rusqlite）存储在设备本地，`catalog_load`/`catalog_save` tauri 命令做整表替换。换设备、重装 app、多设备同时使用时联系人不同步。

---

## 现状梳理

### 客户端存储（Desktop + Mobile 完全镜像）

两端都用 `rusqlite { features = ["bundled"] }`——SQLite C 源码直接编译进二进制，无系统依赖，跨平台（含 Android NDK）编译已验证可行。

**`catalog.db` 现有 schema**：
```sql
CREATE TABLE record (
  user TEXT NOT NULL,   -- 账号公钥 hex（已按用户隔离）
  kind TEXT NOT NULL,   -- "directory" | "added" | "groups" | "channels"
  id   TEXT NOT NULL,
  ord  INTEGER NOT NULL,
  body TEXT NOT NULL,   -- JSON blob
  PRIMARY KEY (user, kind, id)
);
```

当前 `kind="added"` 就是联系人册的客户端缓存。**用户隔离已经存在**，缺的是服务端权威副本和多设备同步。

### 服务端存储（Home Node）

Home node 用 `redb`（纯 Rust 嵌入式 KV，ACID）。现有用途：离线消息 inbox（KV: 设备 id → gram list）、peer 信息、blob 存储、群信息。

当前 roster 在服务端**完全不存在**——节点只知道哪些用户当前在线，不知道任何用户的联系人关系。

---

## 方案设计

### 核心原则

- **Home node 是权威副本**：roster 存在用户的 home node，多设备共享同一份。
- **客户端是缓存**：`catalog.db` 的 `kind="added"` 继续作为本地缓存，登录时从 home node 同步一次；操作时先写服务端、再更新本地缓存。
- **Directory 继续服务发现**：`MemDirectory` 保持现状，用于发现 agent、bot、模型 provider 等服务型实体。roster 和 directory 是并列关系，不是替代关系。
- **不改共享 proto**：所有 RPC 复用现有 `text/plain` Any 信封，传 JSON。

### 数据模型

**RosterEntry（一条联系人记录）**：

```json
{
  "id":         "<对端 entity_id hex，必须>",
  "kind":       "<实体类型，必须：person | model.llm | agent.assistant | ...>",
  "name":       "<展示名，必须：display_name 或用户昵称，不可空>",
  "handle":     "<去中心化地址，必须：local@domain.com 格式，不可空>",
  "remark":     "<备注，可空，用户自定义>",
  "status":     "active | pending_out | pending_in | blocked",
  "added_at":   <unix秒>,
  "updated_at": <unix秒>
}
```

**必须字段说明**：前四个字段（`id`、`kind`、`name`、`handle`）是 roster entry 存在的最低要求。**任何缺少这四个字段之一的记录视为不完整，应在清理时删除，不予保留。** 应用层在写入时必须校验这四个字段均非空，拒绝写入不完整的 entry。

字段语义：
- `id`：对端的 Ed25519 公钥 hex（32 字节 = 64 字符），是唯一标识符，不可变。
- `kind`：实体类型，来自 `Entity.kind`，决定在联系人列表里如何分组和展示图标（人 / 模型 / 智能体 / 机器人等）。
- `name`：展示名，优先用用户自己设的昵称，其次是对端注册时的 `display_name`；不可为空，至少保留 `id` 的前 8 字符作为 fallback。
- `handle`：去中心化地址，格式 `local@domain.com`（如 `alice@example.com`），来自 N1 命名系统（`Entity.attributes["name"]` 或 `NameRecord`）。**过渡期允许为空字符串**——对端尚未认领名字、或 handle 尚未同步到本地时仍可加入 roster，UI 此时展示裸公钥前 8 字符作为 fallback；handle 一旦可知应通过 `roster.update` 补齐。
- `remark`：用户自定义备注，可空，不影响 entry 有效性。

`handle` 的来源与同步：
- 由 home node 的 `name.claim` 流程确立（方案 P1 落地后会持久化进 `Entity.attributes["name"]`）；
- 在 roster 里保存是为了**离线可见**——对端不在线、`directory.query` 查不到时，仍能显示 `alice@example.com` 而非裸公钥；
- `handle` 只从命名系统读取，不由用户填写（用户改的是 `remark`，不是 `handle`）；
- 若对端更新了 handle，通过 `roster.update(id, handle=new_handle)` 同步，home node 做字段级 merge。

`status` 字段现阶段只用 `active`（已添加）和后续扩展预留位。`pending_*` 是未来「好友申请」功能的状态，`blocked` 是屏蔽功能预留。**本期只实现 `active`**，但 schema 定好。

**服务端存储**（Home node，redb）：
```
key:   roster:<account_pubkey_hex>/<entry_id_hex>
value: JSON(RosterEntry)
```

用路径前缀分组，支持按账号前缀扫描（redb 范围查询），无需独立索引表。

### Node RPC 接口（nm-node → nm-client → tauri）

复用 `presence.set` / `presence.query` 的 `text/plain` Any 信封风格：

| 方法 | 入参 | 出参 | 说明 |
|---|---|---|---|
| `roster.list` | `{}` | `[RosterEntry, ...]` | 返回 caller 账号的全部联系人 |
| `roster.add` | `{id, kind, name?}` | `{ok: true, entry: RosterEntry}` | 添加或更新一条 |
| `roster.remove` | `{id}` | `{ok: true}` | 删除一条 |
| `roster.update` | `{id, name?, handle?}` | `{ok: true, entry: RosterEntry}` | 改备注名 / 同步 handle；空字符串不覆盖现有值 |

节点侧 dispatch 在 `handle_command` 里新增四个 arm，与 `presence.query` 紧邻。存储用 `NodeStore`（redb）的新方法 `put_roster` / `get_roster_all` / `del_roster`。

### Tauri 命令层

新增四个命令（Desktop + Mobile 同步加，代码完全相同）：

```rust
roster_list()                          -> Vec<Value>
roster_add(id, kind, name)             -> Value
roster_remove(id)                      -> ()
roster_update(id, name)                -> Value
```

对应前端 `NM.inv("roster_add", { id, kind, name })` 等。

**本地缓存同步策略**：每次操作先调后端 RPC，成功后用返回值更新 `catalog.db` 中 `kind="added"` 的对应行；`imStart` 登录时调 `roster.list` 与本地 `catalog.db` 做 merge（以服务端为准）。

### 前端改动（im.js）

当前 `ADDED` 的四个操作点：

| 位置 | 当前行为 | 改为 |
|---|---|---|
| `addEntity`（im.js:387） | `ADDED.unshift(...)` + `ListCache.put` | `await NM.inv("roster_add", ...)` 成功后更新 `ADDED` |
| `removeEntity`（im.js:395） | `ADDED.filter(...)` + `ListCache.put` | `await NM.inv("roster_remove", ...)` 成功后更新 `ADDED` |
| `imStart` 加载（im.js:115） | `ADDED = snap.added` 从 ListCache 读本地 | 先调 `roster.list` 同步服务端 → 更新 `catalog.db` → `ADDED = result` |
| `imDisconnect` 重置（im.js:821） | `ADDED = []` | 不变 |

`ListCache.put({ added: ADDED })` 调用保留，作为本地缓存写入。

---

## 用户档案（Profile）与 handle 的关联问题

这是比 roster 更根本的一个架构缺口，roster 的 `handle` 字段把它暴露了出来。

### 当前现状（三处分散）

| 数据 | 存储位置 | 写入时机 | 持久化 |
|---|---|---|---|
| `display_name` / `PersonProfile`（头像、bio 等） | `Entity`，客户端注册时 push | `directory.register` | redb（永久） |
| `name@domain`（handle） | `NameRecord`，key=`local@domain` | `name.claim`（独立操作） | redb（永久） |
| `attributes["name"]` = `local@domain` | 运行时计算，query 时临时盖 | `directory.query` | **内存，不持久化** |

三处数据靠 `NameRecord.client_pubkey == Entity.entity_id` 间接关联，没有直接绑定。`Entity.attributes["name"]` 是每次 query 时临时拼出来的，**不会写回 `Entity` 的持久化存储**。这意味着：

- 离线时（对端不在线）查不到 `directory`，就看不到 `name@domain`
- `Entity` 里没有 `handle` 字段，只有运行时 overlay
- home node 没有"完整用户档案"的概念——注册、命名、档案三件事是三个独立 RPC

### 修法选项

**方案 P1（轻量，本期）**：`name.claim` 成功后，home node 自动把 `local@domain` 写进该用户 `Entity.attributes["name"]` 并重新持久化（现在只写 `names` map，不回写 `Entity`）。这样 `Entity` 就带了持久化的 handle，roster 存的 `handle` 直接从 `Entity.attributes["name"]` 取，不需要额外同步。改动点：`name.claim` arm（`crates/nm-node/src/lib.rs:3623`）在 `names.insert` 之后，找到对应 `Entity` 并更新 `attributes["name"]` + 重新 `put_entity`。

**方案 P2（完整，未来）**：在 `PersonProfile` proto 里加 `string handle = 6`，注册流程显式接受 handle 作为输入，home node 在 `directory.register` 时做命名校验并原子性地同时写 `Entity` 和 `NameRecord`。这是"注册时即确立 handle"的正确姿势，但要改 `nm-proto`（共享 proto），影响面更大。

**本期选择方案 P1**，不改共享 proto，改动集中在 nm-node 一个位置。

### 对 roster 的影响

方案 P1 落地后，roster 里的 `handle` 字段来源变为：

- **初次添加时**：客户端调 `roster.add` 时，node 侧从 `Entity.attributes["name"]` 读取（已持久化的 handle）并写入 roster entry
- **不需要客户端主动 update**：handle 在 home node 侧已有权威来源，roster 只是持久化引用
- **对端不在线时**：roster 里有持久化的 handle，直接展示，不依赖 directory 实时查询

## 存储选择决策记录

| 层 | 选择 | 理由 |
|---|---|---|
| **Home node roster** | redb（现有存储） | 与现有 inbox/peer/blob 一致；KV 结构天然适合 `roster:<account>/<id>` 前缀扫描；无需引入第二个存储引擎 |
| **客户端缓存** | rusqlite catalog.db（现有） | 两端已有，`record(user, kind, id)` 表结构直接复用，`kind="added"` 只需改为缓存语义 |
| **未来：消息全文搜索** | 届时评估是否迁 SQLite | roster 本身不需要全文搜索；等该需求出现再整体评估，不提前引入 |

redb 在这里的局限（无 SQL 查询）在 roster 场景下不是问题——联系人数量有限（典型 < 500），全扫一个账号的 roster 条目比任何索引查询都快。

---

## 改动面

| 文件 | 改动类型 | 说明 |
|---|---|---|
| `crates/nm-node/src/lib.rs` | 新增 | 4 个 RPC arm（roster.list/add/remove/update） |
| `crates/nm-node/src/store.rs`（或新增） | 新增 | redb 的 roster CRUD（put/get_all/del） |
| `crates/nm-client/src/lib.rs` | 新增 | `Session::roster_list/add/remove/update` 方法 |
| `clients/app/src-tauri/src/lib.rs` | 新增 | 4 个 tauri 命令 + generate_handler 注册 |
| `clients/mobile/src-tauri/src/lib.rs` | 新增 | 同 Desktop（代码镜像） |
| `clients/app/ui/js/im.js` | 修改 | addEntity/removeEntity/imStart 的 4 个操作点 |
| `clients/mobile/ui/js/im.js`（若存在） | 修改 | 同 Desktop |

**不改**：nm-proto（共享 proto 零改）、前端 catalog.db schema（只改使用语义）、MemDirectory（保持现状做服务发现）。

---

## 不在本期范围内

以下功能**设计时预留了字段**（`status: pending_*/blocked`），但本期不实现：

- **好友申请 / 双向确认**：A 加 B 时通知 B，B 批准后才建立双向关系。
- **屏蔽**：block 后不收消息。
- **分组 / 标签**：在 `RosterEntry` 里加 `tags: []` 字段扩展即可，本期不做。
- **联系人多设备实时推送**：本期登录时一次性同步，不做增量 push。

---

## 工程量估计

| 阶段 | 内容 | 估时 |
|---|---|---|
| nm-node store + RPC arm | redb CRUD + 4 个 dispatch arm | 3h |
| nm-client + tauri（两端） | 4 个方法 + 命令注册 | 2h |
| 前端 im.js 改 4 个操作点 | 含 imStart 登录同步 | 2h |
| 回归测试 | 单节点确定性测试（roster CRUD + 登录同步） | 2h |
| **合计** | | **~1 工作日** |

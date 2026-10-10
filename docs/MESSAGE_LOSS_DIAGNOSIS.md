# 消息丢失 & 在线状态诊断（2026-10-10）

手测暴露的三个相关缺陷，均属「投递/状态」这条线。本文给出每个的**根因（含 file:line 证据）**、**为何现象如此**、**修法**与**验证**。三者在同一分支一次做完。

> 结论先行：三者都**不是** 2026-10-10 那批 bug 修复（f511576/855c21b/c6be0d4）引入的回归——那批只碰 `deliver_direct` 消息路由与传输层 idle，未动 presence/directory/会话优雅下线/gossip 冷启动。这些是**早已存在的架构缺口**，手测把它们逼了出来。

---

## 缺陷 A：跨节点好友「在线状态」恒显示离线（能互发消息）

### 现象
朋友/消息列表里所有用户都显示不在线，但消息能正常互发。

### 根因（功能缺口，非回归）
1. `MemDirectory.query`（`crates/nm-node/src/lib.rs:364`）只遍历**本节点自己注册的实体**（`self.entities`）——directory 是**纯本地内存，不跨节点同步**。
2. presence 盖戳（`crates/nm-node/src/lib.rs:3439`）只作用于**本地 directory 命中的实体**：`e.attributes.insert("presence", presence_status(...))`。
3. 跨节点好友（如 `c07a10…`）在**另一节点**注册 → 不在本节点本地 directory → 前端只能把他当**手动添加（ADDED）**。
4. 前端 `mergedEntities`（`clients/app/ui/js/im.js:35`）对 ADDED 只给 `{...a, added:true}`，**从不含 presence**；唯一带 presence 的 CONTACTS 又查不到他。

→ **跨节点好友的 presence 字段恒为空 → 前端 fallback 显示「离线」**。消息能通，是因为 DM 走公钥寻址投递（与 directory 两条独立的路）。

### 可证伪预测（已与现象吻合）
**同一节点**上的两个用户互为好友时在线态**正常**（走 `account_online` + 本地 directory）；只有**跨节点**好友恒离线。此前同节点测 agent 时在线态正常，正是「再试其他节点用户」才全灭。

### 修法（不碰共享 proto）
后端 `presence_of(entity)`（`crates/nm-node/src/lib.rs:1869`，读 gossip 缓存 + 本地会话 + TTL）已现成，但 app 是纯客户端、够不着。加一条 node RPC：
- **node**：`presence.query` —— 入参 JSON `{ids:[hex,...]}`，出参 JSON `{hex: status}`，复用 `presence.set` 同款 text/plain Any 信封。
- **nm-client**：`Session::presence_query(ids) -> HashMap<String,String>`（照 `name_account` 的 text/plain 范式）。
- **tauri**：`presence_query` 命令透传。
- **前端**：`imRefresh`（`clients/app/ui/js/im.js:138`，每 15s）里对**所有列表项**（含 ADDED）批量查 presence，盖到 `c.presence`。

> 注：presence 缓存里有无跨节点在线态，取决于 presence gossip overlay 是否已成型（与缺陷 C 同源）。overlay 未成型时查到空 = 显示离线，符合预期、不会误报在线。

---

## 缺陷 B（场景 1）：发给「刚退出 app」的用户，重登收不到

### 现象
给刚退出 tauri 主程序的用户发消息，对方上线后收不到该离线消息。

### 根因（app 从不优雅下线 + nm-client 缺按引用下线）
- `Session::close()`（`crates/nm-client/src/lib.rs:324`）本来是对的：发 `Logout` gram → 节点**立即**移除会话（`crates/nm-node/src/lib.rs:2399` `GramKind::Logout => sessions.remove`）→ 后续消息正确转离线库。
- 但它 **consume `self`**，而 app 持 `Arc<Session>`（`clients/app/src-tauri/src/lib.rs:34`），**调不到**。
- `app_quit`（`clients/app/src-tauri/src/tray.rs:767`）只 `app.exit(0)`；`disconnect` 命令（`clients/app/src-tauri/src/lib.rs:2084`）只把 `conn` 置 None。**退出/切号时从不发 Logout，也不 close QUIC。**
- 进程退出 = UDP socket 静默消失（无 FIN）；节点 `watch_conn.closed()`（`crates/nm-node/src/lib.rs:2264` 区）要等 **10s QUIC idle** 才触发。
- 这 10s 窗口内，`try_push`（`crates/nm-node/src/lib.rs:3336`）对死连接 `open_uni`+`write_gram`+`finish()` **本地全部成功**（fire-and-forget）→ `deliver_account` 记 reached → **`push_inbox` 跳过** → **消息永不入库** → 重登 drain 空 → **永久丢失**。

与「刚退出」（优雅退出）精确吻合：10s 内发的消息全丢。

### 修法（nm-client 纯新增 + app）
- **nm-client**：加 `Session::disconnect(&self)`（**按引用**，带 ~800ms 超时——因 `route_send`（`crates/nm-client/src/lib.rs:871`）无超时，死节点会挂死退出路径）：发 Logout + `conn.close(0,"bye")`。
- **app `disconnect` 命令**：`take()` 出 Conn 后先 `session.disconnect().await` 再丢弃。
- **app 退出路径**：前端 `quit` 分支（`clients/app/ui/js/tray.js:132`）在 `app_quit` 前先 `await NM.inv("disconnect")`；`request_quit` 已有 1500ms 宽限，足够 800ms 优雅下线完成。

> 残留：崩溃/强杀/断网仍走 10s idle 兜底（已是 bug4 收窄后的值）。彻底根治需应用层投递 ACK（见缺陷 C 的 ACK 讨论），本次不做。

---

## 缺陷 C（场景 2）：刚上线发消息，对方收不到，过会儿再发就收到

### 现象
刚上线的 tauri 主程序给某个在线用户发消息，对方收不到；过一会再发会收到。

### 根因（冷启动 gossip overlay 空 + publish 无缓冲）
- 远端收件人时 `deliver_direct` 走 gossip。节点/overlay 刚起时：
  - inbox 主题 **0 邻居** → `publish_inbox`（`crates/nm-federation/src/lib.rs:194`）静默丢弃（gossip 无缓冲）。
  - 火管兜底（`group_pub` → `spawn_group_sync`，`crates/nm-node/src/lib.rs` 内 `let _ = topic.publish(bytes).await`，结果丢弃、**无邻居检查**）→ 冷启动时火管 overlay 也空 → **两条路都丢**。
- bug1 修复（855c21b）只堵了「错误退火管」，**没堵「冷启动两条 gossip 都空」**。overlay 成型后再发就通 → 吻合「过一会」。

### 修法（nm-node + nm-federation）
发布进 0 邻居 overlay 时入**本地待发队列**，后台在邻居出现后补发（bounded TTL + 上限条数，防无界堆积）：
- `Federation` 暴露 `publish_inbox` 的「是否真有邻居」给上层（已有 `inbox_neighbors`）。
- `deliver_direct`：当 `per_topic` inbox 与火管 overlay 当前均 0 邻居 → 把 `(to, bytes, deadline)` 压入 `pending_fed`（有界）。
- 一个后台任务（或复用 `spawn_group_sync` 的 ~3s tick）flush：邻居>0 时重发并出队；超 TTL（如 60s）丢弃并 `warn`。
- 收端已有 `(sender,gram_id)` 去重，补发重复无害。

> 备选（更彻底、另开工）：应用层投递 ACK —— 收端回执，发端未收回执则持久重试。可同时根治 B 的残留与 C。本次先上「冷启动待发队列」这一低风险增量。

---

## 实施顺序（同一分支）
1. **缺陷 A**：node `presence.query` + client + tauri + 前端盖戳。
2. **缺陷 B**：nm-client `disconnect(&self)` + app 优雅退出/切号。
3. **缺陷 C**：nm-node/nm-federation 冷启动待发队列。

## 验证
- Rust：`cargo build --workspace` + `cargo test --workspace`（保持 53+ green；多设备/ per_topic e2e 不回归）。
- 新增回归：
  - A：mock 两节点，远端实体 presence 经 `presence.query` 可见。
  - B：建会话 → `disconnect()` → 断言节点 `sessions` 即时移除、后续 DM 入离线库、重连可 drain。
  - C：0 邻居时 publish → 入队；邻居出现后 → 补发到达。
- App：`node --check` 改动的 JS；`cargo check` src-tauri。
- 真机（人工）：跨节点好友在线态显示；给刚退出用户发→重登可收；冷启动即发→对端可收。

## 影响面
- 改：`crates/nm-node`、`crates/nm-client`、`crates/nm-federation`、`clients/app`（src-tauri + ui）。
- 不改：nm-proto（**共享 proto 零改**，全走现有 text/plain Any 信封）、主 workspace 拓扑。

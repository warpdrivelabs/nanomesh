# Nano Mesh Capacity Model

本模型覆盖当前 P2 范围：多节点 soak、服务端容量估算、冷数据分页与管理查询分页。按要求，本阶段不包含消息 E2E 加密与移动端后台能力。

## 1. 运行入口

生成 100 节点计划：

```bash
cargo run -p xtask -- soak --nodes 100 --duration 300 --scenario nat --plan-only
```

真实启动 100 节点：

```bash
cargo run -p xtask -- soak --nodes 100 --duration 300 --scenario nat
```

生成 1000 节点计划：

```bash
cargo run -p xtask -- soak --nodes 1000 --duration 1800 --scenario mixed --plan-only
```

输出目录位于 `target/soak/<run-id>/`，包含每个节点的 `nmd.toml`、身份文件、日志、`FAULT_PLAN.md` 与 `soak-summary.json`。`scenario` 支持 `lan`、`nat`、`selfhost`、`restart`、`churn`、`partition`、`mixed`；`selfhost` 实跑需要提供 `--relay-url` 与 `--dns-url`。

## 2. 观测指标

容量测试以 admin API 和 OpenMetrics 为准：

- `/metrics`：`nmd_online_sessions`、`nmd_rss_bytes`、`nmd_cpu_pct`、`nmd_table_size{table=...}`、消息/字节计数、投递延迟直方图、连接拒绝计数。
- `/system`：在线数、表大小、换网、presence、pkarr、group fanout/ack/retry/giveup。
- `/connections?offset=0&limit=1000`、`/users`、`/devices`、`/peers`：管理查询分页，默认 `limit=200`，最大 `1000`，响应带 `page.total` 和 `page.next_offset`。

## 3. 每连接内存模型

单连接常驻内存近似为：

```text
M_conn = M_iroh_conn + M_session_map + M_principal + M_session_meta + M_transport_buffers + M_runtime_task
M_total_conn = N_online * M_conn + M_fixed
```

当前热内存表主要包括：

- `sessions`：设备或账号公钥到 QUIC/Iroh connection。
- `principals`：设备连接到账号。
- `sessions_meta`：接入时间、管理展示元信息。
- `devices`、`revoked`、`peers`、`groups`、`directory`：启动时由 redb 回填为热索引。

建议测量方式：

1. 空节点启动后记录 `/metrics nmd_rss_bytes`，得到 `M_fixed`。
2. 用 soak 固定节点数和连接数，稳定 2 分钟后再次记录 RSS。
3. `M_conn ~= (RSS_loaded - RSS_empty) / N_online`。
4. 对 100、1000、10000 连接重复测量，检查是否线性；非线性通常来自任务堆积、重传队列或目录热表增长。

## 4. 消息写放大模型

私聊在线直投：

```text
W_direct_online = network_send + optional_receipt_cleanup
```

私聊离线入库：

```text
W_direct_offline = D_offline_target
```

其中 `D_offline_target` 是该账号需要补投的有效设备数。每个设备一条 `INBOX(receiver, gram_id)` redb 记录。

跨节点可靠私聊：

```text
W_cross_node = OUTBOX + OUTBOX_DUE + remote_INBOX_or_direct + DEDUP_DIRECT + ack_delete
```

发送端先持久化 outbox 和到期索引，再发布；收到 `DirectAck` 后删除 outbox 和 due index。接收端以 `DEDUP_DIRECT` 抑制重复。

群文本扇出：

```text
W_group_local = local_offline_devices
W_group_cross = GROUP_OUTBOX + GROUP_OUTBOX_DUE + pending_home_nodes + group_ack_updates
```

群消息 fanout 成本由成员 home 节点数和本地离线设备数共同决定。高并发群的主要风险不是单条消息大小，而是 `pending_home_nodes * retry_count` 造成的重传与 ack 写放大。

频道发布：

```text
W_channel_home = CHANNEL_LOG + CHANNEL_SEQ + CHANNEL_INTAKE
W_channel_publisher = CHANNEL_OUTBOX + CHANNEL_OUTBOX_DUE + ack_delete
W_channel_reader = paged CHANNEL_LOG range scan
```

频道日志按 `(channel_id, seq)` 排序，天然支持 range/page 回填；容量重点是每频道日志保留条数、消息大小与订阅者回填速率。

## 5. 分层存储策略

当前已落地的分层边界：

- 热数据：在线会话、设备状态、目录、群、peer、presence、重传任务索引。
- 冷数据：redb 的 inbox、outbox、group outbox、channel log、blob、dead letter。
- 冷数据分页：`INBOX` 新增 `peek_inbox_page(receiver, after_gram_id, limit)`；重连补投按 256 条一页读取，避免整箱解码到内存。
- 管理分页：connections/users/devices/peers 默认 200 条、最大 1000 条，避免超大节点量时管理 API 一次返回巨大 JSON。

仍需持续推进的边界：

- directory/groups/devices 当前启动时仍会整体回填为热表；百万级对象时需要二级索引与按需加载。
- 管理端前端可继续改为显式翻页 UI，避免操作者误以为默认页就是全量。
- redb 大表裁剪应结合 `/metrics nmd_table_size` 设置报警阈值。

## 6. 容量阈值建议

默认保护参数：

- `limits.max_connections = 10000`，soak 配置提升到 `20000` 便于压力测试。
- 单实体 inbox 上限 `INBOX_MAX_PER_ENTITY = 50000`。
- inbox 补投页大小 `INBOX_DRAIN_PAGE_LIMIT = 256`。
- admin list `DEFAULT_PAGE_LIMIT = 200`，`MAX_PAGE_LIMIT = 1000`。
- 跨节点 outbox horizon 为 24h；超期进入 dead letter。

初始告警建议：

- RSS 连续 10 分钟增长且在线数稳定：检查 inbox/outbox、channel log、dead letter 表大小。
- `group_retransmits / group_fanouts > 5%`：检查网络分区、home 节点不可达或 ack 批处理异常。
- `conn_rejected_server_busy_total` 增长：提高连接上限前先确认 FD、RSS、CPU 与 p99 投递延迟。
- `nmd_table_size{table="inbox"}` 持续增长：检查离线设备、Receipt 清理和客户端重连补投。

## 7. 验收清单

- 100 节点 `nat` soak 能生成配置并启动，`/metrics` 有 RSS、在线、表大小指标。
- 1000 节点 `mixed --plan-only` 能生成计划，供大机或集群环境执行。
- 重连补投大 inbox 时 RSS 不随 inbox 总量一次性上升，只随 256 条页大小波动。
- 管理 API 大列表响应存在 `page` 元信息，且单页不超过 1000 条。
- partition/churn/restart 场景结束后，存活节点可继续通过 admin `/readyz` 响应。

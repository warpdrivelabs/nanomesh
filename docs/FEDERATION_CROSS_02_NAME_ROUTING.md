# 方案二：name@domain 跨联邦直接路由

> **name@domain Direct Routing** — 利用去中心化命名系统（N1），通过 `local@domain` 地址解析出对端 home node 的 iroh addr，然后经 QUIC s2s 直连跨越联邦边界投递消息，不依赖任何共同 gossip overlay，不经网关。

---

## 核心思路

`name@domain` 地址（如 `carol@org.nm`）已经包含了跨联邦路由所需的全部信息：

- `org.nm` 是 Carol 的 home node 声明的域名，通过 pkarr/DNS TXT 可解析出该节点的 iroh node_id 和 addr
- `carol` 是 local_part，连上该节点后发 `name.resolve` RPC 即得 Carol 的 entity_id（Ed25519 公钥）

拿到 Node B1 的 addr 和 Carol 的 entity_id 后，直接用 `fed_relay` 走 QUIC bi-stream 直连投递，完全绕开 gossip overlay。

<img src="data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA4MjAgNDQwIiBmb250LWZhbWlseT0iSW50ZXIsc2Fucy1zZXJpZiI+CjxyZWN0IHdpZHRoPSI4MjAiIGhlaWdodD0iNDQwIiByeD0iMTAiIGZpbGw9IiNmOGZhZmMiLz4KPGVsbGlwc2UgY3g9IjE4NSIgY3k9IjIzMCIgcng9IjE1MCIgcnk9IjE1NSIgZmlsbD0ibm9uZSIgc3Ryb2tlPSIjM2I4MmY2IiBzdHJva2Utd2lkdGg9IjEuNSIgc3Ryb2tlLWRhc2hhcnJheT0iNiA0IiBzdHJva2Utb3BhY2l0eT0iLjQiLz4KPHRleHQgeD0iMTg1IiB5PSI2NCIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1zaXplPSIxMyIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iIzFkNGVkOCIgbGV0dGVyLXNwYWNpbmc9IjEuNSI+RkVELUE8L3RleHQ+CjxlbGxpcHNlIGN4PSI2MzUiIGN5PSIyMzAiIHJ4PSIxNTAiIHJ5PSIxNTUiIGZpbGw9Im5vbmUiIHN0cm9rZT0iIzdjM2FlZCIgc3Ryb2tlLXdpZHRoPSIxLjUiIHN0cm9rZS1kYXNoYXJyYXk9IjYgNCIgc3Ryb2tlLW9wYWNpdHk9Ii40Ii8+Cjx0ZXh0IHg9IjYzNSIgeT0iNjQiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtc2l6ZT0iMTMiIGZvbnQtd2VpZ2h0PSI3MDAiIGZpbGw9IiM2ZDI4ZDkiIGxldHRlci1zcGFjaW5nPSIxLjUiPkZFRC1CPC90ZXh0Pgo8cmVjdCB4PSIxMTUiIHk9IjE5NSIgd2lkdGg9IjEzMCIgaGVpZ2h0PSI1OCIgcng9IjciIGZpbGw9IiMzYjgyZjYiLz4KPHRleHQgeD0iMTgwIiB5PSIyMjAiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtc2l6ZT0iMTIiIGZvbnQtd2VpZ2h0PSI3MDAiIGZpbGw9IiNmZmYiPk5vZGUgQTE8L3RleHQ+Cjx0ZXh0IHg9IjE4MCIgeT0iMjM4IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LXNpemU9IjEwIiBmaWxsPSIjZGJlYWZlIj5hbGljZUBjbXgubm08L3RleHQ+CjxyZWN0IHg9IjU1NSIgeT0iMTk1IiB3aWR0aD0iMTMwIiBoZWlnaHQ9IjU4IiByeD0iNyIgZmlsbD0iIzdjM2FlZCIvPgo8dGV4dCB4PSI2MjAiIHk9IjIyMCIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1zaXplPSIxMiIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iI2ZmZiI+Tm9kZSBCMTwvdGV4dD4KPHRleHQgeD0iNjIwIiB5PSIyMzgiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtc2l6ZT0iMTAiIGZpbGw9IiNlZGU5ZmUiPmNhcm9sQG9yZy5ubTwvdGV4dD4KPHJlY3QgeD0iMzE1IiB5PSIxMTUiIHdpZHRoPSIxOTAiIGhlaWdodD0iNTIiIHJ4PSI3IiBmaWxsPSIjMGYxNzJhIiBmaWxsLW9wYWNpdHk9Ii4wNyIgc3Ryb2tlPSIjMGYxNzJhIiBzdHJva2Utb3BhY2l0eT0iLjMiLz4KPHRleHQgeD0iNDEwIiB5PSIxMzgiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtc2l6ZT0iMTEuNSIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iIzBmMTcyYSI+cGthcnIgLyBETlMgVFhUPC90ZXh0Pgo8dGV4dCB4PSI0MTAiIHk9IjE1NiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1zaXplPSI5LjUiIGZpbGw9IiMzMzQxNTUiIGZvbnQtZmFtaWx5PSJKZXRCcmFpbnMgTW9ubyxtb25vc3BhY2UiPm9yZy5ubSDihpIgTm9kZSBCMSBub2RlX2lkICsgYWRkcjwvdGV4dD4KPHJlY3QgeD0iMzE1IiB5PSIyOTUiIHdpZHRoPSIxOTAiIGhlaWdodD0iNTIiIHJ4PSI3IiBmaWxsPSIjMGYxNzJhIiBmaWxsLW9wYWNpdHk9Ii4wNyIgc3Ryb2tlPSIjMGYxNzJhIiBzdHJva2Utb3BhY2l0eT0iLjMiLz4KPHRleHQgeD0iNDEwIiB5PSIzMTgiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtc2l6ZT0iMTEuNSIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iIzBmMTcyYSI+bmFtZS5yZXNvbHZlIFJQQzwvdGV4dD4KPHRleHQgeD0iNDEwIiB5PSIzMzYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtc2l6ZT0iOS41IiBmaWxsPSIjMzM0MTU1IiBmb250LWZhbWlseT0iSmV0QnJhaW5zIE1vbm8sbW9ub3NwYWNlIj5jYXJvbEBvcmcubm0g4oaSIEVkMjU1MTkgcHVia2V5PC90ZXh0Pgo8ZGVmcz4KPG1hcmtlciBpZD0icjJtIiBtYXJrZXJXaWR0aD0iNyIgbWFya2VySGVpZ2h0PSI3IiByZWZYPSI1IiByZWZZPSIzIiBvcmllbnQ9ImF1dG8iPjxwYXRoIGQ9Ik0wLDBMNiwzTDAsNnoiIGZpbGw9IiM5NGEzYjgiLz48L21hcmtlcj4KPG1hcmtlciBpZD0icjJvIiBtYXJrZXJXaWR0aD0iNyIgbWFya2VySGVpZ2h0PSI3IiByZWZYPSI1IiByZWZZPSIzIiBvcmllbnQ9ImF1dG8iPjxwYXRoIGQ9Ik0wLDBMNiwzTDAsNnoiIGZpbGw9IiNmOTczMTYiLz48L21hcmtlcj4KPC9kZWZzPgo8bGluZSB4MT0iMjQ1IiB5MT0iMjE0IiB4Mj0iMzE1IiB5Mj0iMTQ4IiBzdHJva2U9IiM5NGEzYjgiIHN0cm9rZS13aWR0aD0iMS4zIiBzdHJva2UtZGFzaGFycmF5PSI0IDMiIG1hcmtlci1lbmQ9InVybCgjcjJtKSIvPgo8dGV4dCB4PSIyNTQiIHk9IjE3MiIgZm9udC1zaXplPSI5LjUiIGZpbGw9IiM2NDc0OGIiPuKRoCByZXNvbHZlIGRvbWFpbjwvdGV4dD4KPGxpbmUgeDE9IjUwNSIgeTE9IjE0OCIgeDI9IjU1NSIgeTI9IjIxNCIgc3Ryb2tlPSIjOTRhM2I4IiBzdHJva2Utd2lkdGg9IjEuMyIgc3Ryb2tlLWRhc2hhcnJheT0iNCAzIiBtYXJrZXItZW5kPSJ1cmwoI3IybSkiLz4KPHRleHQgeD0iNTEyIiB5PSIxNjgiIGZvbnQtc2l6ZT0iOS41IiBmaWxsPSIjNjQ3NDhiIj5CMSBhZGRyPC90ZXh0Pgo8bGluZSB4MT0iMjQ1IiB5MT0iMjM4IiB4Mj0iMzE1IiB5Mj0iMzE4IiBzdHJva2U9IiM5NGEzYjgiIHN0cm9rZS13aWR0aD0iMS4zIiBzdHJva2UtZGFzaGFycmF5PSI0IDMiIG1hcmtlci1lbmQ9InVybCgjcjJtKSIvPgo8dGV4dCB4PSIyMjgiIHk9IjI5NCIgZm9udC1zaXplPSI5LjUiIGZpbGw9IiM2NDc0OGIiPuKRoSByZXNvbHZlIG5hbWU8L3RleHQ+CjxsaW5lIHgxPSI1MDUiIHkxPSIzMTgiIHgyPSI1NTUiIHkyPSIyMzgiIHN0cm9rZT0iIzk0YTNiOCIgc3Ryb2tlLXdpZHRoPSIxLjMiIHN0cm9rZS1kYXNoYXJyYXk9IjQgMyIgbWFya2VyLWVuZD0idXJsKCNyMm0pIi8+Cjx0ZXh0IHg9IjUwOCIgeT0iMjk4IiBmb250LXNpemU9IjkuNSIgZmlsbD0iIzY0NzQ4YiI+Y2Fyb2wgcHVia2V5PC90ZXh0Pgo8bGluZSB4MT0iMjQ4IiB5MT0iMjI0IiB4Mj0iNTUzIiB5Mj0iMjI0IiBzdHJva2U9IiNmOTczMTYiIHN0cm9rZS13aWR0aD0iMi41IiBtYXJrZXItZW5kPSJ1cmwoI3IybykiLz4KPHRleHQgeD0iNDAwIiB5PSIyMTIiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtc2l6ZT0iMTEiIGZpbGw9IiNmOTczMTYiIGZvbnQtd2VpZ2h0PSI3MDAiPuKRoiBRVUlDIHMycyBkaXJlY3Qg4oCUIGZlZF9yZWxheShhZGRyLCBncmFtKTwvdGV4dD4KPHRleHQgeD0iNDEwIiB5PSI0MDgiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtc2l6ZT0iMTEiIGZpbGw9IiM5NGEzYjgiPuS4jee7j+S7u+S9lee9keWFs++8jOS4jeS+nei1luWFseWQjCBnb3NzaXAgb3ZlcmxheSDigJQg5pyJIGFkZHIg5Y2z5Y+v55u06L+ePC90ZXh0Pgo8L3N2Zz4=" alt="name@domain 路由拓扑">

这条路不需要任何"跨联邦协议"——它复用已有的 s2s 直连能力（`fed_relay` / `s2s_request`，`crates/nm-node/src/lib.rs:4177`）和命名解析（`name.resolve` RPC）。只需在 `deliver_direct` 里把"找不到对端 gossip 路径"的分支改成"尝试解析 name@domain + s2s 直投"。

---

## 四步流程详解

<img src="data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA4MjAgMzEwIiBmb250LWZhbWlseT0iSW50ZXIsc2Fucy1zZXJpZiI+CjxyZWN0IHdpZHRoPSI4MjAiIGhlaWdodD0iMzEwIiByeD0iMTAiIGZpbGw9IiNmOGZhZmMiLz4KPHRleHQgeD0iNDEwIiB5PSIyOCIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1zaXplPSIxMyIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iIzBmMTcyYSI+bmFtZUBkb21haW4g4oaSIFFVSUMg55u05oqV77ya5Zub5q2l5rWB56iLPC90ZXh0Pgo8cmVjdCB4PSIzMCIgeT0iNDYiIHdpZHRoPSI3NjAiIGhlaWdodD0iNTIiIHJ4PSI2IiBmaWxsPSIjZmZmIiBzdHJva2U9IiMzYjgyZjYiIHN0cm9rZS1vcGFjaXR5PSIuMzUiLz4KPHRleHQgeD0iNTAiIHk9IjY4IiBmb250LXNpemU9IjExLjUiIGZvbnQtd2VpZ2h0PSI3MDAiIGZpbGw9IiMxZDRlZDgiPuatpemqpCAxICBkb21haW4g4oaSIGhvbWUgbm9kZSBhZGRyPC90ZXh0Pgo8dGV4dCB4PSI1MCIgeT0iODciIGZvbnQtc2l6ZT0iMTAuNSIgZmlsbD0iIzMzNDE1NSI+5oyB5pyJICJjYXJvbEBvcmcubm0iIOKGkiDku44gIm9yZy5ubSIg57uPIHBrYXJyL0ROUyBUWFQg6Kej5p6Q5Ye6IE5vZGUgQjEg55qEIGlyb2ggbm9kZV9pZCArIGFkZHI8L3RleHQ+CjxyZWN0IHg9IjMwIiB5PSIxMTAiIHdpZHRoPSI3NjAiIGhlaWdodD0iNTIiIHJ4PSI2IiBmaWxsPSIjZmZmIiBzdHJva2U9IiMzYjgyZjYiIHN0cm9rZS1vcGFjaXR5PSIuMzUiLz4KPHRleHQgeD0iNTAiIHk9IjEzMiIgZm9udC1zaXplPSIxMS41IiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjMWQ0ZWQ4Ij7mraXpqqQgMiAgbmFtZSDihpIgZW50aXR5X2lkIChwdWJrZXkpPC90ZXh0Pgo8dGV4dCB4PSI1MCIgeT0iMTUxIiBmb250LXNpemU9IjEwLjUiIGZpbGw9IiMzMzQxNTUiPlFVSUMg6L+e5YiwIE5vZGUgQjHvvIzlj5EgbmFtZS5yZXNvbHZlKCJjYXJvbEBvcmcubm0iKSBSUEMg4oaSIOi/lOWbniBDYXJvbCDnmoQgRWQyNTUxOSDlhazpkqU8L3RleHQ+CjxyZWN0IHg9IjMwIiB5PSIxNzQiIHdpZHRoPSI3NjAiIGhlaWdodD0iNTIiIHJ4PSI2IiBmaWxsPSIjZmZmIiBzdHJva2U9IiNmOTczMTYiIHN0cm9rZS1vcGFjaXR5PSIuNSIvPgo8dGV4dCB4PSI1MCIgeT0iMTk2IiBmb250LXNpemU9IjExLjUiIGZvbnQtd2VpZ2h0PSI3MDAiIGZpbGw9IiNmOTczMTYiPuatpemqpCAzICBRVUlDIHMycyDnm7TmipU8L3RleHQ+Cjx0ZXh0IHg9IjUwIiB5PSIyMTUiIGZvbnQtc2l6ZT0iMTAuNSIgZmlsbD0iIzMzNDE1NSI+ZmVkX3JlbGF5KGVwLCBub2RlX2IxX2FkZHIsIGdyYW0pIOKGkiBRVUlDIGJpLXN0cmVhbSDihpIgTm9kZSBCMSDmlLbliLDlkI7mjIkgY2Fyb2wgZW50aXR5X2lkIOaKlemAkuaIluWtmOemu+e6v+W6kzwvdGV4dD4KPHJlY3QgeD0iMzAiIHk9IjIzOCIgd2lkdGg9Ijc2MCIgaGVpZ2h0PSI1MiIgcng9IjYiIGZpbGw9IiNmZmYiIHN0cm9rZT0iIzk0YTNiOCIgc3Ryb2tlLW9wYWNpdHk9Ii40Ii8+Cjx0ZXh0IHg9IjUwIiB5PSIyNjAiIGZvbnQtc2l6ZT0iMTEuNSIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iIzY0NzQ4YiI+5q2l6aqkIDQgIOemu+e6v+WkhOeQhjwvdGV4dD4KPHRleHQgeD0iNTAiIHk9IjI3OSIgZm9udC1zaXplPSIxMC41IiBmaWxsPSIjMzM0MTU1Ij5DYXJvbCDlnKjnur8g4oaSIE5vZGUgQjEg55u05oqV5Yiw5rS76LeD5Lya6K+d77ybQ2Fyb2wg56a757q/IOKGkiDlhaUgcmVkYiBpbmJveO+8m+mHjei/nuaXtiBkcmFpbl9pbmJveF90byDoh6rliqjooaXmipU8L3RleHQ+Cjwvc3ZnPg==" alt="四步解析流程">

### 步骤 1：domain → home node addr

`org.nm` 是 Node B1 在 `nmd.toml` 里配置的 `domain` 字段。Node B1 通过 pkarr relay 发布了 `org.nm → iroh addr` 的 TXT 记录（`pkarr_url` 配置）。Alice 的节点用 pkarr 解析或 DNS TXT 查询得到 Node B1 的 iroh addr。这一步的 pkarr 发布逻辑已在 `spawn_group_sync` 的 F2 部分实现。

### 步骤 2：name → entity_id

Alice 的节点连上 Node B1，发 `name.resolve("carol@org.nm")`（现有 RPC，`crates/nm-node/src/lib.rs:3623` 区域），得到 Carol 的 Ed25519 公钥。这个公钥在所有联邦里通用，Alice 可验签 Carol 的任何消息。

### 步骤 3：QUIC s2s 直投

`fed_relay(ep, node_b1_addr, gram)` 直连 Node B1，把 gram 投给它。

```rust
// crates/nm-node/src/lib.rs:4177
async fn fed_relay(ep: &NodeEndpoint, addr: Addr, inner: &Gram) -> Result<(), NodeError> {
    let env = s2s_gram(ep.id_bytes(), GramKind::Relay, Some(payload));
    s2s_request(ep, addr, &env).await.map(|_| ())
}
```

### 步骤 4：离线处理

Node B1 收到 gram 后，根据 receiver 字段（carol entity_id）走本地 `deliver_account`：Carol 在线则直投会话，离线则入 redb inbox，重连时 `drain_inbox_to` 自动补投。整条离线逻辑复用现有机制，无需改动。

---

## 代码依据

`deliver_direct` 里需要新增一个跨联邦分支（伪代码）：

```rust
// 当前路径：per_topic inbox gossip
// 新增分支：当 inbox overlay 0 邻居 且 entity.attributes["name"] 含 "@domain" 时
if per_topic && inbox_neighbors == 0 {
    if let Some(handle) = entity.attributes.get("name") {
        if handle.contains('@') {
            let (local, domain) = handle.split_once('@').unwrap();
            // 1. resolve domain → home node addr (pkarr cache / DNS)
            if let Some(addr) = resolve_domain(&ctx, domain).await {
                // 2. fed_relay: QUIC s2s 直投到 home node
                let _ = fed_relay(&ctx.ep, addr, gram).await;
                return;  // 投出去了，不再走 gossip
            }
        }
    }
}
```

### 现有代码中已有的基础

- `fed_relay` / `s2s_request` — QUIC s2s 直连，已用于 blob.get 和 relay gram
- `pkarr_seeds` 缓存 — F2 已实现从 pkarr 解析节点 addr 的逻辑
- `name.resolve` RPC — 已实现，可跨节点解析 `local@domain`
- entity 的 `home_node` 字段 — 存储了归属节点的 node_id（可直接用于 iroh 拨号）

---

## 挑战与限制

### NAT 穿透
QUIC 直连依赖 iroh relay（配置中的 `Relay` URL）作为打洞媒介。如果 Node B1 的 relay 配置与 Node A1 不同或 relay 不可达，直连会失败。这是独立的基础设施问题，不是跨联邦专有的问题。

### 地址发现延迟
首次遇到 `carol@org.nm` 时，需先做 pkarr DNS 查询（可能几百毫秒），再做 `name.resolve` 握手。两步加起来约 1–2 秒延迟。解决方案：缓存 `domain → addr`，TTL 对齐 pkarr 的 `ttl` 字段（默认 3600s）。

### Presence 不可见
跨联邦用户的 presence 状态无法通过 gossip 传播，Carol 对 Alice 始终显示"离线"。可增加轻量 ping 机制：发消息前先发探测 gram，收到回复说明在线。

### 前提：roster 中有 handle
此方案要求发件方持有收件方的 `name@domain` 地址。当前 roster 的 `handle` 字段正是为此设计的。没有 handle 的跨联邦联系人只能走方案一（网关）或加入同一联邦（方案三）。

---

## 工程量

| 模块 | 内容 | 估时 |
|---|---|---|
| deliver_direct 新增分支 | 检测跨联邦 + 触发 domain 解析 + fed_relay | 1.5 d |
| domain 解析缓存 | pkarr lookup + TTL 缓存（复用 pkarr_seeds） | 1 d |
| name.resolve 跨节点验证 | 确认跨联邦场景下 name.resolve 行为正确 | 0.5 d |
| presence ping | 轻量探测 gram，判断跨联邦在线状态 | 1 d |
| 测试 | 双节点不同联邦，DM + 离线库 + 重登 drain | 1 d |
| **合计** | | **~5 工作日** |

---

## 优劣对比

| 维度 | 评价 |
|---|---|
| 无需网关 | ✅ 点对点直连，无中间节点依赖 |
| 联邦命名空间 | ✅ 两联邦各自独立，仅消息层互通 |
| 实现复杂度 | 低–中，复用现有 fed_relay + name.resolve |
| NAT 穿透 | 依赖 iroh relay，部署需关注 |
| Presence | ❌ 跨联邦 presence 需额外设计 |
| 首次延迟 | +1–2 s（domain 解析 + name.resolve），缓存后消除 |
| 离线消息 | ✅ Node B1 负责离线库 + drain，完全一致 |
| nm-proto 变更 | 不需要 |
| 适合场景 | 偶发跨联邦私信，roster 已有 handle 的联系人 |

---

## 结论

name@domain 直接路由是三个方案里**对现有代码侵入最低**的跨联邦方案，因为 s2s 直连和命名解析的基础设施都已经存在。主要限制是 presence 不可见（需要额外机制）和首次连接的解析延迟。roster 的 `handle` 字段已经是这个方案的数据基础——roster 落地后，这条路径顺带可以实现。

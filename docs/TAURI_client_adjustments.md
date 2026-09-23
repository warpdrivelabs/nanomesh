# Tauri 客户端适配：连接模式 + 按公钥拨号（穿透 NAT）

> 让原生客户端（桌面 / iOS / android）跟上后端已具备的 **nat / selfhost / lan 三模式**与
> **按节点公钥拨号（`online_by_id`）** 能力——即：NATed 客户端能穿透 NAT 连到 NATed 后端节点。

## 1. 背景与问题

现状（`clients/app`）：

- 桥接 `connect(nodeAddr, displayName)`（`src-tauri/src/lib.rs`）用 **`Client::bind_local_random()`**——即 `Minimal` 预设：**只能同网直连、无中继/发现、无法穿透 NAT**，且每次启动都是**随机新身份**。
- 前端要求用户把 `imd` 打印的 **`IM_NODE_ADDR` 整段 JSON** 粘进来（`ChatScreen.tsx`）——地址会变、跨网无效、体验脆弱。

这与本轮已落地的能力脱节：`Client::bind`(N0) / `Client::bind_selfhosted(...)` / `Client::online_by_id(node_pubkey)` 都已就绪（见 `crates/im-client`）。**客户端↔节点本就是又一条 iroh 连接**，用同一套 relay + 打洞 + 按公钥发现即可穿透 NAT。

## 2. 目标与范围

**做（本次）：**

1. 桥接 `connect` 支持三种模式：`nat`(N0 公共设施) / `selfhost`(自建 relay+dns) / `lan`(仅同网)。
2. `nat`/`selfhost` 下**按节点公钥（hex）拨号** → `online_by_id`，发现服务解析地址、穿透 NAT；`lan` 保留**按地址(JSON)** 连。
3. **持久客户端身份**：把 32 字节种子存到应用数据目录（`app_data_dir/imspace.identity`），启动复用——身份稳定（可被寻址、可被授权/拉黑，与黑名单模型一致）。
4. `selfhost` 的 relay/dns 参数从界面传入（与节点指向**同一套**设施）。
5. 前端：连接表单加「模式选择 + 公钥/地址输入 + selfhost 字段」；`CoreTransport` 接口相应调整。

**不做（本次范围外，注明原因）：**

- **频道 pub/sub（gossip）UI**：客户端侧频道 API 尚未实现（`im-client` 无 subscribe/publish；节点侧 gossip 已就绪但"客户端经 home 节点收发频道"是**增量 2**）。待其落地再加频道界面。
- **黑名单管理 UI**：黑名单是**节点(imd)** 侧能力，客户端不是节点，不涉及。
- **Web 端**：浏览器需 `im-gateway`（仍为骨架），`WebGatewayTransport` 维持占位。

## 3. 设计

### 3.1 桥接层 `src-tauri/src/lib.rs`

`connect` 参数扩展（Tauri 自动 camelCase→snake_case）：

```rust
async fn connect(
    app: AppHandle, state: State<'_, AppState>,
    mode: String,               // "nat" | "selfhost" | "lan"
    node: String,               // nat/selfhost: 节点公钥hex(64)；lan: IM_NODE_ADDR(JSON)
    display_name: String,
    relay_urls: Vec<String>,    // selfhost（其余模式传 []）
    pkarr_url: Option<String>,  // selfhost 必填
    dns_origin: Option<String>, // selfhost 可选
) -> Result<String, String>
```

绑定与连接分支：

绑定与连接分支（**同网优先、穿透兜底**）：

| mode | 策略 |
|---|---|
| `lan` | `Client::bind_local`（Minimal）+ `online(addr)`：仅按地址直连，无 NAT 兜底。 |
| `nat` / `selfhost` | **两阶段**：`node` 为完整地址(JSON)时——**阶段1** `bind_local`(Minimal，零基础设施、可离线) + `online(addr)`，短超时(3s)试**同网直连**；失败→**阶段2** `bind`(N0) / `bind_selfhosted` + `online_by_id(id)` 穿透 NAT。`node` 仅为公钥时跳过阶段1，直接穿透（在线时 iroh 仍优先 LAN 直连路径）。 |

> 为何"两阶段"而非单次：iroh 在**路径层**本就优先直连（含 LAN）、中继兜底，故在线时 `online_by_id` 已偏好同网。
> 但 iroh 1.2 **无内置 mDNS 本地发现**，纯离线/无预置地址的同网场景无法只凭公钥发现节点——阶段1 用
> Minimal 端点 + 已知地址做**零基础设施**的同网直连，正好补上这个缺口，也满足"同网不触碰任何中继/发现设施"。
> `node` 传完整地址时，其中已内嵌节点 id，阶段2 直接复用（无需另填公钥）。

持久身份：

```rust
fn load_or_create_seed(app: &AppHandle) -> Result<[u8;32], String> {
    let dir = app.path().app_data_dir()?;           // 需 use tauri::Manager
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("imspace.identity");
    // 存在且 32B 则复用；否则 SecretKey::generate() 写盘。
}
```

其余（`register_as` person、`take_inbox` → `core://event` 推送、`send_to`/`directory_query`/`my_id`）不变；新增 `disconnect`（清空会话，便于切换节点/模式）。

### 3.2 前端

`src/core/transport.ts`——`connect` 改为接收参数对象：

```ts
export type ConnectMode = "nat" | "selfhost" | "lan";
export interface ConnectParams {
  mode: ConnectMode;
  node: string;          // nat/selfhost: 节点公钥hex；lan: 地址JSON
  displayName: string;
  relayUrls?: string[];  // selfhost
  pkarrUrl?: string;     // selfhost
  dnsOrigin?: string;    // selfhost
}
interface CoreTransport { connect(p: ConnectParams): Promise<string>; /* 其余不变 */ }
```

- `tauriTransport.ts`：`invoke("connect", { mode, node, displayName, relayUrls: p.relayUrls ?? [], pkarrUrl: p.pkarrUrl ?? null, dnsOrigin: p.dnsOrigin ?? null })`。
- `webTransport.ts`：`connect(p)` 签名跟随，仍抛"未实现"。
- `store.ts`：`connect(params)` 透传。
- `ChatScreen.tsx`：连接页改为
  - 模式下拉：`nat` / `selfhost` / `lan`；
  - `node` 输入：nat/selfhost 提示"节点公钥(hex, 64位)"，lan 提示"节点地址(JSON)"；
  - selfhost 追加：relay url、pkarr url、dns origin(可选)；
  - 昵称 + 连接按钮。已连接后的目录/会话界面不变。

## 4. 改动文件

- `clients/app/src-tauri/src/lib.rs`（connect 重写 + 身份持久化 + disconnect）
- `clients/app/src/core/transport.ts`、`tauriTransport.ts`、`webTransport.ts`
- `clients/app/src/state/store.ts`、`clients/app/src/screens/ChatScreen.tsx`
- 不改后端 crate（本次纯客户端适配，复用既有 `im-client` API）。

## 5. 验证

1. `cargo build`（`clients/app/src-tauri`，独立 workspace）通过；`tsc && vite build` 前端类型/构建通过。
2. **lan 冒烟**：本机跑 `imd`（lan 模式），客户端选 lan + 粘 `IM_NODE_ADDR` → 连上、注册、目录、收发（等价现状回归）。
3. **nat 冒烟**：`imd`（nat）打印 `IM_NODE_ID`，客户端选 nat + 填该公钥 → `online_by_id` 连上（跨网/NAT 后同样成立）。
4. **selfhost**：客户端选 selfhost + 同一 relay/dns + 节点公钥 → 连上（依赖自建设施可达）。

## 6. 后续（承接本设计）

- 增量 2 落地后：加频道 pub/sub 界面（订阅/发布、频道列表）。
- 身份导入/导出、多身份切换。
- Web：接 `im-gateway`。

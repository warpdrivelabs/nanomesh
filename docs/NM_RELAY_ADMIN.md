# nm-relay-admin — Relay / Pkarr 监控与管理

独立管理台，部署在 **relay.robot.link**（与 `iroh-relay` / `iroh-dns-server` 同机），用于：

- 总览流量、接入/断开、丢包、pkarr 发布指标（抓取本机 Prometheus）
- 观测连上来的对等节点（endpoint id）与粗粒度状态
- **动态拉黑**：拒绝指定 endpoint 接入中继（经 iroh-relay `access.http`）
- Pkarr 探针：按 z-base-32 公钥查一条记录

## 构建

```bash
cargo build --release -p nm-relay-admin
# 产物：target/release/nm-relay-admin（或 CARGO_TARGET_DIR 下）
```

## 运行（relay.robot.link 生产）

已部署形态：

| 端口 | 绑定 | 用途 |
|------|------|------|
| **8450/tcp HTTPS** | `0.0.0.0` | **外网管理台** `https://relay.robot.link:8450/` |
| 9620/tcp HTTP | `127.0.0.1` | 仅本机：iroh-relay `access.http` 回调 |

```bash
# 单元：nm-relay-admin.service → /opt/nmrelay/bin/run-relay-admin.sh
# 证书：复用 /etc/letsencrypt/live/relay.robot.link/
# 云安全组须放行 TCP 8450
```

首次启动日志会打印一次性管理员密码（用户 `admin`），请立即登录改密。

## 接入 iroh-relay 动态拉黑（必需）

编辑 `/etc/iroh-relay/relay.toml`，把原来的 `access = "everyone"` 换成：

```toml
[access.http]
url = "http://127.0.0.1:9620/relay-auth"
bearer_token = "与 --relay-auth-token 相同"
```

然后：

```bash
systemctl restart iroh-relay
# 确认 nm-relay-admin 已先启动，否则 relay 会拒绝所有人
```

每次客户端连中继，relay 会 `POST /relay-auth` 并带上 `X-Iroh-Endpoint-Id`。  
管理台据此记观测；黑名单内的 id 返回非 `true` → **拒绝中继**。

> 注意：拉黑只影响 **中继接入**。已建立的连接需等断开后再生效；pkarr 发布不经此闸门。

## systemd 示例

`/etc/systemd/system/nm-relay-admin.service`：

```ini
[Unit]
Description=nm-relay-admin (relay/pkarr console)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=/opt/nmrelay
ExecStart=/opt/nmrelay/bin/nm-relay-admin \
  --listen 127.0.0.1:9620 \
  --state /opt/nmrelay/relay-admin.state.json \
  --data /opt/nmrelay/relay-admin-data.json \
  --relay-auth-token FILE:/opt/nmrelay/relay-auth.token
# 若 clap 不支持 FILE: 前缀，把 token 直接写在 ExecStart 或 Environment=
Environment=RUST_LOG=nm_relay_admin=info
Restart=on-failure
RestartSec=3

[Install]
WantedBy=multi-user.target
```

（token 建议用 `Environment=RELAY_AUTH_TOKEN=...` 并在后续版本支持环境变量；当前用 CLI 参数。）

## API 摘要

| 路径 | 说明 |
|------|------|
| `POST /relay-auth` | iroh-relay 回调（Bearer + `X-Iroh-Endpoint-Id`） |
| `GET /api/overview` | 指标总览 + 节点/黑名单计数 |
| `GET /api/peers` | 观测到的对等节点 |
| `POST /api/ban` `{id, note?}` | 拉黑 |
| `POST /api/unban` `{id}` | 解禁 |
| `GET /api/pkarr` | pkarr 指标摘要 |
| `GET /api/pkarr/{z32}` | 探针拉取一条 pkarr 包 |

管理 API 需登录 cookie（`ra_session`）。

## 指标局限

iroh-relay Prometheus **没有按 endpoint 的字节计数**，带宽为全站聚合；  
对等节点列表来自 `access.http` 回调观测（需按上文切换 access 模式），不是 QUIC 实时连接表。

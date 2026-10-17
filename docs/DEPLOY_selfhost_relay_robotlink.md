# 自建 iroh-relay + iroh-dns-server 部署实战：robot.link 版（iroh 1.3）

> 目的：部署一套**自建**的 iroh 中继（relay）+ pkarr/DNS 发现服务，作为 nmspace 全部节点/客户端的
> 统一穿透 + 按公钥发现基础设施，**根治 CNAT 节点间直连不可靠导致的跨节点私聊/presence 失效**。
> 本文是**实战记录**（含真实路径/命令/踩坑），可直接照做复现。
>
> 关联：设计论证见 [`CROSSNODE_DELIVERY_DIAGNOSIS_AND_RELAY.md`](./CROSSNODE_DELIVERY_DIAGNOSIS_AND_RELAY.md)；
> 通用两机版见 [`DEPLOY_selfhost_relay_dns.md`](./DEPLOY_selfhost_relay_dns.md)（那份基于 iroh 1.2，本文为 1.3 + robot.link 实况）。

| 项 | 值 |
|---|---|
| 部署日期 | 2026-10-16 |
| iroh 版本 | **1.3.0**（`iroh-relay` / `iroh-dns-server` 均 1.3.0） |
| 宿主 | robot.link（阿里云 ECS，公网 `8.152.101.120`，内网 `172.16.2.136`，cn-beijing，VPC） |
| 系统 | Fedora Linux 44，x86_64，2 核 / 1.6G 内存 / 40G 盘 |
| 域名 | `robot.link`（已有 Let's Encrypt 证书，复用于 relay + pkarr） |
| 模式 | 复用 `robot.link` 域名 + 已有证书，**relay/dns 全用非标端口**，与既有 `nm-domain`（占 80/443）并存 |

---

## 1. 架构与端口规划

因 robot.link 上 80/443 已被既有服务 `nm-domain` 占用、53 被 `systemd-resolve` 占用，且只有
`robot.link` 一个域名（无 `relay.`/`dns.` 子域名解析），故**全部走非标端口、复用同一张证书**。

```
                        公网
                         │
        ┌────────────────┼─────────────────┐
        │                │                 │
   :8443/tcp        :7842/udp          :8444/tcp
  iroh-relay       iroh-relay      iroh-dns-server
  (HTTPS/WSS 中继)  (QUIC 地址发现)   (pkarr /pkarr)
        │                                │
        └──────── robot.link (8.152.101.120) ────────┘
             复用 /etc/letsencrypt/live/robot.link/ 证书

既有服务（不动）：nm-domain :80,:443 / systemd-resolve :53 / nmd :9600 / sshd :22
```

| 组件 | 端口 | 协议 | 用途 | 对外放行 |
|---|---|---|---|---|
| iroh-relay | **8443** | tcp/https+wss | 主中继（NAT 穿透转发 + 打洞协调） | ✅ 需安全组放行 |
| iroh-relay | **7842** | udp | QUIC 地址发现（QAD，助打洞） | ✅ 需安全组放行 |
| iroh-relay | 8080 | tcp/http | captive portal（选配） | 可不放行 |
| iroh-relay | 9090 | tcp | metrics（仅 127.0.0.1） | 否（本机） |
| iroh-dns-server | **8444** | tcp/https | pkarr `GET/PUT /pkarr`（按公钥发现） | ✅ 需安全组放行 |
| iroh-dns-server | 15353 | udp+tcp | DNS（本机，未对外；nmspace 用 pkarr 即可） | 否（本机） |
| iroh-dns-server | 9117 | tcp | metrics（仅 127.0.0.1） | 否（本机） |

**给节点/客户端用的两个 URL**：
- relay：`https://robot.link:8443`
- pkarr：`https://robot.link:8444/pkarr`

---

## 2. 前置条件

- robot.link 可 SSH（`root`）。
- 已有 `robot.link` 的 Let's Encrypt 证书：`/etc/letsencrypt/live/robot.link/{fullchain.pem,privkey.pem}`。
- 服务器 x86_64 Linux（本文 Fedora 44）。

**步骤 1｜安装目录**
```bash
ssh root@robot.link
mkdir -p /opt/nmrelay/bin /etc/iroh-relay /etc/iroh-dns /opt/nmrelay/certs
```

---

## 3. 构建二进制（x86_64）

> robot.link 自身 rustup 未设默认 toolchain、且 crates.io 403，故**在 build box（cmxdev18，同为 x86_64）交叉构建后拷入**。

### 3.1 在 build box 构建
```bash
# 在 cmxdev18（x86_64；CARGO_HOME=/data/cargo，已配 rsproxy 镜像）
export CARGO_HOME=/data/cargo
cd ~ && cargo new relaybuild --bin && cd relaybuild
cargo add iroh-relay@1.3 --features server
cargo add iroh-dns-server@1.3

RL=$(find /data/cargo/registry/src -maxdepth 2 -type d -name 'iroh-relay-1.3.0' | head -1)
DS=$(find /data/cargo/registry/src -maxdepth 2 -type d -name 'iroh-dns-server-1.3.0' | head -1)
# 注意：--manifest-path 指向 registry 源码时，cargo 把 target 建在【源码目录】下
cargo build --release --manifest-path "$RL/Cargo.toml" --features server
cargo build --release --manifest-path "$DS/Cargo.toml"

# 产物：
#   $RL/target/release/iroh-relay
#   $DS/target/release/iroh-dns-server
```

**踩坑**：不能在独立项目里用 `cargo build -p iroh-relay --features server`（报 `cannot specify features for packages outside of workspace`）；必须 `--manifest-path` 直指 registry 源码的 `Cargo.toml`。另：`target/` 会建在**源码目录**下，不在你的项目目录。

### 3.2 拷贝到 robot.link
```bash
# 从 build box 拉到本机中转，再推到 robot.link（robot.link 未授权公钥，用 sshpass 密码）
scp cmxdev@192.168.157.18:$RL/target/release/iroh-relay       /tmp/nmrelay-bin/
scp cmxdev@192.168.157.18:$DS/target/release/iroh-dns-server  /tmp/nmrelay-bin/
sshpass -p '<密码>' scp /tmp/nmrelay-bin/iroh-relay /tmp/nmrelay-bin/iroh-dns-server root@robot.link:/opt/nmrelay/bin/
ssh root@robot.link 'chmod +x /opt/nmrelay/bin/* && /opt/nmrelay/bin/iroh-relay --version'
# → iroh-relay 1.3.0
```

---

## 4. 配置 iroh-relay

**配置文件 `/etc/iroh-relay/relay.toml`**（`--config-path` 传入）：
```toml
enable_relay = true
http_bind_addr = "0.0.0.0:8080"        # captive portal，非标端口避开 nm-domain 的 80
enable_quic_addr_discovery = true      # 开 QAD 助打洞（需 TLS）
enable_metrics = true
metrics_bind_addr = "127.0.0.1:9090"
key_cache_capacity = 1048576
access = "everyone"                     # 内网自建先全放开；可选 allowlist/denylist/shared_token

[tls]
https_bind_addr = "0.0.0.0:8443"       # 主 HTTPS/WSS（非 443）
quic_bind_addr = "0.0.0.0:7842"        # QUIC 地址发现（UDP）
hostname = ["robot.link"]
cert_mode = "Manual"                    # 直接复用已有证书
manual_cert_path = "/etc/letsencrypt/live/robot.link/fullchain.pem"
manual_key_path  = "/etc/letsencrypt/live/robot.link/privkey.pem"
```

**要点**：relay 的 Manual 模式用 **显式路径**（`manual_cert_path`/`manual_key_path`），与 dns-server 不同。

**测试运行**：
```bash
/opt/nmrelay/bin/iroh-relay --config-path /etc/iroh-relay/relay.toml
# 应有日志：QUIC server listening on 0.0.0.0:7842；HTTPS 起在 8443
```
> `iroh-relay` 的参数是 **`--config-path`**（不是 `--config`）。

---

## 5. 配置 iroh-dns-server

**证书摆放（Manual 模式的固定文件名规则）**：
dns-server 的 `CertMode::manual` 从 **`<data_dir>/cert_cache/<域名>.crt`** 与 **`.key`** 读取，
文件名由 `escape_hostname(域名)` 决定（仅保留 `[a-zA-Z0-9-.]`）→ `robot.link` → `robot.link.crt/.key`。

```bash
mkdir -p /opt/nmrelay/dns-data/cert_cache
cp /etc/letsencrypt/live/robot.link/fullchain.pem /opt/nmrelay/dns-data/cert_cache/robot.link.crt
cp /etc/letsencrypt/live/robot.link/privkey.pem  /opt/nmrelay/dns-data/cert_cache/robot.link.key
chmod 644 /opt/nmrelay/dns-data/cert_cache/robot.link.crt
chmod 600 /opt/nmrelay/dns-data/cert_cache/robot.link.key
```

**配置文件 `/etc/iroh-dns/config.toml`**（`--config` 传入）：
```toml
pkarr_put_rate_limit = "smart"
data_dir = "/opt/nmrelay/dns-data"

[https]
port = 8444
bind_addr = "0.0.0.0"
domains = ["robot.link"]
cert_mode = "manual"                    # ← 必须小写！

[dns]                                    # 仅本机（53 被 systemd-resolve 占，且 nmspace 用 pkarr 即可）
port = 15353
bind_addr = "127.0.0.1"
default_soa = "ns1.robot.link hostmaster.robot.link 0 10800 3600 604800 3600"
default_ttl = 30
origins = ["robot.link", "."]
rr_a = "8.152.101.120"
rr_ns = "ns1.robot.link."

[mainline]
enabled = false
```

**踩坑**：
1. `cert_mode` 枚举值**必须小写**（`manual`/`lets_encrypt`/`self_signed`）——写 `Manual` 报 `unknown variant`。
2. Manual 证书路径**不是** `--config` 里给的，而是固定 `<data_dir>/cert_cache/<域名>.crt`——放错目录报 `cannot open certificate file`。
3. `[https] domains` 在 manual 模式**只支持单个域名**（多域名报 `Multiple domains in manual mode are not supported`）。

**测试运行**：
```bash
/opt/nmrelay/bin/iroh-dns-server --config /etc/iroh-dns/config.toml
# 应有日志：HTTPS server listening on 0.0.0.0:8444；DNS server listening on 127.0.0.1:15353
```

---

## 6. systemd 托管（开机自启 + 崩溃重启）

**`/etc/systemd/system/iroh-relay.service`**：
```ini
[Unit]
Description=Iroh Relay (self-hosted, robot.link)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/opt/nmrelay/bin/iroh-relay --config-path /etc/iroh-relay/relay.toml
Restart=on-failure
RestartSec=3
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

**`/etc/systemd/system/iroh-dns.service`**：
```ini
[Unit]
Description=Iroh DNS/pkarr Server (self-hosted, robot.link)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/opt/nmrelay/bin/iroh-dns-server --config /etc/iroh-dns/config.toml
Restart=on-failure
RestartSec=3
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

启用：
```bash
systemctl daemon-reload
systemctl enable --now iroh-relay.service iroh-dns.service
systemctl is-active iroh-relay iroh-dns   # active active
ss -tlnp | grep -E ':8443|:8444|:8080'    # 三个 LISTEN
ss -ulnp | grep ':7842'                    # QUIC
```

---

## 7. ⚠️ 阿里云安全组（必须，本步完成前外部不可达）

robot.link 的对外可达性由**阿里云安全组**控制（本机 firewalld/iptables 均为 ACCEPT，不拦）。
外部实测（从 cmxdev18 扫描）：

| 端口 | 状态 | 说明 |
|---|---|---|
| 8443/tcp | ✅ 已通 | relay 主端口（安全组默认已放行？实测可连） |
| 7842/udp | ❌ filtered | **需放行** |
| 8444/tcp | ❌ filtered | **需放行** |
| 9600 | ❌ filtered | nmd 自带（另有直连问题，未放行） |

**操作**：阿里云控制台 → ECS 实例 `i-2zeho5fztxtsm9tlbamn`（cn-beijing，VPC `vpc-2zedpseunlnshzxc3nqt0`）→ 安全组 → 入方向 → 添加：
- `8444/tcp`  源 `0.0.0.0/0`（pkarr 发现）
- `7842/udp`  源 `0.0.0.0/0`（QUIC 地址发现）
- （`8443/tcp` 若未放行也需加）

本机无 `aliyun` CLI 与凭证，**此步无法脚本化，必须在控制台操作**。

---

## 8. 节点/客户端切换 selfhost

改各 `nmd.toml`：
```toml
mode = "selfhost"

[relay]
url = "https://robot.link:8443"

[dns]
url = "https://robot.link:8444/pkarr"
```
客户端（app）登录时选 `selfhost` 模式，填同一套 relay/pkarr。

**硬要求**：所有节点 + 客户端必须指向**同一套** relay/pkarr（不同 relay/发现域会互相发现不到）。

全量切换清单（seed）：cmxdev18、cmxdev46、cmxlinux、robot.link 自身、mac 本机。

---

## 9. 端到端验证

```bash
# 1) 外部可达性（从任一公网主机）
for p in 8443 8444 7842; do nc -z -w4 robot.link $p && echo "$p open" || echo "$p filtered"; done

# 2) relay TLS + 证书（从有 openssl 的主机）
echo | openssl s_client -connect robot.link:8443 -servername robot.link 2>&1 | grep -E 'verify return|CONNECTED'

# 3) pkarr 可读写
curl -s --max-time 5 https://robot.link:8444/pkarr -o /dev/null -w "pkarr HTTP=%{http_code}\n"

# 4) 切换后：两节点双向私聊往返通 + fed sync 不再 timed out
#    journalctl -u nmd | grep -c 'fed sync failed'   → 应归零
#    admin /system 的 group_acks ≈ group_fanouts     → ack 闭环
```

---

## 10. 运维速查

| 操作 | 命令 |
|---|---|
| 状态 | `systemctl status iroh-relay iroh-dns` |
| 日志 | `journalctl -u iroh-relay -f` / `journalctl -u iroh-dns -f` |
| 重启 | `systemctl restart iroh-relay iroh-dns` |
| 证书续期后 | 更新 `/opt/nmrelay/dns-data/cert_cache/robot.link.{crt,key}` → `systemctl restart iroh-dns`（relay 直接读 letsencrypt 路径，续期即生效） |
| 改配置 | 改 `/etc/iroh-relay/relay.toml` 或 `/etc/iroh-dns/config.toml` → `systemctl restart` |
| 端口查看 | `ss -tlnp \| grep -E ':8443\|:8444'`；`ss -ulnp \| grep 7842` |

**关键文件清单**：
```
/opt/nmrelay/bin/iroh-relay                 # relay 二进制
/opt/nmrelay/bin/iroh-dns-server            # dns-server 二进制
/etc/iroh-relay/relay.toml                  # relay 配置
/etc/iroh-dns/config.toml                   # dns-server 配置
/opt/nmrelay/dns-data/cert_cache/robot.link.{crt,key}   # dns-server 的 Manual 证书
/etc/systemd/system/iroh-relay.service      # systemd
/etc/systemd/system/iroh-dns.service        # systemd
/opt/nmrelay/dns-data/signed-packets-1.db   # pkarr 记录库（持久、自动重建）
```

---

## 11. 故障排查速查

| 现象 | 原因 | 处置 |
|---|---|---|
| `unknown variant 'Manual'` | cert_mode 大小写 | 改小写 `manual` |
| `cannot open certificate file .../cert_cache/x.crt` | 证书没放 cert_cache | 按 §5 放到 `<data_dir>/cert_cache/<域名>.crt` |
| `Multiple domains in manual mode` | manual 多域名 | 只留一个域名 |
| 外部端口 filtered | 阿里云安全组 | 控制台放行（§7） |
| 外部 curl reset / 000，但 openssl 正常 | mac 自带 LibreSSL 3.3.6 与 relay 兼容问题 | 非服务端问题；iroh 用 rustls，不受影响 |
| relay 起不来，8443 被占 | 端口冲突 | 改 `https_bind_addr` |
| `--config` 不识别 | relay 用 `--config-path` | 见 §4 |

---

## 12. 结论与遗留

- **relay（8443）已验证**：外部主机 TLS 握手成功、证书链有效（Let's Encrypt）。
- **pkarr（8444）+ QUIC（7842）**：服务端正常监听，**待安全组放行后即可外部达**。
- **下一步（阻塞项）**：① 阿里云控制台放行 8444/tcp + 7842/udp；② 各 seed/客户端切 `selfhost`；③ 双向私聊 + presence 端到端验证；④ 监控 `fed sync failed` 归零。

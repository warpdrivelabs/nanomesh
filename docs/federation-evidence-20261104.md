# 联邦拨号问题证据包（2026-11-04）

采自本机 nmd（node id `3e1e2c5c0692…`，mode=selfhost，relay.robot.link）今日日志与线上 pkarr 记录。

## 一、pkarr 记录只有 relay TXT，0 条 A 记录

抓取 `https://relay.robot.link:8444/pkarr/<z32(node-id)>`（两节点同为 191 字节、结构相同）：

- 结构：`0-64 签名 | 64-72 时间戳(µs,BE) | 72- DNS 包`
- 内容：仅 1 条 TXT `_iroh4tdidkrzqrre5fq4k97uqjzryzpkuzgrjiohn4kxkzb6eb4yo35to` → `relay=https://relay.robot.link:8443/`
- **没有任何 A 记录**（直连地址/NAT 公网映射均未发布）
- 种子发布时间 12:48:43Z（持续重发布、时间戳新鲜）；本机 12:51:54Z

种子记录 HEX：
```
    0: 1141310fcc145c26a25549becc2b7fe9e867c44421ee1e150741d807bd07362a4173b5cc
   36: 0bf2f56ed5efd8e8483c7a092ee7ca7a268028182449cdcd656ae10e00065f72cc8dd9cf
   72: 000080000000000100000000055f69726f6834746469646b727a71727265356671346b39
  108: 3775716a7a72797a706b757a67726a696f686e346b786b7a6236656234796f3335746f00
  144: 001000010000001e00252472656c61793d68747470733a2f2f72656c61792e726f626f74
  180: 2e6c696e6b3a383434332f
```

推论：dial-by-id 的地址解析结果**只有 relay 一条路**；pkarr 发布走 HTTPS、与 QUIC 中继连接是两条独立路径——记录新鲜 ≠ relay 连接活着。

## 二、本机出方向拨号全部超时（gossip + fed sync）

```
12:37:04 WARN iroh_gossip::net: dial failed: timed out peer=cb98a68b47 / 4072d04515 / e387231cbc
12:37:20 WARN iroh_gossip::net: dial failed: timed out peer=87fee72ead
12:29:42 WARN iroh_gossip::net: dial failed: No addressing information available peer=a78000cc1e
```

今日累计 `fed sync failed` 1085 次；会话抖动模式为「session up → recv Command(gram_id=1) → 100ms 内 down」（入方向同步的短会话形态）。

但入方向正常（数据持续同步）：当前 uptime 1002s，grams_in 593 / grams_out 599，7 个成员 last_seen 全部 <1 分钟。

## 三、本机 NAT 公网映射持续漂移（nmd 检测到后已重发布）

同一 `106.120.215.34` 后的映射端口每次启动/漂移都变：

```
06:27:58 NM_NODE_ADDR.addrs = [Relay, Ip 10.2.186.139:9600, Ip 106.120.215.34:29948, Ip 192.168.144.188:9600]
12:29:40 NM_NODE_ADDR.addrs = [Relay, Ip 10.2.186.139:9600, Ip 106.120.215.34:26985]
```

即使 A 记录发布出来，对称 NAT 下映射对其他对端也未必可用——直连只是「锦上添花」，中继仍是主路径，这更凸显 relay 假死是当前唯一阻塞。

## 四、传输层其他异常（同一问题的不同切面）

```
06:29:00 WARN iroh::protocol: Ignoring connection: accepting failed: authentication failed
12:51:28~12:53:04 WARN iroh::protocol: Ignoring connection: invalid handshake: aborted by peer (×4)
06:29:01 WARN nm_node::protocol: write reply failed: iroh transport error: connection lost
12:29~12:40 WARN iroh_gossip::net: failed to send: connection task send loop terminated (刷屏后触发自重启)
```

本机 nmd 看门狗于 12:29 自重启一次（fdfc8ce 的自重启兜底生效，属对症状的自愈）。

## 五、客户端侧验证（排除法）

- 客户端自愈链路正常：11:24:54 硬重绑 `dial 成功 → 自愈重连成功`（conn=None 盲区修复后实测）
- 客户端消息链路正常：05:50 发送消息单勾→双勾、落库各 1 行
- 登录路径走 registry 提供的地址，不依赖 pkarr——客户端不受本问题影响
- 客户端残留独立问题（另案）：进程内 iroh HTTP 到 relay.robot.link:8444 报 `Error resolving http request`（发布 pkarr 失败），疑似 TLS 指纹被选择性重置（app 的 reqwest 0.12/ring vs nmd 的 0.13/aws-lc），见 `clients/app/src-tauri/src/registry/http.rs:56` 注释先例

## 结论与请求

1. 种子及全部成员节点升级 ≥ eb45ebe 并重启（relay 掉线自愈）——解除当前阻塞
2. transport 侧核实 PkarrPublisher 发布内容/时机：记录里应含直连地址（A 记录），地址发现完成后应重发布——relay 不再单点

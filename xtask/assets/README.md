# NANO MESH 后端发布包

解压即可运行的去中心化网格节点后端：**nmd**（节点守护进程）+ **nm-admind**（Web 管理台）+
**nm-echo**（可选的回声测试机器人）。

## 快速开始

**macOS / Linux**

```bash
./start-all.sh     # 启动 nmd + nm-admind（首次运行自动生成 nmd.toml 与随机管理令牌）
./stop-all.sh      # 停止
./restart-all.sh   # 重启
```

> macOS 若提示「无法验证开发者」：`xattr -dr com.apple.quarantine .` 后重试。

**Windows**

双击 `start-all.bat`（或 PowerShell 运行 `start-all.ps1`）；停止/重启用对应 `stop-all.bat` / `restart-all.bat`。

## 启动后

- 管理台：<http://127.0.0.1:9610/> —— 首次登录 `admin / nmspace-admin`，**请立即修改密码**
- 节点日志 `logs/nmd.log`：其中 `NM_NODE_ID=…` 是本节点公钥（其他节点按它配置对等）
- 数据目录 `data/`：`nmd.identity`（节点私钥，**务必备份、切勿泄露**）与 `nmd.redb`（存储）

## 配置

编辑 `nmd.toml`（改完 `./restart-all.sh` 生效）：

| 项 | 说明 |
|---|---|
| `mode` | `nat`（公共中继+打洞，默认）/ `selfhost`（自建 relay+dns）/ `lan`（仅同网直连） |
| `bind_port` | 固定 UDP 端口（默认 9600；同网直连 + 防火墙放行用） |
| `[[peers]] id` | 对等节点公钥（64 位 hex），可配多个 |
| `[membership] federation` | 联邦名，同名节点自动互相发现 |

管理台监听地址可用环境变量覆盖：`ADMIND_LISTEN=0.0.0.0:8610 ./start-all.sh`。

## 防火墙

对外需放行：**UDP 9600**（节点互联）、**TCP 9610**（管理台，如需外部访问）。
`127.0.0.1:9611` 是节点控制 API，仅本机回环，无需放行。

## 回声机器人（可选，用于收发测试）

```bash
ADDR=$(grep -a '^NM_NODE_ADDR=' logs/nmd.log | head -1 | cut -d= -f2-)
nohup ./bin/nm-echo --node "$ADDR" --name "回声机器人" > logs/echo.log 2>&1 &
```

在客户端目录里刷新即可看到它，发消息它会原样回声。

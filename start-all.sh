#!/usr/bin/env bash
#
# start-all.sh — 一键启动 nmspace 后端守护进程（nmd + nm-admind）。
#
# 启动契约（参考 cmx-portalservice/bash/appctl.sh）：
#   1) cd 到仓库根（nmd.toml / *.identity / *.redb 的相对路径基准）
#   2) cargo build --release 预编译两个守护进程
#   3) nohup 拉起 release 二进制；PID 写入 run/*.pid，日志写入 logs/*.log
#   4) nm-admind 的 --nmd-api / --nmd-token 从 nmd.toml 的 [admin] 段解析
#      （nmd 未配置 [admin] 时给出提示——管理台仍会起，但连不到 nmd 控制 API）
#
# 用法：./start-all.sh
set -euo pipefail
cd "$(dirname "$0")"

ROOT="$(pwd)"
RUN_DIR="$ROOT/run"
LOG_DIR="$ROOT/logs"
NMD_CONFIG="$ROOT/nmd.toml"
ADMIND_LISTEN="${ADMIND_LISTEN:-0.0.0.0:9610}"
mkdir -p "$RUN_DIR" "$LOG_DIR"

# 从 nmd.toml 的 [admin] 段提取键值（去引号）；文件/键不存在则输出空。
toml_admin_val() { # $1 = key（api_addr | api_token）
    [ -f "$NMD_CONFIG" ] || return 0
    awk -v key="$1" '
        /^[[:space:]]*\[/ { sec=$0; gsub(/[[:space:]]/, "", sec) }
        sec == "[admin]" && $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
            sub(/^[^=]*=[[:space:]]*/, "")
            gsub(/^["'"'"']|["'"'"'][[:space:]]*$/, "")
            print; exit
        }
    ' "$NMD_CONFIG"
}

is_running() { [ -f "$1" ] && kill -0 "$(cat "$1")" 2>/dev/null; }

start_one() { # $1=name $2=pidfile $3=logfile ; 其余=命令与参数
    local name="$1" pidfile="$2" logfile="$3"
    shift 3
    if is_running "$pidfile"; then
        echo "[$name] 已在运行 (PID: $(cat "$pidfile"))，跳过"
        return 0
    fi
    echo "[$name] 启动中…"
    nohup "$@" >"$logfile" 2>&1 &
    echo $! >"$pidfile"
    sleep 2
    if is_running "$pidfile"; then
        echo "[$name] 启动成功 (PID: $(cat "$pidfile"))  日志: $logfile"
    else
        echo "[$name] 启动失败，请查看日志: $logfile"
        rm -f "$pidfile"
        return 1
    fi
}

echo "==> 预编译 (cargo build --release -p nmd -p nm-admind)…"
cargo build --release -p nmd -p nm-admind

# 解析 cargo 目标目录：尊重 CARGO_TARGET_DIR / .cargo 配置的共享 target（本仓即用共享 target）。
TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --no-deps --format-version 1 2>/dev/null |
    tr ',' '\n' | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' | head -1)}"
TARGET_DIR="${TARGET_DIR:-$ROOT/target}"
NMD_BIN="$TARGET_DIR/release/nmd"
ADMIND_BIN="$TARGET_DIR/release/nm-admind"

# 1) nmd（核心去中心节点）
start_one "nmd" "$RUN_DIR/nmd.pid" "$LOG_DIR/nmd.log" \
    "$NMD_BIN" --config "$NMD_CONFIG"

# 2) nm-admind（后端管理台，反代 nmd 控制 API）
API_ADDR="$(toml_admin_val api_addr)"
API_TOKEN="$(toml_admin_val api_token)"
if [ -z "$API_ADDR" ] || [ -z "$API_TOKEN" ]; then
    echo "[nm-admind] ⚠ nmd.toml 未配置 [admin] api_addr/api_token —— 管理台可启动，但无法连到 nmd 控制 API。"
    echo "            在 nmd.toml 增加下面几行后重启即可打通："
    echo "              [admin]"
    echo "              api_addr  = \"127.0.0.1:9611\""
    echo "              api_token = \"<自定义共享令牌>\""
    NMD_API="http://127.0.0.1:9611"
else
    NMD_API="http://$API_ADDR"
fi
start_one "nm-admind" "$RUN_DIR/nm-admind.pid" "$LOG_DIR/nm-admind.log" \
    "$ADMIND_BIN" --listen "$ADMIND_LISTEN" --nmd-api "$NMD_API" \
    --nmd-token "$API_TOKEN" --state "$ROOT/admind.state.json"

echo
echo "==> 全部启动完成"
echo "    nmd        日志 $LOG_DIR/nmd.log"
echo "    nm-admind  http://$ADMIND_LISTEN   日志 $LOG_DIR/nm-admind.log"
echo "    查看日志：tail -f $LOG_DIR/nmd.log"
echo "    停止：./stop-all.sh    重启：./restart-all.sh"

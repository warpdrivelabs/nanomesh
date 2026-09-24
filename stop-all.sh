#!/usr/bin/env bash
#
# stop-all.sh — 停止 nmspace 后端守护进程（顺序与启动相反：先 nm-admind，再 nmd）。
#
# 读取 run/*.pid，优雅 TERM，最多等 10s 未退则强制 KILL，最后清理 PID 文件。
#
# 用法：./stop-all.sh
set -euo pipefail
cd "$(dirname "$0")"

ROOT="$(pwd)"
RUN_DIR="$ROOT/run"

stop_one() { # $1=name $2=pidfile
    local name="$1" pidfile="$2" pid
    if [ ! -f "$pidfile" ]; then
        echo "[$name] 未运行（无 PID 文件）"
        return 0
    fi
    pid="$(cat "$pidfile")"
    if ! kill -0 "$pid" 2>/dev/null; then
        echo "[$name] 进程已退出，清理 PID 文件"
        rm -f "$pidfile"
        return 0
    fi
    echo "[$name] 停止中 (PID: $pid)…"
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 10); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done
    if kill -0 "$pid" 2>/dev/null; then
        echo "[$name] 超时，强制终止 (kill -9)"
        kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$pidfile"
    echo "[$name] 已停止"
}

stop_one "nm-admind" "$RUN_DIR/nm-admind.pid"
stop_one "nmd" "$RUN_DIR/nmd.pid"
echo "==> 全部已停止"

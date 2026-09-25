#!/usr/bin/env bash
#
# start-all.sh — NANO MESH 后端一键启动（发布包版：直接运行 ./bin 预编译二进制）。
# 首次运行自动从 nmd.toml.example 生成 nmd.toml（写入随机 admin token）。
# 可用环境变量：ADMIND_LISTEN（管理台监听地址，默认 0.0.0.0:9610）
set -euo pipefail
cd "$(dirname "$0")"
ROOT="$(pwd)"
mkdir -p run logs data
ADMIND_LISTEN="${ADMIND_LISTEN:-0.0.0.0:9610}"

# 首次运行：生成 nmd.toml + 随机 token
if [ ! -f nmd.toml ]; then
    TOKEN=$(openssl rand -hex 16 2>/dev/null || head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')
    sed "s/__ADMIN_TOKEN__/$TOKEN/" nmd.toml.example > nmd.toml
    chmod 600 nmd.toml
    echo "[init] 已生成 nmd.toml（含随机 admin token）"
fi

# 从 nmd.toml 的 [admin] 段提取键值（去引号）
toml_admin_val() { # $1 = key（api_addr | api_token）
    awk -v key="$1" '
        /^[[:space:]]*\[/ { sec=$0; gsub(/[[:space:]]/, "", sec) }
        sec == "[admin]" && $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
            sub(/^[^=]*=[[:space:]]*/, "")
            gsub(/^["'"'"']|["'"'"'][[:space:]]*$/, "")
            print; exit
        }
    ' nmd.toml
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

# 1) nmd（去中心网格节点）
start_one "nmd" "run/nmd.pid" "logs/nmd.log" \
    "$ROOT/bin/nmd" --config "$ROOT/nmd.toml"

# 2) nm-admind（后端管理台，反代 nmd 控制 API）
API_ADDR="$(toml_admin_val api_addr)"
API_TOKEN="$(toml_admin_val api_token)"
start_one "nm-admind" "run/nm-admind.pid" "logs/nm-admind.log" \
    "$ROOT/bin/nm-admind" --listen "$ADMIND_LISTEN" --nmd-api "http://${API_ADDR:-127.0.0.1:9611}" \
    --nmd-token "$API_TOKEN" --state "$ROOT/admind.state.json"

echo
echo "==> 全部启动完成"
echo "    管理台   http://${ADMIND_LISTEN/0.0.0.0/127.0.0.1}/   （首次登录 admin / nmspace-admin，请立即改密）"
echo "    节点日志 logs/nmd.log   管理台日志 logs/nm-admind.log"
echo "    停止：./stop-all.sh    重启：./restart-all.sh"

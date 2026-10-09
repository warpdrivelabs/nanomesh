#!/usr/bin/env bash
#
# test-crossnode.sh — 串行跑 nm-node 的跨节点 e2e（`#[ignore]` 用例）。
#
# 为什么要这个脚本：这些用例用 `Node::bind_local` 在一台机上起多节点 gossip 叠加网。`cargo test` 默认
# **并行**跑多个测试二进制，多个叠加网同时抢本机 QUIC/UDP 栈时 gossip 成网握手会偶发超时 —— 单跑过、
# 并行批量跑偶挂（已 A/B 确认是环境争用，非逻辑 bug）。故这些用例都标了 `#[ignore]`（默认 `cargo test`
# 不跑、不影响日常绿）；需要真跑时用本脚本**逐 target 串行**执行，消除争用。
#
# 用法：
#   ./scripts/test-crossnode.sh                 # 跑 STABLE 组（本机串行可稳过）
#   NM_PKARR_URL=http://127.0.0.1:8090/pkarr \  # 额外跑真 relay 成员索引用例（需活的 iroh-dns-server）
#     ./scripts/test-crossnode.sh
#
# ⚠️ 退出前请先停掉本机其它 iroh 实例（测试后端 nmd/compute/agentd、dns-server 等），
#    它们会和测试节点抢 QUIC 栈，导致 recv 超时假失败。
set -u
cd "$(dirname "$0")/.."

# STABLE：纯 bind_local（Minimal）本机两节点 gossip，串行 + 腾空 QUIC 栈后可稳过。
STABLE_TARGETS=(
  group_gossip
  per_topic_group_e2e
  per_topic_group_retire
  per_topic_inbox_dm
  per_topic_inbox_dm_retire
  per_topic_names
  naming
)

# ENV-DEPENDENT（本脚本不默认跑）：需特定网络环境，与本仓代码无关的既有局限——
#   m2_federation / m2b_lan : Minimal 模式多 endpoint 本机 LAN 互联抖动（作者已注明 test-env only；
#                             生产走 N0/relay）。
#   multi_device_cross      : 用 N0 预设 online_by_id，需 n0 公网发现服务可达。
#   local_direct / real_seed / real_naming : 需 NM_* 环境变量 + 真网种子节点。
# 要跑它们请按各自文件头注释单独运行，并确保对应网络环境就绪。

pass=0; fail=0; failed_list=()
run() {
  echo "──────── $1 ────────"
  if cargo test -p nm-node --test "$1" -- --ignored --test-threads=1; then
    pass=$((pass+1))
  else
    fail=$((fail+1)); failed_list+=("$1")
  fi
}

for t in "${STABLE_TARGETS[@]}"; do run "$t"; done

if [ -n "${NM_PKARR_URL:-}" ]; then
  run member_relay_realdns
else
  echo "──────── member_relay_realdns （跳过：未设 NM_PKARR_URL）────────"
fi

echo
echo "======== 串行跨节点 e2e（STABLE 组）汇总：通过 $pass / 失败 $fail ========"
if [ "$fail" -gt 0 ]; then
  echo "失败：${failed_list[*]}  —— 多为本机 QUIC 栈争用，确认已停其它 iroh 实例后单独重跑："
  echo "  cargo test -p nm-node --test <target> -- --ignored"
  exit 1
fi
echo "STABLE 组全部通过。"

#!/usr/bin/env bash
#
# restart-all.sh — 先停后启（stop-all.sh → start-all.sh）。
#
# 用法：./restart-all.sh
set -euo pipefail
cd "$(dirname "$0")"

./stop-all.sh
echo
sleep 2
./start-all.sh

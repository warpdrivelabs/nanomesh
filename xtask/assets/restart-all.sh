#!/usr/bin/env bash
# restart-all.sh — 先停后启。
set -euo pipefail
cd "$(dirname "$0")"
./stop-all.sh
sleep 2
./start-all.sh

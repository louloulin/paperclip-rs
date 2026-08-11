#!/usr/bin/env bash
# scripts/r625-ux-flow.sh — R625 真实 UX 流程端到端验证
#
# 流程：sign-up/email → sign-in/email → 创建公司 → 创建 agent → 创建 issue
#      → 触发 heartbeat → 订阅 WS /api/live-events → 验证 heartbeat 事件收到
#
# 不依赖 initdb（PG17 已经运行），通过临时 DB 复用。

set -euo pipefail
export LC_ALL=C; export LANG=C
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PG_BIN="${PG_BIN:-/opt/homebrew/opt/postgresql@17/bin}"
SRV_PORT="${PAPERCLIP_R625_HTTP_PORT:-54300}"
DB_NAME="paperclip_r625_$(date +%s)_$$"
DB_URL="postgres://louloulin@127.0.0.1:5432/${DB_NAME}"
LOG_DIR="$ROOT/.e2e-logs"
mkdir -p "$LOG_DIR"

cleanup() {
  set +e
  if [[ -n "${SRV_PID:-}" ]] && kill -0 "$SRV_PID" 2>/dev/null; then
    kill "$SRV_PID" 2>/dev/null || true
  fi
  "$PG_BIN/psql" -h 127.0.0.1 -U louloulin -d postgres -c "DROP DATABASE IF EXISTS ${DB_NAME};" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "[r625] create db: $DB_NAME"
"$PG_BIN/psql" -h 127.0.0.1 -U louloulin -d postgres -c "CREATE DATABASE ${DB_NAME};" >/dev/null

echo "[r625] pc-migrate up"
PAPERCLIP_DATABASE_URL="$DB_URL" RUST_LOG=warn \
  cargo run --quiet -p pc-migrate -- up >"$LOG_DIR/migrate-r625.log" 2>&1

echo "[r625] start pc-server :$SRV_PORT"
PAPERCLIP_DATABASE_URL="$DB_URL" PAPERCLIP_PORT="$SRV_PORT" RUST_LOG=info \
  "$ROOT/target/debug/paperclip-server" >"$LOG_DIR/server-r625.log" 2>&1 &
SRV_PID=$!

# 等 /health 200
HEALTH_OK=0
for i in $(seq 1 60); do
  if curl -fsS "http://localhost:$SRV_PORT/health" >/dev/null 2>&1; then
    HEALTH_OK=1; echo "[r625] pc-server /health 200 after $((i/2))s"; break
  fi
  sleep 0.5
done
[[ $HEALTH_OK -eq 1 ]] || { echo "[r625] FAIL: pc-server /health not 200"; tail -30 "$LOG_DIR/server-r625.log"; exit 1; }

echo "[r625] run UX flow (Python + requests + websockets)"
PAPERCLIP_BASE_URL="http://localhost:$SRV_PORT" \
  python3 "$ROOT/scripts/r625-ux-flow.py" 2>&1

echo "[r625] PASS - full UX flow completed"

#!/usr/bin/env bash
# scripts/e2e-baseline.sh — M2 E2E 基线
# 真实验证：临时 PG → pc-migrate up → 起 pc-server → curl /health → shutdown
# 失败非 0 退出，且打印诊断信息。

set -euo pipefail

# PG 启动要求会话区域为 C（locale 不为 C 时 postmaster 多线程启动会失败）
export LC_ALL=C
export LANG=C

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PG_BIN="${PG_BIN:-/opt/homebrew/opt/postgresql@16/bin}"
DATA_DIR="${TMPDIR:-/tmp}/pc-e2e-pgdata-$$"
PORT="${PAPERCLIP_TEST_PG_PORT:-55432}"
LISTEN_PORT="${PAPERCLIP_TEST_HTTP_PORT:-53100}"
LOG_DIR="$ROOT/.e2e-logs"
mkdir -p "$LOG_DIR"

cleanup() {
  set +e
  if [[ -n "${SERVER_PID:-}" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ -n "${PG_PID:-}" ]] && kill -0 "$PG_PID" 2>/dev/null; then
    "$PG_BIN/pg_ctl" -D "$DATA_DIR" -m fast stop >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

echo "[e2e] init pg data dir at $DATA_DIR"
"$PG_BIN/initdb" -D "$DATA_DIR" -U postgres --auth-local=trust --auth-host=trust >"$LOG_DIR/initdb.log" 2>&1

echo "[e2e] start pg on :$PORT"
"$PG_BIN/pg_ctl" -D "$DATA_DIR" -l "$LOG_DIR/pg.log" \
  -o "-p $PORT -k /tmp -h 127.0.0.1 -c unix_socket_directories=/tmp" start >"$LOG_DIR/pgctl.log" 2>&1
for i in 1 2 3 4 5 6 7 8 9 10; do
  if "$PG_BIN/pg_isready" -h 127.0.0.1 -p "$PORT" -U postgres >/dev/null 2>&1; then break; fi
  sleep 0.5
done

DB_URL="postgres://postgres@127.0.0.1:$PORT/postgres"
echo "[e2e] run pc-migrate up"
PAPERCLIP_DATABASE_URL="$DB_URL" RUST_LOG=info \
  cargo run --quiet -p pc-migrate -- up >"$LOG_DIR/migrate.log" 2>&1

echo "[e2e] count tables"
TABLES=$(DATABASE_URL="$DB_URL" "$PG_BIN/psql" -h 127.0.0.1 -p "$PORT" -U postgres -d postgres -At -c \
  "select count(*) from information_schema.tables where table_schema='public' and table_type='BASE TABLE'")
echo "[e2e] table count = $TABLES"

echo "[e2e] start pc-server on :$LISTEN_PORT"
PAPERCLIP_DATABASE_URL="$DB_URL" PAPERCLIP_PORT="$LISTEN_PORT" RUST_LOG=info \
  cargo run --quiet -p pc-server -- >"$LOG_DIR/server.log" 2>&1 &
SERVER_PID=$!

for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  sleep 0.5
  if curl -fsS "http://127.0.0.1:$LISTEN_PORT/health" >/dev/null 2>&1; then
    echo "[e2e] /health 200 after ${i}*0.5s"
    HEALTH_OK=1; break
  fi
done

HEALTH=$(curl -s -o /tmp/health.body -w "%{http_code}" "http://127.0.0.1:$LISTEN_PORT/health" || echo "000")
echo "[e2e] final /health status = $HEALTH"
echo "[e2e] /health body = $(cat /tmp/health.body)"

if [[ "$HEALTH" != "200" ]]; then
  echo "[e2e] FAIL: /health not 200"
  tail -50 "$LOG_DIR/server.log"
  exit 1
fi

echo "[e2e] PASS"

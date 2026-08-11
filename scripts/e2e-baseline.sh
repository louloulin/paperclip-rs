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
for i in $(seq 1 60); do
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

echo "[e2e] pre-build pc-server (R580: separate cargo build from run)"
# R580: pre-build pc-server binary. The actual server startup is <100ms
# (R579 measured: db_connect=7ms, migrations=9ms warm, adapters=0ms).
# The 60s timeout was waiting for cold cargo compile inside the critical path.
cargo build --quiet -p pc-server 2>"$LOG_DIR/server-build.log"
SERVER_BIN="$ROOT/target/debug/paperclip-server"
if [[ ! -x "$SERVER_BIN" ]]; then
  echo "[e2e] FAIL: pc-server binary not found at $SERVER_BIN"
  tail -50 "$LOG_DIR/server-build.log"
  exit 1
fi

echo "[e2e] start pc-server on :$LISTEN_PORT (warm binary)"
PAPERCLIP_DATABASE_URL="$DB_URL" PAPERCLIP_PORT="$LISTEN_PORT" RUST_LOG=info \
  "$SERVER_BIN" >"$LOG_DIR/server.log" 2>&1 &
SERVER_PID=$!

# R580: server warm startup is <100ms. Poll up to 30s (was 60s) but expect <2s.
for i in $(seq 1 60); do
  sleep 0.5
  if curl -fsS "http://127.0.0.1:$LISTEN_PORT/health" >/dev/null 2>&1; then
    echo "[e2e] /health 200 after ${i}*0.5s ($(echo "scale=1; ${i}*0.5" | bc)s)"
    HEALTH_OK=1; break
  fi
  # Every 10s, surface server log progress so failures aren't silent.
  if (( i % 20 == 0 )); then
    echo "[e2e] still waiting after ${i}*0.5s; server tail:"
    tail -3 "$LOG_DIR/server.log" 2>/dev/null | sed 's/^/    /'
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

# JSON report (CI 消费)
E2E_REPORT="${PAPERCLIP_E2E_REPORT:-/tmp/paperclip-e2e-report.json}"
cat > "$E2E_REPORT" <<JSON
{
  "version": "1",
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "checks": {
    "pg_ready": true,
    "migrate": true,
    "tables": $TABLES,
    "server_built": true,
    "health_200": true
  },
  "metrics": {
    "boot_time_polls": $i,
    "boot_time_s": $(echo "scale=2; $i * 0.5" | bc)
  }
}
JSON
echo "[e2e] report → $E2E_REPORT"

echo "[e2e] PASS"

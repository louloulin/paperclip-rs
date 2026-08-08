#!/usr/bin/env bash
# scripts/e2e-full-stack.sh — M18 前后端端到端 Playwright 验证
#
# 真实启动：临时 PG + migrate + pc-server。
# 然后跑 `tests/e2e` 下的 Playwright spec（API 合约层）。
# 任何步骤失败非 0 退出。

set -euo pipefail
export LC_ALL=C; export LANG=C
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PG_BIN="${PG_BIN:-/opt/homebrew/opt/postgresql@16/bin}"
DATA_DIR="${TMPDIR:-/tmp}/pc-e2e-pgdata-$$"
PG_PORT="${PAPERCLIP_E2E_PG_PORT:-$(( 55440 + (RANDOM % 200) ))}"
SRV_PORT="${PAPERCLIP_E2E_HTTP_PORT:-$(( 53200 + (RANDOM % 200) ))}"
LOG_DIR="$ROOT/.e2e-logs"
mkdir -p "$LOG_DIR"

cleanup() {
  set +e
  [[ -n "${SRV_PID:-}" ]] && kill -0 "$SRV_PID" 2>/dev/null && kill "$SRV_PID" 2>/dev/null
  [[ -n "${PG_PID:-}" ]] && "$PG_BIN/pg_ctl" -D "$DATA_DIR" -m fast stop >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "[m18] init pg at $DATA_DIR"
"$PG_BIN/initdb" -D "$DATA_DIR" -U postgres --auth-local=trust --auth-host=trust >"$LOG_DIR/initdb.log" 2>&1
"$PG_BIN/pg_ctl" -D "$DATA_DIR" -l "$LOG_DIR/pg.log" \
  -o "-p $PG_PORT -k /tmp -h 127.0.0.1 -c unix_socket_directories=/tmp" start >"$LOG_DIR/pgctl.log" 2>&1
for i in 1 2 3 4 5 6 7 8 9 10; do
  if "$PG_BIN/pg_isready" -h 127.0.0.1 -p "$PG_PORT" -U postgres >/dev/null 2>&1; then break; fi
  sleep 0.5
done

DB_URL="postgres://postgres@127.0.0.1:$PG_PORT/postgres"
echo "[m18] pc-migrate up"
PAPERCLIP_DATABASE_URL="$DB_URL" RUST_LOG=info \
  cargo run --quiet -p pc-migrate -- up >"$LOG_DIR/migrate.log" 2>&1

echo "[m18] start pc-server :$SRV_PORT"
PAPERCLIP_DATABASE_URL="$DB_URL" PAPERCLIP_PORT="$SRV_PORT" RUST_LOG=info \
  cargo run --quiet -p pc-server -- >"$LOG_DIR/server.log" 2>&1 &
SRV_PID=$!

HEALTH_OK=0
for i in $(seq 1 120); do
  sleep 0.5
  if curl -fsS -4 "http://localhost:$SRV_PORT/health" >/dev/null 2>&1 || curl -fsS "http://localhost:$SRV_PORT/health" >/dev/null 2>&1; then
    HEALTH_OK=1
    echo "[m18] pc-server /health 200 after $((i/2))s"
    break
  fi
done
[[ $HEALTH_OK -eq 1 ]] || { echo "[m18] FAIL: pc-server /health not 200"; tail -30 "$LOG_DIR/server.log"; exit 1; }

echo "[m18] run Playwright API-flow spec against http://localhost:$SRV_PORT"
cd "$ROOT/tests/e2e"
E2E_SERVER_URL="http://localhost:$SRV_PORT" npx playwright test \
  --project=chromium --reporter=list 2>&1 | tee "$LOG_DIR/playwright.log"
TEST_RC=${PIPESTATUS[0]}
cd "$ROOT"

if [[ $TEST_RC -ne 0 ]]; then
  echo "[m18] FAIL: Playwright tests failed (rc=$TEST_RC)"
  exit 1
fi

echo "[m18] ALL CHECKS PASSED — M18 前后端端到端 ✅"

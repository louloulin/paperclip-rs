#!/usr/bin/env bash
# scripts/ui-happy-path.sh — M18 真实浏览器 UI happy path
#
# 起 PG + pc-migrate + pc-server + Vite dev server + Chromium，
# 跑 Playwright 真实 UI 流程：登录页 → sign-up → 跳转到 dashboard。
# 真实启动 Rust 后端 + 真实浏览器，与 dev-ui-rust.sh 共用 PG/migrate/server 准备。

set -euo pipefail
export LC_ALL=C; export LANG=C
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PG_BIN="${PG_BIN:-/opt/homebrew/opt/postgresql@16/bin}"
DATA_DIR="${TMPDIR:-/tmp}/pc-ui-pgdata-$$"
PG_PORT="${PAPERCLIP_UI_PG_PORT:-$(( 55440 + (RANDOM % 200) ))}"
SRV_PORT="${PAPERCLIP_UI_HTTP_PORT:-$(( 53200 + (RANDOM % 200) ))}"
UI_PORT="${PAPERCLIP_UI_UI_PORT:-$(( 51800 + (RANDOM % 200) ))}"
LOG_DIR="$ROOT/.e2e-logs"
mkdir -p "$LOG_DIR"

cleanup() {
  set +e
  [[ -n "${VITE_PID:-}" ]] && kill -0 "$VITE_PID" 2>/dev/null && kill "$VITE_PID" 2>/dev/null
  [[ -n "${SRV_PID:-}" ]] && kill -0 "$SRV_PID" 2>/dev/null && kill "$SRV_PID" 2>/dev/null
  [[ -n "${PG_PID:-}" ]] && "$PG_BIN/pg_ctl" -D "$DATA_DIR" -m fast stop >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "[ui] init pg at $DATA_DIR"
"$PG_BIN/initdb" -D "$DATA_DIR" -U postgres --auth-local=trust --auth-host=trust >"$LOG_DIR/initdb.log" 2>&1
"$PG_BIN/pg_ctl" -D "$DATA_DIR" -l "$LOG_DIR/pg.log" \
  -o "-p $PG_PORT -k /tmp -h 127.0.0.1 -c unix_socket_directories=/tmp" start >"$LOG_DIR/pgctl.log" 2>&1
for i in 1 2 3 4 5 6 7 8 9 10; do
  if "$PG_BIN/pg_isready" -h 127.0.0.1 -p "$PG_PORT" -U postgres >/dev/null 2>&1; then break; fi
  sleep 0.5
done

DB_URL="postgres://postgres@127.0.0.1:$PG_PORT/postgres"
echo "[ui] pc-migrate up"
PAPERCLIP_DATABASE_URL="$DB_URL" RUST_LOG=info \
  cargo run --quiet -p pc-migrate -- up >"$LOG_DIR/migrate.log" 2>&1

echo "[ui] start pc-server :$SRV_PORT"
PAPERCLIP_DATABASE_URL="$DB_URL" PAPERCLIP_PORT="$SRV_PORT" \
PAPERCLIP_CORS_ALLOWED_ORIGINS="http://localhost:$UI_PORT,http://127.0.0.1:$UI_PORT" RUST_LOG=info \
  cargo run --quiet -p pc-server -- >"$LOG_DIR/server.log" 2>&1 &
SRV_PID=$!

HEALTH_OK=0
for i in $(seq 1 120); do
  sleep 0.5
  if curl -fsS -4 "http://localhost:$SRV_PORT/health" >/dev/null 2>&1 || curl -fsS "http://localhost:$SRV_PORT/health" >/dev/null 2>&1; then
    HEALTH_OK=1
    echo "[ui] pc-server /health 200 after $((i/2))s"
    break
  fi
done
[[ $HEALTH_OK -eq 1 ]] || { echo "[ui] FAIL: pc-server /health not 200"; tail -30 "$LOG_DIR/server.log"; exit 1; }

echo "[ui] start vite dev :$UI_PORT (VITE_API_BASE=http://localhost:$SRV_PORT/api)"
cd "$ROOT/ui"
( PAPERCLIP_API_TARGET="http://localhost:$SRV_PORT" VITE_API_BASE="http://localhost:$SRV_PORT/api" pnpm dev --port "$UI_PORT" --strictPort ) >"$LOG_DIR/vite.log" 2>&1 &
VITE_PID=$!
cd "$ROOT"

VITE_OK=0
for i in $(seq 1 120); do
  sleep 0.5
  if curl -fsS -4 "http://localhost:$UI_PORT" >/dev/null 2>&1 || curl -fsS "http://localhost:$UI_PORT" >/dev/null 2>&1; then
    VITE_OK=1
    echo "[ui] vite ready after $((i/2))s"
    break
  fi
done
[[ $VITE_OK -eq 1 ]] || { echo "[ui] FAIL: vite not ready"; tail -30 "$LOG_DIR/vite.log"; exit 1; }

echo "[ui] run Playwright UI happy-path spec"
cd "$ROOT/tests/e2e"
E2E_UI_URL="http://localhost:$UI_PORT" npx playwright test \
  --project=chromium --reporter=list tests/ui-happy-path.spec.ts 2>&1 | tee "$LOG_DIR/ui-playwright.log"
TEST_RC=${PIPESTATUS[0]}
cd "$ROOT"

if [[ $TEST_RC -ne 0 ]]; then
  echo "[ui] FAIL: UI happy-path test failed (rc=$TEST_RC)"
  exit 1
fi

echo "[ui] ALL CHECKS PASSED — M18 UI happy path ✅"

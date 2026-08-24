#!/usr/bin/env bash
# scripts/run-ui-e2e.sh — Section 5 Playwright e2e harness
#
# Boots: temporary PG → pc-migrate → pc-server → Vite dev server,
# then runs `tests/e2e/playwright/` Playwright specs.
#
# Environment variables:
#   PAPERCLIP_E2E_PG_PORT   PostgreSQL port  (default random in 55440–55639)
#   PAPERCLIP_E2E_HTTP_PORT pc-server port   (default random in 53200–53399)
#   PAPERCLIP_E2E_UI_PORT   Vite dev port    (default random in 51800–51999)
#   E2E_HEADLESS            Set to 0 to watch tests in a headed browser
#   SKIP_PLAYWRIGHT         Set to 1 to only start servers (no tests)
#   PLAYWRIGHT_DIR          Path to playwright test dir (default tests/e2e/playwright)
#
# Requirements:
#   - PostgreSQL 16 (pg_bin configurable via PG_BIN env var)
#   - Rust toolchain (cargo, pc-migrate, pc-server built)
#   - Node.js + pnpm (for Vite dev server)
#   - Playwright browsers: npx playwright install --with-deps chromium

set -euo pipefail
export LC_ALL=C; export LANG=C

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

PG_BIN="${PG_BIN:-/opt/homebrew/opt/postgresql@16/bin}"
DATA_DIR="${TMPDIR:-/tmp}/pc-s5-pgdata-$$"
PG_PORT="${PAPERCLIP_E2E_PG_PORT:-$(( 55440 + (RANDOM % 200) ))}"
SRV_PORT="${PAPERCLIP_E2E_HTTP_PORT:-$(( 53200 + (RANDOM % 200) ))}"
UI_PORT="${PAPERCLIP_E2E_UI_PORT:-$(( 51800 + (RANDOM % 200) ))}"
LOG_DIR="$PROJECT_ROOT/.s5-e2e-logs"
PLAYWRIGHT_DIR="${PLAYWRIGHT_DIR:-$PROJECT_ROOT/tests/e2e/playwright}"
E2E_HEADLESS="${E2E_HEADLESS:-1}"

mkdir -p "$LOG_DIR"

echo "============================================"
echo " Section 5 Playwright E2E — $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
echo "============================================"
echo "  PostgreSQL  : 127.0.0.1:$PG_PORT"
echo "  pc-server   : 127.0.0.1:$SRV_PORT"
echo "  Vite UI     : 127.0.0.1:$UI_PORT"
echo "  Playwright  : $PLAYWRIGHT_DIR"
echo "  headless    : $E2E_HEADLESS"
echo ""

# ---------------------------------------------------------------------------
# Cleanup helper
# ---------------------------------------------------------------------------
cleanup() {
  set +e
  echo "[cleanup] stopping processes..."
  [[ -n "${VITE_PID:-}" ]] && kill -0 "$VITE_PID" 2>/dev/null && kill "$VITE_PID" 2>/dev/null
  [[ -n "${SRV_PID:-}" ]] && kill -0 "$SRV_PID" 2>/dev/null && kill "$SRV_PID" 2>/dev/null
  [[ -n "${PG_PID:-}" ]] && "$PG_BIN/pg_ctl" -D "$DATA_DIR" -m fast stop >/dev/null 2>&1 || true
  echo "[cleanup] done"
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# 1 — PostgreSQL
# ---------------------------------------------------------------------------
echo "[1/4] PostgreSQL init at $DATA_DIR"
if ! command -v "$PG_BIN/initdb" >/dev/null 2>&1; then
  echo "ERROR: $PG_BIN/initdb not found — set PG_BIN or install PostgreSQL 16"
  exit 1
fi

"$PG_BIN/initdb" -D "$DATA_DIR" -U postgres \
  --auth-local=trust --auth-host=trust >"$LOG_DIR/initdb.log" 2>&1

"$PG_BIN/pg_ctl" -D "$DATA_DIR" \
  -l "$LOG_DIR/pg.log" \
  -o "-p $PG_PORT -k /tmp -h 127.0.0.1 -c unix_socket_directories=/tmp" \
  start >"$LOG_DIR/pgctl.log" 2>&1

for i in 1 2 3 4 5 6 7 8 9 10; do
  if "$PG_BIN/pg_isready" -h 127.0.0.1 -p "$PG_PORT" -U postgres >/dev/null 2>&1; then
    echo "[1/4] PostgreSQL ready"
    break
  fi
  sleep 0.5
done

# ---------------------------------------------------------------------------
# 2 — Migration
# ---------------------------------------------------------------------------
DB_URL="postgres://postgres@127.0.0.1:$PG_PORT/postgres"
echo "[2/4] pc-migrate up"
PAPERCLIP_DATABASE_URL="$DB_URL" RUST_LOG=warn \
  cargo run --quiet -p pc-migrate -- up >"$LOG_DIR/migrate.log" 2>&1
echo "[2/4] migration complete"

# ---------------------------------------------------------------------------
# 3 — pc-server
# ---------------------------------------------------------------------------
echo "[3/4] pc-server on :$SRV_PORT"
PAPERCLIP_DATABASE_URL="$DB_URL" \
  PAPERCLIP_PORT="$SRV_PORT" \
  PAPERCLIP_CORS_ALLOWED_ORIGINS="http://127.0.0.1:$UI_PORT,http://localhost:$UI_PORT" \
  RUST_LOG=warn \
  cargo run --quiet -p pc-server -- >"$LOG_DIR/server.log" 2>&1 &
SRV_PID=$!

HEALTH_OK=0
for i in $(seq 1 120); do
  sleep 0.5
  if curl -fsS "http://127.0.0.1:$SRV_PORT/health" >/dev/null 2>&1; then
    HEALTH_OK=1
    echo "[3/4] pc-server /health 200 after $((i/2))s"
    break
  fi
done
if [[ $HEALTH_OK -eq 0 ]]; then
  echo "ERROR: pc-server /health never reached 200"
  tail -30 "$LOG_DIR/server.log"
  exit 1
fi

# ---------------------------------------------------------------------------
# 4 — Vite dev server
# ---------------------------------------------------------------------------
echo "[4/4] Vite dev server on :$UI_PORT"
cd "$PROJECT_ROOT/ui"
( PAPERCLIP_API_TARGET="http://127.0.0.1:$SRV_PORT" \
  VITE_API_BASE="http://127.0.0.1:$SRV_PORT/api" \
  pnpm dev --port "$UI_PORT" --strictPort \
) >"$LOG_DIR/vite.log" 2>&1 &
VITE_PID=$!
cd "$PROJECT_ROOT"

VITE_OK=0
for i in $(seq 1 60); do
  sleep 0.5
  if curl -fsS "http://127.0.0.1:$UI_PORT" >/dev/null 2>&1; then
    VITE_OK=1
    echo "[4/4] Vite ready after $((i/2))s"
    break
  fi
done
if [[ $VITE_OK -eq 0 ]]; then
  echo "ERROR: Vite dev server never responded"
  tail -30 "$LOG_DIR/vite.log"
  exit 1
fi

# ---------------------------------------------------------------------------
# 5 — Playwright tests
# ---------------------------------------------------------------------------
if [[ "${SKIP_PLAYWRIGHT:-0}" == "1" ]]; then
  echo ""
  echo "SKIP_PLAYWRIGHT=1 — servers running, no tests run."
  echo "  pc-server : http://127.0.0.1:$SRV_PORT"
  echo "  UI        : http://127.0.0.1:$UI_PORT"
  echo "  Logs      : $LOG_DIR/"
  echo ""
  echo "Press Ctrl+C to stop."
  sleep infinity
  trap - EXIT
  exit 0
fi

echo ""
echo "============================================"
echo " Running Playwright tests"
echo "============================================"
echo "  E2E_SERVER_URL = http://127.0.0.1:$SRV_PORT"
echo "  E2E_UI_URL     = http://127.0.0.1:$UI_PORT"
echo "  E2E_HEADLESS   = $E2E_HEADLESS"
echo ""

E2E_SERVER_URL="http://127.0.0.1:$SRV_PORT" \
E2E_UI_URL="http://127.0.0.1:$UI_PORT" \
E2E_HEADLESS="$E2E_HEADLESS" \
  npx playwright test \
  --project=chromium \
  --reporter=list \
  2>&1 | tee "$LOG_DIR/playwright.log"

TEST_RC=${PIPESTATUS[0]}
echo ""
if [[ $TEST_RC -eq 0 ]]; then
  echo "ALL TESTS PASSED ✅"
else
  echo "SOME TESTS FAILED ❌ (rc=$TEST_RC)"
  echo "Logs: $LOG_DIR/"
fi
exit $TEST_RC

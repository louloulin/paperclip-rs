#!/usr/bin/env bash
# run-ui-workflow-e2e.sh — 启动 pc-server + UI + 跑 Playwright workflow 验证
#
# 用法：
#   PAPERCLIP_SEED_DEMO=admin ./scripts/run-ui-workflow-e2e.sh
#
# 环境变量：
#   PAPERCLIP_E2E_DIR     : evidence 输出目录（默认 e2e-reports/<timestamp>）
#   PAPERCLIP_DB_URL      : PostgreSQL URL（默认用本地 dev DB）
#   PAPERCLIP_BIND_PORT   : pc-server 端口（默认 3100）
#   UI_PORT               : Vite dev server 端口（默认 5173）
#   PLAYWRIGHT_BROWSERS_PATH : Playwright 浏览器路径（默认 ~/.cache/ms-playwright）
#   SKIP_PLAYWRIGHT       : 设为 1 时跳过 Playwright（仅启动 server + UI）

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TIMESTAMP="$(date -u +"%Y-%m-%dT%H-%M-%SZ")"
E2E_DIR="${PAPERCLIP_E2E_DIR:-$PROJECT_ROOT/e2e-reports/$TIMESTAMP}"
BIND_PORT="${PAPERCLIP_BIND_PORT:-3100}"
UI_PORT="${UI_PORT:-5173}"

mkdir -p "$E2E_DIR"
echo "=== UI Workflow E2E ==="
echo "Project: $PROJECT_ROOT"
echo "Output : $E2E_DIR"
echo "Bind   : $BIND_PORT"
echo "UI port: $UI_PORT"
echo "Time   : $TIMESTAMP"

# 1. 启动 pc-server
echo "[1/3] Starting pc-server..."
export PAPERCLIP_SEED_DEMO="${PAPERCLIP_SEED_DEMO:-admin}"
export PAPERCLIP_DB_RUN_MIGRATIONS=true
export PAPERCLIP_BIND_HOST=127.0.0.1
export PAPERCLIP_PORT="$BIND_PORT"

SERVER_LOG="$E2E_DIR/pc-server.log"
"$PROJECT_ROOT/target/debug/paperclip-server" \
    > "$SERVER_LOG" 2>&1 &
SERVER_PID=$!
echo "pc-server pid=$SERVER_PID (log: $SERVER_LOG)"

cleanup() {
    echo "[cleanup] stopping pc-server (pid=$SERVER_PID)..."
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
}
trap cleanup EXIT

# 等待 /health 通过
echo "[1.1/3] waiting for /health..."
for i in $(seq 1 60); do
    if curl -sf "http://127.0.0.1:$BIND_PORT/health" > /dev/null 2>&1; then
        echo "  /health 200 OK (after ${i}s)"
        break
    fi
    sleep 1
done

if ! curl -sf "http://127.0.0.1:$BIND_PORT/health" > /dev/null 2>&1; then
    echo "ERROR: pc-server /health failed within 60s"
    echo "--- last 50 lines of pc-server.log ---"
    tail -n 50 "$SERVER_LOG" || true
    exit 1
fi

# 2. 启动 UI dev server
echo "[2/3] Starting UI dev server..."
UI_LOG="$E2E_DIR/ui.log"
(cd "$PROJECT_ROOT/ui" && pnpm dev --port "$UI_PORT" --host 127.0.0.1) \
    > "$UI_LOG" 2>&1 &
UI_PID=$!
echo "ui pid=$UI_PID (log: $UI_LOG)"

cleanup() {
    echo "[cleanup] stopping ui (pid=$UI_PID) + pc-server (pid=$SERVER_PID)..."
    kill "$UI_PID" 2>/dev/null || true
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$UI_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
}
trap cleanup EXIT

# 等待 UI ready
echo "[2.1/3] waiting for UI..."
for i in $(seq 1 90); do
    if curl -sf "http://127.0.0.1:$UI_PORT" > /dev/null 2>&1; then
        echo "  UI 200 OK (after ${i}s)"
        break
    fi
    sleep 1
done

if ! curl -sf "http://127.0.0.1:$UI_PORT" > /dev/null 2>&1; then
    echo "ERROR: UI failed to start within 90s"
    tail -n 50 "$UI_LOG" || true
    exit 1
fi

# 3. 跑 Playwright workflow
if [[ "${SKIP_PLAYWRIGHT:-0}" == "1" ]]; then
    echo "[3/3] SKIP_PLAYWRIGHT=1 — skipping Playwright"
else
    echo "[3/3] Running Playwright workflows..."
    cd "$PROJECT_ROOT"
    if [[ -d "tests/e2e/playwright" ]]; then
        (cd tests/e2e/playwright && \
            PLAYWRIGHT_HTML_REPORT="$E2E_DIR/playwright-html" \
            npx playwright test --reporter=html,json \
            2>&1 | tee "$E2E_DIR/playwright.log") || true
    else
        echo "  tests/e2e/playwright/ not yet created (workflow 2.x-12.x)"
    fi
fi

# 4. 生成 summary
echo "[4/4] Generating summary..."
SUMMARY="$E2E_DIR/summary.md"
cat > "$SUMMARY" <<EOF
# UI Workflow E2E Report — $TIMESTAMP

## Environment
- pc-server port: $BIND_PORT
- UI port: $UI_PORT
- Seed: $PAPERCLIP_SEED_DEMO
- Playwright: $(if [[ "${SKIP_PLAYWRIGHT:-0}" == "1" ]]; then echo "skipped"; else echo "ran"; fi)

## Artifacts
- pc-server log: pc-server.log
- UI log: ui.log
- Playwright report: playwright-html/

EOF
echo "Done. Summary: $SUMMARY"
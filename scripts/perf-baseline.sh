#!/usr/bin/env bash
# scripts/perf-baseline.sh — R590 性能基线快速报告
#
# 单次启动 + 100 次采样 → 输出关键指标：
# - 启动时间（warm binary）
# - /health p50 / p99 延迟
# - RSS 内存
# - 与 Node 上游对比
#
# 配套：
# - scripts/long-run-5min.sh（5 分钟长跑版）
# - scripts/e2e-baseline.sh（冒烟测试版）

set -euo pipefail
export LC_ALL=C; export LANG=C

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PG_BIN="${PG_BIN:-/opt/homebrew/opt/postgresql@16/bin}"
DATA_DIR="${TMPDIR:-/tmp}/pc-perf-pgdata-$$"
PG_PORT="${PAPERCLIP_PERF_PG_PORT:-$(( 55800 + (RANDOM % 100) ))}"
SRV_PORT="${PAPERCLIP_PERF_HTTP_PORT:-$(( 53500 + (RANDOM % 100) ))}"
LOG_DIR="$ROOT/.e2e-logs"
mkdir -p "$LOG_DIR"

cleanup() {
  set +e
  [[ -n "${SRV_PID:-}" ]] && kill -0 "$SRV_PID" 2>/dev/null && kill "$SRV_PID" 2>/dev/null
  [[ -n "${PG_PID:-}" ]] && "$PG_BIN/pg_ctl" -D "$DATA_DIR" -m fast stop >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "[perf] init pg at $DATA_DIR"
"$PG_BIN/initdb" -D "$DATA_DIR" -U postgres --auth-local=trust --auth-host=trust >"$LOG_DIR/initdb-perf.log" 2>&1
"$PG_BIN/pg_ctl" -D "$DATA_DIR" -l "$LOG_DIR/pg-perf.log" \
  -o "-p $PG_PORT -k /tmp -h 127.0.0.1 -c unix_socket_directories=/tmp" start >"$LOG_DIR/pgctl-perf.log" 2>&1
for i in 1 2 3 4 5 6 7 8 9 10; do
  if "$PG_BIN/pg_isready" -h 127.0.0.1 -p "$PG_PORT" -U postgres >/dev/null 2>&1; then break; fi
  sleep 0.5
done

DB_URL="postgres://postgres@127.0.0.1:$PG_PORT/postgres"
PAPERCLIP_DATABASE_URL="$DB_URL" RUST_LOG=warn \
  cargo run --quiet -p pc-migrate -- up >"$LOG_DIR/migrate-perf.log" 2>&1

echo "[perf] pre-build pc-server (R580 pattern)"
cargo build --quiet -p pc-server 2>"$LOG_DIR/server-build-perf.log"
SERVER_BIN="$ROOT/target/debug/paperclip-server"

START_TIME=$(date +%s%N)
PAPERCLIP_DATABASE_URL="$DB_URL" PAPERCLIP_PORT="$SRV_PORT" RUST_LOG=info \
  "$SERVER_BIN" >"$LOG_DIR/server-perf.log" 2>&1 &
SRV_PID=$!

# Poll /health
HEALTH_OK=0
for i in $(seq 1 60); do
  sleep 0.5
  if curl -fsS "http://127.0.0.1:$SRV_PORT/health" >/dev/null 2>&1; then
    HEALTH_OK=1; break
  fi
done
[[ $HEALTH_OK -eq 1 ]] || { echo "[perf] FAIL: /health not 200"; exit 1; }

END_TIME=$(date +%s%N)
BOOT_MS=$(echo "($END_TIME - $START_TIME) / 1000000" | bc)

# 采样 /health 延迟
LATENCIES=()
for i in $(seq 1 100); do
  T=$(curl -o /dev/null -s -w '%{time_total}' "http://127.0.0.1:$SRV_PORT/health" || echo "0")
  LATENCIES+=("$T")
done
SORTED=$(printf "%s\n" "${LATENCIES[@]}" | sort -n)
P50=$(echo "$SORTED" | sed -n '50p')
P99=$(echo "$SORTED" | sed -n '99p')
P50_MS=$(echo "$P50 * 1000" | bc | cut -d. -f1)
P99_MS=$(echo "$P99 * 1000" | bc | cut -d. -f1)

# 业务端点合约检查（list 端点应返回 200）
ENDPOINTS=(
  "/api/agents"
  "/api/companies"
  "/api/issues"
  "/api/decisions"
  "/api/approvals"
  "/api/heartbeats"
)
BIZ_OK=0
BIZ_TOTAL=${#ENDPOINTS[@]}
for ep in "${ENDPOINTS[@]}"; do
  STATUS=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$SRV_PORT${ep}")
  if [[ "$STATUS" == "200" ]]; then
    BIZ_OK=$((BIZ_OK + 1))
  fi
done

# 内存
RSS_KB=$(ps -o rss= -p "$SRV_PID" 2>/dev/null | tr -d ' ' || echo "0")
RSS_MB=$(echo "scale=1; $RSS_KB / 1024" | bc)

# 进程数（粗略）
THREADS=$(ps -M -p "$SRV_PID" 2>/dev/null | wc -l || echo "?")

echo ""
echo "=== Perf Baseline (R592) ==="
echo "  Boot time (warm):     ${BOOT_MS}ms"
echo "  /health p50:         ${P50_MS}ms"
echo "  /health p99:         ${P99_MS}ms"
echo "  RSS (idle):          ${RSS_MB}MB"
echo "  Threads:             ${THREADS}"
echo "  Business endpoints:  ${BIZ_OK}/${BIZ_TOTAL} OK"
echo ""

# 与 Node 上游对比（基线参考值）
echo "=== vs Node 上游（参考） ==="
printf "  %-20s %12s %12s %8s\n" "metric" "Node" "Rust" "提升"
printf "  %-20s %12s %12s %8s\n" "boot (warm)" "3000ms" "${BOOT_MS}ms" "$(echo "scale=1; 3000 / $BOOT_MS" | bc)x"
printf "  %-20s %12s %12s %8s\n" "/health p99" "80ms" "${P99_MS}ms" "$(echo "scale=1; 80 / $P99_MS" | bc)x"
printf "  %-20s %12s %12s %8s\n" "RSS (idle)" "250MB" "${RSS_MB}MB" "$(echo "scale=1; 250 / $RSS_MB" | bc)x"

echo ""
# 计算性能断言
P99_MS=$(echo "$P99 * 1000" | bc | cut -d. -f1)
P99_OK=0
[[ -n "$P99_MS" && $P99_MS -lt 30 ]] && P99_OK=1

MAX_RSS_MB=$(echo "$RSS_MB" | cut -d. -f1)
RSS_OK=0
[[ -n "$MAX_RSS_MB" && $MAX_RSS_MB -lt 100 ]] && RSS_OK=1

HEALTH_OK=1  # final 已经在前面通过

BIZ_PASS=0
[[ $BIZ_OK -eq $BIZ_TOTAL ]] && BIZ_PASS=1

# JSON report (for CI 消费)
REPORT_FILE="${PAPERCLIP_PERF_REPORT:-/tmp/paperclip-perf-report.json}"
cat > "$REPORT_FILE" <<JSON
{
  "version": "1",
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "metrics": {
    "boot_ms": $BOOT_MS,
    "health_p50_ms": $P50_MS,
    "health_p99_ms": $P99_MS,
    "rss_mb": $RSS_MB,
    "threads": $THREADS,
    "business_endpoints_ok": $BIZ_OK,
    "business_endpoints_total": $BIZ_TOTAL
  },
  "asserts": {
    "p99_ok": $P99_OK,
    "rss_ok": $RSS_OK,
    "biz_pass": $BIZ_PASS
  },
  "node_baseline": {
    "boot_ms": 3000,
    "health_p99_ms": 80,
    "rss_mb": 250
  }
}
JSON
echo "[perf] report → $REPORT_FILE"

if [[ $P99_OK -eq 1 && $RSS_OK -eq 1 && $BIZ_PASS -eq 1 ]]; then
  echo ""
  echo "[perf] PASS ✅"
  echo "  - /health p99 ${P99_MS}ms < 30ms target"
  echo "  - max RSS ${MAX_RSS_MB}MB < 100MB target"
  echo "  - business endpoints ${BIZ_OK}/${BIZ_TOTAL} OK"
  exit 0
else
  echo ""
  echo "[perf] FAIL ❌"
  [[ $P99_OK -eq 0 ]] && echo "  - p99 ${P99_MS}ms >= 30ms"
  [[ $RSS_OK -eq 0 ]] && echo "  - RSS ${MAX_RSS_MB}MB >= 100MB"
  [[ $HEALTH_OK -eq 0 ]] && echo "  - final health failed"
  [[ $BIZ_PASS -eq 0 ]] && echo "  - business endpoints only ${BIZ_OK}/${BIZ_TOTAL} OK"
  tail -30 "$LOG_DIR/server-perf.log"
  exit 1
fi

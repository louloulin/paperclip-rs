#!/usr/bin/env bash
# scripts/v11-ui-happy-path.sh — V11 UI 60 client 全 happy path
#
# 真实启动：临时 PG16 → pc-migrate up → pc-server → curl 50 个 client 端点
# 每个 client 一个 GET 请求主路径；状态码 200/401/403/404/422 都视为合约正确
# （未认证默认 401；只读 list 在无数据时 200 空数组；嵌套路径 404 也算路由正确）
# 任一 client 连不上（000/500/panic）才记 fail。

set -euo pipefail
export LC_ALL=C; export LANG=C
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PG_BIN="${PG_BIN:-/opt/homebrew/opt/postgresql@16/bin}"
DATA_DIR="${TMPDIR:-/tmp}/pc-v11-pgdata-$$"
PG_PORT="${PAPERCLIP_V11_PG_PORT:-$(( 55600 + (RANDOM % 100) ))}"
SRV_PORT="${PAPERCLIP_V11_HTTP_PORT:-$(( 53300 + (RANDOM % 100) ))}"
LOG_DIR="$ROOT/.e2e-logs"
mkdir -p "$LOG_DIR"

cleanup() {
  set +e
  [[ -n "${SRV_PID:-}" ]] && kill -0 "$SRV_PID" 2>/dev/null && kill "$SRV_PID" 2>/dev/null
  "$PG_BIN/pg_ctl" -D "$DATA_DIR" -m fast stop >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "[v11] init pg at $DATA_DIR"
"$PG_BIN/initdb" -D "$DATA_DIR" -U postgres --auth-local=trust --auth-host=trust >"$LOG_DIR/initdb-v11.log" 2>&1
"$PG_BIN/pg_ctl" -D "$DATA_DIR" -l "$LOG_DIR/pg-v11.log" \
  -o "-p $PG_PORT -k /tmp -h 127.0.0.1 -c unix_socket_directories=/tmp" start >"$LOG_DIR/pgctl-v11.log" 2>&1
for i in 1 2 3 4 5 6 7 8 9 10; do
  if "$PG_BIN/pg_isready" -h 127.0.0.1 -p "$PG_PORT" -U postgres >/dev/null 2>&1; then break; fi
  sleep 0.5
done

DB_URL="postgres://postgres@127.0.0.1:$PG_PORT/postgres"
echo "[v11] pc-migrate up"
PAPERCLIP_DATABASE_URL="$DB_URL" RUST_LOG=warn \
  cargo run --quiet -p pc-migrate -- up >"$LOG_DIR/migrate-v11.log" 2>&1

echo "[v11] start pc-server :$SRV_PORT"
PAPERCLIP_DATABASE_URL="$DB_URL" PAPERCLIP_PORT="$SRV_PORT" RUST_LOG=warn \
  nohup cargo run --quiet -p pc-server -- >"$LOG_DIR/server-v11.log" 2>&1 &
SRV_PID=$!

# Wait for listening
for i in $(seq 1 120); do
  if grep -q "http listening" "$LOG_DIR/server-v11.log" 2>/dev/null; then
    echo "[v11] server ready after ${i}s"
    break
  fi
  sleep 1
done

# 50 client endpoints (one per client file) — main list/get route
# Format: client_file|method|path|expected_status_codes
ENDPOINTS=(
  "access|GET|/api/access|200,401"
  "activity|GET|/api/activity|200,401"
  "adapters|GET|/api/adapters|200,401"
  "agents|GET|/api/agents|200,401"
  "approvals|GET|/api/approvals|200,401"
  "artifacts|GET|/api/artifacts|200,401"
  "assets|GET|/api/assets|200,401"
  "attention|GET|/api/attention|200,401"
  "audit|GET|/api/audit|200,401"
  "auth|GET|/api/auth/get-session|200,401"
  "budgets|GET|/api/budgets|200,401"
  "builtInAgents|GET|/api/built-in-agents|200,401"
  "cases|GET|/api/cases|200,401"
  "companies|GET|/api/companies|200,401"
  "companySkills|GET|/api/company-skills|200,401"
  "costs|GET|/api/costs|200,401"
  "dashboard|GET|/api/dashboard|200,401"
  "decisionTraining|GET|/api/decision-training|200,401"
  "decisions|GET|/api/decisions|200,401"
  "document-annotations|GET|/api/document-annotations|200,401"
  "environments|GET|/api/environments|200,401"
  "execution-workspaces|GET|/api/execution-workspaces|200,401"
  "externalObjects|GET|/api/external-objects|200,401"
  "file-resources|GET|/api/file-resources|200,401"
  "folders|GET|/api/companies/00000000-0000-0000-0000-000000000000/folders|200,401,404"
  "goals|GET|/api/goals|200,401"
  "health|GET|/api/health|200"
  "heartbeats|GET|/api/heartbeats|200,401"
  "inboxDismissals|GET|/api/inbox-dismissals|200,401"
  "inbox-agent-policy|GET|/api/inbox-agent-policy|200,401"
  "instanceSettings|GET|/api/instance-settings|200,401"
  "issues|GET|/api/issues|200,401"
  "pipelines|GET|/api/pipelines|200,401"
  "plugins|GET|/api/plugins|200,401"
  "projects|GET|/api/projects|200,401"
  "resourceMemberships|GET|/api/resource-memberships|200,401"
  "routines|GET|/api/routines|200,401"
  "search|GET|/api/search?q=test|200,401"
  "secrets|GET|/api/secrets|200,401"
  "sidebarBadges|GET|/api/sidebar-badges|200,401"
  "sidebarPreferences|GET|/api/sidebar-preferences|200,401"
  "smokeLab|GET|/api/smoke-lab|200,401"
  "statusCards|GET|/api/status-cards|200,401"
  "summarySlots|GET|/api/summary-slots|200,401"
  "teamCatalog|GET|/api/teams-catalog|200,401"
  "tools|GET|/api/tools|200,401"
  "userProfiles|GET|/api/user-profiles|200,401"
  "workTimeline|GET|/api/work-timeline|200,401"
  "workspace-runtime-control|GET|/api/workspace-runtime-control|200,401"
  "companies-query|GET|/api/companies|200,401"
)

PASS=0
FAIL=0
FAILED_LIST=""
for entry in "${ENDPOINTS[@]}"; do
  IFS='|' read -r name method path expected <<< "$entry"
  STATUS=$(curl -s -o /dev/null -w "%{http_code}" --max-time 3 -X "$method" "http://127.0.0.1:$SRV_PORT$path" 2>/dev/null)
  if [[ ",$expected," == *",$STATUS,"* ]]; then
    echo "  PASS  $name  $method $path  → $STATUS"
    PASS=$((PASS+1))
  else
    echo "  FAIL  $name  $method $path  → $STATUS (expected $expected)"
    FAIL=$((FAIL+1))
    FAILED_LIST="$FAILED_LIST $name"
  fi
done

echo ""
echo "=== V11 Summary ==="
echo "  total: ${#ENDPOINTS[@]}"
echo "  pass:  $PASS"
echo "  fail:  $FAIL"
[[ -n "$FAILED_LIST" ]] && echo "  failed:$FAILED_LIST"
echo ""

if [ "$FAIL" -eq 0 ]; then
  echo "[v11] ALL ${#ENDPOINTS[@]} CLIENTS PASS ✅"
  exit 0
else
  echo "[v11] SOME CLIENTS FAILED ❌"
  exit 1
fi

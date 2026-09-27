#!/usr/bin/env bash
# mc_golden_local_check.sh —— 本仓自造面（`contracts/golden-local/**`）的对账入口。
#
# 它做三件 `--require-pass` 顶不了的事：
#   1. **钉死 env**：`/api/config` 的 omitempty 键集、`feature_flags` 的取值、
#      `/health/realtime` 的访问门三者**全部**由进程 env 决定 ⇒ 换一台机器、或者在
#      开发者本地带着 `GOOGLE_CLIENT_ID` 跑，报告就会漂。这里把每一个相关变量逐个
#      `env -u` / 显式赋值，让回放与调用者的 env **无关**。
#   2. **`--check` 逐字比对**已提交的两份报告（`default/report.json` + `token/report.json`）。
#   3. 🔴 **额外断言 `mismatch == 0 && unmounted == 0`**：`--require-pass` 只断言
#      `pass >= N`（不足 exit 3），**从不**看 mismatch / unmounted；`--check` 的 exit 0
#      也只说明"与快照一致"——一份"13 条全 mismatch"的快照照样自洽。所以必须自己解析。
#
# 用法：
#   bash scripts/mc_golden_local_check.sh            # 回放 + --check + 断言（默认）
#   bash scripts/mc_golden_local_check.sh --write    # 重新生成两份 report.json（契约真变了才用）
#
# 退出码：0 = 全绿；1 = 断言失败 / 报告漂移；2 = 用法错误或二进制缺失。
set -uo pipefail

MODE="check"
case "${1:---check}" in
  --check) MODE="check" ;;
  --write) MODE="write" ;;
  *) echo "usage: $0 [--check|--write]" >&2; exit 2 ;;
esac

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 2

: "${CARGO_TARGET_DIR:=$REPO_ROOT/target}"
export CARGO_TARGET_DIR
BIN="$CARGO_TARGET_DIR/debug/mc-conformance"
if [[ ! -x "$BIN" ]]; then
  echo "mc-conformance binary not found at $BIN" >&2
  echo "build it first:  cargo build -p mc-conformance" >&2
  exit 2
fi

# 逐个清掉所有会改变 /api/config 键集或 /health/realtime 访问门的 env（上游 config.go 的
# 读取面 + 两条路由各自读的 env）。`env -u` 而不是 `unset`：unset 会影响子 shell 的其它用途。
CLEAN_ENV=(
  env -u ALLOW_SIGNUP -u GOOGLE_CLIENT_ID -u DISABLE_WORKSPACE_CREATION
      -u MULTICA_DAEMON_SERVER_URL -u MULTICA_PUBLIC_URL
      -u MULTICA_APP_URL -u FRONTEND_ORIGIN
      -u POSTHOG_API_KEY -u POSTHOG_HOST
      -u ANALYTICS_DISABLED -u ANALYTICS_ENVIRONMENT -u APP_ENV
      -u MULTICA_CDN_DOMAIN -u MULTICA_VCS_INTEGRATION_ENABLED
      -u MULTICA_FEATURE_FLAGS_FILE
      -u MULTICA_TEST_DATABASE_URL
      -u REALTIME_METRICS_TOKEN
)
# 三个受求值的公开 flag：两个 true、一个显式 false ⇒ 6 个公开 flag 的取值全部被钉住。
# （另外三个是 `COMPAT_PUBLIC_FLAGS`，上游硬编码 true，不查 env。）
FLAG_ENV=(
  FF_PLUGINS_V1=true
  FF_COMPOSIO_MCP_APPS=true
  FF_BILLING_WORKSPACE_SUBSCRIPTIONS=false
)

FAILED=0

# run <golden 根> <report 相对路径> <额外的 env 赋值...>
run() {
  local root="$1" report="$2"; shift 2
  local tmp; tmp="$(mktemp)"
  local out rc
  # `--golden` 必须传**仓库相对路径**：绝对路径会把机器相关的绝对路径烙进已提交快照。
  out="$("${CLEAN_ENV[@]}" "$@" "$BIN" --golden "$root" --no-db --json 2>&1)"; rc=$?
  printf '%s\n' "$out" > "$tmp"

  if [[ $rc -ne 0 ]]; then
    echo "FAIL  $root: mc-conformance exited $rc"
    printf '%s\n' "$out" | tail -20 >&2
    FAILED=1; rm -f "$tmp"; return
  fi

  # ---- 断言 1：mismatch == 0 ∧ unmounted == 0 ∧ unevaluable == 0（逐字打印，供人眼核对）----
  local line
  line="$(python3 - "$tmp" <<'PY'
import json, sys
report = json.load(open(sys.argv[1]))
t = report["totals"]
bad = {k: t[k] for k in ("mismatch", "unmounted", "placeholder", "unevaluable") if t[k]}
# 严重度是**声明的顺序**，不是字符串序（"unmounted" 按字典序 > "pass" > "mismatch"，
# 拿 max() 会把一条 mismatch 报成 worst pass —— 判据自己撒谎比没有判据更糟）。
SEVERITY = ["pass", "mismatch", "unmounted", "placeholder", "unevaluable"]
worst = max((f["outcome"] for f in report["fixtures"]), key=SEVERITY.index, default="pass")
print("fixtures {} pass {} mismatch {} unmounted {} placeholder {} unevaluable {} worst {}".format(
    t["fixtures"], t["pass"], t["mismatch"], t["unmounted"], t["placeholder"],
    t["unevaluable"], worst))
sys.exit(1 if bad else 0)
PY
)"
  if [[ $? -ne 0 ]]; then
    echo "FAIL  $root: $line"
    FAILED=1
  else
    echo "ok    $root: $line"
  fi

  if [[ "$MODE" == "write" ]]; then
    "${CLEAN_ENV[@]}" "$@" "$BIN" --golden "$root" --no-db --write "$report" >/dev/null 2>&1
    echo "wrote $report"
  else
    # ---- 断言 2：与已提交报告逐字比对（漂移 ⇒ exit 1）----
    if [[ -f "$report" ]]; then
      if "${CLEAN_ENV[@]}" "$@" "$BIN" --golden "$root" --no-db --check "$report" 2>&1; then
        echo "ok    $report: report matches"
      else
        echo "FAIL  $report: report drifted (regenerate with: $0 --write)"
        FAILED=1
      fi
    else
      echo "FAIL  $report: missing (regenerate with: $0 --write)"
      FAILED=1
    fi
  fi
  rm -f "$tmp"
}

# 根 1：token 未设 ⇒ /health/realtime 走 loopback 分支（harness 不注入 ConnectInfo ⇒ 404）。
run contracts/golden-local/default contracts/golden-local/default/report.json "${FLAG_ENV[@]}"
# 根 2：token 已设 ⇒ 同一路由改走 bearer 分支（200 / 401）。与根 1 互斥，见 PIN。
run contracts/golden-local/token contracts/golden-local/token/report.json \
  "${FLAG_ENV[@]}" REALTIME_METRICS_TOKEN=golden-local-token

if [[ $FAILED -ne 0 ]]; then
  echo "mc_golden_local_check: FAILED"
  exit 1
fi
echo "mc_golden_local_check: OK (mismatch 0 ∧ unmounted 0 on both golden-local roots)"

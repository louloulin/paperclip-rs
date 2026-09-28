#!/usr/bin/env bash
#
# scripts/stop_condition.sh — M10-8：**停止条件的机器化判定**（`docs/64` §2.6 第 3 项 + §9.9）。
#
# `docs/64` §9.9 把「把 paperclip-rs 做成 multica 的 Rust 版本」这句停止条件写成 12 条 Tier-1 硬向量。
# 本脚本是那 12 条**唯一的判定实现**：逐项打印 `期望 / 实测 / PASS|FAIL|SKIP`，末尾给一份
# 机器可解析的 JSON，并给出退出码。**它不实现任何判据** —— 判据全部委托给既有脚本
# （`route_parity.py` / `slash_alias_audit.py` / `mc-conformance` / `schema_drift.py` /
# `file_size_check.py` / `gates.sh` / GitHub check-runs），本脚本只负责**取数 + 逐条对账**。
# 这样做的理由：`docs/64` §9.8 的纪律是「门集合不许因为新工具而漂移」，判定逻辑一旦复制一份
# 就会有两处真相源 ⇒ 复制即回归。
#
# 退出码（与 `scripts/gates.sh` 的语义**逐字一致**）：
#   0 = Tier-1 全绿（12 条全部 PASS）
#   1 = 有判据不达标（还剩多少 = JSON 的 `failing` / `failing_ids`）
#   2 = **没法开跑**（缺库 / 缺二进制 / 前置文件缺失）—— 注意 2 **不是**「绿」也不是「红」，
#       而「没法判定」；`gates.sh` 对 ⑥/⑧ 缺库报的就是这个 2。
#
# 用法：
#   bash scripts/stop_condition.sh                              # 全跑（需要真 PostgreSQL）
#   bash scripts/stop_condition.sh --db-url 'postgres://…'      # 指定库
#   MULTICA_TEST_DATABASE_URL='postgres://…' bash scripts/stop_condition.sh
#   bash scripts/stop_condition.sh --skip-gates                 # 跳过 T1-10（跑全量门禁很贵）
#   bash scripts/stop_condition.sh --gates-log path/to/gates.log  # 用**已跑过**的门禁日志对账，不重跑
#   bash scripts/stop_condition.sh --sha <commit>               # T1-11 查哪个 commit 的 check-runs
#   bash scripts/stop_condition.sh --json-out stop.json         # 末尾 JSON 同时落盘
#   bash scripts/stop_condition.sh --help
#
# 🔴 本脚本**不许**为了让 exit 变绿去改任何既有文件，也**不许**刷新任何快照
# （`crates/mc-conformance/report.json`、`docs/fixtures/route-parity-baseline.json`、
# `scripts/file_size_baseline.tsv` 的刷新权分别归 M10-9 / M10-0 / M10-9）。
# 「还剩多少」是**观测**，不是**可写**的。

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.." || exit 2
ROOT="$PWD"

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-never}"

# ---- 结果累加器：id <TAB> 判据 <TAB> 期望 <TAB> 实测 <TAB> 判定 <TAB> 细节 -------------
RESULTS=""
ORDER=()
verdict_of() { # id -> PASS|FAIL|SKIP-NO-DB|SKIP-NO-GATE|SKIP-NO-ASSET
    local id="$1"
    printf '%s\n' "$RESULTS" | awk -F'\t' -v id="$id" '$1 == id { print $5; exit }'
}
detail_of() {
    local id="$1"
    printf '%s\n' "$RESULTS" | awk -F'\t' -v id="$id" '$1 == id { print $6; exit }'
}
add() { # id criterion expected measured verdict [detail]
    local id="$1" crit="$2" exp="$3" meas="$4" verdict="$5" detail="${6:-}"
    ORDER+=("$id")
    RESULTS="${RESULTS}${id}"$'\t'"${crit}"$'\t'"${exp}"$'\t'"${meas}"$'\t'"${verdict}"$'\t'"${detail}"$'\n'
    printf '  %-6s %-34s expect=%-26s got=%-42s %s\n' "$id" "$crit" "$exp" "$meas" "$verdict"
    [ -n "$detail" ] && printf '         └─ %s\n' "$detail"
    return 0
}

WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

# ---- 参数 ---------------------------------------------------------------------------
DB_URL="${MULTICA_TEST_DATABASE_URL:-}"
SKIP_GATES=0
GATES_LOG=""
SHA=""
JSON_OUT=""

usage() {
    awk 'NR > 2 { if ($0 !~ /^#/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --db-url)     [ $# -ge 2 ] || { echo "error: --db-url needs a value" >&2; exit 2; }; DB_URL="$2"; shift 2 ;;
        --db-url=*)   DB_URL="${1#*=}"; shift ;;
        --skip-gates) SKIP_GATES=1; shift ;;
        --gates-log)  [ $# -ge 2 ] || { echo "error: --gates-log needs a value" >&2; exit 2; }; GATES_LOG="$2"; shift 2 ;;
        --sha)        [ $# -ge 2 ] || { echo "error: --sha needs a value" >&2; exit 2; }; SHA="$2"; shift 2 ;;
        --json-out)   [ $# -ge 2 ] || { echo "error: --json-out needs a value" >&2; exit 2; }; JSON_OUT="$2"; shift 2 ;;
        -h|--help)    usage; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; exit 2 ;;
    esac
done

[ -n "$SHA" ] || SHA="$(git rev-parse HEAD 2>/dev/null || echo '?')"

GOLDEN_DIR="contracts/golden"
CONFORMANCE_REPORT="crates/mc-conformance/report.json"
CONFORMANCE_BIN="target/debug/mc-conformance"

printf '===========================================================\n'
printf ' stop_condition.sh — Tier-1 硬向量逐条判定\n'
printf '   base/HEAD : %s\n' "$SHA"
printf '   repo      : %s\n' "$ROOT"
printf '   db-url    : %s\n' "${DB_URL:-<unset>}"
printf '===========================================================\n\n'

# =====================================================================================
# T1-1 … T1-3 —— ⑦ 路由台账（route_parity.py --json）
# =====================================================================================
printf -- '-- T1-1/T1-2/T1-3  ⑦ route parity ----------------------------------------\n'
RP_JSON="$WORK/route_parity.json"
RP_RC=0
python3 scripts/route_parity.py --json >"$RP_JSON" 2>"$WORK/route_parity.err" || RP_RC=$?
if [ "$RP_RC" -ne 0 ] || ! python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$RP_JSON" 2>/dev/null; then
    add T1-1a "implemented_real == upstream" "456" "route_parity.py failed (exit $RP_RC)" SKIP-NO-ASSET "see below"
    sed -n '1,20p' "$WORK/route_parity.err" >&2
    add T1-2 "⑦ owners 直方图" "empty" "n/a (no report)" SKIP-NO-ASSET
    add T1-3 "⑦ local_only 登记" "all registered" "n/a (no report)" SKIP-NO-ASSET
else
    # 逐条读数（禁用「还差 N 个」这种合并说法：§194 明确要求把两个 placeholder 桶分开打）
    # 函数自己的 $1 = 取值键（一个 python 下标表达式）；python 的 argv[1] = 报告路径。
    rp_get() {
        local key="$1"
        python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval("d"+sys.argv[2]))' \
            "$RP_JSON" "$key"
    }
    UPSTREAM="$(rp_get "['counts']['upstream']")"
    IMPL_REAL="$(rp_get "['counts']['implemented_real']")"
    IMPL_PH="$(rp_get "['counts']['implemented_placeholder']")"
    KGAP="$(rp_get "['counts']['known_gap']")"
    UNCLAIMED="$(rp_get "['counts']['unclaimed']")"
    REGRESS="$(rp_get "['counts']['regressions']")"
    LOCAL="$(rp_get "['counts']['local']")"
    BASELINE="$(rp_get "['sources']['baseline_routes']")"

    [ "$IMPL_REAL" = "456" ] && add T1-1a "implemented_real == 456" "456" "$IMPL_REAL" PASS \
        "upstream=$UPSTREAM；差额 = 456 - $IMPL_REAL = $((456 - IMPL_REAL)) 条仍占位" \
        || add T1-1a "implemented_real == 456" "456" "$IMPL_REAL" FAIL \
            "差 $((456 - IMPL_REAL)) 条：见 T1-1b 的逐条键名（那 $IMPL_PH 条 implemented_placeholder）"

    PH_KEYS="$(python3 -c "
import json
d = json.load(open('$RP_JSON'))
rows = [r for r in d['implemented'] if r.get('placeholder')]
print('; '.join('%s %s (owner=%s, router_line=%s)' % (r['method'], r['path'], r.get('owner','?'), r.get('router_line','?')) for r in rows) or '(none)')
")"
    [ "$IMPL_PH" = "0" ] && add T1-1b "implemented_placeholder == 0" "0" "$IMPL_PH" PASS \
        || add T1-1b "implemented_placeholder == 0" "0" "$IMPL_PH" FAIL "$PH_KEYS"

    [ "$KGAP" = "0" ] && add T1-1c "known_gap == 0" "0" "$KGAP" PASS \
        || add T1-1c "known_gap == 0" "0" "$KGAP" FAIL "$(rp_get "['owners']")"
    [ "$UNCLAIMED" = "0" ] && add T1-1d "unclaimed == 0" "0" "$UNCLAIMED" PASS \
        || add T1-1d "unclaimed == 0" "0" "$UNCLAIMED" FAIL
    [ "$REGRESS" = "0" ] && add T1-1e "regressions == 0" "0" "$REGRESS" PASS \
        || add T1-1e "regressions == 0" "0" "$REGRESS" FAIL
    if [ "$LOCAL" = "$BASELINE" ]; then
        add T1-1f "local == baseline_routes" "$LOCAL == $BASELINE" "$LOCAL == $BASELINE" PASS
    else
        add T1-1f "local == baseline_routes" "equal" "$LOCAL vs $BASELINE" FAIL \
            "差 $((LOCAL - BASELINE)) 条：local 多注册的键不在 baseline 里（需刷 baseline，归 M10-0/M10-9）"
    fi

    # ---- T1-2 owners 直方图 ----
    OWNERS="$(python3 -c "import json; d=json.load(open('$RP_JSON'))['owners']; print('{}' if not d else d)")"
    if [ "$OWNERS" = "{}" ]; then
        add T1-2 "⑦ owners 直方图" "{} (empty)" "{}" PASS "known_gap=0 ⇒ 无缺口即无 owner 计数"
    else
        add T1-2 "⑦ owners 直方图" "{} (empty)" "$OWNERS" FAIL "非空即仍有归属中的尾账"
    fi

    # ---- T1-3 local_only 登记表 ----
    # 登记表就是下面这张表（M10-8 交付，`docs/65-STOP-CONDITION.md` §3 逐条给来源）。
    # 判据 = 「每一条 local_only 都能在表里查到理由」，方向是**表 ⊆ 实测**之外还要**实测 ⊆ 表**：
    # 多出来的（实测有、表没有）= 未登记 ⇒ FAIL。
    cat >"$WORK/local_only_registry.tsv" <<'REG'
GET	/api/issues/:id/reactions	M3-LOCAL-01	docs/15-M3-PLAN.md:213 逐字「本地自造（local_only，M3 不动）」；上游 reactions 面在 /api/reactions/{targetType}/{targetId}
GET	/api/issues/:id/quick-actions	M3PLUS-LOCAL-02	docs/15-M3-PLAN.md:587 逐字「与上游 GET /api/quick-actions/ 不是同一条」；本条是 crates/mc-http/tests/issues/auth.rs:141 的**耐久 501 断言**落点 ⇒ 禁删禁实现
GET	/api/health	OPS-LOCAL-03	docs/64 §9.3 裁定「保留（不收敛）」：服务+DB 综合语义，被 apps/mc-cli/src/main.rs 当探针 + mc-openapi 文档测试断言
GET	/api/health/db	OPS-LOCAL-04	docs/64 §9.3 裁定「保留（不收敛）」：DB 专用探针，CLI 与 conformance 自造 fixture 均引用
GET	/api/openapi.json	OPS-LOCAL-05	docs/22-ROUTE-PARITY.md §3.4「本仓自有的运维面」；mc-openapi 生成的文档端点
GET	/api/me/pats	PATS-LOCAL-06	docs/17-M1-CONTRACT-GAPS.md §D4：主路径已迁 /api/tokens，本键保留一个发布周期作为 deprecated alias（响应带 Deprecation 头）
POST	/api/me/pats	PATS-LOCAL-07	同上 §D4（alias 的 POST 面）
DELETE	/api/me/pats/:id	PATS-LOCAL-08	同上 §D4（alias 的 DELETE 面）
REG
    LO_REPORT="$(python3 - "$RP_JSON" "$WORK/local_only_registry.tsv" <<'PY'
import json, sys
rp = json.load(open(sys.argv[1]))
reg = {}
with open(sys.argv[2], encoding='utf-8') as fh:
    for line in fh:
        line = line.rstrip('\n')
        if not line.strip():
            continue
        method, path, rid, reason = line.split('\t')
        reg[(method, path)] = (rid, reason)
seen, rows = set(), []
for r in rp['local_only']:
    key = (r['method'], r['path'])
    seen.add(key)
    if key in reg:
        ph = ' [placeholder]' if r.get('placeholder') else ''
        rows.append((r['method'], r['path'], 'REGISTERED', reg[key][0] + ph, reg[key][1]))
    else:
        rows.append((r['method'], r['path'], 'UNREGISTERED', '-',
                     '本地有、上游无，但没有登记理由 ⇒ T1-3 不达标'))
for key, (rid, reason) in reg.items():
    if key not in seen:
        rows.append((key[0], key[1], 'STALE', rid, '登记表里这一条实测已不存在 ⇒ 请删表行'))
for m, p, st, rid, why in rows:
    print('\t'.join([m, p, st, rid, why]))
PY
)"
    LO_PH="$(printf '%s\n' "$LO_REPORT" | grep -c '\[placeholder\]' || true)"
    LO_N="$(printf '%s\n' "$LO_REPORT" | grep -c . || true)"
    LO_BAD="$(printf '%s\n' "$LO_REPORT" | awk -F'\t' '$3 != "REGISTERED"' | grep -c . || true)"
    if [ "$LO_BAD" -eq 0 ]; then
        add T1-3 "⑦ local_only 逐条登记" "every member registered" \
            "$LO_N/$LO_N registered" PASS "登记表 ${LO_N} 条 ↔ 实测 $LO_N 条（占位 ${LO_PH} 条单列）"
    else
        add T1-3 "⑦ local_only 逐条登记" "every member registered" \
            "$LO_BAD unregistered/stale of $LO_N" FAIL \
            "$(printf '%s\n' "$LO_REPORT" | awk -F'\t' '$3 != "REGISTERED"' | tr '\n' ';')"
    fi
    printf '%s\n' "$LO_REPORT" | awk -F'\t' '{ printf "         %-6s %-34s %-12s %s\n", $1, $2, $3, $4 }'
fi

# =====================================================================================
# T1-4 —— ⑦b 形态（slash_alias_audit.py）
# =====================================================================================
printf -- '-- T1-4          ⑦b slash-alias 形态 --------------------------------------\n'
SA_JSON="$WORK/slash_alias.json"
SA_RC=0
python3 scripts/slash_alias_audit.py --json >"$SA_JSON" 2>"$WORK/slash.err" || SA_RC=$?
if [ "$SA_RC" -eq 0 ] && python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$SA_JSON" 2>/dev/null; then
    SA_READ="$(python3 -c "
import json
d = json.load(open('$SA_JSON'))
f = d.get('findings') or []
defects = [x for x in f if not x.get('allowlisted')]
print('%d finding(s), %d defect(s), registered=%d, stale_allowlist=%d' % (
    len(f), len(defects), d.get('registered_keys', 0), len(d.get('stale_allowlist') or [])))
")"
    SA_DEFECTS="$(python3 -c "
import json
d = json.load(open('$SA_JSON'))
print(len([x for x in (d.get('findings') or []) if not x.get('allowlisted')]))
")"
    SA_STALE="$(python3 -c "
import json
d = json.load(open('$SA_JSON'))
print(len(d.get('stale_allowlist') or []))
")"
    if [ "$SA_DEFECTS" = "0" ] && [ "$SA_STALE" = "0" ]; then
        add T1-4 "⑦b slash_alias_audit" "exit 0 ∧ defect 0" "$SA_READ" PASS
    else
        add T1-4 "⑦b slash_alias_audit" "exit 0 ∧ defect 0" "$SA_READ" FAIL \
            "defect=$SA_DEFECTS stale_allowlist=$SA_STALE"
    fi
else
    add T1-4 "⑦b slash_alias_audit" "exit 0 ∧ defect 0" "exit $SA_RC (no JSON)" SKIP-NO-ASSET "$(head -3 "$WORK/slash.err" | tr '\n' ' ')"
fi

# =====================================================================================
# T1-5 / T1-7 —— ⑨ 离线层（mc-conformance --no-db）
# =====================================================================================
printf -- '-- T1-5/T1-7     ⑨ conformance (--no-db) -----------------------------------\n'
HAVE_CONFORMANCE=0
NO_DB_JSON=""
if [ -x "$CONFORMANCE_BIN" ]; then
    HAVE_CONFORMANCE=1
else
    if cargo build -q -p mc-conformance >"$WORK/build.log" 2>&1 && [ -x "$CONFORMANCE_BIN" ]; then
        HAVE_CONFORMANCE=1
    fi
fi
if [ "$HAVE_CONFORMANCE" -eq 0 ]; then
    add T1-5 "⑨ --no-db mismatch/unmounted" "mismatch 0 ∧ unmounted 0" "no $CONFORMANCE_BIN" SKIP-NO-ASSET \
        "先跑 cargo build -p mc-conformance"
    add T1-7 "⑨ 两个 rate" "1.0 ∧ 1.0" "no report" SKIP-NO-ASSET
else
    NO_DB_RC=0
    env -u MULTICA_TEST_DATABASE_URL -u MULTICA_DATABASE_URL \
        "$CONFORMANCE_BIN" --golden "$GOLDEN_DIR" --no-db --json >"$WORK/nodb.json" 2>"$WORK/nodb.err" || NO_DB_RC=$?
    CHECK_RC="skip"
    if [ -f "$CONFORMANCE_REPORT" ]; then
        env -u MULTICA_TEST_DATABASE_URL -u MULTICA_DATABASE_URL \
            "$CONFORMANCE_BIN" --golden "$GOLDEN_DIR" --no-db --check "$CONFORMANCE_REPORT" >/dev/null 2>&1
        CHECK_RC=$?
    fi
    if [ "$NO_DB_RC" -eq 0 ] && python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$WORK/nodb.json" 2>/dev/null; then
        NO_DB_JSON="$WORK/nodb.json"
        eval "$(python3 -c "
import json
t = json.load(open('$WORK/nodb.json'))['totals']
print('T1_5_MEAS=%r' % ('fixtures %d pass %d mismatch %d unmounted %d placeholder %d unevaluable %d' % (
    t['fixtures'], t['pass'], t['mismatch'], t['unmounted'], t['placeholder'], t['unevaluable'])))
print('T1_5_BAD=%d' % (t['mismatch'] + t['unmounted']))
")"
        if [ "$T1_5_BAD" = "0" ] && [ "$CHECK_RC" = "0" ]; then
            add T1-5 "⑨ --no-db mismatch/unmounted" "mismatch 0 ∧ unmounted 0" "$T1_5_MEAS" PASS \
                "--check $CONFORMANCE_REPORT exit 0（与已提交快照逐字一致）"
        elif [ "$CHECK_RC" = "skip" ]; then
            add T1-5 "⑨ --no-db mismatch/unmounted" "mismatch 0 ∧ unmounted 0" "$T1_5_MEAS" FAIL \
                "mismatch+unmounted=$T1_5_BAD；且 $CONFORMANCE_REPORT 不存在（--check 未跑）"
        elif [ "$CHECK_RC" != "0" ]; then
            add T1-5 "⑨ --no-db mismatch/unmounted" "mismatch 0 ∧ unmounted 0" "$T1_5_MEAS" FAIL \
                "mismatch+unmounted=$T1_5_BAD；且 --check $CONFORMANCE_REPORT exit=$CHECK_RC（快照漂移；刷新权归 M10-9）"
        else
            add T1-5 "⑨ --no-db mismatch/unmounted" "mismatch 0 ∧ unmounted 0" "$T1_5_MEAS" FAIL \
                "mismatch+unmounted=$T1_5_BAD；--check 与快照一致 ⇒ 快照本身记着差额（刷新权归 M10-9）"
        fi
        eval "$(python3 -c "
import json
r = json.load(open('$NO_DB_JSON'))
m = r.get('mounted_equivalence_rate')
print('R_CONTRACT=%r' % ('%.6f' % r['contract_equivalence_rate']))
print('R_MOUNTED=%r' % ('none' if m is None else '%.6f' % m))
")"
        if [ "$R_CONTRACT" = "1.000000" ] && [ "$R_MOUNTED" = "1.000000" ]; then
            add T1-7 "⑨ 两个 rate" "1.0 ∧ 1.0" "contract $R_CONTRACT ∧ mounted $R_MOUNTED" PASS
        else
            add T1-7 "⑨ 两个 rate" "1.0 ∧ 1.0" "contract $R_CONTRACT ∧ mounted $R_MOUNTED" FAIL
        fi
    else
        add T1-5 "⑨ --no-db mismatch/unmounted" "mismatch 0 ∧ unmounted 0" "mc-conformance exit $NO_DB_RC" SKIP-NO-ASSET \
            "$(head -3 "$WORK/nodb.err" | tr '\n' ' ')"
        add T1-7 "⑨ 两个 rate" "1.0 ∧ 1.0" "no report" SKIP-NO-ASSET
    fi
fi

# =====================================================================================
# T1-6 —— ⑨ 真库层（mc-conformance --db-url）
# =====================================================================================
printf -- '-- T1-6          ⑨ conformance (--db-url) ----------------------------------\n'
DB_PING="fail"
if [ -n "$DB_URL" ]; then
    DB_PING="$(python3 - "$DB_URL" <<'PY'
import socket, sys, urllib.parse
u = urllib.parse.urlparse(sys.argv[1])
try:
    with socket.create_connection((u.hostname or '127.0.0.1', u.port or 5432), timeout=5):
        print('ok')
except OSError as exc:
    print('fail:%s' % exc)
PY
)"
fi
if [ -z "$DB_URL" ]; then
    add T1-6 "⑨ --db-url unevaluable/mismatch" "unevaluable 0 ∧ mismatch 0" "<no --db-url>" SKIP-NO-DB \
        "缺库 ⇒ 本条无法判定 ⇒ 整体 exit 2（沿用 gates.sh 对 ⑥/⑧ 的 exit 2 语义）"
elif [ "$DB_PING" != "ok" ]; then
    add T1-6 "⑨ --db-url unevaluable/mismatch" "unevaluable 0 ∧ mismatch 0" "db unreachable ($DB_PING)" SKIP-NO-DB
elif [ "$HAVE_CONFORMANCE" -eq 0 ]; then
    add T1-6 "⑨ --db-url unevaluable/mismatch" "unevaluable 0 ∧ mismatch 0" "no $CONFORMANCE_BIN" SKIP-NO-ASSET
else
    DB_RC=0
    "$CONFORMANCE_BIN" --golden "$GOLDEN_DIR" --db-url "$DB_URL" --json >"$WORK/db.json" 2>"$WORK/db.err" || DB_RC=$?
    if [ "$DB_RC" -eq 0 ] && python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$WORK/db.json" 2>/dev/null; then
        eval "$(python3 -c "
import json
t = json.load(open('$WORK/db.json'))['totals']
print('T1_6_MEAS=%r' % ('fixtures %d pass %d mismatch %d unmounted %d placeholder %d unevaluable %d' % (
    t['fixtures'], t['pass'], t['mismatch'], t['unmounted'], t['placeholder'], t['unevaluable'])))
print('T1_6_BAD=%d' % (t['unevaluable'] + t['mismatch']))
")"
        if [ "$T1_6_BAD" = "0" ]; then
            add T1-6 "⑨ --db-url unevaluable/mismatch" "unevaluable 0 ∧ mismatch 0" "$T1_6_MEAS" PASS
        else
            add T1-6 "⑨ --db-url unevaluable/mismatch" "unevaluable 0 ∧ mismatch 0" "$T1_6_MEAS" FAIL \
                "unevaluable+mismatch=$T1_6_BAD；未挂载/不可判定的 fixture 需要真实现或真 actor；report.json 的刷新权归 M10-9（LUM-2111）"
        fi
    else
        add T1-6 "⑨ --db-url unevaluable/mismatch" "unevaluable 0 ∧ mismatch 0" "mc-conformance exit $DB_RC" FAIL \
            "$(head -3 "$WORK/db.err" | tr '\n' ' ')"
    fi
fi

# =====================================================================================
# T1-8 —— ⑧ schema drift（需库）
# =====================================================================================
printf -- '-- T1-8          ⑧ schema drift ------------------------------------------\n'
if [ -z "$DB_URL" ]; then
    add T1-8 "⑧ schema_drift missing == 0" "missing 0 (exit 0)" "<no --db-url>" SKIP-NO-DB \
        "⑧ 自带 scratch 库但需要一个存在的库 URL ⇒ 缺库即无法判定"
elif [ "$DB_PING" != "ok" ]; then
    add T1-8 "⑧ schema_drift missing == 0" "missing 0 (exit 0)" "db unreachable ($DB_PING)" SKIP-NO-DB
else
    SD_JSON="$WORK/schema_drift.json"
    SD_RC=0
    MULTICA_TEST_DATABASE_URL="$DB_URL" python3 scripts/schema_drift.py --json >"$SD_JSON" 2>"$WORK/sd.err" || SD_RC=$?
    if python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$SD_JSON" 2>/dev/null; then
        SD_MISSING="$(python3 -c "import json; print(json.load(open('$SD_JSON')).get('counts',{}).get('missing',0))")"
        SD_OK="$(python3 -c "import json; print(json.load(open('$SD_JSON')).get('ok'))")"
        SD_ALL="$(python3 -c "
import json
c = json.load(open('$SD_JSON')).get('counts', {})
print(', '.join('%s=%s' % kv for kv in sorted(c.items())) or 'no differences')
")"
        if [ "$SD_MISSING" = "0" ] && [ "$SD_OK" = "True" ] && [ "$SD_RC" -eq 0 ]; then
            add T1-8 "⑧ schema_drift missing == 0" "missing 0 (exit 0)" "$SD_ALL (ok=True)" PASS
        else
            add T1-8 "⑧ schema_drift missing == 0" "missing 0 (exit 0)" "$SD_ALL (ok=$SD_OK exit=$SD_RC)" FAIL \
                "未登记的 schema 漂移（未登记项归各波次；登记表 = contracts/upstream-schema-deviations.tsv）"
        fi
    else
        add T1-8 "⑧ schema_drift missing == 0" "missing 0 (exit 0)" "exit $SD_RC (no JSON)" FAIL \
            "$(head -3 "$WORK/sd.err" | tr '\n' ' ')"
    fi
fi

# =====================================================================================
# T1-9 —— ⑩ file-size
# =====================================================================================
printf -- '-- T1-9          ⑩ file size ---------------------------------------------\n'
FS_OUT="$WORK/filesize.txt"
FS_RC=0
python3 scripts/file_size_check.py >"$FS_OUT" 2>&1 || FS_RC=$?
FS_HEAD="$(head -1 "$FS_OUT")"
FS_VIOL="$(printf '%s\n' "$FS_HEAD" | sed -n 's/.*violations=\([0-9]*\).*/\1/p')"
FS_VIOL="${FS_VIOL:-?}"
if [ "$FS_RC" -eq 0 ] && [ "$FS_VIOL" = "0" ]; then
    add T1-9 "⑩ file_size_check" "violations 0" "$FS_HEAD" PASS
else
    add T1-9 "⑩ file_size_check" "violations 0" "$FS_HEAD (exit $FS_RC)" FAIL \
        "$(sed -n '/^  /p' "$FS_OUT" | head -8 | tr '\n' ';')"
fi
# 本脚本自身也在门 ⑩ 的扫描范围内（scripts/**/*.sh ⇒ 800 行硬上限）
SELF_LINES="$(wc -l <"${BASH_SOURCE[0]}" | tr -d ' ')"
printf '         (本脚本 %s 行；门 ⑩ 硬上限 800)\n' "$SELF_LINES"

# =====================================================================================
# T1-10 —— 门禁
# =====================================================================================
printf -- '-- T1-10         门禁 ----------------------------------------------------\n'
GATE_LIST="$(bash scripts/gates.sh --list 2>/dev/null)"
GATE_COUNT="$(printf '%s\n' "$GATE_LIST" | grep -c . || true)"
if [ -n "$GATES_LOG" ] || [ "$SKIP_GATES" -eq 0 ]; then
    if [ -n "$GATES_LOG" ]; then
        if [ -f "$GATES_LOG" ]; then
            printf '  用已跑过的门禁日志对账：%s（不重跑）\n' "$GATES_LOG"
        else
            GATES_LOG=""
            SKIP_GATES=1
        fi
    else
        GATES_LOG="$WORK/gates.log"
        printf '  跑全量门禁（bash scripts/gates.sh --with-db）…… 这一段很贵\n'
        if [ -n "$DB_URL" ]; then
            bash scripts/gates.sh --with-db --db-url "$DB_URL" >"$GATES_LOG" 2>&1
        else
            bash scripts/gates.sh >"$GATES_LOG" 2>&1
        fi
        printf '  门禁日志：%s\n' "$GATES_LOG"
    fi
fi
if [ -n "$GATES_LOG" ] && [ -f "$GATES_LOG" ]; then
    GATE_BAD="$(grep -o 'GATE_[A-Z_]*_EXIT=[0-9]*' "$GATES_LOG" | awk -F= '$2 != 0' | sort -u | tr '\n' ' ')"
    GATE_SEEN="$(grep -c -o 'GATE_[A-Z_]*_EXIT=[0-9]*' "$GATES_LOG" || true)"
    if [ -n "$GATE_BAD" ]; then
        add T1-10 "门禁 gates.sh 10/10" "10/10 exit 0" "$GATE_SEEN exit line(s); red: $GATE_BAD" FAIL \
            "逐门复算：bash scripts/gates.sh --only <name>（日志 $GATES_LOG）"
    elif [ "$GATE_SEEN" -lt "$GATE_COUNT" ]; then
        add T1-10 "门禁 gates.sh 10/10" "10/10 exit 0" "only $GATE_SEEN of $GATE_COUNT gates in log" FAIL \
            "日志里缺的门没跑到（用 --with-db 才含 db/schema-drift）"
    else
        add T1-10 "门禁 gates.sh 10/10" "$GATE_COUNT/$GATE_COUNT exit 0" "$GATE_SEEN exit line(s), all 0" PASS
    fi
elif [ "$SKIP_GATES" -eq 1 ]; then
    add T1-10 "门禁 gates.sh 10/10" "10/10 exit 0" "<skipped by --skip-gates>" SKIP-NO-ASSET \
        "判据真实存在（本仓 10 道门），但本轮没跑 ⇒ 无法判定；去掉 --skip-gates 或给 --gates-log"
else
    add T1-10 "门禁 gates.sh 10/10" "10/10 exit 0" "no log" SKIP-NO-ASSET
fi
if printf '%s\n' "$GATE_LIST" | grep -qx 'image'; then
    IMAGE_GATE_RC=2
    if [ -n "$GATES_LOG" ] && [ -f "$GATES_LOG" ]; then
        bash scripts/gates.sh --only image >"$WORK/gate_image.log" 2>&1
        IMAGE_GATE_RC=$?
    fi
    add T1-10b "门禁 gates.sh --only image" "exit 0" "exit $IMAGE_GATE_RC" \
        "$([ "$IMAGE_GATE_RC" -eq 0 ] && echo PASS || echo FAIL)"
else
    add T1-10b "门禁 gates.sh --only image" "exit 0" "gate 'image' does not exist" SKIP-NO-GATE \
        "scripts/gates.sh 的 ALL_GATES 恰为 $GATE_COUNT 个（$(printf '%s' "$GATE_LIST" | tr '\n' ' ')）⇒ T1-10 的后半分句指向**不存在的门**；建门归 M10-7（LUM-2109，docker 面），本片按缺报，不改 gates.sh（写集审计第 ② 条）"
fi

# =====================================================================================
# T1-11 —— CI check-runs
# =====================================================================================
printf -- '-- T1-11         CI check-runs -------------------------------------------\n'
TOKEN="$(git remote get-url origin 2>/dev/null | sed -nE 's#https://x-access-token:([^@]+)@.*#\1#p')"
CR_JSON="$WORK/checkruns.json"
CR_RC=0
if [ -z "$TOKEN" ]; then
    CR_RC=2
elif ! command -v curl >/dev/null 2>&1; then
    CR_RC=2
else
    curl -fsS -H "Authorization: Bearer $TOKEN" \
        "https://api.github.com/repos/louloulin/paperclip-rs/commits/$SHA/check-runs" \
        -o "$CR_JSON" 2>"$WORK/cr.err" || CR_RC=$?
fi
if [ "$CR_RC" -ne 0 ] || ! python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$CR_JSON" 2>/dev/null; then
    add T1-11 "CI 4 job 全绿" "fast/db/contract/image" "cannot read check-runs for $SHA" SKIP-NO-ASSET \
        "需要能访问 GitHub API 的凭据（本机从 origin remote 取 x-access-token）"
else
    # CI 的 check-run name 是 job 的显示名（「fast — fmt / build / clippy / test / file-size」），
    # 所以比对取空格/破折号前的**短名**，否则 3 个绿 job 会被误判成「job 全都不存在」。
    CR_MEAS="$(python3 -c "
import json
d = json.load(open('$CR_JSON'))
print('; '.join('%s=%s' % (r['name'], r.get('conclusion') or r['status']) for r in d.get('check_runs', [])) or 'none')
")"
    CR_BAD="$(python3 -c "
import json
d = json.load(open('$CR_JSON'))
def short(n):
    return n.split(' — ')[0].strip()
print('; '.join(r['name'] for r in d.get('check_runs', []) if r.get('conclusion') != 'success') or '')
")"
    CR_MISSING="$(python3 - "$CR_JSON" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
have = {r['name'].split(' — ')[0].strip() for r in d.get('check_runs', [])}
print(', '.join(j for j in ('fast', 'db', 'contract', 'image') if j not in have) or '')
PY
)"
    if [ -n "$CR_BAD" ]; then
        add T1-11 "CI 4 job 全绿" "fast/db/contract/image success" "$CR_MEAS" FAIL "red job: $CR_BAD"
    elif [ -n "$CR_MISSING" ]; then
        add T1-11 "CI 4 job 全绿" "fast/db/contract/image success" "$CR_MEAS" FAIL \
            "**不存在的 job**：$CR_MISSING（.github/workflows/ci.yml 逐字只有 3 个 job；image job 归 M10-7 / LUM-2109）"
    else
        add T1-11 "CI 4 job 全绿" "fast/db/contract/image success" "$CR_MEAS" PASS
    fi
fi

# =====================================================================================
# T1-12 —— golden-local 对账
# =====================================================================================
printf -- '-- T1-12         golden-local 对账 ---------------------------------------\n'
if [ "$HAVE_CONFORMANCE" -eq 0 ]; then
    add T1-12 "--golden contracts/golden-local" "mismatch 0 ∧ unmounted 0" "no $CONFORMANCE_BIN" SKIP-NO-ASSET
else
    GL_OUT="$WORK/golden_local.txt"
    GL_RC=0
    bash scripts/mc_golden_local_check.sh >"$GL_OUT" 2>&1 || GL_RC=$?
    GL_MEAS="$(python3 - "$GL_OUT" <<'PY'
import re, sys
rows = []
for line in open(sys.argv[1], encoding='utf-8'):
    m = re.match(r'\s*(ok|FAIL)\s+(\S+): fixtures (\d+) pass (\d+) mismatch (\d+) unmounted (\d+) placeholder (\d+) unevaluable (\d+)', line)
    if m:
        rows.append('%s fixtures=%s pass=%s mismatch=%s unmounted=%s placeholder=%s unevaluable=%s' %
                    (m.group(2), m.group(3), m.group(4), m.group(5), m.group(6), m.group(7), m.group(8)))
print(' | '.join(rows) or 'no golden-local rows parsed')
PY
)"
    GL_BAD="$(python3 - "$GL_OUT" <<'PY'
import re, sys
bad = 0
for line in open(sys.argv[1], encoding='utf-8'):
    m = re.match(r'\s*(ok|FAIL)\s+(\S+): fixtures (\d+) pass (\d+) mismatch (\d+) unmounted (\d+)', line)
    if m:
        bad += int(m.group(5)) + int(m.group(6))
print(bad)
PY
)"
    if [ "$GL_RC" -eq 0 ] && [ "$GL_BAD" = "0" ]; then
        add T1-12 "--golden contracts/golden-local" "mismatch 0 ∧ unmounted 0" "$GL_MEAS" PASS
    else
        add T1-12 "--golden contracts/golden-local" "mismatch 0 ∧ unmounted 0" "$GL_MEAS (exit $GL_RC)" FAIL
    fi
fi

# =====================================================================================
# 汇总 + 机器可解析 JSON
# =====================================================================================
printf '\n==================== STOP CONDITION SUMMARY ====================\n'
PASS_N=0; FAIL_N=0; NODB_N=0; NOASSET_N=0; NOGATE_N=0
FAIL_IDS=""; NODB_IDS=""
for id in "${ORDER[@]}"; do
    v="$(verdict_of "$id")"
    case "$v" in
        PASS) PASS_N=$((PASS_N + 1)) ;;
        FAIL) FAIL_N=$((FAIL_N + 1)); FAIL_IDS="${FAIL_IDS}${id} " ;;
        SKIP-NO-DB) NODB_N=$((NODB_N + 1)); NODB_IDS="${NODB_IDS}${id} " ;;
        SKIP-NO-ASSET) NOASSET_N=$((NOASSET_N + 1)) ;;
        SKIP-NO-GATE) NOGATE_N=$((NOGATE_N + 1)) ;;
    esac
    printf '  %-6s %-8s %s\n' "$id" "$v" "$(awk -F'\t' -v id="$id" '$1==id {print $2; exit}' <<<"$RESULTS")"
done
printf '  ---\n  pass=%s fail=%s skip-no-db=%s skip-no-asset=%s skip-no-gate=%s total=%s\n' \
    "$PASS_N" "$FAIL_N" "$NODB_N" "$NOASSET_N" "$NOGATE_N" "${#ORDER[@]}"

EXIT=0
if [ "$NODB_N" -gt 0 ]; then
    EXIT=2
elif [ "$FAIL_N" -gt 0 ] || [ "$NOASSET_N" -gt 0 ] || [ "$NOGATE_N" -gt 0 ]; then
    EXIT=1
fi
printf '  exit=%s\n' "$EXIT"
printf '  failing_ids: %s\n' "${FAIL_IDS:-none}"
printf '  undecidable_ids (exit 2): %s\n' "${NODB_IDS:-none}"
printf '===================================================================\n'

RESULTS="$RESULTS" ORDER_CSV="$(printf '%s,' "${ORDER[@]}")" EXIT_CODE="$EXIT" SHA="$SHA" \
python3 - <<'PY' >"$WORK/stop_condition.json"
import json, os, sys
rows = []
order = [o for o in os.environ['ORDER_CSV'].split(',') if o]
raw = os.environ['RESULTS']
data = {}
for line in raw.splitlines():
    if not line.strip():
        continue
    parts = line.split('\t')
    parts += [''] * (6 - len(parts))
    data[parts[0]] = {
        'criterion': parts[1], 'expected': parts[2], 'observed': parts[3],
        'verdict': parts[4], 'detail': parts[5],
    }
for oid in order:
    d = data.get(oid, {})
    rows.append({'id': oid, **d})
payload = {
    'schema': 'stop-condition/1',
    'sha': os.environ['SHA'],
    'exit_code': int(os.environ['EXIT_CODE']),
    'total': len(rows),
    'pass': sum(1 for r in rows if r['verdict'] == 'PASS'),
    'fail': sum(1 for r in rows if r['verdict'] == 'FAIL'),
    'skip_no_db': sum(1 for r in rows if r['verdict'] == 'SKIP-NO-DB'),
    'skip_no_asset': sum(1 for r in rows if r['verdict'] == 'SKIP-NO-ASSET'),
    'skip_no_gate': sum(1 for r in rows if r['verdict'] == 'SKIP-NO-GATE'),
    'failing_ids': [r['id'] for r in rows if r['verdict'] == 'FAIL'],
    'undecidable_ids': [r['id'] for r in rows if r['verdict'] == 'SKIP-NO-DB'],
    'checks': rows,
}
print(json.dumps(payload, ensure_ascii=False, indent=1))
PY

if [ -n "$JSON_OUT" ]; then
    cp "$WORK/stop_condition.json" "$JSON_OUT" 2>/dev/null || true
    printf '  json: %s\n' "$JSON_OUT"
fi
printf '  json (stdout above): stop_condition.json —— 机器可解析，逐项含 expect/got/verdict\n'

exit "$EXIT"

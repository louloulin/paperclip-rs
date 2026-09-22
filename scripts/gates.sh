#!/usr/bin/env bash
#
# scripts/gates.sh — 本仓「每切片必须全绿」门禁的一键执行（plan1 §6.4）。
#
# 这是门禁命令的**唯一实现**：CI（`.github/workflows/ci.yml`）不重写命令，只调用本脚本
# （`scripts/gates.sh --only <gate>`），因此本地与 CI 跑的是逐字同一批命令，不存在两处漂移。
#
# 十道门（编号与 docs/plan1.md §5 W0 / §6.4、docs/24-W0-CI.md 的表格一一对应）：
#
#   ① fmt              cargo fmt --all --check
#   ② build            cargo build --workspace --all-targets --locked
#   ③ clippy           cargo clippy --workspace --all-targets -- -D warnings
#   ④ clippy-test-util cargo clippy -p mc-http --all-targets --features mc-http/test-util -- -D warnings
#   ⑤ test             cargo test --workspace                      （**不带** MULTICA_TEST_DATABASE_URL）
#   ⑥ db               mc-migrate run --dir migrations + cargo test -p mc-repos -p mc-http --features mc-http/test-util -- --ignored
#   ⑦ route-parity     python3 scripts/route_parity.py --quiet
#   ⑧ schema-drift     python3 scripts/schema_drift.py --quiet      （**需要** MULTICA_TEST_DATABASE_URL）
#   ⑨ conformance      cargo run -q -p mc-conformance -- --no-db --check crates/mc-conformance/report.json
#   ⑩ file-size        python3 scripts/file_size_check.py --quiet    （R7 单文件 800 行硬上限）
#
# 默认跑 ①–⑤ + ⑦ + ⑨ + ⑩（不需要数据库）；`--with-db` 追加 ⑥ 与 ⑧（两者都需要真 PostgreSQL）。
# 每道门打印一行 `GATE_<NAME>_EXIT=<code>`，末尾打印汇总表；任一非 0 → 本脚本 exit 1。
#
# 用法：
#   bash scripts/gates.sh                          # ①–⑤ + ⑦ + ⑨
#   bash scripts/gates.sh --with-db                # ①–⑨（库 URL 见下）
#   bash scripts/gates.sh --with-db --db-url 'postgres://user:pw@127.0.0.1:5432/multica_test'
#   MULTICA_TEST_DATABASE_URL='postgres://…' bash scripts/gates.sh --with-db
#   bash scripts/gates.sh --only fmt,build         # 只跑选中的门（CI 用这个）
#   bash scripts/gates.sh --list                   # 列出闸门名
#
# 退出码：0 = 所有被选中的门全绿；1 = 至少一道门非 0；2 = 用法/前置条件错误
# （例如选了 ⑥/⑧ 却没给库 URL）。注意 2 不是「门失败」，而是「根本没法开跑」。
#
# 已知坑（本仓实测，详见 docs/24-W0-CI.md §例外 与 docs/30-W0-DRIFT-GATE.md）：
#   * ⑤ 绝不能带 `MULTICA_TEST_DATABASE_URL`：`crates/mc-http` 的 `smoke` 集成测试
#     （target 指向根 `tests/smoke.rs`）拿到库就会对**同一个库重跑迁移** →
#     `relation "user" already exists`。本脚本在 ⑤ 上用 `env -u` 显式剥掉该变量，
#     所以即使调用者已经 export 过它，⑤ 也是安全的。
#   * ③ 与 ④ 不可合并：只有 ④ 会检查 `crates/mc-http/tests/*` 的 DB e2e 代码。
#   * ⑥ 必须先建表：`mc-repos` / `mc-http` 的 DB 测试直接 INSERT，**自己不做迁移**。
#   * ⑧ 与 ⑥ 的语义分工：⑥ 回答「迁移能跑 + e2e 能过」，⑧ 回答「跑出来的 schema 还是不是上游那份」。
#     ⑧ 用 `--quiet`（判据是退出码），红了才补打完整报告 —— 绿的时候它有 767 行差异明细，
#     塞进 CI 日志只会把真正的信号淹掉。它对着库 URL 建/删自己的 scratch 库
#     `schema_probe_w0b_drift`，**不读**目标库里的表；但目标库必须存在、该角色要有 CREATEDB 权限，
#     否则脚本 exit 2 → 本脚本记 FAIL（绝不静默跳过）。
#   * ⑩ 只看**跟踪的代码文件**（`git ls-files`，不含 `docs/**`），并把存量违规钉在
#     `scripts/file_size_baseline.tsv` 里：清单外的文件不得超过 800 行，清单内的只允许变短，
#     已达标或已消失的条目必须从清单里删掉。刷新清单用 `--write-baseline`（基线只减不增）。
#     拆分大文件时 **改动会同时打到 mc-http 的热点文件**：拆完先跑 `--only file-size` 确认。
#   * ⑨ 必须显式 `--no-db` 且剥掉库变量：`report.json` 是 **stateless 层**快照，而 mc-conformance 的
#     `--db-url` 带了 `env = "MULTICA_TEST_DATABASE_URL"` —— 谁 export 过这个变量（跑 ⑥/⑧ 的人都会），
#     它就会追加 database 层、把「合并取强者」的报告拿去比 stateless 快照 → 门因为**环境**而红。
#     所以 ⑨ 同时用 `env -u` 与 `--no-db` 两道保险（与 ⑤ 同理）。

set -u
set -o pipefail

# 本机 /usr/bin/cargo 是 1.75，解析不了本仓的 manifest（edition2024 依赖等）；
# 真实工具链装在 ~/.cargo/bin。显式前置，别依赖调用者的 PATH。
export PATH="$HOME/.cargo/bin:$PATH"
# CI 之外的地方（例如 `sh scripts/gates.sh`）也保持一致的行为。
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.." || exit 2

# 门的规范顺序与显示编号（编号 == plan1 §6.4 的清单序号；⑦ 之后的编号由追加切片顺延，不重编）。
# 排列把两道**需要库**的门（⑥ ⑧）放在一起，离线门 ⑦ ⑨ 收尾；因此汇总表里 ⑧ 会印在 ⑦ 之前。
ALL_GATES="fmt build clippy clippy-test-util test db schema-drift route-parity conformance file-size"

gate_label() {
    case "$1" in
        fmt) echo "①" ;;
        build) echo "②" ;;
        clippy) echo "③" ;;
        clippy-test-util) echo "④" ;;
        test) echo "⑤" ;;
        db) echo "⑥" ;;
        schema-drift) echo "⑧" ;;
        route-parity) echo "⑦" ;;
        conformance) echo "⑨" ;;
        file-size) echo "⑩" ;;
        *) echo "?" ;;
    esac
}

gate_env_name() {
    case "$1" in
        fmt) echo "FMT" ;;
        build) echo "BUILD" ;;
        clippy) echo "CLIPPY" ;;
        clippy-test-util) echo "CLIPPY_TEST_UTIL" ;;
        test) echo "TEST" ;;
        db) echo "DB" ;;
        schema-drift) echo "SCHEMA_DRIFT" ;;
        route-parity) echo "ROUTE_PARITY" ;;
        conformance) echo "CONFORMANCE" ;;
        file-size) echo "FILE_SIZE" ;;
        *) echo "UNKNOWN" ;;
    esac
}

usage() {
    # 打印本文件顶部的注释块（第 3 行起，遇第一行非注释即停）。不要写死行号范围：
    # 每加一道门都要改行号的话，`--help` 迟早会截掉最后几行。
    awk 'NR > 2 { if ($0 !~ /^#/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
}

WITH_DB=0
ONLY=""
DB_URL="${MULTICA_TEST_DATABASE_URL:-}"

while [ $# -gt 0 ]; do
    case "$1" in
        --with-db) WITH_DB=1; shift ;;
        --db-url)
            [ $# -ge 2 ] || { echo "error: --db-url needs a value" >&2; exit 2; }
            DB_URL="$2"; shift 2 ;;
        --db-url=*) DB_URL="${1#*=}"; shift ;;
        --only)
            [ $# -ge 2 ] || { echo "error: --only needs a comma-separated gate list" >&2; exit 2; }
            ONLY="$2"; shift 2 ;;
        --only=*) ONLY="${1#*=}"; shift ;;
        --list)
            printf '%s\n' $ALL_GATES
            exit 0 ;;
        -h|--help)
            usage
            exit 0 ;;
        *)
            echo "error: unknown argument: $1" >&2
            echo "try: bash scripts/gates.sh --help" >&2
            exit 2 ;;
    esac
done

# ---- 选择要跑的门 ---------------------------------------------------------
if [ -n "$ONLY" ] && [ "$WITH_DB" -eq 1 ]; then
    echo "error: --only and --with-db are mutually exclusive (name 'db' in --only)" >&2
    exit 2
fi

SELECTED=""
if [ -n "$ONLY" ]; then
    for g in $(printf '%s' "$ONLY" | tr ',' ' '); do
        case " $ALL_GATES " in
            *" $g "*) SELECTED="$SELECTED $g" ;;
            *) echo "error: unknown gate: '$g' (known: $ALL_GATES)" >&2; exit 2 ;;
        esac
    done
else
    SELECTED=" fmt build clippy clippy-test-util test"
    [ "$WITH_DB" -eq 1 ] && SELECTED="$SELECTED db schema-drift"
    # ⑦ ⑨ ⑩ 都是离线确定性门（⑨ 用 --no-db 跑 stateless 层），因此留在默认集合里。
    SELECTED="$SELECTED route-parity conformance file-size"
fi

selected_gate() {
    case "$SELECTED " in
        *" $1 "*) return 0 ;;
        *) return 1 ;;
    esac
}

# ⑥ / ⑧ 的前置条件：必须有库 URL。缺了就直接报用法错误，而不是让测试静默跳过。
if selected_gate db || selected_gate schema-drift; then
    if [ -z "$DB_URL" ]; then
        echo "error: the 'db' / 'schema-drift' gates need a database URL" >&2
        echo "  pass --db-url 'postgres://user:pw@127.0.0.1:5432/<db>' or set MULTICA_TEST_DATABASE_URL" >&2
        echo "  (both gates create what they need: ⑥ migrates that DB, ⑧ creates and drops its own scratch DB)" >&2
        exit 2
    fi
fi

# ---- 执行 -----------------------------------------------------------------
# 结果表：每项 "gate|exit|secs|note"
RESULTS=""
OVERALL=0
GATE_RAN=0
GATE_PASSED=0
START_ALL="$(date +%s)"

record() { # name exit secs note
    RESULTS="${RESULTS}${1}|${2}|${3}|${4}
"
    GATE_RAN=$((GATE_RAN + 1))
    if [ "$2" -eq 0 ]; then
        GATE_PASSED=$((GATE_PASSED + 1))
    else
        OVERALL=1
    fi
}

# run_gate NAME cmd...  —— 收集式（不 fail-fast），失败只记 FAILED，继续跑后续的门。
run_gate() {
    local name="$1"; shift
    local start end rc
    printf '\n=== [%s] gate %s ===\n' "$(gate_label "$name")" "$name"
    printf '$ %s\n' "$*"
    start="$(date +%s)"
    "$@"
    rc=$?
    end="$(date +%s)"
    printf 'GATE_%s_EXIT=%s\n' "$(gate_env_name "$name")" "$rc"
    record "$name" "$rc" "$((end - start))" ""
    return 0
}

# ⑥ 有前置迁移，单独实现（迁移失败则不再跑 e2e，避免拿「表都建不出来」的库刷屏）。
run_db_gate() {
    local start end rc_m rc_e combined note t0 t1 t2
    printf '\n=== [%s] gate db ===\n' "$(gate_label db)"
    printf '$ MULTICA_DATABASE_URL=<db-url> mc-migrate run --dir migrations\n'
    printf '$ MULTICA_TEST_DATABASE_URL=<db-url> cargo test -p mc-repos -p mc-http --features mc-http/test-util -- --ignored\n'

    t0="$(date +%s)"
    MULTICA_DATABASE_URL="$DB_URL" cargo run -p mc-migrate -- run --dir migrations
    rc_m=$?
    t1="$(date +%s)"
    printf 'GATE_DB_MIGRATE_EXIT=%s\n' "$rc_m"

    if [ "$rc_m" -eq 0 ]; then
        MULTICA_TEST_DATABASE_URL="$DB_URL" cargo test -p mc-repos -p mc-http \
            --features mc-http/test-util -- --ignored
        rc_e=$?
        t2="$(date +%s)"
        printf 'GATE_DB_E2E_EXIT=%s\n' "$rc_e"
    else
        rc_e="skip"
        t2="$t1"
        printf 'GATE_DB_E2E_EXIT=skip  # not run: migrate failed (exit %s)\n' "$rc_m"
    fi

    if [ "$rc_m" -eq 0 ] && [ "$rc_e" -eq 0 ]; then
        combined=0
    else
        combined=1
    fi
    printf 'GATE_DB_EXIT=%s\n' "$combined"
    note="migrate=$rc_m,e2e=$rc_e"
    record db "$combined" "$((t2 - t0))" "$note"
    return 0
}

# ⑧ 自带 scratch 库（默认 `schema_probe_w0b_drift`），不下 ⑥ 迁移出来的那个库，所以两个门用同一个库 URL 是安全的。
# 平时 `--quiet`（判据是退出码）；红了才再跑一遍把完整报告打出来 —— 绿的时候那份报告有 700+ 行差异明细。
run_schema_drift_gate() {
    local start end rc
    printf '\n=== [%s] gate schema-drift ===\n' "$(gate_label schema-drift)"
    printf '$ MULTICA_TEST_DATABASE_URL=<db-url> python3 scripts/schema_drift.py --quiet\n'

    start="$(date +%s)"
    MULTICA_TEST_DATABASE_URL="$DB_URL" python3 scripts/schema_drift.py --quiet
    rc=$?

    if [ "$rc" -ne 0 ]; then
        printf '-- schema-drift is red (exit %s); full report follows --\n' "$rc"
        MULTICA_TEST_DATABASE_URL="$DB_URL" python3 scripts/schema_drift.py || true
        printf -- '-- end of schema-drift report --\n'
    fi

    end="$(date +%s)"
    printf 'GATE_SCHEMA_DRIFT_EXIT=%s\n' "$rc"
    record schema-drift "$rc" "$((end - start))" ""
    return 0
}

for gate in $SELECTED; do
    case "$gate" in
        fmt)            run_gate fmt            cargo fmt --all --check ;;
        build)          run_gate build          cargo build --workspace --all-targets --locked ;;
        clippy)         run_gate clippy         cargo clippy --workspace --all-targets -- -D warnings ;;
        clippy-test-util) run_gate clippy-test-util \
                            cargo clippy -p mc-http --all-targets --features mc-http/test-util -- -D warnings ;;
        # ⑤ 显式剥掉 DB 变量：见文件顶部「已知坑」。
        test)           run_gate test env -u MULTICA_TEST_DATABASE_URL -u MULTICA_DATABASE_URL \
                            cargo test --workspace ;;
        db)             run_db_gate ;;
        schema-drift)   run_schema_drift_gate ;;
        route-parity)   run_gate route-parity python3 scripts/route_parity.py --quiet ;;
        # ⑨ 显式 --no-db + 剥掉库变量：见文件顶部「已知坑」。判据是 --check 的退出码
        # （report.json 逐字节比对，漂移 → 1，见 crates/mc-conformance/src/main.rs）。
        conformance)    run_gate conformance env -u MULTICA_TEST_DATABASE_URL -u MULTICA_DATABASE_URL \
                            cargo run -q -p mc-conformance -- --no-db \
                            --check crates/mc-conformance/report.json ;;
        # ⑩ 纯离线、秒级；判据是 scripts/file_size_check.py 的退出码（越限 → 1）。
        file-size)      run_gate file-size python3 scripts/file_size_check.py --quiet ;;
        *)              echo "error: unhandled gate '$gate'" >&2; exit 2 ;;
    esac
done

# ---- 汇总表 ---------------------------------------------------------------
END_ALL="$(date +%s)"

printf '\n============================= GATE SUMMARY =============================\n'
printf '  #  %-17s %5s %6s  %s\n' "gate" "exit" "time" "result"
printf '%s\n' "$RESULTS" | while IFS='|' read -r name rc secs note; do
    [ -n "$name" ] || continue
    if [ "$rc" = "0" ]; then
        result="PASS"
    else
        result="FAIL"
    fi
    if [ -n "$note" ]; then
        printf '  %s  %-17s %5s %5ss  %s  (%s)\n' \
            "$(gate_label "$name")" "$name" "$rc" "$secs" "$result" "$note"
    else
        printf '  %s  %-17s %5s %5ss  %s\n' \
            "$(gate_label "$name")" "$name" "$rc" "$secs" "$result"
    fi
done

# 被跳过的门（本表只列实际跑过的）
NOT_SELECTED=""
for gate in $ALL_GATES; do
    if ! selected_gate "$gate"; then
        NOT_SELECTED="$NOT_SELECTED $gate"
    fi
done
if [ -n "$NOT_SELECTED" ]; then
    printf '  (not selected:%s)\n' "$NOT_SELECTED"
fi

printf '%s\n' "-----------------------------------------------------------------------"
if [ "$OVERALL" -eq 0 ]; then
    printf '  overall: PASS — %s/%s gate(s) green in %ss\n' "$GATE_PASSED" "$GATE_RAN" "$((END_ALL - START_ALL))"
else
    printf '  overall: FAIL — %s/%s gate(s) green in %ss (rerun the red gate(s) with --only)\n' \
        "$GATE_PASSED" "$GATE_RAN" "$((END_ALL - START_ALL))"
fi
printf '%s\n' "======================================================================="

exit "$OVERALL"

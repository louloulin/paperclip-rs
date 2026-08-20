#!/usr/bin/env bash
# scripts/parity-check.sh — Node vs Rust paperclip-rs 模块覆盖度对比
#
# 扫描 paperclip/server/src 与 paperclip/packages/shared/src 的 .ts 文件
# 与 paperclip-rs/crates/ 的 crate 目录，输出：
#   - Node module count
#   - Rust crate count
#   - 已覆盖的 Node module（Rust crate name 包含 Node module basename）
#   - 未覆盖的 Node module 列表（gap report）
#
# 用法：./scripts/parity-check.sh
# 输出：stdout 报告 + docs/parity-trend.md（append）
#
# 退出码：
#   0 = 覆盖率 >= 95%（PASS）
#   1 = 覆盖率 < 95%（FAIL）
#   2 = 执行错误

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
NODE_ROOT="${NODE_ROOT:-/Users/louloulin/Documents/lumosaipaperclip/paperclip}"
RUST_ROOT="${RUST_ROOT:-$PROJECT_ROOT}"
REPORT="${PARITY_REPORT:-$PROJECT_ROOT/docs/parity-trend.md}"
THRESHOLD="${PARITY_THRESHOLD:-95}"

# 辅助：统计 + 收集 basename
collect_modules() {
    local dir="$1"
    local pattern="$2"
    shift 2
    if [[ ! -d "$dir" ]]; then
        return
    fi
    find "$dir" -name "$pattern" "$@" 2>/dev/null \
        | sed 's|.*/||; s/\.'"${pattern#*.}"'$//' \
        | sort -u
}

echo "=== paperclip-rs Parity Check ==="
echo "Node root : $NODE_ROOT"
echo "Rust root : $RUST_ROOT"
echo "Threshold : $THRESHOLD%"
echo "Date      : $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
echo

# 1. Node 服务端 modules
echo "[1/4] Scanning Node server modules..."
NODE_SERVICES=$(collect_modules "$NODE_ROOT/server/src" "*.ts" -not -name "*.test.ts")
NODE_SERVICES_COUNT=$(echo "$NODE_SERVICES" | grep -c . || echo 0)

# 2. Node shared modules
echo "[2/4] Scanning Node shared modules..."
NODE_SHARED=$(collect_modules "$NODE_ROOT/packages/shared/src" "*.ts" -not -name "*.test.ts" -not -name "*.generated.ts")
NODE_SHARED_COUNT=$(echo "$NODE_SHARED" | grep -c . || echo 0)

NODE_TOTAL_COUNT=$((NODE_SERVICES_COUNT + NODE_SHARED_COUNT))
echo "  Node services: $NODE_SERVICES_COUNT"
echo "  Node shared  : $NODE_SHARED_COUNT"
echo "  Total Node   : $NODE_TOTAL_COUNT"

# 3. Rust crates
echo "[3/4] Scanning Rust crates..."
RUST_CRATES=$(ls "$RUST_ROOT/crates/" 2>/dev/null | grep -E "^pc-" | sed 's/^pc-//' | sort -u)
RUST_CRATES_COUNT=$(echo "$RUST_CRATES" | grep -c . || echo 0)
RUST_PUB_FNS=0
if [[ -d "$RUST_ROOT/crates" ]]; then
    RUST_PUB_FNS=$(grep -rE "^[[:space:]]*pub (fn|async fn|struct|enum|trait)" "$RUST_ROOT/crates/" --include="*.rs" 2>/dev/null | wc -l | tr -d ' ')
fi
echo "  Rust crates  : $RUST_CRATES_COUNT"
echo "  Rust pub APIs: $RUST_PUB_FNS"

# 4. Coverage calculation
echo "[4/4] Computing coverage..."

# Collect Rust module-level identifiers (crate name + each .rs file basename without .rs)
RUST_MODULES=""
if [[ -d "$RUST_ROOT/crates" ]]; then
    for crate_dir in "$RUST_ROOT/crates"/pc-*; do
        [[ -d "$crate_dir/src" ]] || continue
        crate_name=$(basename "$crate_dir" | sed 's/^pc-//')
        RUST_MODULES="${RUST_MODULES}${crate_name}\n"
        # Add individual .rs file basenames
        for rs_file in "$crate_dir/src"/*.rs; do
            [[ -f "$rs_file" ]] || continue
            fname=$(basename "$rs_file" .rs)
            # Skip generic filenames that aren't useful for matching
            case "$fname" in
                lib|mod|service|types|errors|helpers|tests|test) continue ;;
            esac
            RUST_MODULES="${RUST_MODULES}${fname}\n"
        done
    done
fi
RUST_MODULES_SORTED=$(echo -e "$RUST_MODULES" | grep -v "^$" | sort -u)

covered=0
gap_list=""
declare -A seen_gap
while IFS= read -r module; do
    [[ -z "$module" ]] && continue
    matched=0
    # Direct match
    if echo "$RUST_MODULES_SORTED" | grep -qx "$module"; then matched=1; fi
    # Normalize hyphens vs underscores
    if [[ $matched -eq 0 ]]; then
        norm=$(echo "$module" | tr '-' '_')
        if echo "$RUST_MODULES_SORTED" | grep -qx "$norm"; then matched=1; fi
    fi
    if [[ $matched -eq 0 ]]; then
        norm=$(echo "$module" | tr '_' '-')
        if echo "$RUST_MODULES_SORTED" | grep -qx "$norm"; then matched=1; fi
    fi
    # Substring match: Rust crate contains Node module basename
    if [[ $matched -eq 0 ]]; then
        if echo "$RUST_CRATES" | grep -qE "(^|-|_)${module}($|-|_)"; then matched=1; fi
    fi
    if [[ $matched -eq 1 ]]; then
        ((covered++)) || true
    else
        if [[ -z "${seen_gap[$module]:-}" ]]; then
            gap_list="${gap_list}${module}\n"
            seen_gap[$module]=1
        fi
    fi
done <<< "$(echo "$NODE_SERVICES"; echo "$NODE_SHARED")"

if [[ $NODE_TOTAL_COUNT -gt 0 ]]; then
    coverage=$(awk "BEGIN { printf \"%.1f\", $covered * 100.0 / $NODE_TOTAL_COUNT }")
else
    coverage="0.0"
fi

echo
echo "=== Results ==="
echo "  Coverage     : ${coverage}%"
echo "  Covered      : $covered"
echo "  Gap          : $((NODE_TOTAL_COUNT - covered))"
echo "  Threshold    : ${THRESHOLD}%"
echo "  Rust pub APIs: $RUST_PUB_FNS"

# Gap report
if [[ -n "$gap_list" ]]; then
    echo
    echo "=== Gap Report (unported Node modules) ==="
    echo -e "$gap_list" | head -50
    GAP_COUNT=$(echo -e "$gap_list" | grep -c . || echo 0)
    echo
    echo "Total gap: $GAP_COUNT unported modules"
    # Persist full gap list to gap-report.txt
    GAP_REPORT="${GAP_REPORT:-$PROJECT_ROOT/docs/parity-gap-report.txt}"
    {
        echo "# paperclip-rs Parity Gap Report"
        echo "# Generated: $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
        echo "# Node total: $NODE_TOTAL_COUNT | Rust crates: $RUST_CRATES_COUNT | Coverage: ${coverage}%"
        echo
        echo -e "$gap_list"
    } > "$GAP_REPORT"
    echo "Full gap list written to $GAP_REPORT"
fi

# Append to parity-trend.md
mkdir -p "$(dirname "$REPORT")"
{
    echo
    echo "## $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    echo "- Node total: $NODE_TOTAL_COUNT"
    echo "- Rust crates: $RUST_CRATES_COUNT"
    echo "- Rust pub APIs: $RUST_PUB_FNS"
    echo "- Covered: $covered"
    echo "- Coverage: ${coverage}%"
    echo "- Gap: $((NODE_TOTAL_COUNT - covered))"
    if [[ -n "$gap_list" ]]; then
        echo "- Unported:"
        echo -e "$gap_list" | sed 's/^/  - /' | head -30
    fi
} >> "$REPORT"

# Exit code
if awk "BEGIN { exit !($coverage >= $THRESHOLD) }"; then
    echo
    echo "[PASS] Coverage ${coverage}% >= ${THRESHOLD}%"
    exit 0
else
    echo
    echo "[FAIL] Coverage ${coverage}% < ${THRESHOLD}%"
    exit 1
fi
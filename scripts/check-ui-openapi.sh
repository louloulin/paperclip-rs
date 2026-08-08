#!/usr/bin/env bash
# scripts/check-ui-openapi.sh — M19 UI client × OpenAPI 路径对齐检查
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/.route-audit"
mkdir -p "$OUT"
python3 "$(dirname "$0")/../scripts/lib/check-ui-openapi.py" "$ROOT" "$OUT"

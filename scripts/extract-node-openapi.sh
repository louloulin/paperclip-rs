#!/usr/bin/env bash
# scripts/extract-node-openapi.sh — M19 从 Node openapi.ts 提取所有注册的
# OpenAPI path/method/operationId，作为对齐参考（不直接解析 OpenAPI 产物）。

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NODE_ROOT="${NODE_ROOT:-$ROOT/../paperclip}"
OUT="$ROOT/.route-audit/node-openapi-paths.tsv"
mkdir -p "$(dirname "$OUT")"

python3 - "$NODE_ROOT" "$OUT" << 'PYEOF'
import os, re, sys
NODE_ROOT, OUT = sys.argv[1:3]
rx = re.compile(r'\.(get|post|put|patch|delete)\(\s*[\'"`]([^\'"` ]+)[\'"`]')
with open(os.path.join(NODE_ROOT, "server/src/routes/openapi.ts")) as f:
    src = f.read()
# Extract path-templated section after "// ----- {section} -----"
with open(OUT, "w") as out:
    for m in rx.finditer(src):
        verb = m.group(1).upper()
        path = m.group(2)
        # The Node openapi.ts is also mounted under /api via app.use("/api", api).
        full = path if path.startswith("/api/") else "/api" + path
        out.write(f"{verb}\t{full}\n")
print(f"wrote {OUT}")
PYEOF
wc -l "$OUT"

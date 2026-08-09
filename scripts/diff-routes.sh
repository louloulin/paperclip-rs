#!/usr/bin/env bash
# scripts/diff-routes.sh — M30 路由字节级对齐度量
#
# 对比 paperclip（Node，路径 ../paperclip）与 paperclip-rs（Rust）两边的
# HTTP method+path 路由表，计算覆盖率与 top 缺口类别。
# 输出 JSON + Markdown 两份报告：.route-audit/route-diff.json 和 .md。

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NODE_ROOT="${NODE_ROOT:-$ROOT/../paperclip}"
RUST_ROOT="$ROOT"
OUT_DIR="$ROOT/.route-audit"
mkdir -p "$OUT_DIR"

python3 - "$NODE_ROOT" "$RUST_ROOT" "$OUT_DIR" << 'PYEOF'
import json, os, re, sys
from collections import defaultdict

NODE_ROOT, RUST_ROOT, OUT_DIR = sys.argv[1:4]

# Files mounted outside the central /api router (per app.ts analysis):
#   - health.ts: app.get("/health", ...) at root
#   - llms.ts: mounted via app.use(llmRoutes(db)) without /api prefix
NO_REPLACE_PREFIX = {"health.ts", "llms.ts"}

def extract_node(node_root):
    """Walk Node server/src/routes/*.ts and pull `router.{verb}("/path", ...)`.

    app.ts mounts every router under `app.use("/api", api)` (line 528), except
    `health` (root) and `llms` (its own prefix). We prepend "/api" accordingly.
    """
    routes = []
    rx = re.compile(r'\b(router|api|app)\.(get|post|put|patch|delete)\(s*[\'"`]([^\'"` ]+)[\'"`]')
    routes_dir = os.path.join(node_root, "server/src/routes")
    if not os.path.isdir(routes_dir):
        sys.exit(f"node routes dir missing: {routes_dir}")
    for fname in sorted(os.listdir(routes_dir)):
        if not fname.endswith(".ts"): continue
        fpath = os.path.join(routes_dir, fname)
        with open(fpath) as f: src = f.read()
        for m in rx.finditer(src):
            verb = m.group(2).upper()
            path = m.group(3)
            if path.startswith("/api/cli-auth"): continue
            if fname in NO_REPLACE_PREFIX:
                full_path = path  # already at root, no /api prefix
            elif path.startswith("/api/"):
                full_path = path
            elif fname in ("companies.ts", "auth.ts"):
                # mounted at /api/<name> in app.ts
                full_path = f"/api/{fname.removesuffix('.ts')}" + path
            else:
                # mounted with `api.use(<name>Routes(db))` (no prefix)
                full_path = "/api" + path
            routes.append((verb, full_path, fname))
    return routes

# Rust wildcard names: collapse `*file_path` / `*filePath` / `*x` → `*filePath`
# (Node path-to-regexp v8 conventionally names wildcards `*filePath`).
WILDCARD_NORMALIZE_RE = re.compile(r"\*[A-Za-z_][A-Za-z0-9_]*")

def normalise_path(p):
    """Collapse `:foo` → `:param` + wildcard names → `*filePath`."""
    p = re.sub(r':[A-Za-z_][A-Za-z0-9_]*', ':param', p)
    p = WILDCARD_NORMALIZE_RE.sub('*filePath', p)
    return p

def strip_rust_comments(src):
    """Strip // line comments so .route() chunks preceded by comments are still matched.

    Block comments are NOT stripped here on purpose: axum wildcard path syntax
    contains `*file_path`, which a naive `/*` detector would falsely treat as
    a C-style block comment start.
    """
    return re.sub(r'//[^\n]*', '', src)

def extract_rust(rust_root):
    """Walk Rust crates/pc-http/src/routes/*.rs and pull every verb chained
    onto a `.route("/path", ...)`. Captures `get/post/put/patch/delete`
    whether the verb is the first argument or chained via `.verb(...)`.

    M30 hardening:
    - strip line/block comments before scanning so leading-comment chunks
      don't get skipped (companies.rs R516 search/extract had this bug).
    - use greedy depth tracking so chained `.route(...).merge(...)` works.
    """
    routes = []
    routes_dir = os.path.join(rust_root, "crates/pc-http/src/routes")
    if not os.path.isdir(routes_dir):
        sys.exit(f"rust routes dir missing: {routes_dir}")
    for fname in sorted(os.listdir(routes_dir)):
        if not fname.endswith(".rs"):
            continue
        fpath = os.path.join(routes_dir, fname)
        with open(fpath) as f:
            raw = f.read()
        src = strip_rust_comments(raw)
        for chunk in re.split(r"\.route\(", src)[1:]:
            # Allow leading whitespace + comments before the string literal.
            pm = re.search(r"""['"]([^'"]+)['"]""", chunk)
            if not pm:
                continue
            path = pm.group(1)
            # Restrict the scan to the same `.route(...)` invocation: stop at
            # the first top-level close paren. Track paren depth starting
            # from 1 (we are inside the .route( call already).
            depth = 1
            tail_chars = []
            for ch in chunk:
                if ch == "(":
                    depth += 1
                elif ch == ")":
                    depth -= 1
                    if depth == 0:
                        break
                tail_chars.append(ch)
            tail = "".join(tail_chars)
            verbs = set()
            for vm in re.finditer(r"\b(get|post|put|patch|delete)\s*\(", tail):
                verbs.add(vm.group(1).upper())
            for v in verbs:
                routes.append((v, path, fname))
    return routes

def norm_set(routes):
    return {(verb, normalise_path(path)) for verb, path, _ in routes}

def category(path):
    if not path.startswith("/"): return "misc"
    parts = path.split("/")
    # Strip leading "/api" so categories group by domain (/api/companies/* → "companies").
    if len(parts) >= 2 and parts[1] == "api":
        parts = [""] + parts[2:]
    if len(parts) < 3: return "root"
    return parts[2]

node = extract_node(NODE_ROOT)
rust = extract_rust(RUST_ROOT)
n_set = norm_set(node)
r_set = norm_set(rust)
common = n_set & r_set
missing = n_set - r_set
extra   = r_set - n_set

cat_missing = defaultdict(int)
for verb, path in missing:
    cat_missing[category(path)] += 1

result = {
    "node_unique":       len(n_set),
    "rust_unique":       len(r_set),
    "common":            len(common),
    "missing_in_rust":   len(missing),
    "extra_in_rust":     len(extra),
    "coverage_method_path": round(100 * len(common) / max(1, len(n_set)), 2),
    "missing_by_category": dict(sorted(cat_missing.items(), key=lambda kv: -kv[1])[:15]),
    "top_missing":       [{"method": v, "path": p} for v, p in sorted(missing)[:50]],
}

with open(os.path.join(OUT_DIR, "route-diff.json"), "w") as f:
    json.dump(result, f, indent=2)

# Markdown summary
md = []
md.append("# M30 — Node ↔ Rust 路由 method+path 重合率")
md.append("")
md.append(f"- Node unique routes: **{result['node_unique']}**")
md.append(f"- Rust unique routes: **{result['rust_unique']}**")
md.append(f"- Common: **{result['common']}**")
md.append(f"- Missing in Rust: **{result['missing_in_rust']}**")
md.append(f"- Extra in Rust:   **{result['extra_in_rust']}**")
md.append(f"- **Coverage (method+path): {result['coverage_method_path']}%**")
md.append("")
md.append("## Top missing categories")
md.append("")
md.append("| Category | Missing count |")
md.append("|---|---:|")
for cat, n in result["missing_by_category"].items():
    md.append(f"| `/api/{cat}/*` | {n} |")
md.append("")
md.append("## Top 50 missing method+path")
md.append("")
md.append("| Method | Path |")
md.append("|---|---|")
for m in result["top_missing"]:
    md.append(f"| {m['method']} | `{m['path']}` |")
md.append("")

with open(os.path.join(OUT_DIR, "route-diff.md"), "w") as f:
    f.write("\n".join(md))

print(f"coverage={result['coverage_method_path']}%  node={result['node_unique']} rust={result['rust_unique']} missing={result['missing_in_rust']}")
print(f"reports: {OUT_DIR}/route-diff.{{json,md}}")
PYEOF

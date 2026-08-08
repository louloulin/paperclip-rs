#!/usr/bin/env python3
"""M19 UI client × OpenAPI 路径对齐检查。"""
import json, os, re, sys

ROOT = sys.argv[1]
OUT  = sys.argv[2]
ui_src = os.path.join(ROOT, "ui/src")

api_rx = re.compile(r"api\.(get|post|put|patch|delete)\(\s*[`'\"](/api/[^'\"`]+)[`'\"]")
fetch_rx = re.compile(r"fetch\(\s*[`'\"](/api/[^'\"`]+)[`'\"\)]")
# Catch-all literal scan for `/api/...` inside any quoted string.
literal_rx = re.compile(r"[`'\"](/api/[^'\"`\s\\]+)[`'\"\\)]")

ui_paths = []
for root, _, files in os.walk(ui_src):
    for fname in files:
        if not fname.endswith(".ts") or fname.endswith(".test.ts") or fname == "client.ts":
            continue
        fpath = os.path.join(root, fname)
        try:
            with open(fpath) as f:
                src = f.read()
        except (UnicodeDecodeError, OSError):
            continue
        rel = os.path.relpath(fpath, ui_src)
        for m in api_rx.finditer(src):
            ui_paths.append((m.group(1).upper(), m.group(2), rel))
        for m in fetch_rx.finditer(src):
            ui_paths.append(("GET", m.group(1), rel))
        for m in literal_rx.finditer(src):
            ui_paths.append(("GET", m.group(1), rel))

def norm(p):
    p = re.sub(r"\$\{[^}]+\}", ":param", p)
    p = re.sub(r"\{[^}]+\}", ":param", p)
    # trim trailing comma/backtick (from templates)
    return p

ui_keys = {(v, norm(p)) for v, p, _ in ui_paths}

with open(os.path.join(OUT, "rust-openapi.json")) as f:
    oa = json.load(f)

def norm_oa(p):
    return re.sub(r"\{[^}]+\}", ":param", p)

oa_keys = set()
for path, ops in oa.get("paths", {}).items():
    for verb in ops:
        if verb.upper() in ("GET","POST","PUT","PATCH","DELETE","OPTIONS","HEAD"):
            oa_keys.add((verb.upper(), norm_oa(path)))

common  = ui_keys & oa_keys
missing = ui_keys - oa_keys
extra   = oa_keys - ui_keys

result = {
    "ui_paths":              len(ui_keys),
    "rust_openapi_paths":    len(oa_keys),
    "covered":               len(common),
    "missing_in_openapi":    len(missing),
    "extra_in_openapi":      len(extra),
    "coverage_ui_paths":     round(100 * len(common) / max(1, len(ui_keys)), 2),
}

with open(os.path.join(OUT, "ui-openapi-overlap.json"), "w") as f:
    json.dump({**result,
               "missing_top30": [{"verb": v, "path": p, "file": fn}
                                 for v, p, fn in sorted(
                                     [(v, p, fn) for v, p, fn in ui_paths
                                      if (v, norm(p)) in missing])[:30]]},
              f, indent=2)

md = [
    "# M19 — UI client × Rust OpenAPI 路径覆盖率",
    "",
    f"- UI 客户端 distinct 调用: **{result['ui_paths']}**",
    f"- Rust OpenAPI paths: **{result['rust_openapi_paths']}**",
    f"- 命中: **{result['covered']}**",
    f"- UI 调用但 OpenAPI 缺失: **{result['missing_in_openapi']}**",
    f"- OpenAPI 声明但 UI 未用: **{result['extra_in_openapi']}**",
    f"- **覆盖率: {result['coverage_ui_paths']}%**",
    "",
    "## Top 30 missing (UI 真实调用，但 OpenAPI 文档未注册)",
    "",
    "| Method | Path | File |",
    "|---|---|---|",
]
for it in sorted([(v, p, fn) for v, p, fn in ui_paths if (v, norm(p)) in missing])[:30]:
    md.append(f"| {it[0]} | `{it[1]}` | {it[2]} |")
md.append("")

with open(os.path.join(OUT, "ui-openapi-overlap.md"), "w") as f:
    f.write("\n".join(md))

print(f"UI paths={result['ui_paths']}  OpenAPI paths={result['rust_openapi_paths']}  covered={result['covered']}  coverage={result['coverage_ui_paths']}%")

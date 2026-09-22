#!/usr/bin/env python3
"""W3b pre-merge / post-merge audit: route inventory, gate risks, golden-path exactness.

The risk this closes
--------------------
Three slices (M3-4 / M3-5 / M3-6) land into `feat/multica-rs-initial` in one cycle.
Before the merge, the integrator has to answer questions that neither `gates.sh` nor
`docs/37` can answer automatically:

1. **What does each slice actually register?** Descriptions drift; the worktree is the
   truth. §12.1 of `docs/37` predicted the W3b route sets from *issue text* only.
2. **Do two slices register the same `(method, path)`?** Two registrations of the same
   key make axum **panic at router build time** — a global outage, not a warning.
3. **Does a slice add a route that a base stub already registers?** M3-6 must *replace*
   6 `not_implemented` registrations in `crates/mc-http/src/routes/issues/mod.rs`; adding
   them instead is case (2).
4. **Will ⑩ file-size be red after the merge?** In-flight files are **untracked**, so
   `scripts/file_size_check.py` (it reads `git ls-files`) cannot see them at all.
5. **Is a golden fixture path satisfied only through the trailing-slash fold?**
   `scripts/route_parity.py` folds `/x` and `/x/` by design (chi serves both), so
   `GET /api/agents` looks satisfied while axum actually 404s it. Measured fact:
   `matchit` 0.7.3 returns `Err(MissingTrailingSlash)` and `axum-0.7.9/src/routing/
   path_router.rs` maps that to "no route" -> 404. This tool therefore checks golden
   fixture paths against the **exact registered literals**, not the folded set.

What it is not
--------------
It does not compile, does not run tests and does not replace `gates.sh`. It is a static,
sub-second read-only audit whose findings are meant to be quoted in the merge cycle.

Usage
-----
    # 1. audit three in-flight slice worktrees (read-only; nothing in them is modified)
    python3 scripts/w3b_premerge_audit.py \
        --slice M3-4=/path/to/lum-1427-*/workdir/paperclip-rs \
        --slice M3-5=/path/to/lum-1428-*/workdir/paperclip-rs \
        --expect scratch/w3b_expect.json

    # 2. after the merge, replay the frozen expectation against the merged tree
    python3 scripts/w3b_premerge_audit.py --merged . --expect scratch/w3b_expect.json

Exit codes: 0 = no finding; 1 = at least one finding (see the `!!` lines); 2 = bad CLI.

Findings are printed as `!!` lines; `!!` on stderr-equivalent severity does **not** mean
"do not merge" — it means "the merge cycle must state a decision for this line".
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys

# Paths no W3b slice may touch: the anchor mechanism owns them (docs/36 §3, docs/15 §7.3).
GUARDED = (
    "crates/mc-http/src/routes/mount.rs",
    "crates/mc-http/src/routes/mod.rs",
    "docs/fixtures/route-parity-baseline.json",
    "crates/mc-conformance/report.json",
    "scripts/file_size_baseline.tsv",
)

METHODS = ("get", "post", "put", "patch", "delete", "head", "options", "trace")
FILE_SIZE_LIMIT = 800
SIZE_SCOPE = (("crates/", (".rs",)), ("apps/", (".rs",)), ("scripts/", (".py", ".sh")))
SKIP_ENV = re.compile(r"MULTICA_TEST_DATABASE_URL|MULTICA_DATABASE_URL|DATABASE_URL")


def sh(args, cwd=None):
    return subprocess.run(args, cwd=cwd, capture_output=True, text=True)


def git(wt, *args):
    r = sh(["git", "-C", wt, *args])
    return r.stdout if r.returncode == 0 else ""


def mask_cfg_test(text: str) -> str:
    """Blank out `#[cfg(test)] mod m { ... }` bodies (route_parity.py skips them too)."""
    out = list(text)
    for m in re.finditer(r"#\[cfg\(test\)\]", text):
        i = text.find("mod", m.end())
        if i < 0:
            continue
        j = text.find("{", i)
        if j < 0:
            continue
        depth = 0
        for k in range(j, len(text)):
            if text[k] == "{":
                depth += 1
            elif text[k] == "}":
                depth -= 1
                if depth == 0:
                    break
        for k in range(m.start(), min(k + 1, len(text))):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def extract_routes(text: str):
    """`.route("<path>", <method-chain>)` -> {(METHOD, path)}; paren-matched, multi-line safe."""
    text = mask_cfg_test(text)
    found = set()
    for m in re.finditer(r"\.route\s*\(", text):
        i = m.end() - 1
        depth = 0
        for k in range(i, len(text)):
            if text[k] == "(":
                depth += 1
            elif text[k] == ")":
                depth -= 1
                if depth == 0:
                    break
        call = text[i + 1 : k]
        lit = re.match(r'\s*"((?:[^"\\]|\\.)*)"', call)
        if not lit:
            continue
        path = lit.group(1)
        for meth in re.findall(r"\b([a-z]+)\s*\(", call[lit.end() :]):
            if meth in METHODS:
                found.add((meth.upper(), path))
    return found


def norm(path: str) -> str:
    return re.sub(r"[:{][^/}]*\}?", "*", path.rstrip("/"))


def canon(key: str) -> str:
    """Canonical `METHOD path` key with `:param` placeholders, trailing slash preserved —
    the shape `docs/fixtures/route-parity-baseline.json` is written in."""
    meth, _, path = key.partition(" ")
    return f"{meth} {re.sub(r'[:{][^/}]*}?', ':param', path)}"


def slice_files(wt: str, base_ref: str | None):
    """Changed + untracked files of a worktree, relative to base_ref when given."""
    files = set()
    for line in git(wt, "status", "--porcelain").splitlines():
        if len(line) > 3:
            files.add(line[3:].strip().strip('"'))
    if base_ref:
        for f in git(wt, "diff", "--name-only", base_ref).splitlines():
            if f.strip():
                files.add(f.strip())
    for f in git(wt, "ls-files", "--others", "--exclude-standard").splitlines():
        if f.strip():
            files.add(f.strip())
    # `git status --porcelain` reports a new directory as one `dir/` entry: expand it.
    out = set()
    for f in files:
        p = os.path.join(wt, f)
        if os.path.isdir(p):
            for base, _, names in os.walk(p):
                for n in names:
                    out.add(os.path.relpath(os.path.join(base, n), wt))
        elif f:
            out.add(f)
    return sorted(out)


def routes_at(wt: str, ref: str | None):
    """Route inventory of a tree: (path -> text) from `ref` (git object) or the worktree."""
    inv = set()
    if ref is None:
        root = os.path.join(wt, "crates/mc-http/src")
        for base, _, names in os.walk(root):
            for n in names:
                if n.endswith(".rs"):
                    p = os.path.join(base, n)
                    inv |= extract_routes(open(p, encoding="utf-8", errors="replace").read())
        return inv
    for f in git(wt, "ls-tree", "-r", "--name-only", ref, "crates/mc-http/src").splitlines():
        if not f.endswith(".rs"):
            continue
        blob = sh(["git", "-C", wt, "show", f"{ref}:{f}"]).stdout
        inv |= extract_routes(blob)
    return inv


def line_count(text: str) -> int:
    return text.count("\n") + (0 if text.endswith("\n") or text == "" else 1)


def size_violations(wt: str, files, baseline: dict):
    """Replicates scripts/file_size_check.py for **untracked** files (it reads ls-files)."""
    bad = []
    for f in files:
        if not (f.endswith(".rs") and f.startswith("crates/")) and not (
            f.startswith("scripts/") and f.endswith((".py", ".sh"))
        ):
            continue
        p = os.path.join(wt, f)
        if not os.path.exists(p):
            continue
        n = line_count(open(p, encoding="utf-8", errors="replace").read())
        rec = baseline.get(f)
        if n > FILE_SIZE_LIMIT and (rec is None or n > rec):
            bad.append((f, n, rec))
    return bad


def read_baseline(wt: str):
    p = os.path.join(wt, "scripts/file_size_baseline.tsv")
    out = {}
    if not os.path.exists(p):
        return out
    for line in open(p, encoding="utf-8"):
        if line.startswith("#") or not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) >= 2 and parts[1].strip().isdigit():
            out[parts[0]] = int(parts[1])
    return out


def fingerprint(wt: str, files):
    rows, digest_rows = [], []
    for f in files:
        p = os.path.join(wt, f)
        if os.path.exists(p):
            h = hashlib.sha256(open(p, "rb").read()).hexdigest()
        else:
            h = "MISSING"
        rows.append({"path": f, "sha256": h})
        digest_rows.append(f"{f} {h}")
    digest = hashlib.sha256("\n".join(digest_rows).encode()).hexdigest()
    return {"digest": digest, "files": rows}


def golden_paths(root: str, golden_dir: str):
    out = []
    d = os.path.join(root, golden_dir)
    for base, _, names in os.walk(d):
        for n in names:
            if n.endswith(".json"):
                fx = json.load(open(os.path.join(base, n), encoding="utf-8"))
                if fx.get("path"):
                    out.append((fx.get("method", "?"), fx["path"], fx.get("id", n)))
    return out


def audit_slice(name, wt, base_ref, baseline, golden):
    files = slice_files(wt, base_ref)
    cur = routes_at(wt, None)
    base = routes_at(wt, base_ref) if base_ref else set()
    add = sorted(cur - base)
    findings = []
    for f in (f for f in files if f in GUARDED):
        findings.append(f"{name}: touches guarded path {f}")
    for meth, path in add:
        if (meth, path) in base:
            findings.append(f"{name}: adds an already-registered key {meth} {path}")
    # A golden fixture path that this slice satisfies only through the trailing-slash fold is a
    # 404 in axum (matchit `Err(MissingTrailingSlash)`): ⑦ folds the two forms, so only this
    # check can see it. `live` = the tree as it will look once this slice is merged.
    live = base | set(add)
    folded = {(m_, norm(p)) for m_, p in add}
    for meth, path, fid in golden:
        if (meth, norm(path)) not in folded:
            continue
        if (meth, path) in live:
            continue
        if (meth, path.rstrip("/") + "/") in live:
            findings.append(
                f"{name}: golden {meth} {path} is served only via the trailing-slash alias "
                f"{path.rstrip('/')}/ -> axum 404s the fixture path ({fid})"
            )
    skips = []
    for f in files:
        if "/tests" not in f and "tests.rs" not in f:
            continue
        p = os.path.join(wt, f)
        if not os.path.exists(p):
            continue
        t = open(p, encoding="utf-8", errors="replace").read()
        t = mask_cfg_test(t)
        n_test = len(re.findall(r"#\[(?:tokio::)?test\]", t))
        if n_test == 0:
            continue
        n_ig = len(re.findall(r"#\[ignore", t))
        env = "yes" if SKIP_ENV.search(t) else "NO"
        skips.append((f, n_test, n_ig, env))
    row = {
        "worktree": os.path.abspath(wt),
        "head": git(wt, "rev-parse", "--short", "HEAD").strip(),
        "base_ref": base_ref,
        "files": files,
        "added_routes": [f"{m} {p}" for m, p in add],
        "added_folded": len({(m, norm(p)) for m, p in add}),
        "size_violations": size_violations(wt, files, baseline),
        "findings": findings,
        "gated_skips": skips,
        "fingerprint": fingerprint(wt, files),
    }
    return row


def report_slices(rows, expect_in=None):
    allkeys = {}
    for r in rows:
        for key in r["added_routes"]:
            allkeys.setdefault(key, []).append(r["name"] if "name" in r else r.head)
    print("== per-slice ==")
    for r in rows:
        print(f"  [{r['name']}] head {r['head']} base {r['base_ref']} files {len(r['files'])} "
              f"added_routes {len(r['added_routes'])} keys = {r['added_folded']} upstream keys "
              f"(folded) fp {r['fingerprint']['digest'][:16]}")
        for m, p in (k.split(" ", 1) for k in r["added_routes"]):
            print(f"      + {m:6} {p}")
        for f, n, rec in r["size_violations"]:
            why = "not in baseline" if rec is None else f"baseline {rec}"
            print(f"      !! ⑩ file-size {n} lines > {FILE_SIZE_LIMIT} ({why}): {f}")
        for f in r["findings"]:
            print(f"      !! {f}")
        for f, nt, ni, env in r["gated_skips"]:
            flag = "" if env == "yes" else "  !! no DB-gate env -> would run against a real/no DB"
            print(f"      ~ {nt:2} tests / {ni} #[ignore] / DB-gate {env}: {f}{flag}")
    dup = {k: v for k, v in allkeys.items() if len(v) > 1}
    print("== cross-slice ==")
    union = {k for r in rows for k in r["added_routes"]}
    print(f"  union {len(union)} keys; duplicates across slices: {dup if dup else 'none'}")
    if expect_in:
        print(f"  expected (frozen) {len(expect_in.get('routes', []))} keys; "
              f"missing now: {sorted(set(expect_in.get('routes', [])) - union) or 'none'}; "
              f"new since freeze: {sorted(union - set(expect_in.get('routes', []))) or 'none'}")
    return dup


def report_merged(wt, baseline, golden, expect_in):
    files = slice_files(wt, None)
    inv = routes_at(wt, None)
    findings = []
    # ⑦ baseline coverage: every live key should be remembered, else deleting it is silent.
    bpath = os.path.join(wt, "docs/fixtures/route-parity-baseline.json")
    remembered = set()
    if os.path.exists(bpath):
        raw = json.load(open(bpath, encoding="utf-8"))
        for item in raw.get("routes", raw if isinstance(raw, list) else []):
            if isinstance(item, str):
                remembered.add(item)
            elif isinstance(item, dict) and item.get("path"):
                remembered.add(f"{item.get('method', 'GET')} {item['path']}")
    live = {canon(f"{m} {p}") for m, p in inv}
    if remembered:
        missing = sorted(k for k in remembered if canon(k) not in live)
        print(f"== ⑦ baseline ==\n  baseline {len(remembered)} keys / live {len(inv)}; "
              f"baseline keys no longer registered: {len(missing)}")
        for k in missing[:10]:
            print(f"      gone: {k}")
        if missing:
            print("      (⑨/⑦ will flag this at merge time: refresh the baseline only after the diff is understood)")
    # ⑩ against the real gate semantics (tracked paths only apply after `git add`).
    for f, n, rec in size_violations(wt, files, baseline):
        findings.append(f"⑩ file-size {n} > {FILE_SIZE_LIMIT}: {f}")
    # golden fixture paths must be served by an **exact** literal, not by the slash fold.
    print("== golden paths (exact literal required) ==")
    seen = {}
    for meth, path, fid in golden:
        if (meth, path) in seen:
            seen[(meth, path)][1] += 1
            continue
        seen[(meth, path)] = [fid, 1]
    for (meth, path), (fid, n) in seen.items():
        exact = (meth, path) in inv
        folded = (meth, path.rstrip("/") + "/") in inv or (meth, path + "/") in inv
        if exact:
            state = "exact"
        elif folded:
            state = "!! ONLY VIA TRAILING-SLASH ALIAS (axum 404s the fixture path)"
            findings.append(f"{meth} {path} registered only as {path.rstrip('/')}/ ({fid})")
        else:
            state = "not registered"
        print(f"  {meth:6} {path:52} {state}  [{n} fixture(s)]")
    if expect_in:
        want = set(expect_in.get("routes", []))
        lost = sorted(want - live)
        print(f"== expectation replay ==\n  frozen {len(want)} / live {len(live)}; "
              f"lost by the merge: {lost or 'none'}")
        if lost:
            findings.append(f"merge dropped frozen routes: {lost}")
    for f in findings:
        print(f"  !! {f}")
    return findings


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="W3b route/gate audit (static, read-only).")
    ap.add_argument("--slice", action="append", default=[], metavar="NAME=PATH")
    ap.add_argument("--base-ref", default=None, help="ref the slices branched from (e.g. 2a759ae)")
    ap.add_argument("--merged", default=None, metavar="PATH", help="post-merge worktree to audit")
    ap.add_argument("--expect", default=None, metavar="FILE", help="frozen route-set JSON (write or read)")
    ap.add_argument("--golden-dir", default="contracts/golden")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    if not args.slice and not args.merged:
        ap.error("give at least one --slice NAME=PATH or --merged PATH")

    base_ref = args.base_ref or os.environ.get("W3B_BASE_REF")
    expect_in = None
    if args.expect and os.path.exists(args.expect):
        expect_in = json.load(open(args.expect, encoding="utf-8"))

    failures = 0
    if args.slice:
        rows = []
        for spec in args.slice:
            if "=" not in spec:
                ap.error(f"--slice wants NAME=PATH, got {spec!r}")
            name, wt = spec.split("=", 1)
            if not os.path.isdir(wt):
                ap.error(f"slice path does not exist: {wt}")
            baseline = read_baseline(wt)
            golden = golden_paths(wt, args.golden_dir)
            row = audit_slice(name, wt, base_ref, baseline, golden)
            row["name"] = name
            rows.append(row)
            failures += len(row["findings"]) + len(row["size_violations"])
        dup = report_slices(rows, expect_in)
        failures += len(dup)
        if args.expect and not os.path.exists(args.expect):
            frozen = {
                "base_ref": base_ref,
                "routes": sorted({k for r in rows for k in r["added_routes"]}),
                "fingerprints": {r["name"]: r["fingerprint"]["digest"] for r in rows},
                "heads": {r["name"]: r["head"] for r in rows},
            }
            json.dump(frozen, open(args.expect, "w", encoding="utf-8"), indent=1, ensure_ascii=False)
            print(f"== froze {len(frozen['routes'])} routes + {len(rows)} fingerprints -> {args.expect}")

    if args.merged:
        baseline = read_baseline(args.merged)
        golden = golden_paths(args.merged, args.golden_dir)
        failures += len(report_merged(args.merged, baseline, golden, expect_in))

    print(f"== audit: {failures} finding(s) ==")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

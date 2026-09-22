#!/usr/bin/env python3
"""Trailing-slash alias audit: does each registered path have the *shape* upstream serves?

The risk this closes
--------------------
`scripts/route_parity.py` folds `/x` and `/x/` into one key on purpose (chi serves both),
so a slice that registers **only** the form upstream happens to spell with a slash looks
satisfied while axum 404s the other one.  Measured fact (docs/37 §14.2): axum 0.7 /
matchit 0.7.3 returns `Err(MissingTrailingSlash)` for `/api/agents` when only
`/api/agents/` is registered, and `axum-0.7.9/src/routing/path_router.rs` turns that into
"no route" -> **404, not a 307 redirect**.  A golden fixture that requests the missing form
turns ⑨ red; a route with no fixture stays silently wrong.

What upstream actually serves (derived from `router.go`, not guessed)
---------------------------------------------------------------------
chi serves **two** paths for one registration when, and only when, the pattern is mounted
as a subrouter and the child route is exactly `"/"`:

    r.Route("/api/runtimes", func(r chi.Router) {   // Mount -> serves /api/runtimes AND /api/runtimes/
        r.Get("/", h.ListAgentRuntimes)             //   (this child is "/")
        r.Route("/{runtimeId}", func(r chi.Router) {
            r.Patch("/", h.UpdateAgentRuntime)      // Mount -> /api/runtimes/{id} AND .../{id}/
            r.Get("/usage", h.GetRuntimeUsage)      // plain -> only .../{id}/usage
        })
    })

A plain `r.Get("/runtime-profiles", h)` inside an enclosing subrouter serves exactly that
one path.  `docs/fixtures/upstream-routes.tsv` preserves the distinction: **a path recorded
with a trailing slash is a mounted subrouter root (both forms); one without is a plain
registration (single form)**.  82 of 472 fixture paths end with `/`.

So the rule this script enforces, per folded `(METHOD, path)`:

    fixture has "/x/"  -> both "/x" and "/x/" must be registered   (MISSING_ALIAS)
    fixture has "/x"   -> exactly "/x" must be registered          (MISSING_EXACT if only "/x/",
                                                                   EXTRA_ALIAS if both)

`MISSING_ALIAS` and `MISSING_EXACT` are defects (exit 1); `EXTRA_ALIAS` is a warning: we
serve a path upstream 404s, which is not a contract we owe anyone but is not parity either.

Two modes
---------
    # 1. audit a tree (default: this repo's crates/mc-http/src)
    python3 scripts/slash_alias_audit.py
    python3 scripts/slash_alias_audit.py --tree /path/to/slice-worktree          # whole tree
    python3 scripts/slash_alias_audit.py --tree /path/to/wt --base-ref e4ee275   # new keys only

    # 2. predict from an issue's declared route table, before any code exists
    python3 scripts/slash_alias_audit.py --declared scratch/m3-6-declared.tsv

It does not compile and does not replace `gates.sh`; it is a static, sub-second read.  It
runs as the second command of gate ⑦ (`scripts/gates.sh --only route-parity`).

Allowlist
---------
Keys we knowingly still owe (the M0 placeholders, plus the two follow-ups in docs/37 §15.3)
live in `docs/fixtures/slash-alias-allowlist.tsv` as `METHOD<TAB>path<TAB>owner<TAB>why`.
An allowlisted key is still *reported*, but is not a defect, so the gate can be green on a
tree that carries known debt.  A row whose key no longer fires is itself a defect (exit 1):
a stale row would mask the next regression of that same key — the same shrink-only rule as
gate ⑩'s `file_size_baseline.tsv`.  Delete the row in the PR that closes the key.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DEFAULT_UPSTREAM = os.path.join(ROOT, "docs", "fixtures", "upstream-routes.tsv")
DEFAULT_ALLOWLIST = os.path.join(ROOT, "docs", "fixtures", "slash-alias-allowlist.tsv")


def load_extractor():
    """Reuse the paren-matching `.route(...)` extractor from the W3b auditor (one parser)."""
    spec = importlib.util.spec_from_file_location(
        "w3b_premerge_audit", os.path.join(HERE, "w3b_premerge_audit.py")
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def fold(path: str) -> str:
    """Canonical key part: param placeholders unified, trailing slash *stripped*.

    Both halves matter: upstream writes `{id}`, axum writes `:id`, and the trailing
    slash is the very thing under audit, so it cannot survive into the comparison key.
    """
    return re.sub(r"[:{][^/}]*}?", ":param", path).rstrip("/") or "/"


def canon(path: str) -> str:
    """Param-unified path with the trailing slash preserved (for reporting)."""
    return re.sub(r"[:{][^/}]*}?", ":param", path)


def load_upstream(path: str) -> dict[tuple[str, str], str]:
    """folded (METHOD, path) -> the raw upstream literal (trailing slash preserved)."""
    out: dict[tuple[str, str], str] = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line.strip() or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            meth, raw = parts[0].strip(), parts[1].strip()
            out[(meth, fold(raw))] = raw
    return out


def load_allowlist(path: str) -> dict[str, tuple[str, str]]:
    """`METHOD<TAB>folded-path` -> (owner, why).  Rows keep the file self-documenting."""
    out: dict[str, tuple[str, str]] = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.split("#", 1)[0].strip()
            if not line:
                continue
            parts = [p.strip() for p in line.split("\t") if p.strip()]
            if len(parts) < 2:
                parts = line.split(None, 2)
            if len(parts) < 2 or not re.fullmatch(r"[A-Za-z]+", parts[0]):
                print(f"error: bad allowlist line: {line!r}", file=sys.stderr)
                raise SystemExit(2)
            if parts[0].upper() == "METHOD":  # header row of the checked-in table
                continue
            owner = parts[2] if len(parts) > 2 else "?"
            why = parts[3] if len(parts) > 3 else ""
            out[f"{parts[0].upper()}\t{fold(parts[1])}"] = (owner, why)
    return out


def audit(routes: set[tuple[str, str]], upstream: dict[tuple[str, str], str]):
    """Shape findings for a registration set (raw literals, as written in source)."""
    by_fold: dict[tuple[str, str], set[str]] = {}
    for meth, raw in routes:
        by_fold.setdefault((meth, fold(raw)), set()).add(raw)

    findings = []
    for key, raws in sorted(by_fold.items()):
        up_raw = upstream.get(key)
        if up_raw is None:
            continue  # not an upstream contract: ⑦'s local_only/known_gap owns it
        up_slash = up_raw.endswith("/")
        has_slash = any(p.endswith("/") for p in raws)
        has_plain = any(not p.endswith("/") for p in raws)
        if up_slash and not (has_slash and has_plain):
            findings.append(
                {
                    "kind": "MISSING_ALIAS",
                    "id": f"{key[0]}\t{key[1]}",
                    "key": f"{key[0]} {canon(key[1])}",
                    "upstream": up_raw,
                    "registered": sorted(raws),
                    "why": "chi Mount + child '/' serves both; axum needs both registered",
                }
            )
        elif not up_slash and has_slash and not has_plain:
            findings.append(
                {
                    "kind": "MISSING_EXACT",
                    "id": f"{key[0]}\t{key[1]}",
                    "key": f"{key[0]} {canon(key[1])}",
                    "upstream": up_raw,
                    "registered": sorted(raws),
                    "why": "upstream registers this pattern plain: only the un-slashed form is served",
                }
            )
        elif not up_slash and has_slash and has_plain:
            findings.append(
                {
                    "kind": "EXTRA_ALIAS",
                    "id": f"{key[0]}\t{key[1]}",
                    "key": f"{key[0]} {canon(key[1])}",
                    "upstream": up_raw,
                    "registered": sorted(raws),
                    "why": "upstream 404s the slashed form; ours serves it (parity, not correctness)",
                }
            )
    return findings


def predict(declared: set[tuple[str, str]], upstream: dict[tuple[str, str], str]):
    """What shapes the declared paths *must* be registered with, given upstream."""
    rows, unknown = [], []
    for meth, raw in sorted(declared):
        up_raw = upstream.get((meth, fold(raw)))
        if up_raw is None:
            unknown.append((meth, raw))
            continue
        dual = up_raw.endswith("/")
        forms = sorted({raw, raw.rstrip("/")} if dual else {raw.rstrip("/")})
        rows.append({"method": meth, "declared": raw, "upstream": up_raw, "dual": dual, "forms": forms})
    return rows, unknown


def render(name: str, routes, findings, declared_rows=None, unknown=None) -> list[str]:
    out = [f"== {name} =="]
    if declared_rows is not None:
        dual_keys = [r for r in declared_rows if r["dual"]]
        out.append(
            f"  declared {len(declared_rows)} upstream key(s); dual-form required: "
            f"{len(dual_keys)} | single-form: {len(declared_rows) - len(dual_keys)}"
        )
        for r in dual_keys:
            out.append(f"      DUAL   {r['method']:<6} {r['declared']:<48} (upstream {r['upstream']})")
            for form in r["forms"]:
                out.append(f"             must register: {form}")
        if unknown:
            out.append(f"  declared paths absent from the upstream fixture: {len(unknown)}")
            for meth, raw in unknown:
                out.append(f"      ?      {meth:<6} {raw}")
    else:
        out.append(f"  registered upstream-key literals: {len(routes)}")
    allowed = [f for f in findings if f.get("owner")]
    bad = [f for f in findings if f["kind"] != "EXTRA_ALIAS" and not f.get("owner")]
    for kind, label in (("MISSING_ALIAS", "missing alias"), ("MISSING_EXACT", "missing exact form"),
                        ("EXTRA_ALIAS", "extra alias (warning)")):
        rows = [f for f in findings if f["kind"] == kind]
        if not rows:
            continue
        out.append(f"  {kind} ({len(rows)}): {label}")
        for f in rows:
            tag = f"  [allowlisted: {f['owner'][0]}]" if f.get("owner") else ""
            out.append(f"      {f['key']:<52} registered={f['registered']} upstream={f['upstream']}{tag}")
            out.append(f"          {f['why']}")
    if not findings:
        out.append("  shapes OK: every registered upstream key matches the form upstream serves")
    else:
        note = f"; {len(allowed)} allowlisted (known debt, see docs/37 §15.3)" if allowed else ""
        out.append(f"  => {len(bad)} defect(s), {len(findings) - len(bad) - len(allowed)} warning(s){note}")
    return out


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tree", default=ROOT, help="worktree root to scan (default: this repo)")
    ap.add_argument("--base-ref", default=None,
                    help="only consider routes added relative to this git ref (uses the W3b auditor's diff)")
    ap.add_argument("--declared", default=None,
                    help="file of `METHOD<tab|space>path` lines to predict shapes for (no scanning)")
    ap.add_argument("--upstream", default=DEFAULT_UPSTREAM)
    ap.add_argument("--allowlist", default=None,
                    help=f"known-debt rows (default: {os.path.relpath(DEFAULT_ALLOWLIST, ROOT)} when present)")
    ap.add_argument("--no-allowlist", action="store_true", help="run strict: every finding is a defect")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--quiet", action="store_true", help="print nothing unless a defect is found")
    args = ap.parse_args(argv)

    allowlist_path = None if args.no_allowlist else (args.allowlist or DEFAULT_ALLOWLIST)
    allow: dict[str, tuple[str, str]] = {}
    if allowlist_path and os.path.exists(allowlist_path):
        allow = load_allowlist(allowlist_path)
    elif args.allowlist:
        print(f"error: allowlist not found: {args.allowlist}", file=sys.stderr)
        return 2

    upstream = load_upstream(args.upstream)
    report = {"upstream": args.upstream, "upstream_keys": len(upstream),
              "allowlist": os.path.relpath(allowlist_path, ROOT) if allow else None,
              "allowlist_rows": len(allow)}

    lines: list[str] = []
    defects = 0

    if args.declared:
        declared = set()
        with open(args.declared, encoding="utf-8") as fh:
            for line in fh:
                line = line.split("#", 1)[0].strip()
                if not line:
                    continue
                parts = line.split()
                if parts[0].upper() == "METHOD":  # header row of a checked-in table
                    continue
                if len(parts) < 2 or not re.fullmatch(r"[A-Za-z]+", parts[0]):
                    print(f"error: bad declared line: {line!r}", file=sys.stderr)
                    return 2
                declared.add((parts[0].upper(), parts[1]))
        rows, unknown = predict(declared, upstream)
        findings = audit(declared, upstream)
        for f in findings:
            f["owner"] = allow.get(f["id"])
        defects = len([f for f in findings if f["kind"] != "EXTRA_ALIAS" and not f.get("owner")])
        report.update({"declared_keys": len(declared), "predicted": rows, "unknown": unknown,
                       "findings": findings})
        lines += render(f"declared route table ({os.path.basename(args.declared)})", declared, findings,
                        declared_rows=rows, unknown=unknown)
    else:
        ext = load_extractor()
        routes = ext.routes_at(args.tree, None)
        if args.base_ref:
            routes = routes - ext.routes_at(args.tree, args.base_ref)
        findings = audit(routes, upstream)
        for f in findings:
            f["owner"] = allow.get(f["id"])
        stale = sorted(k for k in allow if k not in {f["id"] for f in findings})
        defects = len([f for f in findings if f["kind"] != "EXTRA_ALIAS" and not f.get("owner")])
        if stale:
            defects += len(stale)
            for k in stale:
                meth, path = k.split("\t", 1)
                lines.append(
                    f"  STALE allowlist row (the key now fires correctly) — delete it from "
                    f"{os.path.relpath(allowlist_path, ROOT)}: {meth} {canon(path)}"
                )
        report.update({"tree": args.tree, "base_ref": args.base_ref,
                       "registered_keys": len(routes), "stale_allowlist": stale,
                       "findings": findings})
        label = f"tree {args.tree}" + (f" (added vs {args.base_ref})" if args.base_ref else "")
        lines += render(label, routes, findings)

    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=1))
    elif not (args.quiet and defects == 0):
        print("\n".join(lines))

    if defects:
        print(f"FAIL: {defects} trailing-slash shape defect(s)", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

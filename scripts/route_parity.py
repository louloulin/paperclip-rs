#!/usr/bin/env python3
"""Route parity: HTTP routes this repo registers vs the routes upstream multica serves.

The board this answers
----------------------
"Which upstream routes are still missing here, and **who owns each gap**?"

* local half   — statically extracted from `crates/mc-http/src/**/*.rs`
  (axum `.route(path, method_router)` registrations).
* upstream half — `docs/fixtures/upstream-routes.tsv`, a checked-in snapshot of
  upstream `server/cmd/server/router.go`.  **No network access**: refresh the
  snapshot with `scripts/gen_upstream_routes.py` (recipe in docs/22).

Groups produced
---------------
implemented   upstream route registered locally — *including* placeholder handlers
known_gap     upstream route not registered here, fixture names an owner
unclaimed     upstream route not registered here and **no owner** → blind spot
regression    recorded in the checked-in baseline, gone from the source now
local_only    registered here, absent upstream (informational)

Exit code is 1 when `unclaimed` is non-empty, when a route recorded in
`docs/fixtures/route-parity-baseline.json` has disappeared, when duplicate
`(method, path)` pairs are found on either side, or when a registration cannot
be resolved statically — i.e. it fails exactly when the ledger or the router is
broken.

Why a baseline is needed for `regression`
-----------------------------------------
`unclaimed` answers "does every *remaining* upstream route have an owner?".  It
cannot answer "did a route we had already registered just disappear?": once an
implemented route is deleted, the fixture only says the route is owned by some
milestone, so it degrades to a `known_gap` and the run stays green — a silently
lost contract.  `docs/fixtures/route-parity-baseline.json` is the memory of what
this tree registered at a known-good revision (`--write-baseline` refreshes it),
so removing a route becomes a hard failure with the route named.  Only
*shrinking* the set fails: adding routes is what this project does all day.

Static-extraction limits (read before trusting a "gap")
-------------------------------------------------------
This script reads source; it never starts the server.

1. Only `.route(<string literal>, <method-router chain>)` is resolved.  A path
   built with `concat!`/a constant/variable, or a method router that is not a
   visible `get(...)/post(...)` chain, cannot be resolved: the call is reported
   under `unsupported` and the run fails rather than guessing.
2. Route-adding constructs that are *not* `.route(path, MethodRouter)` —
   `.nest(`, `.nest_service(`, `.route_service(`, `.fallback_service(` — register
   paths this script cannot see.  They are detected and reported as
   `limitations` (a warning, not a silent omission).  None exist on this base.
3. Routes added by a helper that takes a `Router` are only visible if the call
   site of that helper is in the scanned tree; `.merge(...)` of whole routers is
   flat and therefore fine, a prefixing helper is not.  Helpers *inside* the
   scanned tree are followed because their `.route(...)` calls are scanned too.
4. `#[cfg(test)]` modules are excluded — a test router is not a served route.
5. Parameters are compared by position, name-blind: `:id` == `{id}` ==
   `:workspaceId`.  Trailing slashes are stripped once per side, because chi
   registers both `/api/inbox` and `/api/inbox/`.
6. A *placeholder* handler — a route registered ahead of its implementation —
   counts as registered but is flagged per route in `--json` and summarised in
   the human output: "registered" must not be read as "implemented".  The
   criterion is the handler *name* (`PLACEHOLDER_HANDLER` below).  The two
   families it matches here, the historical readings taken under the narrower
   one-word criterion, and the one known key a name-based test cannot classify
   are all in `docs/22-ROUTE-PARITY.md` §2.3 and §3.6.
7. `any(...)` (all methods) is treated as satisfying any method on that path.

Usage
-----
    python3 scripts/route_parity.py                  # board + exit code
    python3 scripts/route_parity.py --list-gaps      # every known_gap by owner
    python3 scripts/route_parity.py --json           # machine-readable
    python3 scripts/route_parity.py --quiet          # only the counts
    python3 scripts/route_parity.py --write-baseline # accept the current tree,
                                                     # after adding/retiring routes
    python3 scripts/route_parity.py --no-baseline    # board only, no drift gate
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import re
import sys
from collections import Counter, OrderedDict
from dataclasses import dataclass, field

DEFAULT_ROUTES_DIR = os.path.join("crates", "mc-http", "src")
DEFAULT_UPSTREAM = os.path.join("docs", "fixtures", "upstream-routes.tsv")
DEFAULT_BASELINE = os.path.join("docs", "fixtures", "route-parity-baseline.json")

METHOD_ROUTERS = (
    "get",
    "post",
    "put",
    "patch",
    "delete",
    "head",
    "options",
    "trace",
    "connect",
    "any",
)

# NOT `.route(`-based: reported as limitations instead of silently missed.
OTHER_ROUTE_APIS = (".nest(", ".nest_service(", ".route_service(", ".fallback_service(")

# Placeholder handlers are recognised by *name*: `health::placeholder` (M0/M1
# leftovers) and `not_implemented` (`routes/issues/mod.rs`, the shared upstream-
# parity stub; slices replace it with real handlers as they land).  Before
# LUM-1580 only the literal word `placeholder` matched, so every `not_implemented`
# registration was counted as a *real* implementation — see
# docs/22-ROUTE-PARITY.md §2.3 (criterion) and §3.6 (readings).
PLACEHOLDER_HANDLER = re.compile(r"\b(?:placeholder|not_implemented)\b")

UNCLAIMED_OWNERS = {"", "-", "?", "tbd"}


# --------------------------------------------------------------------------- #
# Rust source scanning
# --------------------------------------------------------------------------- #


def mask_rust(src: str) -> tuple[str, dict[int, str]]:
    """Blank out comments and string *bodies*, keeping every byte offset stable.

    Returns `(masked, literals)`; `literals[offset_of_opening_quote]` is the
    decoded value.  Quote delimiters are kept in `masked` (bodies are blanked)
    so a path literal can still be recognised without re-parsing Rust by hand.
    Char literals and lifetimes are both `'`-prefixed: a lifetime has no closing
    quote, so only `'x'` / `'\\n'` shaped spans are treated as char literals.
    """
    out = list(src)
    literals: dict[int, str] = {}
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            j = src.find("\n", i)
            j = n if j < 0 else j
            for k in range(i, j):
                out[k] = " "
            i = j
            continue
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            for k in range(i, min(j, n)):
                out[k] = " "
            i = j
            continue
        if c == "r" and i + 1 < n and src[i + 1] in "#\"":
            hashes = 0
            j = i + 1
            while j < n and src[j] == "#":
                hashes, j = hashes + 1, j + 1
            if j < n and src[j] == '"':
                close = '"' + "#" * hashes
                end = src.find(close, j + 1)
                end = n if end < 0 else end + len(close)
                literals[j] = src[j + 1 : end - len(close)]
                for k in range(j + 1, end - len(close)):
                    out[k] = " "
                i = end
                continue
        if c == '"':
            j = i + 1
            while j < n:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == '"':
                    break
                j += 1
            literals[i] = _unescape(src[i + 1 : j])
            for k in range(i + 1, min(j, n)):
                out[k] = " "
            i = min(j + 1, n)
            continue
        if c == "'":
            m = re.compile(r"'(?:\\.|[^\\'])'", re.S).match(src, i)
            if m:
                for k in range(i, m.end()):
                    out[k] = " "
                i = m.end()
                continue
        i += 1
    return "".join(out), literals


def _unescape(raw: str) -> str:
    if "\\" not in raw:
        return raw
    out: list[str] = []
    i = 0
    simple = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", '"': '"', "'": "'", "\\": "\\"}
    while i < len(raw):
        c = raw[i]
        if c == "\\" and i + 1 < len(raw):
            nxt = raw[i + 1]
            if nxt in simple:
                out.append(simple[nxt])
                i += 2
                continue
            if nxt == "x" and i + 3 < len(raw):
                out.append(chr(int(raw[i + 2 : i + 4], 16)))
                i += 4
                continue
            if nxt == "u" and raw[i + 2 : i + 3] == "{":
                close = raw.find("}", i + 3)
                if close > 0:
                    out.append(chr(int(raw[i + 3 : close], 16)))
                    i = close + 1
                    continue
        out.append(c)
        i += 1
    return "".join(out)


def matching_paren(masked: str, open_idx: int) -> int:
    """Index just past the `)` closing the `(` at `open_idx` (or len on EOF)."""
    depth = 0
    i = open_idx
    while i < len(masked):
        c = masked[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return len(masked)


def top_level_args(masked: str, start: int, end: int) -> list[tuple[int, int]]:
    """Split the argument span `[start, end)` of a call into top-level arg ranges.

    A trailing comma (`.route(path, chain,)`) does not produce an empty argument.
    """
    args: list[tuple[int, int]] = []
    depth, arg_start = 0, start
    for i in range(start, end):
        c = masked[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == "," and depth == 0:
            args.append((arg_start, i))
            arg_start = i + 1
    args.append((arg_start, end))
    while args and not masked[args[-1][0] : args[-1][1]].strip():
        args.pop()
    return args


def str_literal_at(masked: str, literals: dict[int, str], start: int, end: int) -> str | None:
    """Value of a lone string literal spanning `[start, end)`, else `None`."""
    j = start
    while j < end and masked[j].isspace():
        j += 1
    k = j
    if k < end and masked[k] == "r":  # raw string: r###"..."###
        while k < end and masked[k] in "#r":
            k += 1
    if k < end and masked[k] == '"' and k in literals:
        close = masked.find('"', k + 1, end)
        if close >= 0 and not masked[close + 1 : end].strip():
            return literals[k]
    return None


def test_module_ranges(masked: str) -> list[tuple[int, int]]:
    """Spans covered by `#[cfg(test)] mod ... { ... }` (test routers are not routes)."""
    spans: list[tuple[int, int]] = []
    for m in re.finditer(r"#\[cfg\(\s*test\s*\)\]", masked):
        b = masked.find("{", m.end())
        semi = masked.find(";", m.end())
        if b < 0 or (semi >= 0 and semi < b):
            continue  # `#[cfg(test)] use ...;` — nothing to skip
        spans.append((m.start(), matching_paren(masked, b)))
    return spans


def in_spans(idx: int, spans: list[tuple[int, int]]) -> bool:
    return any(a <= idx < b for a, b in spans)


@dataclass
class LocalRoute:
    method: str
    path: str
    file: str
    line: int
    placeholder: bool = False


@dataclass
class Extraction:
    routes: list[LocalRoute] = field(default_factory=list)
    unsupported: list[str] = field(default_factory=list)
    limitations: list[str] = field(default_factory=list)
    files_scanned: int = 0


def rel_or_abs(path: str, root: str) -> str:
    """`relpath` when `path` is inside `root`, absolute otherwise (readable output)."""
    try:
        rel = os.path.relpath(path, root)
    except ValueError:
        return path
    return path if rel.startswith("..") else rel


def extract_local(root: str, rel_to: str) -> Extraction:
    """Every `.route(...)` registration under `root` (recursive `*.rs`)."""
    files = sorted(glob.glob(os.path.join(root, "**", "*.rs"), recursive=True))
    ex = Extraction(files_scanned=len(files))
    seen_apis: Counter[str] = Counter()
    for path_ in files:
        with open(path_, encoding="utf-8") as fh:
            src = fh.read()
        masked, literals = mask_rust(src)
        tests = test_module_ranges(masked)
        rel = rel_or_abs(path_, rel_to)

        for api in OTHER_ROUTE_APIS:
            seen_apis[api] += masked.count(api)

        for m in re.finditer(r"\.route\s*\(", masked):
            if in_spans(m.start(), tests):
                continue
            line = src.count("\n", 0, m.start()) + 1
            open_idx = masked.index("(", m.end() - 1)
            close_idx = matching_paren(masked, open_idx)
            args = top_level_args(masked, open_idx + 1, close_idx - 1)
            if len(args) != 2:
                ex.unsupported.append(
                    f"{rel}:{line}: .route(...) with {len(args)} args "
                    f"(expected path + method router)"
                )
                continue
            route_path = str_literal_at(masked, literals, *args[0])
            if route_path is None:
                snippet = masked[args[0][0] : args[0][1]].strip().replace("\n", " ")
                ex.unsupported.append(f"{rel}:{line}: non-literal path `{snippet}`")
                continue
            chain = masked[args[1][0] : args[1][1]]
            found = False
            for mm in re.finditer(r"\b(" + "|".join(METHOD_ROUTERS) + r")\s*\(", chain):
                found = True
                call_open = mm.end() - 1
                call_close = matching_paren(chain, call_open)
                handler = chain[call_open + 1 : call_close - 1]
                ex.routes.append(
                    LocalRoute(
                        method="ANY" if mm.group(1) == "any" else mm.group(1).upper(),
                        path=route_path,
                        file=rel,
                        line=line,
                        placeholder=bool(PLACEHOLDER_HANDLER.search(handler)),
                    )
                )
            if not found:
                expr = chain.strip().replace("\n", " ")[:80]
                ex.unsupported.append(
                    f"{rel}:{line}: no method-router call in `.route(\"{route_path}\", {expr})`"
                )

    for api, count in seen_apis.items():
        if count:
            ex.limitations.append(
                f"{count}x `{api}` — registers paths this static scan cannot resolve"
            )
    return ex


# --------------------------------------------------------------------------- #
# upstream fixture + comparison
# --------------------------------------------------------------------------- #


@dataclass
class UpstreamRoute:
    method: str
    path: str
    owner: str
    router_line: int
    fixture_line: int


def read_fixture(path: str) -> tuple[list[UpstreamRoute], str]:
    routes: list[UpstreamRoute] = []
    commit = ""
    with open(path, encoding="utf-8") as fh:
        for lineno, raw in enumerate(fh, 1):
            line = raw.rstrip("\n")
            if not line.strip():
                continue
            if line.lstrip().startswith("#"):
                if line.startswith("# upstream commit:"):
                    commit = line.split(":", 1)[1].strip()
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                raise ValueError(f"{path}:{lineno}: expected `method<TAB>path<TAB>owner`")
            comment = parts[3] if len(parts) > 3 else ""
            m = re.search(r"router\.go:(\d+)", comment)
            routes.append(
                UpstreamRoute(
                    method=parts[0].strip().upper(),
                    path=parts[1].strip(),
                    owner=parts[2].strip() if len(parts) > 2 else "",
                    router_line=int(m.group(1)) if m else 0,
                    fixture_line=lineno,
                )
            )
    return routes, commit


def normalize(path: str, strip_trailing_slash: bool = True) -> str:
    """Name-blind comparison key for a path.

    `strip_trailing_slash=True` folds chi's `/x` + `/x/` pair into one key (used
    for the upstream↔local comparison).  Pass `False` to get the *registration*
    key, where `/x` and `/x/` stay distinct because that is what axum/matchit
    registers — conflating them there would invent duplicates that do not exist.
    """
    if not path.startswith("/"):
        path = "/" + path
    if strip_trailing_slash and len(path) > 1 and path.endswith("/"):
        path = path[:-1]
    segments = []
    for seg in path.split("/"):
        if seg.startswith(":") or (seg.startswith("{") and seg.endswith("}")):
            segments.append(":param")
        elif seg == "*" or seg.startswith("{*"):
            segments.append(":wildcard")
        else:
            segments.append(seg)
    out = "/".join(segments)
    return out or "/"


def duplicates(keys: list[tuple[str, str]]) -> OrderedDict[tuple[str, str], list[int]]:
    """Keys registered more than once — such a pair panics axum's `.merge`."""
    seen: dict[tuple[str, str], list[int]] = {}
    for idx, key in enumerate(keys):
        seen.setdefault(key, []).append(idx)
    return OrderedDict((k, v) for k, v in sorted(seen.items()) if len(v) > 1)


def slash_aliases(keys: list[tuple[str, str]]) -> list[str]:
    """`/x` and `/x/` registered side by side: legal in axum, folded in comparison."""
    groups: dict[tuple[str, str], set[str]] = {}
    for method, raw in keys:
        groups.setdefault((method, normalize(raw)), set()).add(raw)
    return [
        f"{k[0]} " + " + ".join(sorted(v)) for k, v in sorted(groups.items()) if len(v) > 1
    ]


def registration_keys(routes: list[LocalRoute]) -> list[str]:
    """`METHOD path` strings, trailing slash kept: axum registers the two forms apart."""
    return sorted(f"{r.method} {normalize(r.path, strip_trailing_slash=False)}" for r in routes)


def read_baseline(path: str) -> list[str]:
    """Route keys recorded by a previous `--write-baseline`."""
    with open(path, encoding="utf-8") as fh:
        doc = json.load(fh)
    routes = doc["routes"]
    if not isinstance(routes, list) or not all(isinstance(x, str) for x in routes):
        raise ValueError(f"{path}: `routes` must be a list of \"METHOD path\" strings")
    return routes


def write_baseline(path: str, keys: list[str], routes_dir: str, rel_to: str) -> None:
    doc = OrderedDict(
        (
            ("#", "generated by `scripts/route_parity.py --write-baseline`; do not edit by hand"),
            ("why", "routes registered at the last accepted revision — losing one fails the run"),
            ("routes_dir", rel_or_abs(routes_dir, rel_to)),
            ("routes", keys),
        )
    )
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(doc, fh, ensure_ascii=False, indent=2)
        fh.write("\n")


def build_report(
    routes_dir: str, upstream_path: str, rel_to: str, baseline_path: str | None = None
) -> dict:
    ex = extract_local(routes_dir, rel_to)
    upstream, commit = read_fixture(upstream_path)

    local_keys = [(r.method, normalize(r.path)) for r in ex.routes]
    local_reg = [(r.method, normalize(r.path, strip_trailing_slash=False)) for r in ex.routes]
    upstream_keys = [(r.method, normalize(r.path)) for r in upstream]
    upstream_reg = [(r.method, normalize(r.path, strip_trailing_slash=False)) for r in upstream]
    local_any_paths = {k[1] for k in local_keys if k[0] == "ANY"}
    local_set = set(local_keys)

    implemented, known_gap, unclaimed = [], [], []
    for route, key in zip(upstream, upstream_keys):
        if key in local_set or key[1] in local_any_paths:
            implemented.append(route)
        elif route.owner.lower() in UNCLAIMED_OWNERS:
            unclaimed.append(route)
        else:
            known_gap.append(route)

    upstream_norm = set(upstream_keys)
    local_only = [
        r for r, key in zip(ex.routes, local_keys) if key not in upstream_norm
    ]

    local_dups = duplicates(local_reg)
    upstream_dups = duplicates(upstream_reg)
    owner_counts = Counter(r.owner or "<unclaimed>" for r in known_gap + unclaimed)
    placeholder_keys = {(r.method, normalize(r.path)) for r in ex.routes if r.placeholder}
    implemented_placeholders = [r for r in implemented if (r.method, normalize(r.path)) in placeholder_keys]

    baseline = read_baseline(baseline_path) if baseline_path else []
    baseline_note = ""
    if baseline_path and not os.path.exists(baseline_path):
        baseline_note = f"no baseline at {rel_or_abs(baseline_path, rel_to)} — drift gate off"
    live = set(registration_keys(ex.routes))
    regressions = sorted(set(baseline) - live)

    return {
        "ok": not unclaimed
        and not regressions
        and not local_dups
        and not upstream_dups
        and not ex.unsupported,
        "sources": {
            "routes_dir": rel_or_abs(routes_dir, rel_to),
            "files_scanned": ex.files_scanned,
            "upstream": rel_or_abs(upstream_path, rel_to),
            "upstream_commit": commit,
            "baseline": rel_or_abs(baseline_path, rel_to) if baseline_path else "(none)",
            "baseline_note": baseline_note,
            "baseline_routes": len(baseline),
        },
        "counts": {
            "upstream": len(upstream),
            "local": len(ex.routes),
            "implemented": len(implemented),
            "implemented_real": len(implemented) - len(implemented_placeholders),
            "implemented_placeholder": len(implemented_placeholders),
            "known_gap": len(known_gap),
            "unclaimed": len(unclaimed),
            "regressions": len(regressions),
            "local_only": len(local_only),
            "local_only_placeholder": sum(1 for r in local_only if r.placeholder),
        },
        "owners": dict(sorted(owner_counts.items(), key=lambda kv: (-kv[1], kv[0]))),
        "duplicates": {
            "local": [f"{k[0]} {k[1]}" for k in local_dups],
            "upstream": [f"{k[0]} {k[1]}" for k in upstream_dups],
        },
        "slash_aliases": slash_aliases(local_reg),
        "unsupported": ex.unsupported,
        "limitations": ex.limitations,
        "implemented": [
            {
                "method": r.method,
                "path": r.path,
                "owner": r.owner,
                "router_line": r.router_line,
                "placeholder": (r.method, normalize(r.path)) in placeholder_keys,
            }
            for r in implemented
        ],
        "known_gap": [
            {
                "method": r.method,
                "path": r.path,
                "owner": r.owner,
                "router_line": r.router_line,
            }
            for r in known_gap
        ],
        "unclaimed": [
            {"method": r.method, "path": r.path, "router_line": r.router_line}
            for r in unclaimed
        ],
        "regressions": [
            {"method": key.split(" ", 1)[0], "path": key.split(" ", 1)[1]}
            for key in regressions
        ],
        "local_only": [
            {
                "method": r.method,
                "path": r.path,
                "file": r.file,
                "line": r.line,
                "placeholder": r.placeholder,
            }
            for r in local_only
        ],
    }


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #


def render_human(rep: dict, list_gaps: bool, quiet: bool) -> str:
    c = rep["counts"]
    out = []
    out.append(
        f"upstream {c['upstream']} (commit {rep['sources']['upstream_commit'][:12] or '?'})"
        f" | local {c['local']} registered"
        f" | baseline {rep['sources']['baseline_routes']}"
    )
    out.append(
        f"  implemented {c['implemented_real']:>4} real"
        f" + {c['implemented_placeholder']:>3} placeholder"
        f" = {c['implemented']:>4} / {c['upstream']}"
        f"   known_gap {c['known_gap']:>4}"
        f"   unclaimed {c['unclaimed']:>4}"
        f"   regression {c['regressions']:>3}"
        f"   local_only {c['local_only']:>4}"
    )
    if rep["sources"]["baseline_note"]:
        out.append(f"  note: {rep['sources']['baseline_note']}")
    if not quiet and rep["owners"]:
        out.append("  gaps by owner: " + "  ".join(f"{k}={v}" for k, v in rep["owners"].items()))
    for where, dups in rep["duplicates"].items():
        if dups:
            out.append(f"  !! duplicate ({where}) route keys: {', '.join(dups)}")
    if rep["slash_aliases"]:
        out.append(
            "  note: same route with and without trailing slash (legal; folded in comparison): "
            + ", ".join(rep["slash_aliases"])
        )
    if rep["unsupported"]:
        out.append("  !! unresolved registrations (fix or extend the extractor):")
        out += [f"     {s}" for s in rep["unsupported"]]
    if rep["limitations"]:
        out.append("  note: routes may exist that this static scan cannot see:")
        out += [f"      {s}" for s in rep["limitations"]]
    if rep["unclaimed"]:
        out.append(f"  !! {len(rep['unclaimed'])} UNCLAIMED upstream routes (no owner):")
        for r in rep["unclaimed"]:
            out.append(f"     {r['method']:6} {r['path']}   # router.go:{r['router_line']}")
    if rep["regressions"]:
        out.append(
            f"  !! {len(rep['regressions'])} route(s) present in the baseline are gone "
            "from the source (a lost contract, not a gap):"
        )
        for r in rep["regressions"]:
            out.append(f"     {r['method']:6} {r['path']}")
        out.append(
            "     fix by restoring the route; if the removal is deliberate, accept it with "
            "`python3 scripts/route_parity.py --write-baseline`"
        )
    if not quiet and rep["local_only"]:
        out.append("  local-only (not upstream; keep or retire deliberately):")
        for r in rep["local_only"]:
            tag = " [placeholder]" if r["placeholder"] else ""
            out.append(f"     {r['method']:6} {r['path']}{tag}   # {r['file']}:{r['line']}")
    if list_gaps:
        out.append("  known gaps (owner  method  path):")
        by_owner: dict[str, list[dict]] = {}
        for r in rep["known_gap"]:
            by_owner.setdefault(r["owner"], []).append(r)
        for owner in sorted(by_owner):
            out.append(f"    [{owner}] {len(by_owner[owner])}")
            for r in by_owner[owner]:
                out.append(f"      {r['method']:6} {r['path']}")
    out.append("OK: every upstream route is either implemented or owned" if rep["ok"] else "FAIL")
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    ap = argparse.ArgumentParser(
        description="Upstream route parity + gap ownership board",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument(
        "--routes-dir",
        default=os.path.join(root, DEFAULT_ROUTES_DIR),
        help="tree scanned for axum .route(...) registrations (recursive)",
    )
    ap.add_argument(
        "--upstream",
        default=os.path.join(root, DEFAULT_UPSTREAM),
        help="upstream route fixture (no network access)",
    )
    ap.add_argument("--json", action="store_true", help="machine-readable report on stdout")
    ap.add_argument("--list-gaps", action="store_true", help="print every known gap, grouped by owner")
    ap.add_argument("--quiet", action="store_true", help="counts only")
    ap.add_argument(
        "--baseline",
        default=os.path.join(root, DEFAULT_BASELINE),
        help="route snapshot used for the lost-route (regression) gate",
    )
    ap.add_argument(
        "--no-baseline",
        action="store_true",
        help="skip the regression gate (board only)",
    )
    ap.add_argument(
        "--write-baseline",
        action="store_true",
        help="record the current routes as the accepted baseline, then report",
    )
    args = ap.parse_args(argv)

    baseline = None if args.no_baseline else args.baseline
    if args.write_baseline and not baseline:
        print("error: --write-baseline needs a baseline path (--baseline), not --no-baseline", file=sys.stderr)
        return 2

    try:
        if args.write_baseline:
            extraction = extract_local(args.routes_dir, root)
            if extraction.unsupported:
                raise ValueError(
                    "refusing to write a baseline while registrations are unresolved: "
                    + "; ".join(extraction.unsupported)
                )
            os.makedirs(os.path.dirname(baseline), exist_ok=True)
            write_baseline(baseline, registration_keys(extraction.routes), args.routes_dir, root)
        rep = build_report(args.routes_dir, args.upstream, root, baseline)
    except (OSError, ValueError, KeyError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    if args.json:
        print(json.dumps(rep, ensure_ascii=False, indent=2))
    else:
        print(render_human(rep, args.list_gaps, args.quiet))
    return 0 if rep["ok"] else 1


if __name__ == "__main__":
    try:
        _code = main()
        sys.stdout.flush()  # flush here: at shutdown it is too late to catch this
    except BrokenPipeError:  # `| head` — the reader went away, that is not a failure
        devnull = os.open(os.devnull, os.O_WRONLY)
        os.dup2(devnull, sys.stdout.fileno())
        _code = 0
    raise SystemExit(_code)

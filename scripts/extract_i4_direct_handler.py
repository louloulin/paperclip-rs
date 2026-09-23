#!/usr/bin/env python3
"""Rule I4 — direct-handler-call fixtures, and the handler→route table behind them.

Upstream's handler tests mostly drive a handler *directly*, bypassing its router::

    w := httptest.NewRecorder()
    testHandler.GetChatChannelHistory(w, taskActorReq("/api/chat/history", taskID))
    if w.Code != http.StatusOK { t.Fatalf("expected 200, got %d", w.Code) }

That call is a request-construction site — an oracle for `<method> <path> → w.Code`
— but rules I1–I3 only recognise ``testutil.Call`` / ``authRequest`` / ``http.Get``,
so every one of them was invisible to ``contracts/golden/`` (``docs/42`` §6.3 counts
223 such sites in the M4 domain alone).  This module is rule I4.

It lives beside ``extract_upstream_fixtures.py`` rather than inside it because that
file is pinned by the 800-line gate (``scripts/file_size_baseline.tsv``); the host
module hands over its own namespace (``i4.run(ex, globals())``) so neither file
imports the other.

Rule I4 (bounded), following ``docs/42`` §6.3:

1. a site is ``<recv>.<Handler>(<recorder>, <request>, ...)`` where ``<Handler>`` is
   a handler that ``server/cmd/server/router.go`` actually registers
   (:func:`handler_index`);
2. the request is traced exactly like I1 (``httptest.NewRequest`` / ``newRequest`` /
   ``newRequestAs`` · ``withURLParam`` · local helpers), *including* through a
   wrapper that hands the request back (``chatPendingCtxAs``, ``squadScopeReq``) —
   see :func:`transparent_return` and :func:`assigned_requests`;
3. ``withURLParam(req, k, v)`` becomes a ``{k}`` placeholder (the host's
   ``canonicalise``);
4. the expected status is the ``<recorder>.Code`` comparison that follows the call;
5. the literal path is checked against ``docs/fixtures/upstream-routes.tsv`` after
   the ``-``→``/`` compatibility fold; an unregistered path is recorded as
   ``path_not_registered``, never dropped;
6. ``docs/fixtures/handler-routes.tsv`` (handler → route key) is generated from that
   same table plus the ``router.go`` line each row cites, by
   ``scripts/upstream_handler_index.py`` — see that module's CLI:
   ``python3 scripts/upstream_handler_index.py --upstream <checkout> --write-handler-routes``.

One deliberate difference from I1–I3: I4 resolves requests through helper wrappers
(``withChatTestWorkspaceCtx`` …), so a site can still carry a symbol the runner
cannot bind (``$taskID``, ``$squadID`` …).  ``mc-conformance`` refuses to invent
values (``Bindings::resolve`` → ``Err``) rather than mis-state a request, and its
protocol test requires a *stateless* verdict for every anonymous fixture, so such a
site is **skipped** with ``value_unresolved`` naming the symbol — the record keeps
the reason instead of the fixture.  The same applies to a **member** actor whose
identity is a symbol the runner cannot fabricate (``newRequestAs(memberID, …)``):
the runner would replay it as its own seeded owner, asserting a *different role*
than upstream did.
"""

from __future__ import annotations

import re
from typing import Any

from upstream_handler_index import handler_index

#: Candidate/report kind of a direct-handler site, and the reasons I4 adds to the
#: host's ``SKIP_REASONS`` vocabulary.  ``internal_error`` was already emitted by the
#: host's own guard but missing from that tuple.
KIND = "direct_handler"
ADDED_SKIP_REASONS = (
    "recorder_var_unresolved",
    "path_not_registered",
    "status_not_equality",
    "internal_error",
)

#: ``<recv>.<Handler>(<recorder>, <request>, …)`` — only ``<Handler>`` names the route
#: table registers reach the index, and only an upper-case one (a field access like
#: ``testHandler.Queries.X(...)`` is not a handler call).
SITE = re.compile(r"(?<![\w.])(?P<recv>[A-Za-z_][A-Za-z0-9_]*)\.(?P<name>[A-Z][A-Za-z0-9_]*)\s*\(")
RECORDER = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
CODE_CMP = re.compile(r"\b%s\s*\.\s*Code\s*(?P<op>!=|==|>=|<=)\s*(?P<rhs>[^;{}\n]+)")
LEGACY_SEGMENT = re.compile(r"/api/([a-z0-9]+)-([a-z0-9]+)")

# Filled by :func:`run` so the functions below can borrow the host module's classes,
# regexes and helpers without importing it.
_HOST: dict[str, Any] = {}
_INDEX: dict[str, list[dict[str, Any]]] = {}
_ROUTES: dict[str, list[dict[str, Any]]] = {}


# --------------------------------------------------------------------------- #
# site detection
# --------------------------------------------------------------------------- #


def trim_span(masked: str, span: tuple[int, int]) -> tuple[int, int]:
    """Shrink a span past surrounding whitespace.

    ``split_args`` keeps the whitespace around an argument, and the host's request
    resolution reads a span as ``text(span).strip()`` — so a span starting on a space
    would make it look for the argument's ``(`` one character early.  Every argument
    I4 handles is trimmed here before it is used.
    """
    a, b = span
    while a < b and masked[a].isspace():
        a += 1
    while b > a and masked[b - 1].isspace():
        b -= 1
    return (a, b)


def find_sites(
    host: dict[str, Any], masked: str, index: dict[str, list[dict[str, Any]]]
) -> list[tuple[int, str, list[tuple[int, int]], int]]:
    """`(pos, handler, args, call_end)` for every ``<recv>.<Handler>(recorder, req…)``.

    The handler-name filter is what keeps ``testHandler.Queries.X(...)`` (a field
    access) and ``testHandler.Bus.Y(...)`` out; a name that ``router.go`` does not
    register is not a handler call.
    """
    out: list[tuple[int, str, list[tuple[int, int]], int]] = []
    for m in SITE.finditer(masked):
        if m.group("name") not in index:
            continue
        open_idx = m.end() - 1
        close = host["matching"](masked, open_idx)
        args = [trim_span(masked, a) for a in host["split_args"](masked, open_idx + 1, close - 1)]
        if len(args) < 2:
            continue
        out.append((m.start(), m.group("name"), args, close + 1))
    return out


def _codes(
    host: dict[str, Any], region: str, recorder: str
) -> tuple[list[int], list[int], list[int]]:
    """``(<!= codes>, <== codes>, <bound codes>)`` for ``<recorder>.Code`` in ``region``.

    Only equality is an assertion.  ``if w.Code >= 300`` bounds the status without
    pinning it, so reading 300 out of it would be a guess.
    """
    ne: list[int] = []
    eq: list[int] = []
    bounds: list[int] = []
    pattern = CODE_CMP.pattern % re.escape(recorder)
    for m in re.finditer(pattern, region):
        codes = host["statuses_in"](m.group("rhs"))
        op = m.group("op")
        if op == "!=":
            ne.extend(codes)
        elif op == "==":
            eq.extend(codes)
        else:
            bounds.extend(codes)
    return ne, eq, bounds


# --------------------------------------------------------------------------- #
# request tracing through a wrapper helper
# --------------------------------------------------------------------------- #


def _arm(masked: str, guard_end: int, pos: int) -> bool:
    """Whether ``pos`` sits in the ``else`` arm of an ``if`` whose ``{`` ends at guard_end."""
    depth = 0
    for ch in masked[guard_end:pos]:
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth < 0:
                return True
    return False


def assigned_requests(
    host: dict[str, Any], interp: Any, file: str, body: tuple[int, int], name: str, ctx: Any
) -> list[tuple[int, Any]]:
    """Requests assigned to ``name`` anywhere in ``body``, in source order.

    ``squadScopeReq`` builds its request in one arm of an ``if`` — a shape the
    statement walk cannot see, because the whole ``if/else`` is a single statement.
    The arm is chosen by the guard when the guard compares a parameter that is
    bound to a literal at the call site (``userID == ""``), and otherwise the
    assignment list is returned unresolved so the caller can refuse to guess.
    """
    masked = interp.sources[file][1]
    region = masked[body[0] : body[1]]
    cands: list[tuple[int, int, int]] = []  # (assign_pos, rhs_start, rhs_end)
    pattern = r"(?<![.\w=!<>])%s\s*=\s*(?!=)" % re.escape(name)
    for m in re.finditer(pattern, region):
        rhs_start = body[0] + m.end()
        rhs_end = host["asi_end"](masked, rhs_start)
        if rhs_end <= rhs_start:
            continue
        cands.append((body[0] + m.start(), rhs_start, rhs_end))
    if not cands:
        return []
    guards = list(re.finditer(r"\bif\s+([A-Za-z_][A-Za-z0-9_]*)\s*(==|!=)\s*\"\"\s*\{", region))
    resolved: list[tuple[int, Any]] = []
    for assign_pos, rhs_start, rhs_end in cands:
        guard = None
        for g in guards:
            if body[0] + g.end() <= assign_pos:
                guard = g
            else:
                break
        want: Optional[bool] = None
        if guard is not None:
            val = interp.resolve_scalar(
                file, (body[0] + guard.start(1), body[0] + guard.end(1)), ctx, 1
            )
            if val is not None and val.is_literal and isinstance(val.value, str):
                is_empty = val.value == ""
                want = (is_empty if guard.group(2) == "==" else not is_empty)
        if want is not None and _arm(masked, body[0] + guard.end(), assign_pos) == (not want):
            # The assignment is in the arm the guard does not select.
            continue
        st = interp.resolve_request(file, (rhs_start, rhs_end), ctx, 1)
        if st is not None:
            resolved.append((assign_pos, st))
        if want is None:
            break
    return resolved


def transparent_return(
    host: dict[str, Any], interp: Any, file: str, body: tuple[int, int], final: Any, depth: int
) -> Optional[Any]:
    """The request a helper hands back — ``return req`` / ``return req.WithContext(…)``.

    Called by the host's ``Interpreter.interpret`` once its plain ``return <ident>``
    scan comes up empty, so I1–I3 keep their exact current answers and only the
    cases that used to resolve to nothing change.
    """
    masked = interp.sources[file][1]
    region = masked[body[0] : body[1]]
    for m in reversed(list(re.finditer(r"(?m)^[ \t]*return[ \t]+([^;\n]*)$", region))):
        head = re.match(r"([A-Za-z_][A-Za-z0-9_]*)", m.group(1).strip())
        if head is None:
            continue
        name = head.group(1)
        got = interp.resolve_request(file, (body[0] + m.start(1), body[0] + m.start(1) + len(name)), final, depth + 1)
        if got is not None:
            return got
        cands = assigned_requests(host, interp, file, body, name, final)
        if cands:
            return cands[0][1].clone()
    return None


# --------------------------------------------------------------------------- #
# path normalisation against the route table
# --------------------------------------------------------------------------- #


def _segments(path: str) -> list[str]:
    return [s for s in path.strip("/").split("/") if s]


def _param(seg: str) -> bool:
    return seg.startswith("{") and seg.endswith("}")


def _structural(row_path: str, path: str) -> Optional[dict[str, str]]:
    """Match a path against a registered route, `{...}` in the route being any one segment.

    A concrete segment is an *instance* of a route parameter — upstream's tests build
    `/api/agents/<uuid>` for the route `/api/agents/{agentId}` — so it matches and the
    fixture keeps the literal it observed.  Only a fixture placeholder that names the
    route's parameter differently is renamed (`{sessionID}` → `{sessionId}`).
    """
    a, b = _segments(row_path), _segments(path)
    if len(a) != len(b):
        return None
    renames: dict[str, str] = {}
    for x, y in zip(a, b):
        if _param(x):
            if _param(y) and x != y:
                renames[y.strip("{}")] = x.strip("{}")
            continue
        if x != y:
            return None
    return renames


def compat_path(path: str) -> Optional[str]:
    """``/api/chat-sessions/x`` → ``/api/chat/sessions/x`` (upstream's older URLs)."""
    m = LEGACY_SEGMENT.search(path)
    if m is None:
        return None
    return path[: m.start()] + "/api/%s/%s" % (m.group(1), m.group(2)) + path[m.end() :]


def match_route(method: str, path: str) -> tuple[Optional[dict[str, Any]], dict[str, str], Optional[str], int]:
    """The registered route for ``method path``: `(row, renames, compat note, candidates)`."""
    rows = _ROUTES.get(method, [])
    canonical = path.rstrip("/") or "/"
    for row in rows:
        if row["path"].rstrip("/") == canonical:
            return row, {}, None, 1
    folded = compat_path(canonical)
    note: Optional[str] = None
    target = canonical
    if folded is not None:
        for row in rows:
            if row["path"].rstrip("/") == folded.rstrip("/"):
                return row, {}, "path normalised: upstream asked for the historical %s, registered as %s" % (
                    canonical,
                    row["path"],
                ), 1
        target, note = folded, "path normalised from upstream's historical %s to %s" % (canonical, folded)
    cands = [(r, _structural(r["path"], target)) for r in rows]
    cands = [(r, rn) for r, rn in cands if rn is not None]
    if len(cands) == 1:
        return cands[0][0], cands[0][1], note, 1
    return None, {}, note, len(cands)


# --------------------------------------------------------------------------- #
# the rule
# --------------------------------------------------------------------------- #


def run(ex: Any, host: dict[str, Any]) -> None:
    """Extract every direct-handler site in ``ex.test_files``."""
    global _HOST, _INDEX, _ROUTES
    _HOST = host
    _INDEX, _joined = handler_index(ex.root)
    _ROUTES = {}
    for handler, rows in _INDEX.items():
        for row in rows:
            _ROUTES.setdefault(row["method"], []).append(row)
    for rel in sorted(ex.test_files):
        src, masked, _lits = ex.sources[rel]
        sites = find_sites(host, masked, _INDEX)
        if not sites:
            continue
        funcs = host["find_functions"](masked, rel)
        interp = host["Interpreter"](ex.sources, ex.funcs, ex.pkg_literals)
        other = host["request_site_positions"](masked)
        stops = sorted([pos for pos, _kind, _a, _c in sites] + [pos for pos, _k in other])
        test_level: set[int] = set()
        for fn in funcs:
            if fn.receiver or not fn.name.startswith(("Test", "Benchmark", "Example")):
                continue
            for pos, _name, _args, _end in sites:
                if fn.body[0] <= pos < fn.body[1]:
                    test_level.add(pos)
        for pos, name, _args, _end in sites:
            if pos not in test_level:
                ex.helper.append(
                    host["HelperSite"](rel, host["line_of"](src, pos), host["enclosing_test"](funcs, pos), KIND)
                )
                continue
            ex.candidates.append(
                host["Candidate"](rel, host["line_of"](src, pos), host["enclosing_test"](funcs, pos), KIND, pos)
            )
        for fn in funcs:
            if fn.receiver or not fn.name.startswith(("Test", "Benchmark", "Example")):
                continue
            mine = [s for s in sites if fn.body[0] <= s[0] < fn.body[1]]
            if not mine:
                continue
            try:
                checkpoints = host["walk"](rel, masked, interp, fn.body, host["WalkCtx"]())
            except Exception as exc:  # no site may be lost to a walk failure
                for pos, _name, _args, _end in mine:
                    ex.skips.append(
                        host["Skip"](
                            rel,
                            host["line_of"](src, pos),
                            fn.name,
                            KIND,
                            "internal_error",
                            "walking %s failed: %s" % (fn.name, repr(exc)[:160]),
                        )
                    )
                continue
            for pos, name, args, call_end in mine:
                before = (len(ex.fixtures), len(ex.skips))
                ctx = host["state_before"](checkpoints, pos)
                for vctx in ex.variants(masked, fn, pos, ctx):
                    try:
                        extract_site(ex, host, rel, src, masked, interp, fn, pos, name, args, call_end, stops, vctx)
                    except Exception as exc:  # a crash must never hide a site
                        ex.skips.append(
                            host["Skip"](
                                rel,
                                host["line_of"](src, pos),
                                fn.name,
                                KIND,
                                "internal_error",
                                repr(exc)[:200],
                            )
                        )
                        break
                made = len(ex.fixtures) - before[0]
                if made == 0 and len(ex.skips) == before[1]:
                    ex.skips.append(
                        host["Skip"](
                            rel,
                            host["line_of"](src, pos),
                            fn.name,
                            KIND,
                            "helper_not_followed",
                            "site produced neither a fixture nor a reason",
                        )
                    )
                ex.produced[(rel, pos)] = made


def extract_site(
    ex: Any,
    host: dict[str, Any],
    rel: str,
    src: str,
    masked: str,
    interp: Any,
    fn: Any,
    pos: int,
    name: str,
    args: list[tuple[int, int]],
    call_end: int,
    stops: list[int],
    ctx: Any,
) -> None:
    line = host["line_of"](src, pos)

    def skip(reason: str, detail: str) -> None:
        ex.skips.append(host["Skip"](rel, line, fn.name, KIND, reason, detail))

    # 4. the expected status: `<recorder>.Code` comparisons, before the next site.
    recorder = interp.text(rel, args[0])
    if not RECORDER.match(recorder):
        return skip("recorder_var_unresolved", "recorder argument %r is not a plain variable" % recorder)
    stop = fn.body[1]
    for later in stops:
        if later > call_end:
            stop = min(stop, later)
            break
    ne, eq, bounds = _codes(host, masked[call_end:stop], recorder)
    codes = sorted(set(ne)) if ne else sorted(set(eq))
    if not codes:
        if bounds:
            return skip(
                "status_not_equality",
                "`%s.Code` is only bounded (%s) before line %d, which pins no status"
                % (recorder, sorted(set(bounds)), host["line_of"](src, stop)),
            )
        return skip(
            "no_status_assertion",
            "no `%s.Code` comparison before line %d" % (recorder, host["line_of"](src, stop)),
        )
    if len(codes) != 1:
        return skip("ambiguous_status", "`%s.Code` comparisons in the same block: %s" % (recorder, codes))
    status = codes[0]

    # 2. the request, traced exactly like I1 (plus wrapper unwrapping).
    req = interp.resolve_request(rel, args[1], ctx)
    if req is None:
        return skip("request_var_unresolved", "request argument %r is not traceable" % interp.text(rel, args[1]).strip())
    if req.problems:
        return skip(req.problems[0], ", ".join(sorted(set(req.problems))))
    if not req.method or not req.method.is_literal or not isinstance(req.method.value, str):
        return skip("method_unresolved", "HTTP method is not a literal")
    if req.body_var:
        made_at = ctx.defs.get(req.body_var, (0, 0))[0]
        changed_at = ctx.mutated.get(req.body_var)
        if changed_at is not None and changed_at > made_at:
            return skip(
                "body_mutated_after_construction",
                "%s is modified after the request body was built from it" % req.body_var,
            )

    # 3. placeholders, then 5. the route table.
    path, path_params, query = host["canonicalise"](req)
    if host["MARK"] in path or any(host["MARK"] in v for v in query.values()):
        return skip("path_not_literal", "unsubstituted symbol marker in the URL")
    row, renames, compat_note, candidates = match_route(req.method.value, path)
    if row is None:
        detail = "%s %s is not in docs/fixtures/upstream-routes.tsv" % (req.method.value, path)
        if candidates:
            detail += " (%d registered route(s) match it structurally: %s)" % (
                candidates,
                ", ".join(
                    "%s %s" % (r["method"], r["path"])
                    for r in _ROUTES.get(req.method.value, [])
                    if _structural(r["path"], path) is not None
                ),
            )
        return skip("path_not_registered", detail)

    headers, actor = host["split_headers"](req.headers)
    needs = host["symbols_in"](path_params, query, actor, headers)
    unknown = sorted(needs - set(host["BINDABLE"]))
    ident_needs = host["symbols_in"](actor.get("upstream_identity", {}))
    ident_unbound = sorted(ident_needs - set(host["BINDABLE"]))
    if ident_unbound and actor["kind"] == "member":
        # The runner would swap this member for its own seeded owner: that asserts a
        # different role than upstream did, so it is a skipped site, not a fixture.
        return skip(
            "value_unresolved",
            "member identity needs values the runner cannot fabricate: %s" % ", ".join(ident_unbound),
        )
    if unknown:
        return skip(
            "value_unresolved",
            "fixture needs values the runner cannot bind: %s" % ", ".join(unknown),
        )

    if renames:
        path = re.sub(r"\{([A-Za-z0-9_]+)\}", lambda m: "{%s}" % renames.get(m.group(1), m.group(1)), path)
        path_params = {renames.get(k, k): v for k, v in path_params.items()}
    notes = [
        "upstream drove the handler directly (%s); replayed here against the router, which also "
        "applies its middleware" % name,
        "status read from the `%s.Code` comparison that follows the call" % recorder,
    ]
    if compat_note:
        notes.append(compat_note)
    if renames:
        notes.append(
            "path parameters renamed to the registered route template %s (%s)"
            % (row["path"], ", ".join("{%s}→{%s}" % (k, v) for k, v in sorted(renames.items())))
        )
    via = "handler"
    fixture = host["Fixture"](
        id="",
        domain=host["domain_of"](path),
        slug="",
        method=req.method.value,
        path=path,
        path_params=path_params,
        query=query,
        headers=headers,
        actor=actor,
        body=req.body.value if req.body is not None else None,
        status=status,
        json_subset=host["map_subset"](masked, interp.src(rel)[2], stop, fn.body[1]),
        source={"file": rel, "line": line, "test": fn.name, "site": KIND, "via": via},
        notes=notes,
        via=via,
        bindings={s: host["BINDABLE"][s] for s in sorted(needs) if s in host["BINDABLE"]},
    )
    ex.fixtures.append(fixture)


# --------------------------------------------------------------------------- #
# stats
# --------------------------------------------------------------------------- #


def scope(ex: Any) -> dict[str, Any]:
    """The `scope` block of ``contracts/golden/stats.json`` (host ``build_stats``)."""
    return {
        "in_scope": [
            "testutil.Call(t, handler, request)",
            "authRequest(t, method, path, body)",
            "http.Get(url)",
            "http.NewRequest(method, url, body)",
            "testHandler.<Handler>(w, req) + a following `<w>.Code` comparison (rule I4, docs/42 §6.3)",
        ],
        "not_extracted": {
            "description": (
                "`w := httptest.NewRecorder()` sites rule I4 still cannot turn into a fixture: the "
                "request is built where the checkpoint walk cannot see it (inside a closure or a "
                "different helper), the call does not compare `w.Code`, or its path is absent from "
                "docs/fixtures/upstream-routes.tsv.  Each one is a `direct_handler` row of "
                "extraction-report.tsv with its reason."
            ),
            "recorder_constructions": ex.recorder_sites,
        },
    }



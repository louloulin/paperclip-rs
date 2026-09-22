#!/usr/bin/env python3
"""Extract language-neutral HTTP golden fixtures from upstream multica's Go tests.

Upstream multica carries ~415k lines of Go tests that already encode the contract
of every endpoint: *this request, from this actor, gets this status* (and, where
the test looks, *these fields*).  Reading them by hand does not scale, and writing
a second e2e suite here could only ever prove this repo agrees with itself.  So
upstream's own tests become fixtures this repo replays against its own axum
router: plan1 **P5** ("上游测试即 golden / oracle") and the machinery behind the
**§8** metric 契约等价率 (`docs/27-W0-GOLDEN-FIXTURES.md`).

Contract of this program
------------------------
* It only *reads* upstream and *writes* `contracts/golden/`.
* **Nothing is dropped silently.**  Every request-construction site it sees ends
  up either as a fixture or as one row of `contracts/golden/extraction-report.tsv`
  with a machine-checkable reason, so 抽取率 is recomputable by counting a file
  instead of trusting prose.
* **Nothing is guessed.**  A field that is neither a literal, nor a symbolic name,
  nor a documented identity placeholder makes the case *skipped*: a fixture that
  misstates the upstream request would "prove" equivalence with a request
  upstream never made.

Why a masking/matching hybrid instead of a Go AST tool: `go` is not installed
here and this slice must reproduce offline.  A full Go parser is out of scope, so
this implements the slice of Go the supported idioms actually use — comment and
string masking that preserves byte offsets, bracket/argument matching over the
masked text, ASI-correct statement splitting, and a request mini-interpreter that
follows local helpers.  Everything else is reported.

Supported idioms
----------------
I1  ``testutil.Call(t, handler, req).Want(status)[.JSON(&v)|.Map()]`` where
    ``req`` is traceable through ``httptest.NewRequest`` / ``testutil.JSONRequest``
    / ``http.NewRequest`` plus ``Header.Set``, ``testutil.WithHeaders``,
    ``testutil.WithURLParams``, ``withURLParam``, including through local helpers
    such as ``newRequest`` / ``authRequest``.
I2  ``resp := authRequest(t, method, path, body)`` (the router-level helper in
    ``server/cmd/server``) plus a following ``resp.StatusCode != <status>``.
I3  ``for _, v := range []string{...}``: unrolled into one fixture per element
    (upstream's route-auth tables).

Usage
-----
    git clone --depth 1 --single-branch --branch main \\
        https://github.com/louloulin/multica ../upstream-multica   # docs/20 §1
    python3 scripts/extract_upstream_fixtures.py --upstream ../upstream-multica
    python3 scripts/extract_upstream_fixtures.py --upstream ../upstream-multica --check

Exit codes: 0 = ok, 1 = --check found drift, 2 = usage / bad upstream tree.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from typing import Any, Optional

SCHEMA_VERSION = 1

# `internal/handler` is what the issue names; `cmd/server` is added because it is
# the only place upstream exercises the *router* rather than handlers called
# directly — the closest thing upstream has to a route-level oracle.
DEFAULT_SCAN = ("server/internal/handler", "server/cmd/server")

# Identity headers become `actor`, not raw `headers`: their values are upstream
# DB fixtures (`testUserID`) with no meaning here, so the runner has to bind them
# to a locally seeded identity.  Keeping them in `headers` would make every
# fixture look un-replayable; lifting identity out is what makes
# "anonymous → 401" replayable with no database at all.
IDENTITY_HEADERS = (
    "x-user-id",
    "x-agent-id",
    "x-task-id",
    "x-workspace-id",
    "x-workspace-slug",
    "authorization",
    "x-actor-source",
)
AGENT_HEADERS = ("x-agent-id", "x-task-id")

CTORS = {"httptest.NewRequest", "testutil.JSONRequest", "http.NewRequest"}
HEADER_MODIFIERS = {"testutil.WithHeaders"}
URLPARAM_MODIFIERS = {"testutil.WithURLParams", "withURLParam", "withURLParams"}

HTTP_METHODS = {
    "http.MethodGet": "GET",
    "http.MethodPost": "POST",
    "http.MethodPut": "PUT",
    "http.MethodPatch": "PATCH",
    "http.MethodDelete": "DELETE",
    "http.MethodHead": "HEAD",
    "http.MethodOptions": "OPTIONS",
    "http.MethodTrace": "TRACE",
}

STATUS_CODES = {
    "OK": 200, "Created": 201, "Accepted": 202, "NoContent": 204, "NotModified": 304,
    "BadRequest": 400, "Unauthorized": 401, "Forbidden": 403, "NotFound": 404,
    "MethodNotAllowed": 405, "Conflict": 409, "Gone": 410, "UnprocessableEntity": 422,
    "TooManyRequests": 429, "InternalServerError": 500, "NotImplemented": 501,
}

IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
SELECTOR = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*$")
MARK = "\x00"

SKIP_REASONS = (
    "no_status_assertion",
    "ambiguous_status",
    "body_unresolved",
    "body_mutated_after_construction",
    "path_not_literal",
    "method_unresolved",
    "value_unresolved",
    "helper_not_followed",
    "request_var_unresolved",
)

SITE_PATTERNS = (
    (r"testutil\.Call\s*\(", "testutil.Call"),
    (r"\bauthRequest\s*\(", "authRequest"),
    (r"\bhttp\.Get\s*\(", "http.Get"),
    (r"\bhttp\.NewRequest\s*\(", "http.NewRequest"),
)

# The runner has to invent a value for every symbol a fixture still carries, and
# an invented value can change the expected status.  Only these two are promised
# by the runner (to a seeded identity, or to a fresh UUID with no database), so
# anything else makes the case skipped instead of quietly wrong.
BINDABLE = {"$testUserID": "user_id", "$testWorkspaceID": "workspace_id"}

# `for _, path := range []string{...}` tables: upstream's auth matrices are written
# this way, and each element is a separate oracle case.
LOOP_TABLE = re.compile(r"for\s+(?:_,\s*)?([A-Za-z_]\w*)\s*:=\s*range\s+\[\]string\s*\{")


# --------------------------------------------------------------------------- #
# Go masking: blank comments and string bodies, keep every byte offset
# --------------------------------------------------------------------------- #

_ESCAPES = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", '"': '"', "'": "'", "\\": "\\"}


def go_unescape(raw: str) -> str:
    if "\\" not in raw:
        return raw
    out: list[str] = []
    i = 0
    while i < len(raw):
        c = raw[i]
        if c == "\\" and i + 1 < len(raw):
            nxt = raw[i + 1]
            if nxt in _ESCAPES:
                out.append(_ESCAPES[nxt])
                i += 2
                continue
            if nxt == "x" and len(raw) >= i + 4:
                out.append(chr(int(raw[i + 2 : i + 4], 16)))
                i += 4
                continue
            if nxt == "u" and len(raw) >= i + 6:
                out.append(chr(int(raw[i + 2 : i + 6], 16)))
                i += 6
                continue
        out.append(c)
        i += 1
    return "".join(out)


def mask_go(src: str) -> tuple[str, dict[int, str]]:
    """Blank comments/string bodies, preserving offsets.

    ``literals[offset_of_open_quote]`` is the decoded value.  Quote delimiters
    survive so a literal stays recognisable without re-parsing Go, while braces
    and parens inside comments or strings are blanked — that is what makes the
    bracket matcher below correct.
    """
    out = list(src)
    literals: dict[int, str] = {}
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c == "/" and src[i : i + 2] == "//":
            j = src.find("\n", i)
            j = n if j < 0 else j
            out[i:j] = [" "] * (j - i)
            i = j
            continue
        if c == "/" and src[i : i + 2] == "/*":
            j = src.find("*/", i + 2)
            j = n if j < 0 else j + 2
            out[i:j] = [" "] * (j - i)
            i = j
            continue
        if c == "`":
            j = src.find("`", i + 1)
            j = n if j < 0 else j
            literals[i] = src[i + 1 : j]
            out[i + 1 : j] = [" "] * (j - i - 1)
            i = min(j + 1, n)
            continue
        if c in ('"', "'"):
            j = i + 1
            while j < n:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == c:
                    break
                j += 1
            if c == '"':
                literals[i] = go_unescape(src[i + 1 : min(j, n)])
            out[i + 1 : min(j, n)] = [" "] * (min(j, n) - i - 1)
            i = min(j + 1, n)
            continue
        i += 1
    return "".join(out), literals


def matching(masked: str, open_idx: int) -> int:
    """Index just past the bracket closing the one at ``open_idx``."""
    close = {"(": ")", "[": "]", "{": "}"}.get(masked[open_idx])
    if close is None:
        return open_idx
    depth, i = 0, open_idx
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


def split_args(masked: str, start: int, end: int) -> list[tuple[int, int]]:
    """Split a call's argument span into top-level argument spans."""
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
    return [a for a in args if masked[a[0] : a[1]].strip()]


def line_of(src: str, idx: int) -> int:
    return src.count("\n", 0, idx) + 1


def is_ident(text: str) -> bool:
    return bool(IDENT.fullmatch(text.strip()))


# --------------------------------------------------------------------------- #
# ASI-aware statement splitting
# --------------------------------------------------------------------------- #

_BLOCK_HEAD = re.compile(
    r"^(?:\}\s*|\)\s*|(?:if|for|switch|select|else)\b[^{;]*\{|case\b[^:]*:|default:|"
    r"(?:go\s+)?func\s*\([^)]*\)[^{;]*\{)+"
)


def strip_headers(text: str) -> str:
    """Drop leading block headers/labels so the body statement can be matched.

    ``if path == "verify-code" { body["code"] = "x" }`` is a single ASI
    statement; the interpreter only cares about the assignment in it.
    """
    prev, cur = None, text.strip()
    while prev != cur:
        prev, cur = cur, _BLOCK_HEAD.sub("", cur).strip()
    return cur


def asi_end(masked: str, start: int) -> int:
    """End of the Go statement starting at ``start``.

    Mirrors Go's semicolon insertion: a newline terminates a statement only when
    the last token is an identifier, literal, ``)``, ``]``, ``}``, ``++`` or
    ``--``.  ``{`` and ``,`` continue it, which keeps multi-line composite
    literals and call chains in one piece.
    """
    depth, i = 0, start
    n = len(masked)
    while i < n:
        c = masked[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                return i
            depth -= 1
        elif depth == 0 and c == ";":
            return i
        elif c == "\n" and depth == 0:
            last = masked[start:i].rstrip()
            if not last:
                return i
            if last[-1].isalnum() or last[-1] in "_)]}\"'`+":
                return i
        i += 1
    return n


def statements(masked: str, body: tuple[int, int]) -> list[tuple[int, int]]:
    """Every statement inside a function body, in source order, blocks included.

    Flattening is deliberate: requests are usually built and asserted inside a
    ``t.Run`` closure or a ``for`` body, and the interpreter is linear, so
    enclosing blocks are transparent.
    """
    out: list[tuple[int, int]] = []
    i, end = body[0] + 1, body[1] - 1
    while i < end:
        if masked[i] == "\n":
            i += 1
            continue
        j = max(asi_end(masked, i), i + 1)
        if masked[i:j].strip():
            out.append((i, j))
        i = j
    return out


# --------------------------------------------------------------------------- #
# Go structure
# --------------------------------------------------------------------------- #


@dataclass
class GoFunc:
    name: str
    receiver: str
    params: list[str]
    body: tuple[int, int]
    file: str


def find_functions(masked: str, file: str) -> list[GoFunc]:
    funcs: list[GoFunc] = []
    for m in re.finditer(r"(?m)^func\s+", masked):
        j = m.end()
        receiver = ""
        if masked[j : j + 1] == "(":
            close = matching(masked, j)
            receiver = masked[j + 1 : close - 1].strip()
            j = close
        nm = IDENT.match(masked, j)
        if not nm:
            continue
        k = masked.find("(", nm.end())
        if k < 0:
            continue
        close = matching(masked, k)
        brace = masked.find("{", close)
        if brace < 0:
            continue
        funcs.append(
            GoFunc(
                nm.group(0),
                receiver,
                param_names(masked[k + 1 : close - 1]),
                (brace, matching(masked, brace)),
                file,
            )
        )
    return funcs


def param_names(span_text: str) -> list[str]:
    """Parameter identifiers in order, types dropped.

    Returns ``[]`` for any signature this does not understand (variadic, grouped
    types, anonymous params).  Callers read that as "cannot follow this helper",
    which is the honest outcome rather than a guess.
    """
    cleaned = re.sub(r"\s+", " ", span_text.strip())
    if not cleaned or "..." in cleaned:
        return []
    parts: list[str] = []
    depth, cur = 0, ""
    for c in cleaned:
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        if c == "," and depth == 0:
            parts.append(cur)
            cur = ""
            continue
        cur += c
    if cur.strip():
        parts.append(cur)
    names: list[str] = []
    for part in parts:
        words = part.strip().split()
        if len(words) < 2:
            if len(words) == 1 and is_ident(words[0]):
                names.append(words[0])
                continue
            return []
        if not is_ident(words[0]):
            return []
        names.append(words[0])
    return names


def package_literals(files: dict[str, tuple[str, str, dict[int, str]]]) -> dict[str, str]:
    """``name = "<literal>"`` definitions, first one wins.

    These are the only upstream values a fixture may carry verbatim: they are
    compiled in, not database rows.  ``var testUserID string`` has no literal and
    therefore stays a symbol.
    """
    values: dict[str, str] = {}
    for _rel, (_src, masked, literals) in files.items():
        for m in re.finditer(r"(?m)^\s*(?:var|const)?\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*", masked):
            start = m.end()
            if masked[start : start + 1] == '"' and start in literals:
                values.setdefault(m.group(1), literals[start])
    return values


# --------------------------------------------------------------------------- #
# Literal / body decoding
# --------------------------------------------------------------------------- #


@dataclass
class Value:
    kind: str  # "literal" | "symbol"
    value: Any = None
    note: str = ""

    @property
    def is_literal(self) -> bool:
        return self.kind == "literal"

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Value) and (self.kind, self.value) == (other.kind, other.value)

    def __hash__(self) -> int:
        return hash((self.kind, self.value))


def sym(name: str) -> Value:
    return Value("symbol", f"${name}")


def decode_scalar(masked: str, literals: dict[int, str], span: tuple[int, int]) -> Optional[Value]:
    raw = masked[span[0] : span[1]]
    text = raw.strip()
    if not text:
        return None
    lead = span[0] + (len(raw) - len(raw.lstrip()))
    if masked[lead : lead + 1] in ('"', "`") and lead in literals:
        # The span must be *only* that literal.  `/a/` in `"/a/"+id` also starts
        # with a quote, and reading it as the whole URL would silently shorten a
        # request into a different route.
        end = lead + 1
        quote = masked[lead]
        while end < span[1] and masked[end] != quote:
            end += 1
        if end >= span[1] or masked[end + 1 : span[1]].strip():
            return None
        return Value("literal", literals[lead])
    if text == "nil":
        return Value("literal", None)
    if text in ("true", "false"):
        return Value("literal", text == "true")
    if re.fullmatch(r"-?\d+", text):
        return Value("literal", int(text))
    if re.fullmatch(r"-?\d+\.\d+", text):
        return Value("literal", float(text))
    return None


COMPOSITE_HEAD = re.compile(
    r"^(map\[string\](?:any|interface\{\}|string)|\[\]any|\[\]interface\{\}|\[\]string|"
    r"testutil\.Cols|\[\]map\[string\](?:any|interface\{\}))\s*\{"
)


def decode_body(masked: str, literals: dict[int, str], span: tuple[int, int]) -> Optional[Value]:
    """Decode a request body expression into JSON, or ``None`` if not fully known.

    Partial knowledge is refused on purpose: a body with one guessed field would
    assert a request upstream never sent.
    """
    raw = masked[span[0] : span[1]]
    text = raw.strip()
    if not text:
        return None
    lead = span[0] + (len(raw) - len(raw.lstrip()))
    scalar = decode_scalar(masked, literals, (lead, span[1]))
    if scalar is not None:
        if isinstance(scalar.value, str):
            try:
                return Value("literal", json.loads(scalar.value), "json string body")
            except json.JSONDecodeError:
                return Value("literal", scalar.value, "raw string body")
        return Value("literal", scalar.value, "empty body")

    m = re.match(r"(?:bytes|strings)\.NewReader\s*\(", text)
    if m:
        open_idx = lead + m.end() - 1
        inner = (open_idx + 1, matching(masked, open_idx) - 1)
        if masked[inner[0] : inner[1]].strip().startswith("[]byte("):
            b_open = masked.index("(", inner[0])
            return decode_body(masked, literals, (b_open + 1, matching(masked, b_open) - 1))
        lit = decode_scalar(masked, literals, inner)
        if lit is not None and isinstance(lit.value, str):
            try:
                return Value("literal", json.loads(lit.value), "json string body")
            except json.JSONDecodeError:
                return Value("literal", lit.value, "raw string body")
        return None

    cm = COMPOSITE_HEAD.match(text)
    if not cm:
        return None
    head = cm.group(1)
    brace = lead + cm.end() - 1
    entries = split_args(masked, brace + 1, matching(masked, brace) - 1)
    if head.startswith("[]") and not head.startswith("[]map["):
        out: list[Any] = []
        for e in entries:
            v = decode_body(masked, literals, e)
            if v is None or not v.is_literal:
                return None
            out.append(v.value)
        return Value("literal", out, head)
    result: dict[str, Any] = {}
    for e in entries:
        colon = top_level_colon(masked, e)
        if colon < 0:
            return None
        key_text = masked[e[0] : colon].strip()
        if key_text[:1] in ('"', "`"):
            kv = decode_scalar(masked, literals, (e[0], colon))
            if kv is None or not isinstance(kv.value, str):
                return None
            key = kv.value
        elif is_ident(key_text):
            key = key_text
        else:
            return None
        v = decode_body(masked, literals, (colon + 1, e[1]))
        if v is None or not v.is_literal:
            return None
        result[key] = v.value
    return Value("literal", result, head)


def top_level_colon(masked: str, span: tuple[int, int]) -> int:
    depth = 0
    for i in range(span[0], span[1]):
        c = masked[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == ":" and depth == 0:
            return i
    return -1


# --------------------------------------------------------------------------- #
# Request interpreter
# --------------------------------------------------------------------------- #


@dataclass
class ReqState:
    method: Optional[Value] = None
    pieces: list[str] = field(default_factory=list)
    markers: dict[str, Value] = field(default_factory=dict)
    urlparams: dict[str, Value] = field(default_factory=dict)
    headers: dict[str, Value] = field(default_factory=dict)
    body: Optional[Value] = None
    body_var: Optional[str] = None
    problems: list[str] = field(default_factory=list)

    def clone(self) -> "ReqState":
        return ReqState(
            self.method,
            list(self.pieces),
            dict(self.markers),
            dict(self.urlparams),
            dict(self.headers),
            self.body,
            self.body_var,
            list(self.problems),
        )


@dataclass
class Binding:
    file: str
    span: tuple[int, int]
    ctx: "WalkCtx"


@dataclass
class WalkCtx:
    """Variable state at one point inside one function body."""

    env: dict[str, ReqState] = field(default_factory=dict)
    defs: dict[str, tuple[int, int]] = field(default_factory=dict)
    decls: dict[str, str] = field(default_factory=dict)
    mutated: dict[str, int] = field(default_factory=dict)
    encoded: dict[str, tuple[int, int]] = field(default_factory=dict)
    marshal: dict[str, tuple[int, int]] = field(default_factory=dict)
    bindings: dict[str, Binding] = field(default_factory=dict)

    def clone(self) -> "WalkCtx":
        return WalkCtx(
            {k: v.clone() for k, v in self.env.items()},
            dict(self.defs),
            dict(self.decls),
            dict(self.mutated),
            dict(self.encoded),
            dict(self.marshal),
            dict(self.bindings),
        )


class Interpreter:
    def __init__(
        self,
        sources: dict[str, tuple[str, str, dict[int, str]]],
        funcs: dict[str, GoFunc],
        pkg_literals: dict[str, str],
    ):
        self.sources = sources
        self.funcs = funcs
        self.pkg_literals = pkg_literals

    # -- primitives -------------------------------------------------------- #

    def src(self, file: str) -> tuple[str, str, dict[int, str]]:
        return self.sources[file]

    def text(self, file: str, span: tuple[int, int]) -> str:
        return self.sources[file][1][span[0] : span[1]].strip()

    def resolve_scalar(self, file: str, span: tuple[int, int], ctx: WalkCtx, depth: int = 0) -> Optional[Value]:
        if depth > 6:
            return None
        masked, literals = self.sources[file][1], self.sources[file][2]
        text = self.text(file, span)
        if not text:
            return None
        if text in HTTP_METHODS:
            return Value("literal", HTTP_METHODS[text])
        lit = decode_scalar(masked, literals, span)
        if lit is not None:
            return lit
        if is_ident(text):
            if text in ctx.bindings:
                b = ctx.bindings[text]
                got = self.resolve_scalar(b.file, b.span, b.ctx, depth + 1)
                if got is not None:
                    return got
            elif text in ctx.defs:
                got = self.resolve_scalar(file, ctx.defs[text], ctx, depth + 1)
                if got is not None:
                    return got
            if text in self.pkg_literals:
                return Value("literal", self.pkg_literals[text], "package const")
            # A value the test computed (an id read out of an earlier response)
            # stays symbolic.  Returning None here would silently drop it and
            # shorten the URL into a *different* route.
            return sym(text)
        if SELECTOR.fullmatch(text):
            return sym(text)
        return None

    def resolve_path(self, file: str, span: tuple[int, int], ctx: WalkCtx, depth: int = 0) -> Optional[dict[str, Any]]:
        """Resolve a URL expression into literal pieces plus symbol markers."""
        if depth > 6:
            return None
        text = self.text(file, span)
        if not text:
            return None
        lit = decode_scalar(self.sources[file][1], self.sources[file][2], span)
        if lit is not None and isinstance(lit.value, str):
            return {"pieces": [lit.value], "markers": {}}
        if text == "testServer.URL":
            return {"pieces": [""], "markers": {}}
        if is_ident(text):
            if text in ctx.bindings:
                b = ctx.bindings[text]
                got = self.resolve_path(b.file, b.span, b.ctx, depth + 1)
                if got is not None:
                    return got
            elif text in ctx.defs:
                got = self.resolve_path(file, ctx.defs[text], ctx, depth + 1)
                if got is not None:
                    return got
            return None
        parts = self.split_concat(file, span)
        if len(parts) < 2:
            return None
        pieces: list[str] = []
        markers: dict[str, Value] = {}
        for p in parts:
            ptext = self.text(file, p)
            if not ptext:
                return None
            if ptext == "testServer.URL":
                pieces.append("")
                continue
            sub = self.resolve_path(file, p, ctx, depth + 1)
            if sub is not None:
                pieces.append("".join(sub["pieces"]))
                markers.update(sub["markers"])
                continue
            val = self.resolve_scalar(file, p, ctx, depth + 1)
            if val is None or val.kind != "symbol":
                return None
            mark = f"{MARK}{len(markers)}{MARK}"
            markers[mark] = val
            pieces.append(mark)
        return {"pieces": pieces, "markers": markers}

    def split_concat(self, file: str, span: tuple[int, int]) -> list[tuple[int, int]]:
        masked = self.sources[file][1]
        parts: list[tuple[int, int]] = []
        depth, start = 0, span[0]
        for i in range(span[0], span[1]):
            c = masked[i]
            if c in "([{":
                depth += 1
            elif c in ")]}":
                depth -= 1
            elif c == "+" and depth == 0:
                parts.append((start, i))
                start = i + 1
        parts.append((start, span[1]))
        return parts

    # -- request construction --------------------------------------------- #

    def resolve_request(self, file: str, span: tuple[int, int], ctx: WalkCtx, depth: int = 0) -> Optional[ReqState]:
        if depth > 6:
            return None
        text = self.text(file, span)
        if not text:
            return None
        m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)\s*\(", text)
        if m:
            return self.resolve_call(file, span, m.group(1), ctx, depth)
        if is_ident(text):
            if text in ctx.env:
                return ctx.env[text].clone()
            if text in ctx.bindings:
                b = ctx.bindings[text]
                return self.resolve_request(b.file, b.span, b.ctx, depth + 1)
            if text in ctx.defs:
                return self.resolve_request(file, ctx.defs[text], ctx, depth + 1)
        return None

    def resolve_call(self, file: str, span: tuple[int, int], name: str, ctx: WalkCtx, depth: int) -> Optional[ReqState]:
        masked = self.sources[file][1]
        open_idx = span[0] + self.text(file, span).index("(")
        args = split_args(masked, open_idx + 1, matching(masked, open_idx) - 1)

        if name in CTORS:
            if len(args) < 3:
                return None
            st = ReqState()
            st.method = self.resolve_scalar(file, args[0], ctx)
            path = self.resolve_path(file, args[1], ctx)
            if path is None:
                st.problems.append("path_not_literal")
                return st
            if st.method is None:
                st.problems.append("method_unresolved")
                return st
            st.pieces = path["pieces"]
            st.markers = path["markers"]
            body_text = self.text(file, args[2])
            if body_text in ("nil", "http.NoBody"):
                st.body = Value("literal", None, "absent")
            else:
                st.body = self.resolve_body(file, args[2], ctx, st)
                if st.body is None:
                    st.problems.append("body_unresolved")
            return st

        if name in HEADER_MODIFIERS:
            if len(args) < 3:
                return None
            base = self.resolve_request(file, args[0], ctx, depth + 1)
            if base is None:
                return None
            st = base.clone()
            for k, v in self.pairs(file, args[1:], ctx):
                st.headers[k] = v
            return st

        if name in URLPARAM_MODIFIERS:
            if len(args) < 3:
                return None
            base = self.resolve_request(file, args[0], ctx, depth + 1)
            if base is None:
                return None
            st = base.clone()
            for k, v in self.pairs(file, args[1:], ctx):
                st.urlparams[k] = v
            return st

        fn = self.funcs.get(name)
        if fn is not None and not fn.receiver and len(fn.params) == len(args):
            sub = WalkCtx()
            for pname, arg in zip(fn.params, args):
                sub.bindings[pname] = Binding(file, arg, ctx)
            return self.interpret(fn.file, fn.body, sub, depth + 1)
        return None

    def pairs(self, file: str, args: list[tuple[int, int]], ctx: WalkCtx) -> list[tuple[str, Value]]:
        """Read alternating ``"key", value`` arguments."""
        out: list[tuple[str, Value]] = []
        i = 0
        while i + 1 < len(args) + 1 and i + 1 <= len(args):
            k = self.resolve_scalar(file, args[i], ctx)
            if k is None or not k.is_literal or not isinstance(k.value, str) or i + 1 >= len(args):
                return []
            v = self.resolve_scalar(file, args[i + 1], ctx)
            if v is None:
                return []
            out.append((k.value, v))
            i += 2
        return out

    def resolve_body(self, file: str, span: tuple[int, int], ctx: WalkCtx, st: ReqState, depth: int = 0) -> Optional[Value]:
        """Decode a request body argument, following buffers, marshals and params."""
        if depth > 6:
            return None
        masked, literals = self.sources[file][1], self.sources[file][2]
        text = self.text(file, span)
        direct = decode_body(masked, literals, span)
        if direct is not None:
            return direct
        if text.startswith("&") and is_ident(text[1:]):
            inner = ctx.encoded.get(text[1:])
            if inner is not None:
                return self.resolve_body(file, inner, ctx, st, depth + 1)
            return None
        # Unwrap the buffer/marshal chain a body normally travels through.
        wrapper = re.match(r"^(?:json\.Marshal|(?:bytes|strings)\.NewReader)\s*\(", text)
        if wrapper:
            open_idx = span[0] + wrapper.end() - 1
            return self.resolve_body(file, (open_idx + 1, matching(masked, open_idx) - 1), ctx, st, depth + 1)
        if is_ident(text):
            st.body_var = text
            if text in ctx.marshal:
                return self.resolve_body(file, ctx.marshal[text], ctx, st, depth + 1)
            if text in ctx.bindings:
                b = ctx.bindings[text]
                return self.resolve_body(b.file, b.span, b.ctx, st, depth + 1)
            if text in ctx.defs:
                return self.resolve_body(file, ctx.defs[text], ctx, st, depth + 1)
            return None
        return None

    # -- function body ---------------------------------------------------- #

    def interpret(self, file: str, body: tuple[int, int], ctx: WalkCtx, depth: int = 0) -> Optional[ReqState]:
        """Walk a body and return the request its ``return`` denotes."""
        if depth > 4:
            return None
        masked = self.sources[file][1]
        final = walk(file, masked, self, body, ctx)[-1][1]
        for m in reversed(list(re.finditer(r"(?m)^\s*return\s+([A-Za-z_][A-Za-z0-9_]*)\s*$", masked[body[0] : body[1]]))):
            name = m.group(1)
            if name in final.env:
                return final.env[name].clone()
            if name in final.defs:
                got = self.resolve_request(file, final.defs[name], final, depth + 1)
                if got is not None:
                    return got
        return None


def walk(
    file: str, masked: str, interp: Interpreter, body: tuple[int, int], ctx: WalkCtx
) -> list[tuple[int, WalkCtx]]:
    """Forward-walk a body, checkpointing variable state after every statement."""
    checkpoints: list[tuple[int, WalkCtx]] = []
    for start, stop in statements(masked, body):
        raw = masked[start:stop]
        text = strip_headers(raw)
        if text:
            # `strip_headers` drops a block header, which moves the text; anchor it
            # back onto the source so every offset below stays absolute.
            base = start + raw.rindex(text)
            apply_statement(file, interp, (base, stop), text, ctx)
        checkpoints.append((stop, ctx.clone()))
    return checkpoints


def apply_statement(file: str, interp: Interpreter, span: tuple[int, int], text: str, ctx: WalkCtx) -> None:
    masked = interp.sources[file][1]

    # 0. Statements that live inside a block header (`if body != nil { ... }`) are
    #    flattened into one statement, so scan the whole span for the buffer and
    #    marshal steps that build a request body.
    raw = masked[span[0] : span[1]]
    for m in re.finditer(r"([A-Za-z_]\w*)\s*(?::=|=)\s*(?:bytes|strings)\.NewReader\s*\(\s*([A-Za-z_]\w*)\s*\)", raw):
        ctx.defs[m.group(1)] = (span[0] + m.start(2), span[0] + m.end(2))
    for m in re.finditer(r"([A-Za-z_]\w*)\s*,\s*_\s*:=\s*json\.Marshal\s*\(", raw):
        open_idx = span[0] + m.end() - 1
        args = split_args(masked, open_idx + 1, matching(masked, open_idx) - 1)
        if args:
            ctx.marshal[m.group(1)] = args[0]

    # 1. `json.NewEncoder(&buf).Encode(x)` — remember what a buffer holds
    enc = re.match(r"^[A-Za-z_][A-Za-z0-9_\.]*NewEncoder\s*\(\s*&([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*\.Encode\s*\(", text)
    if enc:
        open_idx = span[0] + text.rindex("(")
        args = split_args(masked, open_idx + 1, matching(masked, open_idx) - 1)
        if args:
            ctx.encoded[enc.group(1)] = args[0]
        return

    # 2. index/map mutation — remembered so a mutated body is never used as-is
    mut = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\s*\[", text)
    if mut and "=" in text:
        ctx.mutated.setdefault(mut.group(1), span[0])
        return
    dele = re.match(r"^delete\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)", text)
    if dele:
        ctx.mutated.setdefault(dele.group(1), span[0])
        return

    # 3. declared response variable: `var created IssueResponse`
    decl = re.match(r"^var\s+([A-Za-z_][A-Za-z0-9_]*)\s+([A-Za-z_][A-Za-z0-9_\[\]\.\*]*)", text)
    if decl:
        ctx.decls[decl.group(1)] = decl.group(2)
        ctx.env.pop(decl.group(1), None)
        ctx.defs.pop(decl.group(1), None)
        return

    # 4. `req.Header.Set("k", "v")`
    hdr = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\.Header\.Set\s*\(", text)
    if hdr and hdr.group(1) not in ctx.env and hdr.group(1) in ctx.defs:
        # `req, err := http.NewRequest(...)` parks the request in `defs` until a
        # header call proves it is one.
        st = interp.resolve_request(file, ctx.defs[hdr.group(1)], ctx)
        if st is not None:
            ctx.env[hdr.group(1)] = st
    if hdr and hdr.group(1) in ctx.env:
        open_idx = masked.find("(", span[0])
        args = split_args(masked, open_idx + 1, matching(masked, open_idx) - 1)
        if len(args) >= 2:
            k = interp.resolve_scalar(file, args[0], ctx)
            v = interp.resolve_scalar(file, args[1], ctx)
            if k is not None and k.is_literal and isinstance(k.value, str) and v is not None:
                ctx.env[hdr.group(1)].headers[k.value] = v
        return

    # 5. `name = expr` / `name := expr` / `var name = expr`
    assign = re.match(r"^(?:var\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*(?:,\s*[A-Za-z_][A-Za-z0-9_]*\s*)*\s*(?::=|=)\s*", text)
    if not assign:
        return
    name = assign.group(1)
    rhs_text = text[assign.end() :].strip()
    if not rhs_text:
        return
    rhs = (span[0] + text.index(rhs_text), span[0] + text.index(rhs_text) + len(rhs_text))
    ctx.env.pop(name, None)
    ctx.defs.pop(name, None)
    multi = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\s*(?:,\s*[A-Za-z_][A-Za-z0-9_]*)*\s*:=", text)
    if multi and "," in text[: text.index(":=")]:
        first = text[: text.index(":=")].split(",")[0].strip()
        do = re.match(r"^http\.DefaultClient\.Do\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)", rhs_text)
        if do:
            # `resp, err := http.DefaultClient.Do(req)` aliases the request, which
            # may still be awaiting its own resolution in `defs`.
            inner = do.group(1)
            if inner in ctx.env:
                ctx.env[first] = ctx.env[inner].clone()
            elif inner in ctx.defs:
                ctx.defs[first] = ctx.defs[inner]
            return
        marshal = re.match(r"^json\.Marshal\s*\(", rhs_text)
        if marshal:
            open_idx = rhs[0] + rhs_text.index("(")
            args = split_args(masked, open_idx + 1, matching(masked, open_idx) - 1)
            if args:
                ctx.marshal[first] = args[0]
            return
        st = interp.resolve_request(file, rhs, ctx)
        if st is not None:
            ctx.env[first] = st
        else:
            ctx.defs[first] = rhs
        return
    st = interp.resolve_request(file, rhs, ctx)
    if st is not None:
        ctx.env[name] = st
        return
    ctx.defs[name] = rhs


# --------------------------------------------------------------------------- #
# Candidates, fixtures, reporting
# --------------------------------------------------------------------------- #


@dataclass
class Candidate:
    file: str
    line: int
    test: str
    kind: str
    pos: int


@dataclass
class HelperSite:
    """A request construction inside a helper rather than a test.

    Upstream builds its requests in `newRequest`/`authRequest`; listing those
    sites keeps the report complete without inflating the extraction rate with
    code that asserts nothing of its own.
    """

    file: str
    line: int
    func: str
    site: str


@dataclass
class Skip:
    file: str
    line: int
    test: str
    kind: str
    reason: str
    detail: str


@dataclass
class Fixture:
    id: str
    domain: str
    slug: str
    method: str
    path: str
    path_params: dict[str, str]
    query: dict[str, str]
    headers: dict[str, str]
    actor: dict[str, Any]
    body: Any
    status: int
    json_subset: dict[str, Any]
    source: dict[str, Any]
    notes: list[str]
    via: str = "handler"
    bindings: dict[str, str] = field(default_factory=dict)


def request_site_positions(masked: str) -> list[tuple[int, str]]:
    sites: list[tuple[int, str]] = []
    for pattern, kind in SITE_PATTERNS:
        for m in re.finditer(pattern, masked):
            sites.append((m.start(), kind))
    sites.sort()
    return sites


def enclosing_test(funcs: list[GoFunc], pos: int) -> str:
    best = "?"
    for fn in funcs:
        if fn.body[0] <= pos < fn.body[1] and fn.name.startswith(("Test", "Benchmark", "Example")):
            best = fn.name
    return best


def state_before(checkpoints: list[tuple[int, WalkCtx]], pos: int) -> WalkCtx:
    """Variable state just before the statement containing ``pos``."""
    chosen = WalkCtx()
    for stop, ctx in checkpoints:
        if stop <= pos:
            chosen = ctx
        else:
            break
    return chosen


def response_var(masked: str, pos: int) -> Optional[str]:
    line_start = max(masked.rfind("\n", 0, pos), masked.rfind(";", 0, pos), masked.rfind("{", 0, pos)) + 1
    m = re.search(r"([A-Za-z_][A-Za-z0-9_]*)\s*(?:,\s*[A-Za-z_][A-Za-z0-9_]*)?\s*(?::=|=)\s*$", masked[line_start:pos])
    return m.group(1) if m else None


def following_statuses(masked: str, start: int, limit: int, var: str) -> list[int]:
    """Statuses compared against ``var`` before the next request site."""
    stop = min(limit, start + 4000)
    for pos, _kind in request_site_positions(masked):
        if pos > start:
            stop = min(stop, pos)
            break
    region = masked[start:stop]
    codes: list[int] = []
    for m in re.finditer(re.escape(var) + r"\.StatusCode\s*(?:!=|==)\s*([^;{\n]+)", region):
        codes.extend(statuses_in(m.group(1)))
    return codes


def statuses_in(text: str) -> list[int]:
    out: list[int] = []
    for m in re.finditer(r"http\.Status([A-Za-z]+)", text):
        code = STATUS_CODES.get(m.group(1))
        if code is not None:
            out.append(code)
    for m in re.finditer(r"(?<![\w.])(\d{3})(?![\w.])", text):
        out.append(int(m.group(1)))
    return out


def canonicalise(st: ReqState) -> tuple[str, dict[str, str], dict[str, str]]:
    """Materialise the path, naming placeholders after their URL params."""
    joined = "".join(st.pieces)
    path_part, _, query_part = joined.partition("?")
    path_params: dict[str, str] = {}
    for mark, val in st.markers.items():
        if mark not in path_part:
            continue
        name = None
        for k, v in st.urlparams.items():
            if v == val:
                name = k
                break
        if name is None:
            name = str(val.value).lstrip("$").split(".")[-1]
        path_part = path_part.replace(mark, "{%s}" % name)
        path_params[name] = val.value
    path = re.sub(r"/{2,}", "/", path_part) or "/"
    if len(path) > 1:
        path = path.rstrip("/")
    query: dict[str, str] = {}
    for part in query_part.split("&"):
        if not part:
            continue
        k, _, v = part.partition("=")
        if not k:
            continue
        for mark, val in st.markers.items():
            if mark in v:
                v = v.replace(mark, val.value)
        query[k] = v
    return path, path_params, query


def domain_of(path: str) -> str:
    """Fixture bucket, derived from the route (issues, agents, auth, ...)."""
    segs = [s for s in path.split("/") if s and not s.startswith("{")]
    if not segs:
        return "_root"
    head = segs[0]
    if head in ("api", "v1", "internal") and len(segs) > 1:
        head = segs[1]
    head = head.lstrip("_") or "_root"
    return re.sub(r"[^a-z0-9_]+", "_", head.lower())


class Extractor:
    def __init__(self, root: str, scan: tuple[str, ...], commit: str, commit_date: str):
        self.root = root
        self.scan = scan
        self.commit = commit
        self.commit_date = commit_date
        self.sources: dict[str, tuple[str, str, dict[int, str]]] = {}
        self.test_files: list[str] = []
        self.funcs: dict[str, GoFunc] = {}
        self.pkg_literals: dict[str, str] = {}
        self.fixtures: list[Fixture] = []
        self.skips: list[Skip] = []
        self.candidates: list[Candidate] = []
        self.helper: list[HelperSite] = []
        self.produced: dict[tuple[str, int], int] = {}
        self.recorder_sites = 0

    # -- load -------------------------------------------------------------- #

    def load(self) -> None:
        for sub in self.scan:
            base = os.path.join(self.root, sub)
            for dirpath, _dirs, filenames in os.walk(base):
                for fn in sorted(filenames):
                    if not fn.endswith(".go"):
                        continue
                    rel = os.path.relpath(os.path.join(dirpath, fn), self.root)
                    with open(os.path.join(dirpath, fn), encoding="utf-8", errors="replace") as fh:
                        src = fh.read()
                    masked, literals = mask_go(src)
                    self.sources[rel] = (src, masked, literals)
                    if fn.endswith("_test.go"):
                        self.test_files.append(rel)
        for rel, (_src, masked, _lits) in self.sources.items():
            for fn in find_functions(masked, rel):
                self.funcs.setdefault(fn.name, fn)
        self.pkg_literals = package_literals(self.sources)
        self.recorder_sites = sum(
            masked.count("httptest.NewRecorder()") for _src, masked, _lits in self.sources.values()
        )

    # -- run --------------------------------------------------------------- #

    def variants(self, masked: str, fn: GoFunc, pos: int, ctx: WalkCtx) -> list[WalkCtx]:
        """Contexts to try for one site: one per ``range []string`` element.

        Upstream writes its route/auth matrices as string tables; each element is
        an independent oracle, so the site is unrolled instead of being skipped
        for carrying a loop variable.
        """
        binding = loop_binding(masked, fn.file, fn.body, pos)
        if not binding:
            return [ctx]
        name, elements = binding
        out: list[WalkCtx] = []
        for element in elements:
            variant = ctx.clone()
            variant.defs[name] = element
            out.append(variant)
        return out or [ctx]

    def run(self) -> None:
        for rel in sorted(self.test_files):
            self.extract_file(rel)

    def extract_file(self, rel: str) -> None:
        src, masked, _lits = self.sources[rel]
        funcs = find_functions(masked, rel)
        interp = Interpreter(self.sources, self.funcs, self.pkg_literals)
        sites = request_site_positions(masked)
        if not sites:
            return
        test_level: set[int] = set()
        for fn in funcs:
            if fn.receiver or not fn.name.startswith(("Test", "Benchmark", "Example")):
                continue
            for pos, _kind in sites:
                if fn.body[0] <= pos < fn.body[1]:
                    test_level.add(pos)
        for pos, kind in sites:
            if pos not in test_level:
                self.helper.append(
                    HelperSite(rel, line_of(src, pos), enclosing_test(funcs, pos), kind)
                )
                continue
            self.candidates.append(Candidate(rel, line_of(src, pos), enclosing_test(funcs, pos), kind, pos))
        for fn in funcs:
            if fn.receiver or not fn.name.startswith(("Test", "Benchmark", "Example")):
                continue
            checkpoints = walk(rel, masked, interp, fn.body, WalkCtx())
            for pos, kind in [s for s in sites if fn.body[0] <= s[0] < fn.body[1]]:
                before = (len(self.fixtures), len(self.skips))
                for vctx in self.variants(masked, fn, pos, state_before(checkpoints, pos)):
                    try:
                        self.extract_site(rel, src, masked, interp, fn, pos, kind, vctx)
                    except Exception as exc:  # a crash must never hide a site
                        self.skips.append(
                            Skip(
                                rel,
                                line_of(src, pos),
                                fn.name,
                                kind,
                                "internal_error",
                                repr(exc)[:200],
                            )
                        )
                        break
                made = len(self.fixtures) - before[0]
                if made == 0 and len(self.skips) == before[1]:
                    self.skips.append(
                        Skip(
                            rel,
                            line_of(src, pos),
                            fn.name,
                            kind,
                            "helper_not_followed",
                            "site produced neither a fixture nor a reason",
                        )
                    )
                self.produced[(rel, pos)] = made

    def extract_site(
        self,
        rel: str,
        src: str,
        masked: str,
        interp: Interpreter,
        fn: GoFunc,
        pos: int,
        kind: str,
        ctx: WalkCtx,
    ) -> None:
        end = asi_end(masked, pos)
        chain = masked[pos:end]
        line = line_of(src, pos)

        def skip(reason: str, detail: str) -> None:
            self.skips.append(Skip(rel, line, fn.name, kind, reason, detail))

        if kind == "testutil.Call":
            call_open = pos + chain.index("(")
            args = split_args(masked, call_open + 1, matching(masked, call_open) - 1)
            if len(args) < 3:
                return skip("helper_not_followed", "testutil.Call with fewer than 3 arguments")
            req = interp.resolve_request(rel, args[2], ctx)
            want = re.search(r"\.Want(?:OneOf)?\s*\(", chain)
            if want is None:
                return skip("no_status_assertion", "no .Want(...) in the call chain")
            if chain[want.start() :].startswith(".WantOneOf"):
                return skip("ambiguous_status", "WantOneOf accepts several statuses")
            wopen = pos + want.end() - 1
            wargs = split_args(masked, wopen + 1, matching(masked, wopen) - 1)
            if len(wargs) != 1:
                return skip("ambiguous_status", ".Want() with %d arguments" % len(wargs))
            codes = sorted(set(statuses_in(masked[wargs[0][0] : wargs[0][1]])))
            if len(codes) != 1:
                return skip("ambiguous_status", ".Want() status tokens: %s" % codes)
            status = codes[0]
            subset = map_subset(masked, interp.src(rel)[2], end, fn.body[1])
        else:
            if kind == "http.Get":
                call_open = pos + chain.index("(")
                args = split_args(masked, call_open + 1, matching(masked, call_open) - 1)
                req = ReqState(method=Value("literal", "GET"))
                if not args:
                    return skip("path_not_literal", "http.Get() without a URL argument")
                path = interp.resolve_path(rel, args[0], ctx)
                if path is None:
                    req.problems.append("path_not_literal")
                else:
                    req.pieces, req.markers = path["pieces"], path["markers"]
            elif kind == "authRequest":
                req = interp.resolve_call(rel, (pos, end), "authRequest", ctx, 0)
            else:
                req = interp.resolve_request(rel, (pos, end), ctx)
            lhs = response_var(masked, pos)
            if lhs is None:
                return skip("no_status_assertion", "%s result is not bound to a variable" % kind)
            codes = sorted(set(following_statuses(masked, end, fn.body[1], lhs)))
            if not codes:
                return skip("no_status_assertion", "no status comparison on %s" % lhs)
            if len(codes) != 1:
                return skip("ambiguous_status", "status comparisons on %s: %s" % (lhs, codes))
            status = codes[0]
            subset = map_subset(masked, interp.src(rel)[2], end, fn.body[1])

        if req is None:
            return skip("request_var_unresolved", "request expression is not traceable")
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

        path, path_params, query = canonicalise(req)
        if MARK in path or any(MARK in v for v in query.values()):
            return skip("path_not_literal", "unsubstituted symbol marker in the URL")
        headers, actor = split_headers(req.headers)
        needed = symbols_in(path_params, query, actor, headers)
        unknown = sorted(needed - set(BINDABLE))
        if unknown:
            return skip(
                "value_unresolved",
                "fixture needs values the runner cannot bind: %s" % ", ".join(unknown),
            )
        via = "router" if kind in ("authRequest", "http.Get") else via_of(masked, kind, pos, end)
        fixture = Fixture(
            id="",
            domain=domain_of(path),
            slug="",
            method=req.method.value,
            path=path,
            path_params=path_params,
            query=query,
            headers=headers,
            actor=actor,
            body=req.body.value if req.body is not None else None,
            status=status,
            json_subset=subset,
            source={"file": rel, "line": line, "test": fn.name, "site": kind, "via": via},
            notes=[],
            via=via,
            bindings={s: BINDABLE[s] for s in sorted(needed)},
        )
        if via == "handler":
            fixture.notes.append(
                "upstream drove the handler directly; replayed here against the router, "
                "which also applies its middleware"
            )
        self.fixtures.append(fixture)


def via_of(masked: str, kind: str, pos: int, end: int) -> str:
    """Whether upstream exercised the router or a bare handler.

    `testutil.Call(t, testHandler.CreateIssue, req)` passes a *method*, so upstream
    never ran its own routing or middleware; `testutil.Call(t, router, req)` did.
    """
    if kind != "testutil.Call":
        return "handler"
    chain = masked[pos:end]
    open_idx = chain.index("(")
    args = split_args(masked, pos + open_idx + 1, matching(masked, pos + open_idx) - 1)
    if len(args) < 2:
        return "handler"
    return "handler" if "." in masked[args[1][0] : args[1][1]] else "router"

def split_headers(headers: dict[str, Value]) -> tuple[dict[str, str], dict[str, Any]]:
    """Separate transport headers from identity, and name the actor.

    Identity headers carry upstream's own DB fixture values; the runner binds them
    to a locally seeded identity.  Lifting them out is what lets "anonymous → 401"
    be replayed with no database at all.
    """
    plain: dict[str, str] = {}
    ident: dict[str, str] = {}
    for key, val in sorted(headers.items()):
        if val.value is None:
            continue
        if key.lower() in IDENTITY_HEADERS:
            ident[key] = val.value if isinstance(val.value, str) else json.dumps(val.value)
        else:
            plain[key] = val.value if isinstance(val.value, str) else json.dumps(val.value)
    lower = {k.lower() for k in ident}
    if lower & set(AGENT_HEADERS):
        kind = "agent"
    elif "authorization" in lower:
        kind = "token"
    elif ident:
        kind = "member"
    else:
        kind = "anonymous"
    actor: dict[str, Any] = {"kind": kind}
    if ident:
        actor["upstream_identity"] = ident
    return plain, actor


def literal_at(masked: str, literals: dict[int, str], start: int) -> Optional[tuple[Value, int]]:
    """Decode exactly one literal token at/after ``start``, with its end offset."""
    i = start
    while i < len(masked) and masked[i] in " \t\r\n":
        i += 1
    quote = masked[i : i + 1]
    if quote in ('"', "`"):
        close = masked.find(quote, i + 1)
        if close < 0 or i not in literals:
            return None
        return Value("literal", literals[i]), close + 1
    m = re.match(r"-?\d+(?:\.\d+)?", masked[i : i + 40])
    if m:
        text = m.group(0)
        return Value("literal", float(text) if "." in text else int(text)), i + m.end()
    for word, val in (("true", True), ("false", False), ("nil", None)):
        if masked[i : i + len(word)] == word and not IDENT.match(masked, i + len(word)):
            return Value("literal", val), i + len(word)
    return None


def map_subset(masked: str, literals: dict[int, str], start: int, limit: int) -> dict[str, Any]:
    """Read ``resp.Map()["k"] != <literal>`` assertions as a JSON subset.

    Deliberately narrow: only a *literal* right-hand side is provable.  A symbolic
    one means the assertion depends on upstream's own data fixtures, and a subset
    with the wrong value would be worse than no assertion at all.  An ``==``
    comparison asserts an *absent* value, which no subset can express.
    """
    stop = min(limit, start + 2000)
    out: dict[str, Any] = {}
    pattern = re.compile(r"\.Map\(\)\s*\[\s*\"([^\"]+)\"\s*\]")
    for m in re.finditer(pattern, masked[start:stop]):
        key = m.group(1)
        pos = start + m.end()
        # `got := resp.Map()["error"]` ... `if got != "..."`
        bind = re.search(r"([A-Za-z_]\w*)\s*(?:,\s*\w+\s*)?:=\s*$", masked[max(start, pos - 90) : pos])
        if bind:
            tail = masked[pos : pos + 400]
            cmp = re.search(r"\b" + re.escape(bind.group(1)) + r"\s*!=\s*", tail)
            if cmp:
                lit = literal_at(masked, literals, pos + cmp.end())
                if lit is not None:
                    out[key] = lit[0].value
            continue
        cmp = re.match(r"\s*[;,)]?\s*[A-Za-z_][A-Za-z0-9_]*\s*!=\s*", masked[pos : pos + 80])
        if not cmp:
            continue
        lit = literal_at(masked, literals, pos + cmp.end())
        if lit is not None:
            out[key] = lit[0].value
    return out


def loop_binding(
    masked: str, file: str, body: tuple[int, int], pos: int
) -> Optional[tuple[str, list[tuple[int, int]]]]:
    """The `range` element a site sits in, if it sits in one.

    Handles both `for _, p := range []string{...}` and upstream's more common
    `paths := []string{...}` followed by `for _, p := range paths`.
    """
    for m in LOOP_TABLE.finditer(masked, body[0], body[1]):
        brace = m.end() - 1
        close = matching(masked, brace)
        if not (brace < pos < close):
            continue
        return m.group(1), split_args(masked, brace + 1, close - 1)
    for m in re.finditer(r"for\s+(?:_,\s*)?([A-Za-z_]\w*)\s*:=\s*range\s+([A-Za-z_]\w*)\b", masked[body[0] : body[1]]):
        loop_var, table = m.group(1), m.group(2)
        brace = masked.index("{", body[0] + m.end())
        close = matching(masked, brace)
        if not (brace < pos < close):
            continue
        decl = re.search(
            r"(?:var\s+)?" + re.escape(table) + r"\s*(?::=|=)\s*\[\]string\s*\{",
            masked[body[0] : brace],
        )
        if not decl:
            return None
        table_brace = body[0] + decl.end() - 1
        return loop_var, split_args(masked, table_brace + 1, matching(masked, table_brace) - 1)
    return None


def symbols_in(*groups: Any) -> set[str]:
    found: set[str] = set()

    def collect(value: Any) -> None:
        if isinstance(value, str):
            if value.startswith("$"):
                found.add(value)
        elif isinstance(value, dict):
            for v in value.values():
                collect(v)
        elif isinstance(value, list):
            for v in value:
                collect(v)

    for g in groups:
        collect(g)
    return found


def build_stats(ex: Extractor) -> dict[str, Any]:
    by_domain: dict[str, int] = {}
    by_status: dict[str, int] = {}
    by_actor: dict[str, int] = {}
    by_via: dict[str, int] = {}
    offline = 0
    with_subset = 0
    for f in ex.fixtures:
        by_domain[f.domain] = by_domain.get(f.domain, 0) + 1
        by_status[str(f.status)] = by_status.get(str(f.status), 0) + 1
        by_actor[f.actor["kind"]] = by_actor.get(f.actor["kind"], 0) + 1
        by_via[f.via] = by_via.get(f.via, 0) + 1
        if f.actor["kind"] == "anonymous":
            offline += 1
        if f.json_subset:
            with_subset += 1
    by_reason: dict[str, int] = {}
    for s in ex.skips:
        by_reason[s.reason] = by_reason.get(s.reason, 0) + 1
    by_kind: dict[str, int] = {}
    for c in ex.candidates:
        by_kind[c.kind] = by_kind.get(c.kind, 0) + 1
    extracted_sites = sum(1 for c in ex.candidates if ex.produced.get((c.file, c.pos)))
    total = len(ex.candidates)
    return {
        "schema_version": SCHEMA_VERSION,
        "candidate_sites": {"total": total, "by_kind": order(by_kind)},
        "fixtures": {
            "total": len(ex.fixtures),
            "by_domain": order(by_domain),
            "by_status": order(by_status),
            "by_actor_kind": order(by_actor),
            "by_via": order(by_via),
            "offline_decidable": offline,
            "with_json_subset": with_subset,
        },
        "scope": {
            "in_scope": [
                "testutil.Call(t, handler, request)",
                "authRequest(t, method, path, body)",
                "http.Get(url)",
                "http.NewRequest(method, url, body)",
            ],
            "not_extracted": {
                "description": (
                    "handler-direct calls that pass an httptest.ResponseRecorder and read "
                    "w.Code, e.g. testHandler.CreateIssue(w, req); upstream bypasses its own "
                    "router there, so a replayed status would differ for test-level reasons"
                ),
                "recorder_constructions": ex.recorder_sites,
            },
        },
        "skipped": {"total": len(ex.skips), "by_reason": order(by_reason)},
        "extraction_rate": {
            "extracted_sites": extracted_sites,
            "candidate_sites": total,
            "rate": round(extracted_sites / total, 4) if total else 0.0,
        },
    }


def order(d: dict[str, Any]) -> dict[str, Any]:
    return {k: d[k] for k in sorted(d)}


def slugify(text: str) -> str:
    return re.sub(r"[^A-Za-z0-9]+", "-", text).strip("-") or "case"


def number_fixtures(fixtures: list[Fixture]) -> None:
    """Assign stable ids and file slugs, sorted for a reproducible tree."""
    fixtures.sort(key=lambda f: (f.domain, f.source["file"], f.source["line"], f.id))
    per_domain: dict[str, int] = {}
    for f in fixtures:
        per_domain[f.domain] = per_domain.get(f.domain, 0) + 1
        ordinal = per_domain[f.domain]
        line = f.source["line"]
        f.id = "%s/%s@%s:%d#%d" % (f.domain, f.source["test"], f.source["file"], line, ordinal)
        f.slug = "%03d-%s-L%d" % (ordinal, slugify(f.source["test"])[:48], line)


def fixture_json(f: Fixture, commit: str) -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "id": f.id,
        "method": f.method,
        "path": f.path,
        "path_params": f.path_params,
        "query": f.query,
        "headers": f.headers,
        "actor": f.actor,
        "body": f.body,
        "expect": {"status": f.status, "json_subset": f.json_subset, "headers": {}},
        "source": {
            "file": f.source["file"],
            "line": f.source["line"],
            "test": f.source["test"],
            "site": f.source["site"],
            "via": f.via,
            "commit": commit,
        },
        "extraction": {"notes": f.notes, "bindings": f.bindings},
    }


def emit(ex: Extractor, out_dir: str, commit: str, commit_date: str, scan: tuple[str, ...]) -> None:
    """Write the whole tree; called on a temp dir so --check can diff it."""
    number_fixtures(ex.fixtures)
    for f in ex.fixtures:
        d = os.path.join(out_dir, f.domain)
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, f.slug + ".json"), "w", encoding="utf-8") as fh:
            json.dump(fixture_json(f, commit), fh, indent=2, ensure_ascii=False, sort_keys=False)
            fh.write("\n")

    made: dict[tuple[str, int], int] = {}
    for f in ex.fixtures:
        key = (f.source["file"], f.source["line"])
        made[key] = made.get(key, 0) + 1
    rows: list[tuple[str, int, str, str, str, str, str]] = []
    for c in ex.candidates:
        n = ex.produced.get((c.file, c.pos), 0)
        if n:
            rows.append((c.file, c.line, c.test, c.kind, "extracted", "-", "%d fixture(s)" % n))
            continue
        why = [s for s in ex.skips if s.file == c.file and s.line == c.line and s.kind == c.kind]
        reason = why[0].reason if why else "helper_not_followed"
        detail = why[0].detail if why else "no recorded outcome"
        rows.append((c.file, c.line, c.test, c.kind, "skipped", reason, detail.replace("\t", " ").replace("\n", " ")))
    for h in ex.helper:
        rows.append(
            (
                h.file,
                h.line,
                h.func or "-",
                h.site,
                "helper_site",
                "-",
                "request built inside a test helper, asserted by its callers",
            )
        )
    rows.sort()
    with open(os.path.join(out_dir, "extraction-report.tsv"), "w", encoding="utf-8") as fh:
        fh.write("file\tline\ttest\tsite\toutcome\treason\tdetail\n")
        for r in rows:
            fh.write("\t".join(str(x) for x in r) + "\n")

    stats = build_stats(ex)
    # Keep the report and the histogram from ever disagreeing: the report rows are
    # the single record of what happened to each site, so they are the source.
    by_reason: dict[str, int] = {}
    for r in rows:
        if r[4] == "skipped":
            by_reason[r[5]] = by_reason.get(r[5], 0) + 1
    stats["skipped"]["by_reason"] = order(by_reason)
    stats["skipped"]["total"] = sum(by_reason.values())
    stats["extraction_rate"]["extracted_sites"] = len(made)
    stats["extraction_rate"]["candidate_sites"] = len(ex.candidates)
    stats["extraction_rate"]["rate"] = (
        round(len(made) / len(ex.candidates), 4) if ex.candidates else 0.0
    )
    with open(os.path.join(out_dir, "stats.json"), "w", encoding="utf-8") as fh:
        json.dump(stats, fh, indent=2, sort_keys=True)
        fh.write("\n")

    with open(os.path.join(out_dir, "PIN"), "w", encoding="utf-8") as fh:
        fh.write(
            "# Upstream revision the golden fixtures were extracted from.\n"
            "# Regenerate with: python3 scripts/extract_upstream_fixtures.py"
            " --upstream <checkout>\n"
            "upstream_repo https://github.com/louloulin/multica\n"
            "upstream_commit %s\n"
            "upstream_date %s\n"
            "extractor scripts/extract_upstream_fixtures.py\n"
            "schema_version %d\n"
            "scan_dirs %s\n" % (commit, commit_date, SCHEMA_VERSION, " ".join(scan))
        )


def tree_snapshot(root: str) -> dict[str, bytes]:
    snap: dict[str, bytes] = {}
    for dirpath, _dirs, files in os.walk(root):
        for fn in sorted(files):
            full = os.path.join(dirpath, fn)
            with open(full, "rb") as fh:
                snap[os.path.relpath(full, root)] = fh.read()
    return snap


def diff_trees(want: dict[str, bytes], got: dict[str, bytes]) -> list[str]:
    problems: list[str] = []
    for rel in sorted(set(want) | set(got)):
        if rel not in got:
            problems.append("missing: %s" % rel)
        elif rel not in want:
            problems.append("unexpected: %s" % rel)
        elif want[rel] != got[rel]:
            problems.append("differs: %s" % rel)
    return problems


def git_info(root: str) -> tuple[str, str]:
    def run(*args: str) -> str:
        out = subprocess.run(
            ["git", "-C", root, *args], capture_output=True, text=True, check=True
        )
        return out.stdout.strip()

    try:
        commit = run("rev-parse", "HEAD")
        date = run("log", "-1", "--format=%cI")
    except (subprocess.CalledProcessError, FileNotFoundError) as exc:
        raise SystemExit("cannot read git metadata from %s: %s" % (root, exc))
    return commit, date


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--upstream", required=True, help="path to an upstream multica checkout")
    ap.add_argument("--out", default=os.path.join("contracts", "golden"), help="fixture output dir")
    ap.add_argument("--scan", nargs="+", default=list(DEFAULT_SCAN), help="upstream subtrees to scan")
    ap.add_argument("--check", action="store_true", help="verify the tree is byte-identical; write nothing")
    args = ap.parse_args(argv)

    root = os.path.abspath(args.upstream)
    if not os.path.isdir(root):
        print("error: --upstream %s is not a directory" % root, file=sys.stderr)
        return 2
    scan = tuple(args.scan)
    for sub in scan:
        if not os.path.isdir(os.path.join(root, sub)):
            print("error: %s has no %s/" % (root, sub), file=sys.stderr)
            return 2

    commit, commit_date = git_info(root)
    ex = Extractor(root, scan, commit, commit_date)
    ex.load()
    ex.run()

    out_dir = os.path.abspath(args.out)
    with tempfile.TemporaryDirectory() as tmp:
        staged = os.path.join(tmp, "golden")
        os.makedirs(staged)
        emit(ex, staged, commit, commit_date, scan)
        stats = json.load(open(os.path.join(staged, "stats.json"), encoding="utf-8"))
        fresh = tree_snapshot(staged)

        if args.check:
            if not os.path.isdir(out_dir):
                print("error: %s does not exist; run without --check first" % out_dir, file=sys.stderr)
                return 1
            problems = diff_trees(fresh, tree_snapshot(out_dir))
            if problems:
                for p in problems[:40]:
                    print(p, file=sys.stderr)
                if len(problems) > 40:
                    print("... and %d more" % (len(problems) - 40), file=sys.stderr)
                print("error: %d fixture-tree differences" % len(problems), file=sys.stderr)
                return 1
            print("ok: %d fixtures reproduce byte-identically" % stats["fixtures"]["total"])
            return 0

        if os.path.isdir(out_dir):
            shutil.rmtree(out_dir)
        os.makedirs(os.path.dirname(out_dir), exist_ok=True)
        shutil.move(staged, out_dir)

    rate = stats["extraction_rate"]
    print(
        "wrote %d fixtures from %d candidate sites (抽取率 %.1f%%), %d skipped"
        % (
            stats["fixtures"]["total"],
            rate["candidate_sites"],
            100.0 * rate["rate"],
            stats["skipped"]["total"],
        )
    )
    print("  report: %s" % os.path.join(args.out, "extraction-report.tsv"))
    for reason, count in stats["skipped"]["by_reason"].items():
        print("  skipped/%-38s %d" % (reason, count))
    return 0


if __name__ == "__main__":
    sys.exit(main())

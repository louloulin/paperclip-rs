"""Splitting a migration file into top-level statements, and extension gating.

Everything here works on *text*: nothing in this module ever talks to a server.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from .contract import EXTENSION_MARKERS, SchemaToolError, _CREATE_EXTENSION_RE
# --------------------------------------------------------------------------- #
# SQL statement splitting
# --------------------------------------------------------------------------- #

_DOLLAR_TAG_RE = re.compile(r"\$([A-Za-z_][A-Za-z0-9_]*)?\$")


@dataclass(frozen=True)
class Statement:
    """One top-level SQL statement of a migration file."""

    sql: str  # without the trailing semicolon, surrounding whitespace stripped
    line: int  # 1-based line of its first character in the source file

    @property
    def text_with_terminator(self) -> str:
        return self.sql + ";"


def _skip_quoted(text: str, i: int, quote: str, *, backslash_escapes: bool) -> int:
    n = len(text)
    i += 1
    while i < n:
        c = text[i]
        if backslash_escapes and c == "\\":
            i += 2
            continue
        if c == quote:
            if i + 1 < n and text[i + 1] == quote:  # doubled quote is an escape
                i += 2
                continue
            return i + 1
        i += 1
    return n


def split_sql_statements(sql: str) -> list[Statement]:
    """Split a migration file into top-level statements.

    Understands single quotes, `E'...'` backslash escapes, quoted identifiers,
    `--` line comments, *nested* `/* */` block comments and `$tag$ ... $tag$`
    dollar quoting — the four constructs whose absence would silently corrupt a
    statement.  Comment-only spans are dropped from a statement's edges, but
    comment text is kept inside it (it is part of the statement's source).

    Raises `SchemaToolError` for an unterminated dollar-quote, because a
    mis-split migration is worse than a dead stop.
    """
    line_starts = [0]
    for m in re.finditer("\n", sql):
        line_starts.append(m.end())

    def line_of(pos: int) -> int:
        import bisect

        return bisect.bisect_right(line_starts, pos)

    statements: list[Statement] = []
    i, n = 0, len(sql)
    start: int | None = None

    def flush(end: int) -> None:
        nonlocal start
        if start is None:
            return
        chunk = sql[start:end]
        stripped = chunk.strip()
        if stripped:
            offset = start + (len(chunk) - len(chunk.lstrip()))
            statements.append(Statement(sql=stripped, line=line_of(offset)))
        start = None

    while i < n:
        c = sql[i]

        if c == "-" and i + 1 < n and sql[i + 1] == "-":
            j = sql.find("\n", i)
            i = n if j < 0 else j
            continue

        if c == "/" and i + 1 < n and sql[i + 1] == "*":
            depth, i = 1, i + 2
            while i < n and depth:
                if sql.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif sql.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            if depth:
                raise SchemaToolError("unterminated /* block comment")
            continue

        if c == "'":
            prev = sql[i - 1] if i else ""
            prev2 = sql[i - 2] if i >= 2 else ""
            is_escaped_string = prev in ("E", "e") and not (prev2.isalnum() or prev2 == "_")
            if start is None:
                start = i
            i = _skip_quoted(sql, i, "'", backslash_escapes=is_escaped_string)
            continue

        if c == '"':
            if start is None:
                start = i
            i = _skip_quoted(sql, i, '"', backslash_escapes=False)
            continue

        if c == "$":
            m = _DOLLAR_TAG_RE.match(sql, i)
            if m:
                tag = m.group(0)
                j = sql.find(tag, m.end())
                if j < 0:
                    raise SchemaToolError(f"unterminated dollar quote {tag} at line {line_of(i)}")
                if start is None:
                    start = i
                i = j + len(tag)
                continue

        if c == ";":
            flush(i)
            i += 1
            continue

        if not c.isspace() and start is None:
            start = i

        i += 1

    flush(n)
    return statements


def code_only(sql: str) -> str:
    """Blank out comments and string *literals*, keeping code and quoted identifiers.

    Used for dependency detection only; nothing is ever executed from the result.

    * `-- line comments` and nested `/* block comments */` are dropped, so prose
      about an extension is never mistaken for a dependency;
    * `'...'` / `E'...'` literals are dropped for the same reason — an extension
      name inside a RAISE message or a stored value is data, not a dependency;
    * `"..."` quoted identifiers are **kept**, because `CREATE EXTENSION "pgcrypto"`
      and a table column typed at an operator class spell the dependency in quotes;
    * `$$ ... $$` bodies are kept verbatim, because a function body that calls
      `cron.schedule()` genuinely depends on pg_cron.
    """
    out: list[str] = []
    i, n = 0, len(sql)
    while i < n:
        c = sql[i]
        if c == "-" and i + 1 < n and sql[i + 1] == "-":
            j = sql.find("\n", i)
            i = n if j < 0 else j
            out.append(" ")
            continue
        if c == "/" and i + 1 < n and sql[i + 1] == "*":
            depth, i = 1, i + 2
            while i < n and depth:
                if sql.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif sql.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            out.append(" ")
            continue
        if c == "'":
            prev = sql[i - 1] if i else ""
            prev2 = sql[i - 2] if i >= 2 else ""
            escapes = prev in ("E", "e") and not (prev2.isalnum() or prev2 == "_")
            i = _skip_quoted(sql, i, "'", backslash_escapes=escapes)
            out.append("''")
            continue
        out.append(c)
        i += 1
    return "".join(out)


def required_extensions(sql: str) -> set[str]:
    return {m.group(1).lower() for m in _CREATE_EXTENSION_RE.finditer(code_only(sql))}


def assert_extension_markers_cover(extensions: set[str]) -> None:
    """A new upstream extension dependency must not slip through unnoticed."""
    unknown = sorted(e for e in extensions if e not in EXTENSION_MARKERS)
    if unknown:
        raise SchemaToolError(
            "these extensions appear in the vendored migrations but have no entry in "
            f"EXTENSION_MARKERS: {', '.join(unknown)}.  Add their markers (extension "
            "name, operator classes, qualified calls) to scripts/schema_snapshot.py, "
            "then rerun."
        )


def unavailable_extension_markers(sql: str, available: set[str]) -> str | None:
    """Return the marker of the first missing extension this statement needs."""
    body = code_only(sql).lower()
    for ext, markers in EXTENSION_MARKERS.items():
        if ext in available:
            continue
        for marker in markers:
            if marker in body:
                return f"{ext}:{marker}"
    return None


def objects_created_by(sql: str) -> list[tuple[str, str]]:
    """Best-effort list of `(kind, name)` a statement was going to create.

    Only used to make the deviation registry actionable at object granularity
    (`idx_issue_properties_bigm`, not "line 20 of some file").  A `DO $$ ... $$`
    guard is searched for the `CREATE INDEX` / `CREATE EXTENSION` statements it
    wraps, which is exactly how the upstream pg_bigm/pg_cron guards are written.
    """
    body = code_only(sql)
    found: list[tuple[str, str]] = []
    for m in re.finditer(
        r"CREATE\s+(?:UNIQUE\s+)?INDEX\s+(?:CONCURRENTLY\s+)?(?:IF\s+NOT\s+EXISTS\s+)?"
        r"([A-Za-z_\"][A-Za-z0-9_\"$.]*)",
        body,
        re.IGNORECASE,
    ):
        found.append(("index", m.group(1).strip('"')))
    for m in _CREATE_EXTENSION_RE.finditer(body):
        found.append(("extension", m.group(1).lower()))
    seen: set[tuple[str, str]] = set()
    unique: list[tuple[str, str]] = []
    for item in found:
        if item not in seen:
            seen.add(item)
            unique.append(item)
    return unique

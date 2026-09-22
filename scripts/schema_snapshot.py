#!/usr/bin/env python3
"""Schema snapshot primitives shared by `build_upstream_schema.py` and `schema_drift.py`.

Two things live here, and nothing else:

1. **Applying a migration directory to a scratch database.**  Migrations are
   replayed file by file, in file-name order, in *autocommit* mode (no
   `-1/--single-transaction`): 233 of the 560 upstream migrations contain
   `CREATE INDEX CONCURRENTLY`, which PostgreSQL refuses to run inside a
   transaction block.  Statements that cannot run on this PostgreSQL instance
   are **skipped and recorded**, never skipped silently — see
   `ApplyReport.skipped` and `contracts/upstream-apply-exceptions.tsv`.

2. **Snapshotting the resulting schema into a comparable form.**  Everything is
   read back through `information_schema` / `pg_catalog` (never by parsing the
   migration text), normalised by the rules in `NORMALIZATION`, and emitted as a
   flat, sorted list of `{"kind", "key", "def"}` objects.  `schema_drift.py`
   diffs two such snapshots; `build_upstream_schema.py` also renders the same
   database through `pg_dump` for a human-readable contract.

Normalisation contract (kept in one place on purpose — a drift report is only
as trustworthy as this list)
---------------------------------------------------------------------------
* **Server**: PostgreSQL 16 (`16.15` on the machine these numbers came from).
  `format_type()` output is PG-version dependent, hence the pin.  A different
  major version may legitimately produce a different snapshot.
* **Schema**: `public` only.  Every key is unqualified, and the `public.`
  qualifier is stripped from every stored definition.
* **Type aliases**: `format_type(atttypid, atttypmod)` — so `varchar(255)` is
  stored as `character varying(255)`, and `varchar` (no length) as
  `character varying`.  This is what makes a hand-written `VARCHAR(255)` and an
  upstream `character varying(255)` compare equal.
* **Defaults**: `pg_get_expr(adbin, adrelid)` with whitespace collapsed.
  PostgreSQL renders these canonically (`'{}'::jsonb`, `now()`,
  `nextval('issue_seq'::regclass)`), so an equivalent-but-differently-written
  default still compares equal.
* **Sequences**: `serial` columns are snapshotted twice — through the column
  default (`nextval(...)`) and through the sequence object itself.
* **Extension members**: objects owned by an extension (`pg_depend.deptype='e'`)
  are excluded, because `pg_dump` excludes them too; the extension set itself is
  recorded in `meta.extensions`.  Otherwise the 67 functions pgcrypto/pg_trgm
  install into `public` would masquerade as migration-defined objects.
* **Constraint-backed indexes**: excluded.  A PRIMARY KEY / UNIQUE / EXCLUDE
  constraint owns an index, and `pg_dump` emits it as part of the constraint;
  counting both would double-report every primary key.
* **Functions**: `pg_get_functiondef()`, `public.` stripped, line endings
  normalised.  Bodies are kept verbatim (comments included): in this project a
  function body *is* part of the contract.
* **Columns** carry `attnum`, so a reordering is a difference, not a no-op.
* **Constraints** carry `convalidated`, because a `NOT VALID` CHECK/FK is a weaker
  contract than a validated one (`478` adds two `NOT VALID` constraints on
  `issue_status`; the repo adding the same constraint validated is a *different*
  schema even though `pg_get_constraintdef` matches).

Only the Python 3 standard library plus the `psql` / `pg_dump` binaries are
required; no network access, no hard-coded credentials.
"""

from __future__ import annotations

import hashlib
import re
import subprocess
import sys
import urllib.parse
from dataclasses import dataclass, field
from pathlib import Path

# --------------------------------------------------------------------------- #
# normalisation contract, in machine-readable form (embedded in every snapshot)
# --------------------------------------------------------------------------- #

NORMALIZATION: dict[str, str] = {
    "server": "PostgreSQL 16 (format_type()/pg_get_*def() output is version dependent)",
    "schema": "public only; keys are unqualified and the public. qualifier is stripped from definitions",
    "type_aliases": "format_type(atttypid, atttypmod) — varchar(255) == character varying(255)",
    "defaults": "pg_get_expr(adbin, adrelid), whitespace collapsed",
    "sequences": "serial/identity columns are visible via their nextval() default and as sequence objects",
    "extension_members": "objects with pg_depend.deptype='e' are excluded; the extension set is recorded instead",
    "constraint_indexes": "indexes backing a PK/UNIQUE/EXCLUDE constraint are excluded — the constraint row covers them",
    "functions": "pg_get_functiondef(), public. stripped, line endings normalised, bodies verbatim",
    "whitespace": "runs of whitespace inside definitions collapse to one space",
}

SNAPSHOT_FORMAT = 1

#: extensions whose objects are not portable, and the markers that identify a
#: statement depending on them.  A statement mentioning a marker of an extension
#: that is *not installed* is skipped up front and recorded, instead of being left
#: to fail (or to be swallowed by an upstream `DO ... EXCEPTION` guard).
#:
#: Markers are matched against **code only** (`code_only()` blanks comments *and*
#: single-quoted string literals), because that is what a dependency is.  Upstream
#: migration 103 is the cautionary example: its data guard mentions "pg_cron is not
#: running" inside a *message string*, which is not a dependency, while the sibling
#: `DO` block that really calls `cron.unschedule()` is one.  Matching raw text would
#: have skipped both and quietly dropped the guard.
#:
#: Note what is deliberately **not** a marker: `gen_random_uuid()`, `digest()` and
#: `crypt()` are core PostgreSQL since 14, so depending on them does not require
#: pgcrypto on PostgreSQL 16 (`gen_random_uuid` alone appears in 59 vendored files).
#: Listing them as pgcrypto markers would have skipped most of the schema on a
#: pgcrypto-less server.
#:
#: Only the extensions the vendored set actually uses are listed — 4 of them, exactly
#: matching the `CREATE EXTENSION` statements `required_extensions()` finds.
#: `assert_extension_markers_cover()` fails loudly if upstream starts using another,
#: so this table cannot rot silently.
EXTENSION_MARKERS: dict[str, tuple[str, ...]] = {
    "pg_bigm": ("pg_bigm", "gin_bigm_ops", "gist_bigm_ops"),
    "pg_cron": ("pg_cron", "cron."),
    "pg_trgm": ("pg_trgm", "gin_trgm_ops", "gist_trgm_ops"),
    "pgcrypto": ("pgp_", "crypt(", "digest("),
}

_CREATE_EXTENSION_RE = re.compile(
    r"""CREATE\s+EXTENSION\s+(?:IF\s+NOT\s+EXISTS\s+)?["']?([A-Za-z0-9_.-]+)["']?""",
    re.IGNORECASE,
)


class SchemaToolError(RuntimeError):
    """Anything the operator has to fix: bad URL, bad migration, failing psql."""


# --------------------------------------------------------------------------- #
# connection URLs
# --------------------------------------------------------------------------- #


def parse_db_url(raw: str) -> str:
    """Validate a libpq URL and return it unchanged (used for error messages)."""
    if not raw:
        raise SchemaToolError("no database URL: pass --db-url or set MULTICA_TEST_DATABASE_URL")
    split = urllib.parse.urlsplit(raw)
    if split.scheme not in ("postgres", "postgresql"):
        raise SchemaToolError(
            f"--db-url must be a postgres:// URL (got scheme {split.scheme!r}); "
            "libpq keyword strings are not supported"
        )
    return raw


def _netloc(split: urllib.parse.SplitResult, *, drop_password: bool) -> str:
    user = urllib.parse.quote(split.username or "", safe="")
    netloc = f"{user}@" if user else ""
    host = split.hostname or ""
    if ":" in host:  # IPv6 literal
        host = f"[{host}]"
    netloc += host
    if split.port:
        netloc += f":{split.port}"
    if not drop_password and split.password:
        netloc = netloc.replace("@", f":{urllib.parse.quote(split.password, safe='')}@", 1)
    return netloc


def url_with_database(raw: str, database: str) -> str:
    """Return `raw` pointed at `database` instead."""
    split = urllib.parse.urlsplit(parse_db_url(raw))
    return urllib.parse.urlunsplit(
        (split.scheme, _netloc(split, drop_password=False), "/" + database, split.query, "")
    )


def database_name(raw: str) -> str:
    return urllib.parse.urlsplit(parse_db_url(raw)).path.lstrip("/") or "postgres"


def _argv_conn(raw: str) -> tuple[str, dict[str, str]]:
    """Split a URL into (password-free URL for argv, extra environment).

    The password never appears in `ps` output; it travels in `PGPASSWORD` for
    the child process only.  Nothing here is ever written to disk.
    """
    split = urllib.parse.urlsplit(parse_db_url(raw))
    env: dict[str, str] = {}
    if split.password:
        env["PGPASSWORD"] = urllib.parse.unquote(split.password)
    clean = urllib.parse.urlunsplit(
        (split.scheme, _netloc(split, drop_password=True), split.path, split.query, "")
    )
    return clean, env


def redact(raw: str) -> str:
    """`postgres://u:secret@h/db` → `postgres://u:***@h/db`, for logs and docs."""
    split = urllib.parse.urlsplit(raw)
    if not split.password:
        return raw
    return raw.replace(f":{split.password}@", ":***@", 1)


def _run(argv: list[str], env_extra: dict[str, str], stdin: str | None, timeout: int | None):
    env = None
    if env_extra:
        import os

        env = dict(os.environ)
        env.update(env_extra)
    try:
        return subprocess.run(
            argv, input=stdin, capture_output=True, text=True, timeout=timeout, env=env
        )
    except FileNotFoundError as exc:  # pragma: no cover - environment problem
        raise SchemaToolError(f"{argv[0]} not found on PATH: {exc}") from exc


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


# --------------------------------------------------------------------------- #
# psql / pg_dump plumbing
# --------------------------------------------------------------------------- #


@dataclass
class SkippedStatement:
    file: str
    line: int
    category: str  # "extension-unavailable" | "unsupported-statement"
    reason: str
    statement: str
    objects: list[tuple[str, str]] = field(default_factory=list)


def skip_object_keys(skip: SkippedStatement) -> list[tuple[str, str]]:
    """Object keys a skipped statement was going to create.

    A statement whose DDL the extractor could not parse yields one synthetic key,
    `statement:<file>:<line>`.  That fallback is what stops a skip from escaping the
    deviation registry: every skip maps to at least one key, every key must be
    registered in `contracts/schema-deviations.tsv`, and `schema_drift.py` fails on an
    unregistered one.  `build_upstream_schema.py` renders these same keys into
    `contracts/upstream-apply-exceptions.tsv`, so the two files agree by construction.
    """
    return skip.objects or [("statement", f"{skip.file}:{skip.line}")]


@dataclass
class ApplyReport:
    applied: list[str] = field(default_factory=list)
    skipped: list[SkippedStatement] = field(default_factory=list)
    notices: list[tuple[str, str]] = field(default_factory=list)  # (file, notice text)
    statements: int = 0

    @property
    def unexpected(self) -> list[SkippedStatement]:
        return [s for s in self.skipped if s.category != "extension-unavailable"]

    def objects_skipped(self) -> dict[tuple[str, str], list[SkippedStatement]]:
        by_object: dict[tuple[str, str], list[SkippedStatement]] = {}
        for skip in self.skipped:
            for obj in skip_object_keys(skip):
                by_object.setdefault(obj, []).append(skip)
        return by_object


class ScratchDatabase:
    """A throwaway database owned by one script run.

    The URL passed in points at *some* existing database on the target server (its
    credentials and host are reused); the scratch database is created and, unless
    `keep` is set, dropped again on the way out.
    """

    def __init__(self, base_url: str, name: str, *, keep: bool = False, timeout: int = 600):
        self.base_url = parse_db_url(base_url)
        self.name = name
        self.keep = keep
        self.timeout = timeout
        self.url = url_with_database(self.base_url, name)
        self._created = False

    def __enter__(self) -> "ScratchDatabase":
        self.server_sql(f'DROP DATABASE IF EXISTS "{self.name}"')
        self.server_sql(f'CREATE DATABASE "{self.name}"')
        self._created = True
        return self

    def __exit__(self, *exc) -> None:
        if self._created and not self.keep:
            try:
                self.server_sql(f'DROP DATABASE IF EXISTS "{self.name}"')
            except SchemaToolError as err:  # pragma: no cover - cleanup is best effort
                print(f"warning: could not drop scratch database {self.name}: {err}", file=sys.stderr)

    def server_sql(self, sql: str) -> None:
        res = self.run_psql(self.base_url, ["-c", sql])
        if res.returncode != 0:
            raise SchemaToolError(f"psql failed ({res.returncode}) on {sql!r}: {res.stderr.strip()}")

    def run_psql(self, db_url: str, args: list[str], stdin: str | None = None):
        conn, env = _argv_conn(db_url)
        argv = ["psql", "-X", "-q", "-v", "ON_ERROR_STOP=1", "--no-psqlrc", "-d", conn, *args]
        return _run(argv, env, stdin, self.timeout)

    def query(self, sql: str) -> list[list[str]]:
        """Run a query with unaligned/tab output and return non-empty rows split on tabs."""
        res = self.run_psql(self.url, ["-tA", "-F", "\t", "-c", sql])
        if res.returncode != 0:
            raise SchemaToolError(f"query failed: {res.stderr.strip()}\nSQL: {sql}")
        rows = []
        for line in res.stdout.splitlines():
            if line == "":
                continue
            rows.append(line.split("\t"))
        return rows

    def scalar(self, sql: str) -> str:
        rows = self.query(sql)
        return rows[0][0] if rows else ""

    def server_version(self) -> str:
        return self.scalar("SHOW server_version")

    def installed_extensions(self) -> set[str]:
        return {r[0].lower() for r in self.query("SELECT extname FROM pg_extension")}

    def available_extensions(self) -> set[str]:
        return {r[0].lower() for r in self.query("SELECT name FROM pg_available_extensions")}

    def pg_dump_schema(self) -> str:
        conn, env = _argv_conn(self.url)
        argv = [
            "pg_dump",
            "-d",
            conn,
            "--schema-only",
            "--schema=public",
            "--no-owner",
            "--no-privileges",
            "--no-comments",
        ]
        res = _run(argv, env, None, self.timeout)
        if res.returncode != 0:
            raise SchemaToolError(f"pg_dump failed: {res.stderr.strip()}")
        return res.stdout


# --------------------------------------------------------------------------- #
# applying migrations
# --------------------------------------------------------------------------- #

_NOTICE_RE = re.compile(r"^NOTICE:\s*(.*)$", re.MULTILINE)
_STDIN_ERR_RE = re.compile(r"<stdin>:(\d+):\s*ERROR:\s*(.*)", re.MULTILINE)
_TXN_CONTROL_RE = re.compile(r"^(BEGIN|START\s+TRANSACTION|COMMIT|ROLLBACK|END)\b", re.IGNORECASE)


def apply_migrations(
    db: ScratchDatabase,
    paths: list[Path],
    *,
    available: set[str],
    strict: bool = True,
) -> ApplyReport:
    """Replay `paths` (already in the order to apply them) into `db`.

    File order is the caller's; each file is fed to one psql process in
    autocommit mode.  Statements needing an extension this instance does not
    have are skipped *before* running (and recorded).  If a file still fails, the
    failure is located by psql's `<stdin>:<line>` report, recorded, and the file
    resumes at the next statement — earlier statements of that file are not
    replayed, so a partial failure cannot double-apply anything.
    """
    report = ApplyReport()
    for path in paths:
        text = path.read_text(encoding="utf-8")
        for ext in required_extensions(text):
            assert_extension_markers_cover({ext})
        statements = split_sql_statements(text)
        if not statements:
            report.applied.append(path.name)
            continue

        runnable: list[Statement] = []
        for stmt in statements:
            report.statements += 1
            marker = unavailable_extension_markers(stmt.sql, available)
            if marker is not None:
                report.skipped.append(
                    SkippedStatement(
                        file=path.name,
                        line=stmt.line,
                        category="extension-unavailable",
                        reason=f"extension not installed on this server ({marker})",
                        statement=stmt.sql,
                        objects=objects_created_by(stmt.sql),
                    )
                )
                continue
            m = _TXN_CONTROL_RE.match(stmt.sql)
            if m:
                raise SchemaToolError(
                    f"{path.name}:{stmt.line}: top-level transaction control ({m.group(1)}) is not "
                    "supported by this runner — it applies each statement in autocommit mode so that "
                    "CREATE INDEX CONCURRENTLY works. Split the file or extend the runner first."
                )
            runnable.append(stmt)

        _run_file(db, path, runnable, report, strict=strict)
        report.applied.append(path.name)
    return report


def _run_file(
    db: ScratchDatabase,
    path: Path,
    runnable: list[Statement],
    report: ApplyReport,
    *,
    strict: bool,
) -> None:
    """Feed `runnable` to psql, resuming past recorded hard failures."""
    offset = 0
    while offset < len(runnable):
        chunk = runnable[offset:]
        batch = "\n".join(s.text_with_terminator for s in chunk) + "\n"
        # line number inside `batch` for each statement, so psql's own report maps back
        starts, pos = [], 0
        for s in chunk:
            starts.append(pos + 1)
            pos += len(s.text_with_terminator) + 1
        res = db.run_psql(db.url, ["-f", "-"], stdin=batch)
        for notice in _NOTICE_RE.findall(res.stderr):
            report.notices.append((path.name, notice))
        if res.returncode == 0:
            return
        m = _STDIN_ERR_RE.search(res.stderr)
        if m is None:
            raise SchemaToolError(
                f"{path.name}: psql failed (exit {res.returncode}) without a locatable error:\n"
                f"{res.stderr.strip()}"
            )
        line, message = int(m.group(1)), m.group(2).strip()
        index = max(i for i, start in enumerate(starts) if start <= line)
        failed = chunk[index]
        skip = SkippedStatement(
            file=path.name,
            line=failed.line,
            category="unsupported-statement",
            reason=f"psql: {message}",
            statement=failed.sql,
            objects=objects_created_by(failed.sql),
        )
        report.skipped.append(skip)
        if strict:
            raise SchemaToolError(
                f"{path.name}:{failed.line}: statement failed and had to be skipped — "
                f"{message}\n"
                "Rerun with --lenient to record it and carry on, or fix the cause.  "
                "This is deliberately fatal: an unexpected skip changes the snapshot."
            )
        offset += index + 1


# --------------------------------------------------------------------------- #
# snapshotting
# --------------------------------------------------------------------------- #

_PUBLIC_PREFIX_RE = re.compile(r"\bpublic\.")
#: string literals and quoted identifiers, whose *contents* are data or names we
#: must not rewrite (`DEFAULT 'public.example'` is a value, not a schema prefix).
_PROTECTED_RE = re.compile(r"'(?:[^']|'')*'|\"(?:[^\"]|\"\")*\"")


def collapse_ws(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


def strip_public_prefix(text: str) -> str:
    """Drop the `public.` qualifier outside quoted string literals and identifiers."""
    out: list[str] = []
    pos = 0
    for m in _PROTECTED_RE.finditer(text):
        out.append(_PUBLIC_PREFIX_RE.sub("", text[pos : m.start()]))
        out.append(m.group(0))
        pos = m.end()
    out.append(_PUBLIC_PREFIX_RE.sub("", text[pos:]))
    return "".join(out)


def normalize_definition(text: str) -> str:
    """Apply the `public.` / whitespace part of the normalisation contract."""
    return collapse_ws(strip_public_prefix(text))


def normalize_function_source(text: str) -> str:
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    lines = [line.rstrip() for line in text.split("\n")]
    return strip_public_prefix("\n".join(lines).strip())


_TABLES_SQL = """
SELECT c.relname, c.relkind, c.relpersistence
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'public' AND c.relkind IN ('r','p')
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = c.oid AND d.deptype = 'e')
 ORDER BY c.relname
"""

_COLUMNS_SQL = r"""
SELECT c.relname, a.attnum::text, a.attname,
       format_type(a.atttypid, a.atttypmod),
       CASE WHEN a.attnotnull THEN 'not null' ELSE 'null' END,
       regexp_replace(COALESCE(pg_get_expr(ad.adbin, ad.adrelid), ''), E'\\s+', ' ', 'g'),
       a.attidentity, a.attgenerated,
       CASE WHEN a.attcollation <> t.typcollation
            THEN (SELECT collname FROM pg_collation WHERE oid = a.attcollation)
            ELSE '' END
  FROM pg_attribute a
  JOIN pg_class c ON c.oid = a.attrelid
  JOIN pg_namespace n ON n.oid = c.relnamespace
  JOIN pg_type t ON t.oid = a.atttypid
  LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum
 WHERE n.nspname = 'public' AND c.relkind IN ('r','p')
   AND a.attnum > 0 AND NOT a.attisdropped
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = c.oid AND d.deptype = 'e')
 ORDER BY c.relname, a.attnum
"""

_CONSTRAINTS_SQL = r"""
SELECT c.relname, con.conname, con.contype,
       regexp_replace(pg_get_constraintdef(con.oid, true), '\s+', ' ', 'g'),
       CASE WHEN con.convalidated THEN 'validated' ELSE 'not validated' END
  FROM pg_constraint con
  JOIN pg_class c ON c.oid = con.conrelid
  JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'public' AND con.contype IN ('p','u','f','c','x')
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = con.oid AND d.deptype = 'e')
 ORDER BY c.relname, con.conname
"""

_INDEXES_SQL = r"""
SELECT ic.relname, c.relname,
       regexp_replace(pg_get_indexdef(i.indexrelid), '\s+', ' ', 'g'),
       CASE WHEN i.indisunique THEN 'unique' ELSE '' END,
       CASE WHEN i.indisvalid THEN 'valid' ELSE 'INVALID' END
  FROM pg_index i
  JOIN pg_class ic ON ic.oid = i.indexrelid
  JOIN pg_class c ON c.oid = i.indrelid
  JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'public'
   AND NOT EXISTS (SELECT 1 FROM pg_constraint con WHERE con.conindid = i.indexrelid)
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = ic.oid AND d.deptype = 'e')
 ORDER BY ic.relname
"""

_FUNCTIONS_SQL = r"""
SELECT p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')',
       p.prokind, l.lanname, p.provolatile,
       CASE WHEN p.proisstrict THEN 'strict' ELSE '' END,
       CASE WHEN p.prosecdef THEN 'security definer' ELSE '' END,
       replace(encode(convert_to(pg_get_functiondef(p.oid), 'UTF8'), 'base64'), E'\n', '')
  FROM pg_proc p
  JOIN pg_namespace n ON n.oid = p.pronamespace
  JOIN pg_language l ON l.oid = p.prolang
 WHERE n.nspname = 'public'
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e')
 ORDER BY 1
"""

_TRIGGERS_SQL = r"""
SELECT c.relname, t.tgname,
       regexp_replace(pg_get_triggerdef(t.oid, true), '\s+', ' ', 'g')
  FROM pg_trigger t
  JOIN pg_class c ON c.oid = t.tgrelid
  JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'public' AND NOT t.tgisinternal
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = t.oid AND d.deptype = 'e')
 ORDER BY c.relname, t.tgname
"""

_SEQUENCES_SQL = r"""
SELECT s.sequencename,
       format('%s|%s|%s|%s|%s', s.start_value, s.min_value, s.max_value, s.increment_by, s.cycle)
  FROM pg_sequences s
 WHERE s.schemaname = 'public'
 ORDER BY s.sequencename
"""

_TYPES_SQL = r"""
SELECT t.typname, t.typtype,
       COALESCE((SELECT string_agg(e.enumlabel, ',' ORDER BY e.enumsortorder)
                   FROM pg_enum e WHERE e.enumtypid = t.oid), ''),
       COALESCE(format_type(t.typbasetype, t.typtypmod), '')
  FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
 WHERE n.nspname = 'public' AND t.typtype IN ('e','d','c')
   AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = t.oid AND d.deptype = 'e')
   AND t.typrelid = 0
 ORDER BY t.typname
"""

_VIEWS_SQL = r"""
SELECT v.viewname, regexp_replace(v.definition, '\s+', ' ', 'g')
  FROM pg_views v WHERE v.schemaname = 'public'
 ORDER BY v.viewname
"""

_MATVIEWS_SQL = r"""
SELECT m.matviewname, regexp_replace(m.definition, '\s+', ' ', 'g')
  FROM pg_matviews m WHERE m.schemaname = 'public'
 ORDER BY m.matviewname
"""


def snapshot_objects(db: ScratchDatabase) -> list[dict]:
    """Read the whole `public` schema back as a sorted list of normalised objects."""
    import base64

    objects: list[dict] = []

    for relname, relkind, persistence in db.query(_TABLES_SQL):
        objects.append(
            {
                "kind": "table",
                "key": relname,
                "def": {
                    "relkind": {"r": "ordinary table", "p": "partitioned table"}[relkind],
                    "persistence": {"p": "permanent", "u": "unlogged", "t": "temporary"}[persistence],
                },
            }
        )

    for relname, attnum, attname, typ, nullability, default, identity, generated, collation in db.query(
        _COLUMNS_SQL
    ):
        objects.append(
            {
                "kind": "column",
                "key": f"{relname}.{attname}",
                "def": {
                    "table": relname,
                    "position": int(attnum),
                    "type": collapse_ws(typ),
                    "nullability": nullability,
                    "default": normalize_definition(default),
                    "identity": {"": "", "a": "always", "d": "by default"}[identity],
                    "generated": {"": "", "s": "stored", "v": "virtual"}.get(generated, generated),
                    "collation": collation,
                },
            }
        )

    for relname, conname, contype, definition, validated in db.query(_CONSTRAINTS_SQL):
        objects.append(
            {
                "kind": "constraint",
                "key": f"{relname}.{conname}",
                "def": {
                    "table": relname,
                    "type": {
                        "p": "PRIMARY KEY",
                        "u": "UNIQUE",
                        "f": "FOREIGN KEY",
                        "c": "CHECK",
                        "x": "EXCLUDE",
                    }[contype],
                    "definition": normalize_definition(definition),
                    "validated": validated == "validated",
                },
            }
        )

    for indexname, relname, definition, unique, validity in db.query(_INDEXES_SQL):
        objects.append(
            {
                "kind": "index",
                "key": indexname,
                "def": {
                    "table": relname,
                    "definition": normalize_definition(definition),
                    "unique": unique == "unique",
                    "valid": validity == "valid",
                },
            }
        )

    for signature, prokind, lang, volatility, strictness, security, definition_b64 in db.query(
        _FUNCTIONS_SQL
    ):
        definition = normalize_function_source(base64.b64decode(definition_b64).decode("utf-8"))
        objects.append(
            {
                "kind": "function",
                "key": collapse_ws(signature),
                "def": {
                    "kind": {"f": "function", "p": "procedure", "a": "aggregate", "w": "window"}[
                        prokind
                    ],
                    "language": lang,
                    "volatility": {"i": "immutable", "s": "stable", "v": "volatile"}[volatility],
                    "strict": strictness == "strict",
                    "security": security or "invoker",
                    "definition": definition,
                    "md5": hashlib.md5(definition.encode("utf-8")).hexdigest(),
                },
            }
        )

    for relname, tgname, definition in db.query(_TRIGGERS_SQL):
        objects.append(
            {
                "kind": "trigger",
                "key": f"{relname}.{tgname}",
                "def": {"table": relname, "definition": normalize_definition(definition)},
            }
        )

    for seqname, props in db.query(_SEQUENCES_SQL):
        start, minimum, maximum, increment, cycle = props.split("|")
        objects.append(
            {
                "kind": "sequence",
                "key": seqname,
                "def": {
                    "start": start,
                    "min": minimum,
                    "max": maximum,
                    "increment": increment,
                    "cycle": cycle == "t",
                },
            }
        )

    for typname, typtype, labels, basetype in db.query(_TYPES_SQL):
        objects.append(
            {
                "kind": "type",
                "key": typname,
                "def": {
                    "type": {"e": "enum", "d": "domain", "c": "composite"}.get(typtype, typtype),
                    "labels": labels,
                    "base": collapse_ws(basetype),
                },
            }
        )

    for viewname, definition in db.query(_VIEWS_SQL):
        objects.append(
            {
                "kind": "view",
                "key": viewname,
                "def": {"definition": normalize_definition(definition)},
            }
        )

    for matviewname, definition in db.query(_MATVIEWS_SQL):
        objects.append(
            {
                "kind": "materialized view",
                "key": matviewname,
                "def": {"definition": normalize_definition(definition)},
            }
        )

    objects.sort(key=lambda o: (o["kind"], o["key"]))
    return objects


def build_snapshot(
    db: ScratchDatabase,
    *,
    label: str,
    source: str,
    meta: dict | None = None,
) -> dict:
    return {
        "format": SNAPSHOT_FORMAT,
        "meta": {
            "label": label,
            "source": source,
            "server_version": db.server_version(),
            "database": db.name,
            "extensions": sorted(db.installed_extensions()),
            "normalization": NORMALIZATION,
            **(meta or {}),
        },
        "objects": snapshot_objects(db),
    }


# --------------------------------------------------------------------------- #
# rendering `pg_dump` output as a stable, reviewable contract
# --------------------------------------------------------------------------- #

_DROP_LINE_PREFIXES = ("\\", "SET ", "SELECT pg_catalog.set_config", "-- Dumped")


def normalize_dump(raw: str) -> str:
    """Strip everything that is not schema, and everything that is not stable.

    Dropped on purpose:

    * `\\restrict` / `\\unrestrict` — pg_dump 16.15 emits a **random** token,
      which would make every regeneration a diff;
    * `SET` / `set_config` lines — session GUCs, not schema;
    * `--` comment lines — including pg_dump's `-- Dumped from database version`
      banner and its `-- Name: ...; Type: ...` object headers, both of which
      change with the pg_dump build;
    * repeated blank lines.
    """
    out: list[str] = []
    blank = False
    for line in raw.replace("\r\n", "\n").split("\n"):
        stripped = line.strip()
        if not stripped:
            blank = True
            continue
        if stripped.startswith("--") or stripped.startswith(_DROP_LINE_PREFIXES):
            continue
        if blank and out:
            out.append("")
        blank = False
        out.append(line.rstrip())
    return "\n".join(out).strip() + "\n"


def snapshot_header(*, repo: str, commit: str, generated_by: str, extra: list[str]) -> str:
    lines = [
        "-- contracts/upstream-schema.sql — upstream multica schema snapshot (generated, read only)",
        "--",
        f"-- upstream repo   : {repo}",
        f"-- upstream commit : {commit}",
        f"-- generated by    : {generated_by}",
        "-- regenerate      : scripts/build_upstream_schema.py (see docs/25-W0-SCHEMA-DRIFT.md §3)",
        "--",
        "-- This file is a *snapshot*, not a migration.  CI never regenerates it, it only reads it.",
        "-- It is the output of `pg_dump --schema-only` after replaying all 560 vendored",
        "-- `migrations/upstream/*.up.sql` on PostgreSQL 16, with the normalisation in",
        "-- scripts/schema_snapshot.py (NORMALIZATION): `public.` stripped, whitespace collapsed,",
        "-- psql meta-commands and session GUCs removed, extension-owned objects excluded (that is",
        "-- what pg_dump does too).  No timestamp is embedded, so regenerating an unchanged schema",
        "-- produces byte-identical output.",
        "--",
    ]
    lines += [f"-- {line}" if line else "--" for line in extra]
    lines += [
        "--",
        "-- Statements that upstream guards (or that this PostgreSQL cannot run) are skipped while",
        "-- building the snapshot, never silently: every one is listed in",
        "-- contracts/upstream-apply-exceptions.tsv and cross-referenced from",
        "-- contracts/schema-deviations.tsv.",
        "",
        "",
    ]
    return "\n".join(lines) + "\n"


def write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 16), b""):
            digest.update(chunk)
    return digest.hexdigest()

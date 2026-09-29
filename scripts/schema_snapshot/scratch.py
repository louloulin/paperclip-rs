"""The throwaway database and the migration replay loop."""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

from .contract import SchemaToolError
from .dburl import _argv_conn, _run, parse_db_url, url_with_database
from .sqlsplit import (
    Statement,
    assert_extension_markers_cover,
    objects_created_by,
    required_extensions,
    split_sql_statements,
    unavailable_extension_markers,
)
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

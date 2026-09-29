"""libpq URL parsing / redaction, and the `subprocess` wrapper psql runs through."""

from __future__ import annotations

import os
import subprocess
import urllib.parse

from .contract import SchemaToolError
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

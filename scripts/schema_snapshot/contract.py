"""Normalisation contract: the machine-readable rules embedded in every snapshot.

Kept in its own module on purpose — a drift report is only as trustworthy as
`NORMALIZATION`, and it is written verbatim into every snapshot file.
"""

from __future__ import annotations

import re
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

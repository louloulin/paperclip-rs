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

from .contract import (
    EXTENSION_MARKERS,
    NORMALIZATION,
    SNAPSHOT_FORMAT,
    SchemaToolError,
    _CREATE_EXTENSION_RE,
)
from .dburl import (
    _netloc,
    database_name,
    parse_db_url,
    redact,
    url_with_database,
)
from .objects import (
    build_snapshot,
    collapse_ws,
    normalize_definition,
    normalize_dump,
    normalize_function_source,
    sha256_of,
    snapshot_header,
    snapshot_objects,
    strip_public_prefix,
    write_text,
)
from .scratch import (
    ApplyReport,
    ScratchDatabase,
    SkippedStatement,
    _run_file,
    apply_migrations,
    skip_object_keys,
)
from .sqlsplit import (
    Statement,
    assert_extension_markers_cover,
    code_only,
    objects_created_by,
    required_extensions,
    split_sql_statements,
    unavailable_extension_markers,
)

__all__ = [
    "EXTENSION_MARKERS",
    "NORMALIZATION",
    "SNAPSHOT_FORMAT",
    "ApplyReport",
    "ScratchDatabase",
    "SchemaToolError",
    "SkippedStatement",
    "Statement",
    "_CREATE_EXTENSION_RE",
    "_netloc",
    "_run_file",
    "apply_migrations",
    "assert_extension_markers_cover",
    "build_snapshot",
    "code_only",
    "collapse_ws",
    "database_name",
    "normalize_definition",
    "normalize_dump",
    "normalize_function_source",
    "objects_created_by",
    "parse_db_url",
    "redact",
    "required_extensions",
    "sha256_of",
    "skip_object_keys",
    "snapshot_header",
    "snapshot_objects",
    "split_sql_statements",
    "strip_public_prefix",
    "unavailable_extension_markers",
    "url_with_database",
    "write_text",
]

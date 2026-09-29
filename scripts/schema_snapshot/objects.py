"""Reading a migrated schema back as a sorted, normalised object list, and rendering."""

from __future__ import annotations

import hashlib
import re
from pathlib import Path

from .contract import NORMALIZATION, SNAPSHOT_FORMAT
from .scratch import ScratchDatabase
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

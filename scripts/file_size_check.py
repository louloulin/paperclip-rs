#!/usr/bin/env python3
"""R7 file-size gate: no source file may exceed a hard line limit (default 800).

The risk this closes
--------------------
`docs/plan1.md` risk register **R7** (concurrent slice conflicts / workspace
ambiguity) names three controls: the anchor mechanism, **a hard 800-line cap per
file** and the workspace uniqueness check.  The cap is invisible to `clippy`, so
the register named this script — which did not exist, and `scripts/gates.sh` had
no size gate either, i.e. the cap was unenforced.  Oversized *shared* files are
the main source of 3-way conflicts between agents, which is the trigger R7
describes, so this is a correctness control, not a style preference.

What is checked
---------------
Only **code**, by tracked path (from `git ls-files`, so `target/` and untracked
files are never scanned):

    crates/**/*.rs   apps/**/*.rs   scripts/**/*.py   scripts/**/*.sh
    .github/workflows/**/*.yml

Documentation (`docs/**/*.md`) is deliberately **not** checked: long prose is
intentional and follows a different curve.  Matching is prefix + suffix, depth
unlimited (the register spells the globs as `scripts/*.py`; nesting a script
does not exempt it).

How a file can fail (exit 1)
----------------------------
    1. not in the baseline and over the limit          -> split the file, or
                                                          register it as legacy
    2. in the baseline and **over its recorded lines** -> it grew: split it back
    3. in the baseline and <= the limit                -> it already complies:
                                                          delete the entry
    4. in the baseline and no longer tracked by git    -> delete the entry

Rule 2 is the point of the baseline: entries record *legacy* violations so this
gate can land without a 14-file refactor in one slice, and every later change to
one of them must be a net shrink.  The baseline therefore only ever shrinks:

    基线只减不增，新增违规不得写进白名单。

`--write-baseline` regenerates the file from the current tree (for a rename or a
legitimate shrink); it prints the added / removed / updated entries so the pull
request shows exactly what moved, and it warns when it records a *new*
violation.  Regenerate it with the default `--limit`: a baseline written with a
custom limit will trip rule 3 under the default limit, loudly, by design.

Line count = number of newline characters, plus one when the final line has
content but no trailing newline (identical to `wc -l` for well-formed files, and
strict where `wc -l` would under-count by one).

Usage
-----
    python3 scripts/file_size_check.py                  # table + exit code
    python3 scripts/file_size_check.py --quiet          # only on failure
    python3 scripts/file_size_check.py --limit 400      # tighter local run
    python3 scripts/file_size_check.py --write-baseline # accept the current tree

Exit codes: 0 = no violation; 1 = at least one violation (also for a baseline
that cannot be read or parsed — that is a violation, not a usage error);
2 = bad command line.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys

DEFAULT_LIMIT = 800
DEFAULT_BASELINE = os.path.join("scripts", "file_size_baseline.tsv")

# Scope: (path prefix, accepted suffixes). `docs/` is absent on purpose.
SCOPE = (
    ("crates/", (".rs",)),
    ("apps/", (".rs",)),
    ("scripts/", (".py", ".sh")),
    (".github/workflows/", (".yml",)),
)

BASELINE_HEADER = (
    "# scripts/file_size_baseline.tsv — R7「单文件 800 行硬上限」的**存量违规**白名单。\n"
    "# 格式：<path>\\t<lines>（按 path 排序；# 开头为注释，空行忽略）。\n"
    "# 生成 / 刷新：python3 scripts/file_size_check.py --write-baseline\n"
    "# 执行者：scripts/file_size_check.py（本地）与 scripts/gates.sh 的门 ⑩ file-size（CI）。\n"
    "#\n"
    "# 基线只减不增，新增违规不得写进白名单。\n"
    "#\n"
    "# 判据（见 scripts/file_size_check.py --help）：\n"
    "#   * 清单外文件 > 上限          -> 失败（新代码必须自己拆到 800 以内）\n"
    "#   * 清单内文件 > 记录的行数    -> 失败（只允许变短，不允许变长）\n"
    "#   * 清单内文件 <= 上限         -> 失败（请从基线删除，它已达标）\n"
    "#   * 清单内文件已不在 git 里    -> 失败（请从基线删除）\n"
)


# --------------------------------------------------------------------------- #
# Scope / counting
# --------------------------------------------------------------------------- #


def in_scope(path: str) -> bool:
    """Is `path` a tracked *code* path covered by the size cap?"""
    for prefix, suffixes in SCOPE:
        if path.startswith(prefix) and path.endswith(suffixes):
            return True
    return False


def tracked_files(repo: str) -> list[str]:
    """Every path `git ls-files` reports, repo-relative, sorted."""
    proc = subprocess.run(
        ["git", "-C", repo, "ls-files", "-z"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    raw = proc.stdout.decode("utf-8", "surrogateescape")
    return sorted(p for p in raw.split("\0") if p)


def count_lines(path: str) -> int:
    """Newlines, +1 when the final line is non-empty without a trailing newline."""
    with open(path, "rb") as handle:
        data = handle.read()
    newlines = data.count(b"\n")
    if data and not data.endswith(b"\n"):
        newlines += 1
    return newlines


# --------------------------------------------------------------------------- #
# Baseline
# --------------------------------------------------------------------------- #


def read_baseline(path: str) -> dict[str, int]:
    entries: dict[str, int] = {}
    with open(path, encoding="utf-8") as handle:
        for lineno, line in enumerate(handle, 1):
            line = line.rstrip("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) != 2 or not parts[1].strip().isdigit():
                raise ValueError(f"{path}:{lineno}: expected `<path>\\t<lines>`, got {line!r}")
            name = parts[0].strip()
            if name in entries:
                raise ValueError(f"{path}:{lineno}: duplicate entry for {name}")
            entries[name] = int(parts[1])
    return entries


def render_baseline(entries: dict[str, int]) -> str:
    body = "".join(f"{name}\t{entries[name]}\n" for name in sorted(entries))
    return BASELINE_HEADER + body


def write_baseline(
    path: str,
    current: dict[str, int],
    changes: dict[str, set[str]],
    limit: int,
    had_previous: bool,
) -> None:
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(render_baseline(current))
    added = sorted(changes["added"])
    removed = sorted(changes["removed"])
    updated = sorted(changes["updated"])
    print(f"baseline written: {path} ({len(current)} entry/entries, limit {limit})")
    for label, names in (("added", added), ("removed", removed), ("updated", updated)):
        print(f"  {label}: {', '.join(names) if names else '(none)'}")
    if added and had_previous:
        print(
            "WARNING: 本次写入了 %d 条**新增**违规（基线只减不增，新增违规不得写进白名单）：\n"
            "  %s" % (len(added), ", ".join(added)),
            file=sys.stderr,
        )


# --------------------------------------------------------------------------- #
# Report
# --------------------------------------------------------------------------- #


def scan(repo: str, limit: int) -> tuple[list[str], dict[str, int], dict[str, int]]:
    """Return `(tracked paths, in-scope sizes, in-scope sizes over the limit)`."""
    tracked = tracked_files(repo)
    sizes = {p: count_lines(os.path.join(repo, p)) for p in tracked if in_scope(p)}
    return tracked, sizes, {p: n for p, n in sizes.items() if n > limit}


def violations(
    sizes: dict[str, int],
    baseline: dict[str, int],
    tracked: set[str],
    limit: int,
) -> list[tuple[str, int, int | None, str]]:
    """`(path, lines, recorded or None, reason)` for every violation, path-sorted."""
    found = []
    for path in sorted(set(sizes) | set(baseline)):
        lines = sizes.get(path)
        recorded = baseline.get(path)
        if lines is None:
            if path in tracked:
                found.append((path, recorded or 0, recorded, "不在检查范围内，请从基线删除"))
            else:
                found.append((path, recorded or 0, recorded, "已不在 git 中，请从基线删除"))
        elif recorded is None:
            if lines > limit:
                found.append((path, lines, None, f"不在基线里且超过 {limit} 行上限"))
        elif lines > limit:
            if lines > recorded:
                found.append((path, lines, recorded, f"超过基线记录（{recorded} 行），只允许变短"))
        else:
            found.append((path, lines, recorded, f"已达标（<= {limit} 行），请从基线删除"))
    return found


def render_violations(rows: list[tuple[str, int, int | None, str]], limit: int) -> str:
    name_w = max(4, max(len(r[0]) for r in rows))
    out = [f"VIOLATIONS ({len(rows)}):", f"  {'lines':>6} {'limit':>6} {'baseline':>8}  {'path':<{name_w}}  reason"]
    for path, lines, recorded, reason in rows:
        base = "-" if recorded is None else str(recorded)
        out.append(f"  {lines:>6} {limit:>6} {base:>8}  {path:<{name_w}}  {reason}")
    out.append("")
    out.append(
        "fix: split the file (new code), or shrink it back (it regressed), "
        "or drop the entry from scripts/file_size_baseline.tsv (it complies)."
    )
    return "\n".join(out)


def render_ok(sizes: dict[str, int], baseline: dict[str, int], limit: int) -> str:
    out = [
        f"file_size_check: limit={limit}  scanned={len(sizes)}  baseline={len(baseline)}  violations=0",
        f"  {'lines':>6} {'limit':>6} {'over':>6}  path",
        "  (baseline entries — legacy violations, each one only allowed to shrink)",
    ]
    for path in sorted(baseline):
        lines = sizes.get(path, baseline[path])
        out.append(f"  {lines:>6} {limit:>6} {lines - limit:>6}  {path}")
    out.append("")
    out.append("  OK: 0 violation(s)")
    return "\n".join(out)


# --------------------------------------------------------------------------- #
# Entry point
# --------------------------------------------------------------------------- #


def repo_root() -> str:
    proc = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return proc.stdout.decode("utf-8", "surrogateescape").strip()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="file_size_check.py",
        description="R7 file-size gate: no tracked source file above the line limit.",
    )
    parser.add_argument("--limit", type=int, default=DEFAULT_LIMIT, help=f"hard line limit (default {DEFAULT_LIMIT})")
    parser.add_argument("--baseline", default=DEFAULT_BASELINE, help="legacy-violation whitelist (path<TAB>lines)")
    parser.add_argument("--write-baseline", action="store_true", help="regenerate the whitelist from this tree")
    parser.add_argument("--quiet", action="store_true", help="print nothing unless the gate is red")
    args = parser.parse_args(argv)

    if args.limit < 1:
        parser.error("--limit must be >= 1")

    try:
        root = repo_root()
    except (OSError, subprocess.CalledProcessError) as exc:
        print(f"error: not a git repository ({exc})", file=sys.stderr)
        return 1

    baseline_path = args.baseline if os.path.isabs(args.baseline) else os.path.join(root, args.baseline)

    tracked, sizes, over = scan(root, args.limit)

    if args.write_baseline:
        try:
            previous = read_baseline(baseline_path) if os.path.exists(baseline_path) else {}
        except (OSError, ValueError) as exc:
            print(f"error: {exc}", file=sys.stderr)
            return 1
        changes = {
            "added": set(over) - set(previous),
            "removed": set(previous) - set(over),
            "updated": {p for p in set(over) & set(previous) if over[p] != previous[p]},
        }
        write_baseline(baseline_path, over, changes, args.limit, had_previous=bool(previous))
        return 0

    try:
        baseline = read_baseline(baseline_path)
    except FileNotFoundError:
        print(f"error: baseline not found: {baseline_path}", file=sys.stderr)
        print("  create it once with: python3 scripts/file_size_check.py --write-baseline", file=sys.stderr)
        return 1
    except (OSError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    rows = violations(sizes, baseline, set(tracked), args.limit)
    if rows:
        print(render_violations(rows, args.limit))
        return 1
    if not args.quiet:
        print(render_ok(sizes, baseline, args.limit))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BrokenPipeError:  # `| head` — the reader went away, that is not a failure
        devnull = os.open(os.devnull, os.O_WRONLY)
        os.dup2(devnull, sys.stdout.fileno())
        sys.exit(0)

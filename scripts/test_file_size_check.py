#!/usr/bin/env python3
"""门 ⑩ 的判定器 `scripts/file_size_check.py`（329 行）的用例（LUM-2617 / T1-6-P）。

`docs/37 §283` 记下的形态：**判定器有语义、有分支、有归因，但没有任何东西在它说错时
告诉它**。`scripts/gates.sh:20` 的门 ⑩（`python3 scripts/file_size_check.py --quiet`）
每一轮都在报「本仓有多少个文件超了 800 行硬上限」，而这个算读数的脚本在
`scripts/tests.manifest` 里**没有任何用例**（`grep -rIl` 全仓复核：它只出现在
`.github/workflows/ci.yml`、`contracts/*.tsv` 与 `docs/**` 里 ⇒ **被运行，从未被测试**）。
同族的三片：`LUM-2606`(T1-6-K) 钉 `route_parity.py`、`LUM-2613`(T1-6-N) 钉
`section_alloc_check.py`（经 `harvest_preflight.py`）、`LUM-2615`(T1-6-O) 钉 H7。

🔴 **本片最贵的一条**（本轮当场实测，四段读数见 `docs/37 §288`）：

    P0  干净树，基线记 850，文件 850 行          -> rc=0
    P1  同一个文件长到 900 行                     -> rc=1   （rule 2 正确判红）
    P2  操作员跑 `--write-baseline`              -> rc=0，**stderr 0 字节（无任何告警）**
    P3  **同一棵树**再跑门                       -> rc=0

⇒ **一条 50 行的真实回归，被一条命令静默洗成绿。** 根因在 `main()` 的 `changes` 计算与
`write_baseline()` 的告警条件之间：`added` 走 `if added and had_previous:` 的 WARNING 分支，
而 `updated`（`{p for p in set(over) & set(previous) if over[p] != previous[p]}`）只被
`print` 成一行 `  updated: <path>`，**不检查方向、不告警、不改退出码**
⇒ **同一个不变式，在「新增」路径上被强制，在「变长」路径上只是被打印。**

而这条规则本轮刚产生过真实成本（`docs/37 §283` 承重一）：762 行的
`test_route_parity.py` 追加即红、登记基线被规则禁止，**唯一合法出口是拆文件**
⇒ 它的执行器自己却留着一个一键绕过口。

**本片只测不改**（与 `LUM-2608` 同形）：把缺陷钉成**有编号的已知缺陷**
（`KNOWN_DEFECTS` 四字段表 + `@unittest.expectedFailure`，登记表与装饰器**双向**受检），
**修不修是 owner 裁决**（三种修法的代价差别很大，见 `docs/37 §288 §5`）。

Run: ``python3 scripts/test_file_size_check.py``（零 Rust / 零 cargo / 零真库 / 零磁盘）
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import file_size_check as FSC  # noqa: E402  — the gate module itself, exercised as written

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPT = os.path.join(REPO_ROOT, "scripts", "file_size_check.py")

#: 缺陷号 → 钉住它的用例、它声明的契约、当轮实测到的现象、收口方式。
#: **四条字段都必填**（下面有用例检查）—— 只写「有个已知缺陷」而不写「怎么算收口」，
#: 那张表就退化回散文。
KNOWN_DEFECTS = {
    "KD-1": {
        "case": "TestKnownDefects.test_kd1_write_baseline_refuses_to_launder_a_grown_entry",
        "claim": "`基线只减不增，新增违规不得写进白名单` + `--help` rule 2"
                "（在基线里且**超过**记录行数 ⇒ 失败，只允许变短）⇒ `--write-baseline`"
                " 不得把一个**变长**的基线条目原样收下",
        "observed": "P2：`--write-baseline` rc=0、**stderr 0 字节**、把 850 改写成 900；"
                    "P3：同一棵树再跑门 rc=0 ⇒ 50 行回归被一条命令洗成绿",
        "close": "在 `write_baseline()` 里对 `updated` 中「变大」的条目做告警（方案 b）"
                 "或直接 `return 1`（方案 a）后删掉装饰器；owner 裁决见 docs/37 §288",
    },
}


# --------------------------------------------------------------------------- #
# Fixtures: a throwaway git repository, so `git ls-files` (the gate's own source of
# "tracked") is real.  Nothing here touches scripts/file_size_baseline.tsv.
# --------------------------------------------------------------------------- #


def _write(path: str, text: str) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(text)


def _lines(n: int) -> str:
    """`n` newline-terminated lines."""
    return "".join(f"// line {i}\n" for i in range(1, n + 1))


def _git(repo: str, *args: str) -> None:
    subprocess.run(
        ["git", "-C", repo, "-c", "user.email=gate@example.invalid",
         "-c", "user.name=gate", "-c", "commit.gpgsign=false", *args],
        check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )


def make_repo(tmp: str, files: dict[str, str], commit: bool = True) -> str:
    """A git repository under `tmp` containing `files` (repo-relative -> content)."""
    repo = os.path.join(tmp, "repo")
    os.makedirs(repo, exist_ok=True)
    _git(repo, "init", "-q", ".")
    for rel, body in files.items():
        _write(os.path.join(repo, rel), body)
    if commit:
        _git(repo, "add", "-A")
        _git(repo, "commit", "-q", "-m", "fixture")
    return repo


def run_gate(repo: str, *argv: str) -> tuple[int, str, str]:
    """Run the real `scripts/file_size_check.py` with `cwd=repo` (it resolves its own root)."""
    proc = subprocess.run(
        [sys.executable, SCRIPT, *argv],
        cwd=repo, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    return proc.returncode, proc.stdout, proc.stderr


def baseline_text(entries: dict[str, int]) -> str:
    return "# fixture baseline\n" + "".join(f"{k}\t{v}\n" for k, v in sorted(entries.items()))


def read_text(path: str) -> str:
    with open(path, encoding="utf-8") as handle:
        return handle.read()


class _RepoCase(unittest.TestCase):
    """Base class: one throwaway repo per test, torn down afterwards."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.tmp = self._tmp.name

    def repo(self, files: dict[str, str], commit: bool = True) -> str:
        return make_repo(self.tmp, files, commit=commit)


# --------------------------------------------------------------------------- #
# Scope: what the gate looks at
# --------------------------------------------------------------------------- #


class TestScope(_RepoCase):
    def test_oversized_crate_file_fails_the_gate(self):
        repo = self.repo({"crates/a.rs": _lines(801), "scripts/file_size_baseline.tsv": "# empty\n"})
        rc, out, err = run_gate(repo, "--quiet")
        self.assertEqual(rc, 1, out + err)
        self.assertIn("crates/a.rs", out)
        self.assertIn("不在基线里且超过 800 行上限", out)

    def test_a_file_exactly_at_the_limit_passes(self):
        repo = self.repo({"crates/a.rs": _lines(800), "scripts/file_size_baseline.tsv": "# empty\n"})
        rc, out, err = run_gate(repo, "--quiet")
        self.assertEqual(rc, 0, out + err)
        self.assertEqual(out, "", "--quiet must print nothing while green")

    def test_docs_prose_is_deliberately_out_of_scope(self):
        """`docs/**/*.md` 不在 SCOPE 里 —— 长文是有意的，与代码不同曲线。"""
        repo = self.repo({"docs/huge.md": _lines(5000), "scripts/file_size_baseline.tsv": "# empty\n"})
        self.assertEqual(run_gate(repo, "--quiet")[0], 0)

    def test_untracked_files_are_never_scanned(self):
        """扫描源是 `git ls-files` ⇒ 没 add 的文件不进读数。"""
        repo = self.repo({"crates/a.rs": _lines(10), "scripts/file_size_baseline.tsv": "# empty\n"})
        _write(os.path.join(repo, "crates/untracked.rs"), _lines(900))
        self.assertEqual(run_gate(repo, "--quiet")[0], 0)

    def test_nested_scripts_are_still_in_scope(self):
        """register 写的是 `scripts/*.py`；嵌套不豁免（R7 原话：nesting does not exempt it）。"""
        repo = self.repo({"scripts/deep/nested.py": _lines(801), "scripts/file_size_baseline.tsv": "# empty\n"})
        rc, out, _ = run_gate(repo, "--quiet")
        self.assertEqual(rc, 1, out)
        self.assertIn("scripts/deep/nested.py", out)

    def test_workflow_yml_is_in_scope(self):
        repo = self.repo({".github/workflows/ci.yml": _lines(801), "scripts/file_size_baseline.tsv": "# empty\n"})
        self.assertEqual(run_gate(repo, "--quiet")[0], 1)

    def test_documentation_paths_are_outside_every_scope_prefix(self):
        """`apps/` 与 `crates/` 只收 `.rs`；`scripts/` 只收 `.py`/`.sh`。"""
        repo = self.repo({
            "apps/web/main.ts": _lines(900),
            "scripts/notes.txt": _lines(900),
            "scripts/file_size_baseline.tsv": "# empty\n",
        })
        self.assertEqual(run_gate(repo, "--quiet")[0], 0)


# --------------------------------------------------------------------------- #
# The four rules from --help
# --------------------------------------------------------------------------- #


class TestRule1NewViolation(_RepoCase):
    def test_not_in_baseline_and_over_the_limit_fails(self):
        repo = self.repo({"crates/new.rs": _lines(850), "scripts/file_size_baseline.tsv": "# empty\n"})
        rc, out, _ = run_gate(repo, "--quiet")
        self.assertEqual(rc, 1, out)
        self.assertIn("不在基线里且超过 800 行上限", out)
        self.assertIn("split the file", out)

    def test_not_in_baseline_but_under_the_limit_passes(self):
        repo = self.repo({"crates/new.rs": _lines(799), "scripts/file_size_baseline.tsv": "# empty\n"})
        self.assertEqual(run_gate(repo, "--quiet")[0], 0)


class TestRule2Grew(_RepoCase):
    def test_growing_past_the_recorded_line_count_fails(self):
        """🔴 承重规则的**正常路径**：它在「不洗白」时是有效的。"""
        repo = self.repo({
            "crates/legacy.rs": _lines(900),
            "scripts/file_size_baseline.tsv": baseline_text({"crates/legacy.rs": 850}),
        })
        rc, out, _ = run_gate(repo, "--quiet")
        self.assertEqual(rc, 1, out)
        self.assertIn("超过基线记录（850 行），只允许变短", out)

    def test_shrinking_is_allowed_and_still_over_the_limit(self):
        """规则说的是「只允许变短」—— 变短必须绿，否则这条规则无法执行。"""
        repo = self.repo({
            "crates/legacy.rs": _lines(820),
            "scripts/file_size_baseline.tsv": baseline_text({"crates/legacy.rs": 850}),
        })
        rc, out, err = run_gate(repo, "--quiet")
        self.assertEqual(rc, 0, out + err)

    def test_exactly_the_recorded_line_count_passes(self):
        repo = self.repo({
            "crates/legacy.rs": _lines(850),
            "scripts/file_size_baseline.tsv": baseline_text({"crates/legacy.rs": 850}),
        })
        self.assertEqual(run_gate(repo, "--quiet")[0], 0)

    def test_the_violation_table_reports_recorded_and_actual(self):
        repo = self.repo({
            "crates/legacy.rs": _lines(900),
            "scripts/file_size_baseline.tsv": baseline_text({"crates/legacy.rs": 850}),
        })
        _, out, _ = run_gate(repo)
        self.assertIn("VIOLATIONS (1):", out)
        self.assertIn("900", out)
        self.assertIn("850", out)


class TestRule3Complies(_RepoCase):
    def test_an_entry_that_dropped_to_the_limit_must_be_deleted(self):
        repo = self.repo({
            "crates/legacy.rs": _lines(700),
            "scripts/file_size_baseline.tsv": baseline_text({"crates/legacy.rs": 850}),
        })
        rc, out, _ = run_gate(repo, "--quiet")
        self.assertEqual(rc, 1, out)
        self.assertIn("已达标（<= 800 行），请从基线删除", out)

    def test_dropping_the_entry_turns_it_green(self):
        repo = self.repo({
            "crates/legacy.rs": _lines(700),
            "scripts/file_size_baseline.tsv": "# empty\n",
        })
        self.assertEqual(run_gate(repo, "--quiet")[0], 0)


class TestRule4Untracked(_RepoCase):
    def test_an_entry_whose_file_left_git_must_be_deleted(self):
        repo = self.repo({
            "crates/kept.rs": _lines(850),
            "scripts/file_size_baseline.tsv": baseline_text({"crates/legacy.rs": 850}),
        })
        rc, out, _ = run_gate(repo, "--quiet")
        self.assertEqual(rc, 1, out)
        self.assertIn("已不在 git 中，请从基线删除", out)

    def test_a_tracked_but_out_of_scope_entry_says_so(self):
        """同一个「文件读不到大小」的分支有**两个**来源，判词必须区分。"""
        repo = self.repo({
            "docs/notes.md": _lines(900),
            "scripts/file_size_baseline.tsv": baseline_text({"docs/notes.md": 900}),
        })
        rc, out, _ = run_gate(repo, "--quiet")
        self.assertEqual(rc, 1, out)
        self.assertIn("不在检查范围内，请从基线删除", out)


# --------------------------------------------------------------------------- #
# Baseline I/O
# --------------------------------------------------------------------------- #


class TestBaselineParsing(_RepoCase):
    def _with_baseline(self, body: str) -> str:
        return self.repo({"crates/a.rs": _lines(10), "scripts/file_size_baseline.tsv": body})

    def test_a_missing_baseline_is_a_violation_not_a_usage_error(self):
        """docstring: 「also for a baseline that cannot be read or parsed — that is a
        violation, not a usage error」⇒ rc=1，且给出创建命令。"""
        repo = self.repo({"crates/a.rs": _lines(10)})
        rc, out, err = run_gate(repo, "--quiet")
        self.assertEqual(rc, 1, out + err)
        self.assertIn("baseline not found", err)
        self.assertIn("--write-baseline", err)

    def test_comments_and_blank_lines_are_ignored(self):
        repo = self._with_baseline("# c\n\n#\tstill a comment\n")
        self.assertEqual(run_gate(repo, "--quiet")[0], 0)

    def test_a_malformed_line_is_rejected_with_its_line_number(self):
        repo = self._with_baseline("# c\ncrates/a.rs\tnot-a-number\n")
        rc, _, err = run_gate(repo, "--quiet")
        self.assertEqual(rc, 1)
        self.assertIn(":2:", err)
        self.assertIn("expected", err)

    def test_a_two_field_line_is_required(self):
        repo = self._with_baseline("crates/a.rs\t10\textra\n")
        self.assertEqual(run_gate(repo, "--quiet")[0], 1)

    def test_a_duplicate_entry_is_rejected(self):
        repo = self._with_baseline("crates/a.rs\t10\ncrates/a.rs\t11\n")
        rc, _, err = run_gate(repo, "--quiet")
        self.assertEqual(rc, 1)
        self.assertIn("duplicate entry", err)

    def test_a_custom_baseline_path_is_honoured(self):
        repo = self.repo({"crates/a.rs": _lines(900), "scripts/other.tsv": "# empty\n"})
        self.assertEqual(run_gate(repo, "--quiet", "--baseline", "scripts/other.tsv")[0], 1)


class TestLineCounting(_RepoCase):
    """`wc -l` under-counts a final unterminated line; the gate must not."""

    def _count(self, body: str) -> int:
        path = os.path.join(self.tmp, "probe.rs")
        with open(path, "wb") as handle:
            handle.write(body.encode("utf-8"))
        return FSC.count_lines(path)

    def test_newline_terminated_file_counts_its_newlines(self):
        self.assertEqual(self._count("a\nb\nc\n"), 3)

    def test_a_final_unterminated_line_still_counts(self):
        self.assertEqual(self._count("a\nb\nc"), 3)

    def test_an_empty_file_counts_zero(self):
        self.assertEqual(self._count(""), 0)


# --------------------------------------------------------------------------- #
# CLI surface
# --------------------------------------------------------------------------- #


class TestCli(_RepoCase):
    def test_a_non_positive_limit_is_a_usage_error(self):
        repo = self.repo({"crates/a.rs": _lines(10), "scripts/file_size_baseline.tsv": "# empty\n"})
        self.assertEqual(run_gate(repo, "--limit", "0")[0], 2)

    def test_the_limit_is_configurable(self):
        repo = self.repo({"crates/a.rs": _lines(50), "scripts/file_size_baseline.tsv": "# empty\n"})
        self.assertEqual(run_gate(repo, "--quiet")[0], 0)
        self.assertEqual(run_gate(repo, "--quiet", "--limit", "40")[0], 1)

    def test_the_ok_table_names_the_limit_and_zero_violations(self):
        repo = self.repo({"crates/legacy.rs": _lines(850),
                          "scripts/file_size_baseline.tsv": baseline_text({"crates/legacy.rs": 850})})
        rc, out, _ = run_gate(repo)
        self.assertEqual(rc, 0, out)
        self.assertIn("limit=800", out)
        self.assertIn("violations=0", out)
        self.assertIn("crates/legacy.rs", out)


# --------------------------------------------------------------------------- #
# --write-baseline
# --------------------------------------------------------------------------- #


class TestWriteBaseline(_RepoCase):
    def test_it_records_the_current_tree_with_the_documented_header(self):
        repo = self.repo({"crates/b.rs": _lines(820), "crates/a.rs": _lines(810),
                          "scripts/file_size_baseline.tsv": "# empty\n"})
        rc, out, err = run_gate(repo, "--write-baseline")
        self.assertEqual(rc, 0, out + err)
        body = read_text(os.path.join(repo, "scripts/file_size_baseline.tsv"))
        self.assertIn("基线只减不增", body)
        # sorted by path
        self.assertLess(body.index("crates/a.rs"), body.index("crates/b.rs"))
        self.assertIn("crates/a.rs\t810", body)

    def test_it_reports_added_removed_and_updated(self):
        repo = self.repo({
            "crates/kept.rs": _lines(810),
            "scripts/file_size_baseline.tsv": baseline_text({"crates/gone.rs": 820, "crates/old.rs": 830}),
        })
        rc, out, _ = run_gate(repo, "--write-baseline")
        self.assertEqual(rc, 0, out)
        self.assertIn("added: crates/kept.rs", out)
        self.assertIn("removed: crates/gone.rs, crates/old.rs", out)

    def test_a_new_violation_is_written_but_warned_about(self):
        """`added` 路径上的不变式**是被强制**的（stderr WARNING）—— 与 KD-1 的对照面。"""
        repo = self.repo({"crates/new.rs": _lines(830),
                          "scripts/file_size_baseline.tsv": baseline_text({"crates/old.rs": 830})})
        rc, out, err = run_gate(repo, "--write-baseline")
        self.assertEqual(rc, 0, out)
        self.assertIn("WARNING", err)
        self.assertIn("crates/new.rs", err)
        self.assertIn("基线只减不增", err)

    def test_no_warning_when_the_baseline_did_not_exist_before(self):
        """首次生成不是「往白名单里加违规」⇒ 不告警（`had_previous` 门）。"""
        repo = self.repo({"crates/new.rs": _lines(830)})
        rc, _, err = run_gate(repo, "--write-baseline")
        self.assertEqual(rc, 0)
        self.assertEqual(err, "")

    def test_a_shrink_is_recorded_without_a_warning(self):
        repo = self.repo({
            "crates/legacy.rs": _lines(810),
            "scripts/file_size_baseline.tsv": baseline_text({"crates/legacy.rs": 850}),
        })
        rc, out, err = run_gate(repo, "--write-baseline")
        self.assertEqual(rc, 0, out + err)
        self.assertEqual(err, "")
        self.assertIn("updated: crates/legacy.rs", out)
        self.assertIn("crates/legacy.rs\t810", read_text(os.path.join(repo, "scripts/file_size_baseline.tsv")))

    def test_a_baseline_written_with_a_custom_limit_trips_rule_3_under_the_default(self):
        """docstring 写明的自我保护：非默认 limit 写出的基线在默认 limit 下**大声**判红。"""
        repo = self.repo({"crates/a.rs": _lines(120), "scripts/file_size_baseline.tsv": "# empty\n"})
        self.assertEqual(run_gate(repo, "--write-baseline", "--limit", "100")[0], 0)
        rc, out, _ = run_gate(repo, "--quiet")
        self.assertEqual(rc, 1, out)
        self.assertIn("已达标", out)

    def test_the_written_baseline_makes_the_gate_green_on_the_same_tree(self):
        repo = self.repo({"crates/a.rs": _lines(830), "scripts/file_size_baseline.tsv": "# empty\n"})
        self.assertEqual(run_gate(repo, "--write-baseline")[0], 0)
        self.assertEqual(run_gate(repo, "--quiet")[0], 0)


# --------------------------------------------------------------------------- #
# Known-defect registry — both directions, same shape as test_route_parity_defects.py
# --------------------------------------------------------------------------- #


def _marked_expected_failures() -> set[str]:
    """Every `test_*` in this module carrying `@unittest.expectedFailure`."""
    marked = set()
    for name, obj in sorted(globals().items()):
        if not (isinstance(obj, type) and issubclass(obj, unittest.TestCase)):
            continue
        for attr in sorted(dir(obj)):
            if attr.startswith("test") and getattr(obj, attr).__dict__.get(
                "__unittest_expecting_failure__", False
            ):
                marked.add(f"{name}.{attr}")
    return marked


class TestKnownDefectRegistry(unittest.TestCase):
    """登记表与装饰器**双向**受检：只查一个方向时，删掉一条登记就变成「没人再看它」。"""

    def test_every_registered_defect_has_a_standing_xfail_case(self):
        registered = {d["case"] for d in KNOWN_DEFECTS.values()}
        self.assertEqual(registered - _marked_expected_failures(), set())

    def test_nothing_is_marked_xfail_without_being_registered(self):
        registered = {d["case"] for d in KNOWN_DEFECTS.values()}
        self.assertEqual(_marked_expected_failures() - registered, set())

    def test_each_entry_names_a_claim_an_observation_and_a_way_out(self):
        for defect_id, entry in sorted(KNOWN_DEFECTS.items()):
            for field in ("case", "claim", "observed", "close"):
                self.assertTrue(entry.get(field), f"{defect_id}.{field} is empty")

    def test_the_registry_covers_exactly_the_one_defect_of_288(self):
        self.assertEqual(sorted(KNOWN_DEFECTS), ["KD-1"])
        self.assertEqual(len(_marked_expected_failures()), 1)

    def test_the_registry_is_json_serialisable_so_docs_can_cite_it(self):
        self.assertEqual(json.loads(json.dumps(KNOWN_DEFECTS)), KNOWN_DEFECTS)
        for defect_id in KNOWN_DEFECTS:
            self.assertRegex(defect_id, r"^KD-\d+$")


class TestKnownDefects(_RepoCase):
    @unittest.expectedFailure
    def test_kd1_write_baseline_refuses_to_launder_a_grown_entry(self):
        """🔴 KD-1：`--write-baseline` 不得把一个**变长**的基线条目原样收下。

        三段探针（本轮当场实测，`docs/37 §288`）：

            P0  基线记 850、文件 850 行            -> 门 rc=0
            P1  文件长到 900 行                    -> 门 rc=1（rule 2 正确判红）
            P2  跑 `--write-baseline`              -> rc=0、**stderr 0 字节**
            P3  同一棵树再跑门                     -> 门 rc=0    ← 50 行回归被洗成绿

        根因：`main()` 的 `updated` 集合是 `{p for p in set(over) & set(previous) if
        over[p] != previous[p]}`（**不看方向**），而 `write_baseline()` 只对 `added and
        had_previous` 打 WARNING，`updated` 只被 `print` 成一行 ⇒ 同一个不变式在两条路径上
        强度不同。

        本条断言的是**不变式**而不是某一种修法：「洗白」这一步必须**留下痕迹** ——
        要么退出码非零（方案 a：拒绝写入），要么 stderr 明确点名这个变大的条目（方案 b：告警）。
        两种修法都能让本条由 xfail 变 `UNEXPECTED SUCCESS`，所以修好的人只需要删装饰器；
        但**登记表也必须同时改**（另一条用例会红），否则「修好了」与「登记表过期」是同一个绿。

        ⚠️ 本条**不**断言「P3 门仍然红」：方案 b 下基线照样被写成 900、门照样转绿 ——
        那是 `docs/37 §288.5` 明确留给 owner 裁决的取舍，不是缺陷本身。把 P3 写进这里
        等于 cycle 自行选了方案 a（那是本片 `§5` 明令不许做的：*cycle 不自行选*）。
        「同一棵树转绿」这个今天行为由下面的 `test_kd1_today_…` 绿用例钉住。
        """
        repo = self.repo({
            "crates/legacy.rs": _lines(900),
            "scripts/file_size_baseline.tsv": baseline_text({"crates/legacy.rs": 850}),
        })
        # P0/P1：先确认门本来是红的（否则下面的断言没有意义）。
        self.assertEqual(run_gate(repo, "--quiet")[0], 1, "P1: the gate must be red before laundering")

        rc, _, err = run_gate(repo, "--write-baseline")           # P2
        self.assertTrue(
            rc != 0 or ("WARNING" in err and "crates/legacy.rs" in err),
            f"P2: --write-baseline laundered a 50-line regression silently (rc={rc}, stderr={err!r})",
        )

    def test_kd1_today_write_baseline_launders_a_grown_entry_silently(self):
        """KD-1 的**今天行为**（绿）：与 xfail 那条互为对照，缺一条就只剩单边。"""
        repo = self.repo({
            "crates/legacy.rs": _lines(900),
            "scripts/file_size_baseline.tsv": baseline_text({"crates/legacy.rs": 850}),
        })
        self.assertEqual(run_gate(repo, "--quiet")[0], 1, "P1")
        rc, out, err = run_gate(repo, "--write-baseline")          # P2
        self.assertEqual(rc, 0, out)
        self.assertEqual(err, "", "the 'updated' path emits nothing on stderr at all")
        self.assertIn("updated: crates/legacy.rs", out, "it is only printed, never enforced")
        self.assertIn("crates/legacy.rs\t900", read_text(os.path.join(repo, "scripts/file_size_baseline.tsv")))
        self.assertEqual(run_gate(repo, "--quiet")[0], 0, "P3: the regression is now green")  # P3

    def test_the_contrast_added_is_enforced_while_updated_is_only_printed(self):
        """把不对称本身钉成读数：`added` 有 WARNING，`updated`（变长）没有。"""
        repo = self.repo({
            "crates/kept.rs": _lines(900),
            "crates/legacy.rs": _lines(900),
            "scripts/file_size_baseline.tsv": baseline_text({"crates/legacy.rs": 850}),
        })
        rc, out, err = run_gate(repo, "--write-baseline")
        self.assertEqual(rc, 0, out)
        self.assertIn("added: crates/kept.rs", out)
        self.assertIn("updated: crates/legacy.rs", out)
        self.assertIn("WARNING", err)
        self.assertIn("crates/kept.rs", err)          # 新增：被点名
        self.assertNotIn("crates/legacy.rs", err)     # 变长：不在 stderr 里


# --------------------------------------------------------------------------- #
# The real repository's own baseline — read only, asserted, never written.
# --------------------------------------------------------------------------- #


class TestRealBaseline(unittest.TestCase):
    def test_the_real_baseline_still_shrinks_only(self):
        """本仓的基线当前只有一条 `scripts/extract_upstream_fixtures.py`。

        这条不跑编译，只断言**基线文件本身**满足不变式：每条记录的行数都 **>=** 文件
        当前的行数（`recorded >= actual`）—— 一旦某条被 `--write-baseline` 洗成「记录值
        等于当前值」而文件其实在变长，这里立刻红。它是 §288 那条缺陷在**本仓**上的暴露面读数。
        """
        path = os.path.join(REPO_ROOT, "scripts", "file_size_baseline.tsv")
        entries = {}
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                line = line.rstrip("\n")
                if not line.strip() or line.lstrip().startswith("#"):
                    continue
                name, _, count = line.partition("\t")
                entries[name.strip()] = int(count)
        self.assertTrue(entries, "the real baseline is not empty")
        for name, recorded in sorted(entries.items()):
            with open(os.path.join(REPO_ROOT, name), "rb") as handle:
                data = handle.read()
            actual = data.count(b"\n") + (1 if data and not data.endswith(b"\n") else 0)
            self.assertGreaterEqual(
                recorded, actual,
                f"{name}: recorded {recorded} but the file is now {actual} lines "
                f"=> the baseline was laundered (KD-1)",
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)

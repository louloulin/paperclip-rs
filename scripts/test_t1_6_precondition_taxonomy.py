#!/usr/bin/env python3
"""Discriminant tests for `t1_6_precondition_taxonomy.py` (LUM-2602 / T1-6-I).

Run: ``python3 scripts/test_t1_6_precondition_taxonomy.py`` — no cargo, no database,
no ``target/``, no disk growth (pure stdlib ``unittest``, like gate ⑫ requires).

Why this file exists
--------------------
`docs/37 §272` recorded the defect family this slice closes: **20 Python cases were
green, and no gate executed them.**  Gate ⑫ (`LUM-2600` / PR #183) wired
`scripts/test_*.py` into CI — but that glob only matches the **top level** of
`scripts/`, so the classifiers that actually decide *which slice owns which of the
61 T1-6 gaps* (this 339-line module plus the 1053-line
`scripts/t1_6_realm_diff_taxonomy/` package) kept **zero** tests and **zero**
gate coverage.  The tests for this module therefore live at the top level, where
the pre-existing glob already reaches them, and the package-internal tests live
next to the package.

Every case below is a **discriminant**, not a smoke test: each one pins a claim the
module's own docstring/注释 makes, and is written so that the *opposite*
implementation fails it.  The strongest of these are the three documented pitfalls
(坑 ①②③) in the module docstring — they are the places where a plausible refactor
silently changes the per-family membership that every dispatch decision reads.
"""

import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from typing import cast

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(SCRIPTS)
if SCRIPTS not in sys.path:
    sys.path.insert(0, SCRIPTS)

import t1_6_precondition_taxonomy as pre  # noqa: E402

GOLDEN = os.path.join(REPO_ROOT, "contracts", "golden")


def _row(**over):
    """一条 db-mode report 行（字段名照 `mc-conformance --json` 的 fixture 对象）。"""
    row = {
        "id": "chat/TestSome@server/internal/handler/chat_test.go:100#1",
        "domain": "chat",
        "method": "GET",
        "path": "/api/chat/history",
        "actor": "agent",
        "status_expected": 200,
        "requires": ["daemon_token"],
        "detail": "  - daemon_token: daemon token required\n",
        "source": "server/internal/handler/chat_test.go:100",
    }
    row.update(over)
    return row


class TestMechanismKeysPitfall2(unittest.TestCase):
    """坑 ②：`detail` 里 `||` 之后是同一段文本的 tier 重复 ⇒ 按出现次数计数会虚高。"""

    def test_repeated_key_is_counted_once_per_fixture(self):
        detail = (
            "  - external_oauth: first tier\n"
            "||   - external_oauth: second tier (same key, repeated)\n"
            "||   - bare_handler_no_wiring: only in the first tier\n"
        )
        self.assertEqual(
            pre.mechanism_keys(detail),
            ["external_oauth", "bare_handler_no_wiring"],
        )

    def test_frequency_table_counts_fixtures_not_occurrences(self):
        """同一键在两条 fixture 里各出现两次 ⇒ 频次 2，不是 4。"""
        detail = "  - daemon_token: a\n||   - daemon_token: b\n"
        out = pre.build(
            [_row(id="a/One@x.go:1#1", detail=detail),
             _row(id="b/Two@x.go:2#2", detail=detail)],
            {"unevaluable": 2}, "conformance_db_json", True,
        )
        self.assertEqual(out["mechanism_key_frequency_by_fixture"]["daemon_token"], 2)

    def test_non_bullet_lines_are_not_keys(self):
        detail = "actor kind Agent needs a real credential; nothing else here\n"
        self.assertEqual(pre.mechanism_keys(detail), [])
        self.assertEqual(pre.detail_sentinel(detail), "AGENT_CREDENTIAL")

    def test_bullet_needs_a_key_shaped_token(self):
        """`- 123abc: x` 不是机制键（BULLET 要求 `[A-Za-z_]` 开头）⇒ 不许被当成键。"""
        self.assertEqual(pre.mechanism_keys("  - 123abc: not a key\n"), [])


class TestDetailSentinel(unittest.TestCase):
    def test_sentinel_is_sorted_joined_keys(self):
        detail = "  - zeta: z\n  - alpha: a\n"
        self.assertEqual(pre.detail_sentinel(detail), "alpha+zeta")

    def test_no_keys_and_unrecognised_prose_is_no_bullet(self):
        self.assertEqual(pre.detail_sentinel(""), pre.NO_BULLET)
        self.assertEqual(
            pre.detail_sentinel("something entirely different"), pre.NO_BULLET
        )

    def test_golden_placeholder_is_not_mistaken_for_a_mechanism(self):
        """静态面用 `"{actor}: (golden 未记录 detail)"` 占位 ⇒ 必须落 `NO_BULLET`。

        这是把「detail 键」与「requires 键」混用时最容易踩的一格：占位文本长得像
        一条散文，但它是**我们自己造的**，不是上游给的机制。
        """
        self.assertEqual(pre.detail_sentinel("agent: (golden 未记录 detail)"), pre.NO_BULLET)


class TestGroupKey(unittest.TestCase):
    def test_key_is_sorted_and_deduplicated(self):
        self.assertEqual(pre.group_key(["webhook_rate_limiter_denying", "daemon_token"]), "+".join(
            sorted(["webhook_rate_limiter_denying", "daemon_token"])))
        self.assertEqual(pre.group_key(["daemon_token", "daemon_token"]), "daemon_token")

    def test_empty_requires_is_the_sentinel_bucket(self):
        self.assertEqual(pre.group_key([]), pre.NO_REQUIRES)

    def test_collapsed_group_is_a_union_not_a_priority_pick(self):
        """坑 ③：折叠是判断，不是判别式 ⇒ 三键集合必须整组保留，不得取首。"""
        key = pre.group_key(["bare_handler_no_wiring", "daemon_token", "db_fault_injection"])
        self.assertEqual(
            key.split("+"),
            ["bare_handler_no_wiring", "daemon_token", "db_fault_injection"],
        )


class TestSourceFile(unittest.TestCase):
    def test_strips_the_line_number(self):
        self.assertEqual(
            pre.source_file("server/internal/handler/chat_test.go:772"),
            "server/internal/handler/chat_test.go",
        )

    def test_missing_source_yields_empty_not_a_crash(self):
        self.assertEqual(pre.source_file(""), "")
        # 注解是 `str`，但实现里有 `or ""` 的运行期兜底；用 cast 明说「故意撒谎」，
        # 免得静态检查把这条判别式当成类型错误删掉。静态面下 `source` 恒为格式化串，
        # 真正依赖这条兜底的是 db-mode 行里 `source` 缺字段的情形。
        self.assertEqual(pre.source_file(cast(str, None)), "")

    def test_source_without_a_colon_is_returned_verbatim(self):
        self.assertEqual(pre.source_file("chat_test.go"), "chat_test.go")


class TestStaticCandidateCriterion(unittest.TestCase):
    """输入 B 的判据 = `requires` 非空 ∪ `actor.kind == agent`。"""

    def test_requires_makes_a_candidate_even_for_a_member(self):
        self.assertTrue(pre.is_candidate(["daemon_token"], "member"))

    def test_agent_without_requires_is_a_candidate(self):
        self.assertTrue(pre.is_candidate([], "agent"))

    def test_member_without_requires_is_not_a_candidate(self):
        self.assertFalse(pre.is_candidate([], "member"))


class TestBuild(unittest.TestCase):
    def test_reconciliation_balances_and_equals_unevaluable_in_db_mode(self):
        rows = [
            _row(id="a/One@x.go:1#1", requires=["daemon_token"]),
            _row(id="b/Two@x.go:2#2", requires=[]),
            _row(id="c/Three@x.go:3#3", requires=["daemon_token", "db_fault_injection"]),
        ]
        out = pre.build(rows, {"unevaluable": 3}, "conformance_db_json", True)
        rec = out["reconciliation"]
        self.assertTrue(rec["balanced"])
        self.assertTrue(rec["equals_unevaluable"])
        self.assertEqual(out["subfamily_count"], 3)

    def test_static_mode_refuses_to_reconcile_against_unevaluable(self):
        """🔴 `outcome` 不在契约里 ⇒ 静态面是对**超集**，不许谎报 `unevaluable` 对账。"""
        out = pre.build([_row()], {"unevaluable": 1}, "contracts_golden", False)
        self.assertIsNone(out["reconciliation"]["equals_unevaluable"])
        self.assertFalse(out["reconcilable"])
        self.assertTrue(out["reconciliation"]["balanced"])

    def test_detail_uniformity_is_reported_per_subfamily(self):
        same = "  - daemon_token: same text\n"
        rows = [
            _row(id="a/One@x.go:1#1", requires=["daemon_token"], detail=same),
            _row(id="b/Two@x.go:2#2", requires=["daemon_token"], detail="  - daemon_token: other\n"),
        ]
        out = pre.build(rows, {"unevaluable": 2}, "conformance_db_json", True)
        fam = next(s for s in out["subfamilies"] if s["name"] == "daemon_token")
        self.assertEqual(fam["count"], 2)
        self.assertFalse(fam["detail_uniform"])
        self.assertEqual(fam["mechanism_keys"], ["daemon_token"])

    def test_subfamilies_are_ordered_by_size_then_name(self):
        rows = [
            _row(id="a/One@x.go:1#1", requires=["k_one"]),
            _row(id="b/Two@x.go:2#2", requires=["k_two"]),
            _row(id="c/Three@x.go:3#3", requires=["k_two"]),
        ]
        out = pre.build(rows, {"unevaluable": 3}, "conformance_db_json", True)
        self.assertEqual([s["name"] for s in out["subfamilies"]], ["k_two", "k_one"])


class TestGoldenLoader(unittest.TestCase):
    """真实 `contracts/golden/**` 的纯静态扫描（零编译、零真库、零磁盘增长）。"""

    def test_loads_rows_and_never_claims_reconcilability(self):
        rows, totals, kind, ok = pre.load_from_golden(GOLDEN)
        self.assertGreater(len(rows), 0)
        self.assertEqual(kind, "contracts_golden")
        self.assertFalse(ok)
        self.assertEqual(totals, {})
        self.assertEqual([r["id"] for r in rows], sorted(r["id"] for r in rows))
        for row in rows:
            self.assertTrue(pre.is_candidate(row["requires"], str(row["actor"])), row["id"])
            self.assertEqual(row["requires"], sorted(set(row["requires"])))
            self.assertIn("status_expected", row)
            self.assertNotIn("outcome", row)

    def test_every_row_falls_in_exactly_one_subfamily(self):
        """读数自洽：子族条数之和 == 候选数（这一条是派工单的分母）。"""
        rows, _t, _k, _o = pre.load_from_golden(GOLDEN)
        out = pre.build(rows, {}, "contracts_golden", False)
        self.assertTrue(out["reconciliation"]["balanced"])
        self.assertEqual(
            out["reconciliation"]["sum_of_subfamilies"], out["candidates"]
        )

    def test_non_fixture_json_is_skipped_not_crashed_on(self):
        fixture = {
            "id": "chat/TestX@x.go:1#1",
            "method": "GET",
            "path": "/api/chat/history",
            "actor": {"kind": "agent"},
            "expect": {"status": 200},
            "extraction": {"requires": ["daemon_token"]},
            "source": {"file": "x.go", "line": 1},
        }
        with tempfile.TemporaryDirectory() as tmp:
            with open(os.path.join(tmp, "junk.json"), "w", encoding="utf-8") as fh:
                json.dump({"totals": 1}, fh)                      # 无 id/expect ⇒ 跳过
            with open(os.path.join(tmp, "broken.json"), "w", encoding="utf-8") as fh:
                fh.write("{not json")                            # 解析失败 ⇒ 跳过
            with open(os.path.join(tmp, "ok.json"), "w", encoding="utf-8") as fh:
                json.dump(fixture, fh)
            rows, _t, _k, _o = pre.load_from_golden(tmp)
        self.assertEqual([r["id"] for r in rows], ["chat/TestX@x.go:1#1"])


class TestCli(unittest.TestCase):
    def test_main_golden_json_exits_zero_and_emits_parseable_json(self):
        argv = sys.argv
        out = io.StringIO()
        err = io.StringIO()
        try:
            sys.argv = ["t1_6_precondition_taxonomy.py", "--golden", GOLDEN, "--json"]
            with redirect_stdout(out), redirect_stderr(err):
                rc = pre.main()
        finally:
            sys.argv = argv
        self.assertEqual(rc, 0)
        doc = json.loads(out.getvalue())
        self.assertEqual(doc["input_kind"], "contracts_golden")
        self.assertFalse(doc["reconcilable"])
        self.assertGreater(doc["candidates"], 0)

    def test_main_render_human_marks_the_superset_warning(self):
        """人读输出必须自带「这是超集」的警告，否则会被当成 27 条的派工单。"""
        argv = sys.argv
        out = io.StringIO()
        try:
            sys.argv = ["t1_6_precondition_taxonomy.py", "--golden", GOLDEN]
            with redirect_stdout(out), redirect_stderr(io.StringIO()):
                rc = pre.main()
        finally:
            sys.argv = argv
        self.assertEqual(rc, 0)
        text = out.getvalue()
        self.assertIn("超集", text)
        self.assertIn("db-mode json", text)


if __name__ == "__main__":
    unittest.main(verbosity=2)

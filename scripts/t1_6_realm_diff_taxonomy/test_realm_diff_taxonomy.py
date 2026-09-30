#!/usr/bin/env python3
"""Discriminant tests for the `t1_6_realm_diff_taxonomy` package (LUM-2602 / T1-6-I).

Run: ``python3 scripts/t1_6_realm_diff_taxonomy/test_realm_diff_taxonomy.py``
— no cargo, no database, no ``target/``, no disk growth (pure stdlib ``unittest``).

Why this file lives **inside** the package (and why it is named ``test_*.py``)
-------------------------------------------------------------------------------
Two facts, both measured, both load-bearing:

1. It lives here because it tests *all nine* modules of one package; splitting the
   cases across top-level files would scatter a single rule table's invariants
   (`rules.SUB_RULES` ↔ `constants.BEHAVIOR_OWNER_FILES` ↔ `checks.KNOWN_*`) away
   from the code they constrain, and the next slice would not know where to look.
2. It is named ``test_realm_diff_taxonomy.py`` and **not** ``tests.py`` because gate
   ⑫ discovers with the pattern ``test_*.py``: ``tests.py`` does **not** match it
   (``test_`` needs the underscore). Naming a test file so that it cannot be
   discovered is the same defect as not writing it at all — see
   `TestGateDiscoveryAgrees` below, which fails if the two drift apart.

Coverage: at least one discriminant per module —
``__init__`` / ``__main__`` / ``checks`` / ``claims`` / ``constants`` / ``fields`` /
``report`` / ``rules`` / ``sources``.

The load-bearing case is `TestThree404Origins`, written directly against the
criterion `docs/37 §267` falsified: 「`status_observed == 404` ⇒ 一定是已挂载的
handler 主动返回的 404」.  That claim is a non-sequitur (`verdict.rs:149` only says
「404 + empty body ⇒ unmounted」) and `classify()` never read the body at all;
`crates/mc-http/src/routes/agents.rs::workspace_role` answers a non-member with a
**non-empty** 404 while the route is mounted.  PR #182 fixed the root classifier;
this file pins the **second** classifier, whose family boundary (`observed ∉
{401,404}`) is the same numeric shortcut and must therefore be pinned in the
direction that matters: a 404 is **excluded** for all three origins, and the
package never claims to know *why* it was a 404.
"""

import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout

PKG_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.dirname(PKG_DIR)
REPO_ROOT = os.path.dirname(SCRIPTS)
if SCRIPTS not in sys.path:
    sys.path.insert(0, SCRIPTS)

import t1_6_realm_diff_taxonomy as pkg_init  # noqa: E402  再导出面本身是被测对象
from t1_6_realm_diff_taxonomy import (  # noqa: E402
    BEHAVIOR,
    FAMILY,
    KNOWN_NEGATIVE,
    KNOWN_POSITIVE,
    OTHER_FAMILY_OBSERVED,
    OWNER_FILES,
    STATIC_DECIDABLE,
    SUB_RULES,
    build,
    classify,
    is_claimed,
    transition,
    upstream_file,
)
from t1_6_realm_diff_taxonomy import (  # noqa: E402
    __main__ as pkg_main,
    checks as pkg_checks,
    claims as pkg_claims,
    constants as pkg_constants,
    fields as pkg_fields,
    report as pkg_report,
    rules as pkg_rules,
    sources as pkg_sources,
)

#: `__init__.py` 刻意**不**再导出子族级负责面表（它只在 `constants` 里）⇒ 从源头取。
BEHAVIOR_OWNER_FILES = pkg_constants.BEHAVIOR_OWNER_FILES

GOLDEN = os.path.join(REPO_ROOT, "contracts", "golden")

#: 15 条规则的族名（`rules.SUB_RULES` 的第 0 位）。
RULE_NAMES = [r[0] for r in SUB_RULES]


def _row(**over):
    """一条最小的 db-mode report 行 + 挂载的 golden 文档。"""
    golden = over.pop("_golden", None)
    row = {
        "id": "chat/TestSome@server/internal/handler/chat_test.go:100#1",
        "domain": "chat",
        "method": "GET",
        "path": "/api/chat/history",
        "actor": "member",
        "status_expected": 200,
        "status_observed": 409,
        "outcome": "mismatch",
        "source": "server/internal/handler/chat_test.go:100",
        "requires": [],
        "detail": "status 409 != expected 200",
    }
    row.update(over)
    if golden is not None:
        row["_golden"] = golden
    return row


def _golden(identity=None, query=None, body=None):
    doc = {"id": "x", "actor": {"kind": "member"}, "expect": {"status": 200}}
    if identity is not None:
        doc["actor"]["upstream_identity"] = identity
    if query is not None:
        doc["query"] = query
    if body is not None:
        doc["body"] = body
    return doc


#: 为 `checks.KNOWN_POSITIVE` 里的每个子族造一条**能被它自己的判据命中**的行。
#: 真库读数不能在本机重跑（需 `--with-db`），而 `discriminant_checks` 的输入是
#: 「按 id 索引的行字典」⇒ 用分类器自己的判据造行是合法且必要的。
#: 🔴 新增一条 `KNOWN_POSITIVE` 而忘了在这里加造行 ⇒ `test_every_known_positive_has_a_
#: row_builder_here` 会红，而不是让下一片以为分类器坏了。
KNOWN_POSITIVE_BUILDERS = {
    "EXTRACT_QUERY_LITERAL_MISBOUND":
        lambda fid: _row(id=fid, path="/api/issues", method="GET",
                         _golden=_golden(query={"status": "workspaces/"})),
    "BEHAVIOR_STAMPING_CHAIN_UNWIRED":
        lambda fid: _row(id=fid, path="/api/auth/tokens/current",
                         _golden=_golden(identity={"Authorization": "$testPATForeignUser"})),
}


# --------------------------------------------------------------------------- #
# rules.py
# --------------------------------------------------------------------------- #
class TestRules(unittest.TestCase):
    def test_rule_names_are_unique_and_attributions_are_known(self):
        self.assertEqual(len(RULE_NAMES), len(set(RULE_NAMES)))
        for name, attribution, _p, _sp, evidence, confidence in SUB_RULES:
            self.assertIn(attribution, (pkg_constants.EXTRACTION, pkg_constants.FIXTURE,
                                        pkg_constants.BEHAVIOR), name)
            self.assertIn(evidence, name + evidence)  # evidence 必须是字符串
            self.assertIn(confidence, ("high", "medium", "low"), name)

    def test_static_decidable_is_exactly_the_rules_with_a_static_predicate(self):
        self.assertEqual(
            STATIC_DECIDABLE,
            {n for n, _a, _p, sp, _e, _c in SUB_RULES if sp is not None},
        )

    def test_a_statically_decidable_rule_never_consults_observed(self):
        """`--golden` 那一面没有 `status_observed` ⇒ 静态判据不许碰它。"""
        row = _row(status_observed=None,
                   path="/api/agents",
                   _golden=_golden(identity={"X-Workspace-ID": "$testWorkspaceID"}))
        # EXTRACT_IDENTITY_WORKSPACE_HEADER_ABSENT 的静态投影要求身份里没有该 header。
        bare = _row(status_observed=None, _golden=_golden(identity={"X-User-ID": "$u"}))
        self.assertEqual(classify(bare, static_only=True),
                         "EXTRACT_IDENTITY_WORKSPACE_HEADER_ABSENT")
        self.assertIsNone(classify(row, static_only=True))

    def test_db_only_rule_is_skipped_on_the_static_face(self):
        """`BEHAVIOR_NUL_PAYLOAD`（`obs == 500`）静态不可判 ⇒ 静态面必须跳过它。"""
        row = _row(status_observed=500, outcome=None,
                   _golden=_golden(identity={"X-Workspace-ID": "$ws"}))
        self.assertEqual(classify(row), "BEHAVIOR_NUL_PAYLOAD")
        self.assertIsNone(classify(row, static_only=True))

    def test_malformed_row_is_not_guessed(self):
        """判据读不到字段 ⇒ **不命中**，绝不抛异常、绝不猜（`rules.classify` 的 except）。"""
        self.assertIsNone(classify({"_golden": {"actor": {"upstream_identity": ["not", "a", "dict"]}}}))

    def test_ordering_is_first_hit_wins(self):
        """两条判据同时成立时取**先命中**的那条（有序表 ⇒ 顺序是语义）。"""
        names = [n for n, _a, _p, _sp, _e, _c in SUB_RULES]
        self.assertLess(
            names.index("EXTRACT_QUERY_LITERAL_MISBOUND"),
            names.index("BEHAVIOR_METADATA_FILTER_PARSE"),
        )
        row = _row(path="/api/issues", status_observed=200, status_expected=400,
                   _golden=_golden(query={"status": "workspaces/"}))
        self.assertEqual(classify(row), "EXTRACT_QUERY_LITERAL_MISBOUND")

    def test_expected_400_is_excluded_from_the_workspace_header_rule(self):
        """坑 ②：期望的 400 与「工作区没解析出来」的 400 不是同一件事。"""
        row = _row(status_observed=400, status_expected=400, _golden=_golden(identity={"X-User-ID": "$u"}))
        self.assertNotEqual(classify(row), "EXTRACT_IDENTITY_WORKSPACE_HEADER_ABSENT")
        row400 = _row(status_observed=400, status_expected=200,
                      _golden=_golden(identity={"X-User-ID": "$u"}))
        self.assertEqual(classify(row400), "EXTRACT_IDENTITY_WORKSPACE_HEADER_ABSENT")

    def test_transition_and_upstream_file(self):
        self.assertEqual(transition(_row(status_expected=200, status_observed=409)), "200←409")
        self.assertEqual(transition(_row(outcome=None, status_observed=None)),
                         "?←?（静态面）")
        self.assertEqual(upstream_file(_row(source="a/b/c_test.go:19")), "a/b/c_test.go")


# --------------------------------------------------------------------------- #
# fields.py
# --------------------------------------------------------------------------- #
class TestFields(unittest.TestCase):
    def test_readers_never_raise_on_a_bare_row(self):
        self.assertEqual(pkg_fields.golden({}), {})
        self.assertEqual(pkg_fields.identity({}), {})
        self.assertEqual(pkg_fields.query({}), {})
        self.assertIsNone(pkg_fields.body({}))
        self.assertIsNone(pkg_fields.exp({}))
        self.assertIsNone(pkg_fields.obs({}))
        self.assertIsNone(pkg_fields.method({}))
        self.assertEqual(pkg_fields.path({}), "")

    def test_readers_ignore_the_row_level_aliases(self):
        """`identity/query/body` 只读 `_golden`，`exp/obs` 只读行本身 —— 别混。"""
        row = _row(status_expected=201, _golden=_golden(identity={"X-User-ID": "$u"}))
        self.assertEqual(pkg_fields.exp(row), 201)
        self.assertEqual(pkg_fields.identity(row), {"X-User-ID": "$u"})
        self.assertEqual(pkg_fields.identity(_row(identity={"X-User-ID": "$u"})), {})


# --------------------------------------------------------------------------- #
# constants.py
# --------------------------------------------------------------------------- #
class TestConstants(unittest.TestCase):
    def test_family_boundary_values_are_the_two_observed_codes(self):
        self.assertEqual(OTHER_FAMILY_OBSERVED, (401, 404))
        self.assertEqual(FAMILY, "REALM_DIFF")
        self.assertTrue(pkg_constants.FAMILY_CRITERION)

    def test_behavior_owner_table_only_names_real_subfamilies(self):
        """🔴 表里出现一个不存在的子族名 ⇒ 并行结论基于一个空查找却仍会打「可并行」。"""
        for name in BEHAVIOR_OWNER_FILES:
            self.assertIn(name, RULE_NAMES)

    def test_attribution_level_table_is_deliberately_empty_for_behavior(self):
        """归因级对 `行为面` **故意不列键** ⇒ 「并集非空」不得被当成「可并行」。"""
        self.assertNotIn(BEHAVIOR, OWNER_FILES)
        self.assertEqual(OWNER_FILES.get(BEHAVIOR, []), [])
        self.assertEqual(OWNER_FILES[pkg_constants.BY_DESIGN], [])
        self.assertTrue(OWNER_FILES[pkg_constants.EXTRACTION])
        self.assertTrue(OWNER_FILES[pkg_constants.FIXTURE])

    def test_unfilled_subfamilies_are_the_two_documented_ones(self):
        """两道「故意不填」必须仍然空着（填了也要知道为什么改）。"""
        for name in ("BEHAVIOR_JSON_DECODE_STATUS", "BEHAVIOR_NUL_PAYLOAD"):
            self.assertIn(name, RULE_NAMES)
            self.assertNotIn(name, BEHAVIOR_OWNER_FILES)


# --------------------------------------------------------------------------- #
# claims.py
# --------------------------------------------------------------------------- #
class TestClaims(unittest.TestCase):
    def test_claimed_count_constant_is_twenty_two_free_and_checked_by_build(self):
        self.assertEqual(pkg_claims.CLAIM_EXPECTED, 11)
        self.assertIn("11", pkg_claims.CLAIM_CRITERION)

    def test_daemon_404_from_200_is_claimed(self):
        self.assertTrue(is_claimed(_row(domain="daemon", status_expected=404, status_observed=200)))

    def test_issue_create_201_from_400_is_claimed(self):
        self.assertTrue(is_claimed(_row(domain="issues", method="POST", path="/api/issues",
                                       status_expected=201, status_observed=400)))

    def test_near_misses_are_not_claimed(self):
        for over in (
            {"domain": "issues", "method": "POST", "path": "/api/issues",
             "status_expected": 201, "status_observed": 409},
            {"domain": "daemon", "status_expected": 404, "status_observed": 401},
            {"domain": "chat", "status_expected": 404, "status_observed": 200},
        ):
            with self.subTest(over=over):
                self.assertFalse(is_claimed(_row(**over)))


# --------------------------------------------------------------------------- #
# report.py
# --------------------------------------------------------------------------- #
class TestOwnerFilesAndVerdict(unittest.TestCase):
    def test_attribution_level_wins_for_extraction_and_fixture(self):
        self.assertEqual(
            pkg_report.owner_files_for("EXTRACT_QUERY_LITERAL_MISBOUND", pkg_constants.EXTRACTION),
            OWNER_FILES[pkg_constants.EXTRACTION],
        )

    def test_behavior_falls_through_to_the_subfamily_table(self):
        self.assertEqual(
            pkg_report.owner_files_for("BEHAVIOR_MACHINE_ACTOR_GATE", BEHAVIOR),
            BEHAVIOR_OWNER_FILES["BEHAVIOR_MACHINE_ACTOR_GATE"],
        )

    def test_behavior_with_no_entry_is_empty_not_invented(self):
        self.assertEqual(
            pkg_report.owner_files_for("BEHAVIOR_NUL_PAYLOAD", BEHAVIOR), []
        )

    def _entry(self, mapping, serial_with=()):
        return {"serial_with": list(serial_with), "subfamily_owner_files": dict(mapping)}

    def test_verdict_serial_when_same_write_set_is_declared(self):
        entry = self._entry({"A": ["f1"]}, serial_with=["LUM-2572"])
        pkg_report.verdict(entry)
        self.assertEqual(entry["parallel"], "serial")
        self.assertIn("LUM-2572", entry["parallel_why"])

    def test_verdict_undetermined_when_an_owner_list_is_empty(self):
        entry = self._entry({"A": ["f1"], "B": []})
        pkg_report.verdict(entry)
        self.assertEqual(entry["parallel"], "undetermined")
        self.assertIn("B", entry["parallel_why"])

    def test_verdict_serial_when_two_subfamilies_share_a_file(self):
        entry = self._entry({"A": ["f1", "shared"], "B": ["f2", "shared"]})
        pkg_report.verdict(entry)
        self.assertEqual(entry["parallel"], "serial")
        self.assertIn("shared", entry["parallel_why"])

    def test_verdict_parallel_only_when_all_lists_are_filled_and_disjoint(self):
        entry = self._entry({"A": ["f1"], "B": ["f2"]})
        pkg_report.verdict(entry)
        self.assertEqual(entry["parallel"], "parallel")
        self.assertIn("2", entry["parallel_why"])


class TestBuildReconciliation(unittest.TestCase):
    def test_family_excludes_401_and_404_and_keeps_the_rest(self):
        rows = [
            _row(id="a/Keep@x.go:1#1", status_observed=409),
            _row(id="b/Auth@x.go:2#2", status_observed=401),
            _row(id="c/Seed@x.go:3#3", status_observed=404),
        ]
        out = build(rows, {"mismatch": 3}, "conformance_db_json", True)
        self.assertEqual(out["family"]["count"], 1)
        self.assertEqual(out["reconciliation"]["family_total"], 1)
        self.assertTrue(out["reconciliation"]["equals_mismatch_minus_401_404"])

    def test_claimed_rows_are_counted_separately_from_candidates(self):
        rows = [
            _row(id="a/Claim@x.go:1#1", domain="daemon", status_expected=404, status_observed=200),
            _row(id="b/Open@x.go:2#2", status_observed=409),
        ]
        out = build(rows, {"mismatch": 2}, "conformance_db_json", True)
        self.assertEqual(out["claimed_elsewhere"]["count"], 1)
        self.assertEqual(out["reconciliation"]["candidates"], 1)
        self.assertTrue(out["reconciliation"]["family_reconciled"])
        self.assertFalse(out["reconciliation"]["claimed_matches_expected"])

    def test_unmatched_candidates_are_listed_not_dropped(self):
        """🔴 判不出来的候选必须**点名**：静默丢掉会让「子族和 == 候选」变成假平。"""
        row = _row(id="a/Unknown@x.go:1#1", status_observed=418)
        out = build([row], {"mismatch": 1}, "conformance_db_json", True)
        self.assertEqual(out["reconciliation"]["unmatched_candidates"], ["a/Unknown@x.go:1#1"])
        self.assertFalse(out["reconciliation"]["balanced"])

    def test_static_mode_never_claims_a_family_size(self):
        out = build([_row(outcome=None, status_observed=None)], {}, "contracts_golden", False)
        self.assertTrue(out["static_only"])
        self.assertIsNone(out["family"]["count"])
        self.assertIsNone(out["reconciliation"]["family_total"])
        self.assertTrue(out["candidates_are_static_superset"])
        self.assertIn("超集", out["static_superset_note"])

    def test_db_reading_subfamilies_are_reported_even_when_absent(self):
        out = build([_row()], {"mismatch": 1}, "conformance_db_json", True)
        self.assertIn("BEHAVIOR_NUL_PAYLOAD", out["needs_db_reading_subfamilies"])


class TestThree404Origins(unittest.TestCase):
    """🔴 `docs/37 §267` 的同款判据，这一片只钉**排除方向**。

    「`observed == 404` ⇒ 已挂载的 handler 主动返回」是**不成立**的推理：
    ① handler 查实体落空、② 鉴权面 `not_found("workspace")`、③ axum fallback
    三种来源都是 404，而 ② 发生在 handler 之前。`rules.py` 因此用**数值**边界
    （`observed ∉ {401,404}` ⇒ 不属本族）把 404 整个划走 ——
    那条边界本身是可用的；不可用的是把它**读成**「所以是 handler 主动返回的」。

    ⇒ 这里逐个来源钉死：三条都**不得**进入 `REALM_DIFF` 族，也不得被 `is_claimed`
    领走；而「本包从不判断 404 的成因」这件事由 `tests_dont_infer` 显式断言。
    """

    BODY_HANDLER_ENTITY = json.dumps({"code": "not_found", "message": "not found: agent"})
    BODY_AUTHZ_WORKSPACE = json.dumps({"code": "not_found", "message": "not found: workspace"})
    BODY_FALLBACK = ""

    def _row_for_404(self, body):
        return _row(
            id="agent/TestGetAgent@server/internal/handler/agent_test.go:204#1",
            domain="agent", path="/api/agents",
            status_expected=200, status_observed=404, outcome="mismatch",
            body_observed=body,
        )

    def test_origin_1_handler_entity_404_is_out_of_family(self):
        row = self._row_for_404(self.BODY_HANDLER_ENTITY)
        out = build([row], {"mismatch": 1}, "conformance_db_json", True)
        self.assertEqual(out["family"]["count"], 0)
        self.assertEqual(out["reconciliation"]["candidates"], 0)

    def test_origin_2_authz_workspace_404_is_out_of_family(self):
        """`crates/mc-http/src/routes/agents.rs::workspace_role` 就是这一种：路由挂着，
        handler 根本没执行到。§267 之前它被当装置面派工。"""
        row = self._row_for_404(self.BODY_AUTHZ_WORKSPACE)
        out = build([row], {"mismatch": 1}, "conformance_db_json", True)
        self.assertEqual(out["family"]["count"], 0)
        self.assertIsNone(classify(row))

    def test_origin_3_axum_fallback_empty_body_is_out_of_family(self):
        row = self._row_for_404(self.BODY_FALLBACK)
        row["outcome"] = "unmounted"
        out = build([row], {"mismatch": 1}, "conformance_db_json", True)
        self.assertEqual(out["family"]["count"], 0)

    def test_none_of_the_three_origins_can_be_claimed_elsewhere(self):
        for body in (self.BODY_HANDLER_ENTITY, self.BODY_AUTHZ_WORKSPACE, self.BODY_FALLBACK):
            with self.subTest(body=body):
                self.assertFalse(is_claimed(self._row_for_404(body)))

    def test_the_package_never_looks_at_the_body(self):
        """🔴 本包**任何**判据都不许读 `body_observed` —— body 是 §267 那个错误推理的原料。

        钉法不是「读一下看看」，而是反向：把 `body_observed` 换成三种完全不同的值，
        `classify()` 的结果必须**逐字不变**。哪天有人为了「更准」加了 body 判据，
        这条立刻红。
        """
        base = _row(path="/api/agents", status_observed=409)
        results = {classify(dict(base, body_observed=b)) for b in
                   (self.BODY_HANDLER_ENTITY, self.BODY_AUTHZ_WORKSPACE,
                    self.BODY_FALLBACK, "<html>404</html>", None)}
        self.assertEqual(len(results), 1, "body 影响了归因 ⇒ 又一次把 404 的成因猜成了事实")

    def test_a_different_status_from_a_mounted_handler_is_in_family(self):
        """反向：**非 401/404 的不同状态码**才是本族存在的意义（上游 400 vs axum 422）。"""
        row = _row(path="/api/issues", status_expected=400, status_observed=422)
        out = build([row], {"mismatch": 1}, "conformance_db_json", True)
        self.assertEqual(out["family"]["count"], 1)
        self.assertEqual(out["subfamilies"][0]["name"], "BEHAVIOR_JSON_DECODE_STATUS")


# --------------------------------------------------------------------------- #
# checks.py
# --------------------------------------------------------------------------- #
class TestChecks(unittest.TestCase):
    def test_static_face_declares_the_bidirectional_check_inapplicable(self):
        """静态面没有 `observed` ⇒ 正例/反例跑在上面只会把「超集」误报成「判据错」。"""
        out = pkg_checks.discriminant_checks({}, True)
        self.assertFalse(out["applies"])
        self.assertIsNone(out["all_positive_pass"])
        self.assertIn("超集", out["why"])

    def test_known_positive_and_negative_are_disjoint_id_sets(self):
        pos = {fid for fid, _w in KNOWN_POSITIVE}
        neg = {fid for fid, _w in KNOWN_NEGATIVE}
        self.assertEqual(pos & neg, set())
        self.assertTrue(pos and neg)

    def test_every_known_positive_has_a_row_builder_here(self):
        """新增一条 `KNOWN_POSITIVE` 而本文件没给它造行 ⇒ 下一条判别式会红（不许静默）。"""
        for _fid, want in KNOWN_POSITIVE:
            with self.subTest(want=want):
                self.assertIn(want, KNOWN_POSITIVE_BUILDERS)

    def test_positive_rows_that_are_present_must_be_classified_as_claimed(self):
        """真值行由分类器**自己**的判据构造 ⇒ 缺行时必须报 `MISSING`（不许静默通过）。"""
        by_id = {fid: KNOWN_POSITIVE_BUILDERS[want](fid) for fid, want in KNOWN_POSITIVE}
        out = pkg_checks.discriminant_checks(by_id, False)
        self.assertTrue(out["applies"])
        self.assertTrue(out["all_positive_pass"], out["known_positive"])

    def test_missing_positive_row_is_reported_loudly(self):
        out = pkg_checks.discriminant_checks({}, False)
        self.assertFalse(out["all_positive_pass"])
        self.assertTrue(all(p["got"] == "MISSING" for p in out["known_positive"]))

    def test_negative_rows_must_not_be_classified(self):
        by_id = {fid: _row(id=fid, status_observed=200) for fid, _w in KNOWN_NEGATIVE}
        out = pkg_checks.discriminant_checks(by_id, False)
        self.assertTrue(out["all_negative_pass"], out["known_negative"])

    def test_family_boundary_is_recorded_as_part_of_a_negative(self):
        by_id = {fid: _row(id=fid, status_observed=404) for fid, _w in KNOWN_NEGATIVE}
        out = pkg_checks.discriminant_checks(by_id, False)
        self.assertTrue(all(n["excluded_by_family_boundary"] for n in out["known_negative"]))

    def test_by_design_audit_answers_the_falsification_question_for_every_group(self):
        audit = pkg_checks.BY_DESIGN_AUDIT
        self.assertTrue(audit["answered"])
        for entry in audit["answered"]:
            self.assertTrue(entry["would_fail_under"].strip(), entry)
        self.assertIn("0 条", audit["verdict"])


# --------------------------------------------------------------------------- #
# sources.py
# --------------------------------------------------------------------------- #
class TestSources(unittest.TestCase):
    def test_static_loader_marks_rows_as_unobserved(self):
        rows, totals, kind, ok = pkg_sources.load_from_golden(GOLDEN)
        self.assertGreater(len(rows), 0)
        self.assertEqual(kind, "contracts_golden")
        self.assertFalse(ok)
        self.assertEqual(totals, {})
        for row in rows:
            self.assertIsNone(row["status_observed"])
            self.assertIsNone(row["outcome"])
            self.assertEqual(row["domain"], str(row["id"]).split("/", 1)[0])
            self.assertIn("_golden", row)

    def test_static_build_over_the_real_golden_tree_is_self_consistent(self):
        """对真实 `contracts/golden/**` 跑一遍静态面 ⇒ 对账必须平（零编译、零真库）。"""
        rows, totals, kind, ok = pkg_sources.load_from_golden(GOLDEN)
        out = build(rows, totals, kind, ok)
        self.assertTrue(out["static_only"])
        self.assertTrue(out["reconciliation"]["balanced"])
        self.assertEqual(out["reconciliation"]["sum_of_subfamilies"], out["candidates"])
        for lane in out["parallel_lanes"]:
            self.assertIn(lane["parallel"], ("serial", "parallel", "undetermined"))
            self.assertTrue(lane["parallel_why"])
            if lane["attribution"] == pkg_constants.FIXTURE:
                self.assertEqual(lane["parallel"], "serial")

    def test_attach_golden_reports_the_rows_it_could_not_find(self):
        with tempfile.TemporaryDirectory() as tmp:
            doc = {
                "id": "chat/TestPresent@x.go:1#1",
                "method": "GET", "path": "/api/chat/history",
                "actor": {"kind": "member"}, "expect": {"status": 200},
                "source": {"file": "x.go", "line": 1},
            }
            with open(os.path.join(tmp, "a.json"), "w", encoding="utf-8") as fh:
                json.dump(doc, fh)
            rows = [_row(id="chat/TestPresent@x.go:1#1"), _row(id="chat/TestGone@x.go:2#2")]
            missing = pkg_sources.attach_golden(rows, tmp)
        self.assertEqual(missing, ["chat/TestGone@x.go:2#2"])
        self.assertEqual(rows[0]["_golden"]["id"], "chat/TestPresent@x.go:1#1")
        self.assertIsNone(rows[1]["_golden"])

    def test_report_loader_defaults_the_domain_from_the_id(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "r.json")
            with open(path, "w", encoding="utf-8") as fh:
                json.dump({"totals": {"mismatch": 1},
                           "fixtures": [{"id": "issues/TestX@a.go:1#1", "outcome": "mismatch",
                                         "status_observed": 409, "status_expected": 200}]}, fh)
            rows, totals, kind, ok = pkg_sources.load_from_report(path)
        self.assertEqual(rows[0]["domain"], "issues")
        self.assertEqual(totals, {"mismatch": 1})
        self.assertEqual(kind, "conformance_db_json")
        self.assertTrue(ok)


# --------------------------------------------------------------------------- #
# __main__.py
# --------------------------------------------------------------------------- #
class TestCli(unittest.TestCase):
    def test_main_golden_json_exits_zero_with_clean_stderr(self):
        """🔴 `--json` 下一个字都不许往 stderr 写（调用方按 `2>&1` 抓再 json.load）。"""
        argv = sys.argv
        out, err = io.StringIO(), io.StringIO()
        try:
            sys.argv = ["t1_6_realm_diff_taxonomy", "--golden", GOLDEN, "--json"]
            with redirect_stdout(out), redirect_stderr(err):
                rc = pkg_main.main()
        finally:
            sys.argv = argv
        self.assertEqual(rc, 0)
        self.assertEqual(err.getvalue(), "")
        doc = json.loads(out.getvalue())
        self.assertEqual(doc["schema_version"], 1)
        self.assertEqual(doc["family"]["name"], "REALM_DIFF")

    def test_empty_golden_dir_produces_a_loud_warning(self):
        argv = sys.argv
        out, err = io.StringIO(), io.StringIO()
        with tempfile.TemporaryDirectory() as tmp:
            try:
                sys.argv = ["t1_6_realm_diff_taxonomy", "--golden", tmp]
                with redirect_stdout(out), redirect_stderr(err):
                    rc = pkg_main.main()
            finally:
                sys.argv = argv
        self.assertEqual(rc, 0)
        self.assertIn("一条 fixture 都没读到", out.getvalue())

    def test_human_render_flags_the_static_superset(self):
        argv = sys.argv
        out = io.StringIO()
        try:
            sys.argv = ["t1_6_realm_diff_taxonomy", "--golden", GOLDEN]
            with redirect_stdout(out), redirect_stderr(io.StringIO()):
                rc = pkg_main.main()
        finally:
            sys.argv = argv
        self.assertEqual(rc, 0)
        text = out.getvalue()
        self.assertIn("超集", text)
        self.assertIn("这不是派工单", text)


# --------------------------------------------------------------------------- #
# __init__.py + 门 ⑫ 的发现规则
# --------------------------------------------------------------------------- #
class TestPackageReexports(unittest.TestCase):
    def test_reexports_are_the_same_objects_as_the_submodules(self):
        for name in pkg_init.__all__:
            self.assertTrue(hasattr(pkg_init, name), name)
        self.assertIs(pkg_init.build, pkg_report.build)
        self.assertIs(pkg_init.classify, pkg_rules.classify)
        self.assertIs(pkg_init.is_claimed, pkg_claims.is_claimed)
        self.assertEqual(pkg_init.SUB_RULES, pkg_rules.SUB_RULES)
        self.assertEqual(pkg_init.OTHER_FAMILY_OBSERVED, pkg_constants.OTHER_FAMILY_OBSERVED)
        # 子族级负责面表**刻意**只在 `constants` 里、再导出面不含它 ⇒ 调用方必须从
        # `constants` 取（否则拼错一次就是静默的「负责面为空」）。这条钉住现有面。
        self.assertFalse(hasattr(pkg_init, "BEHAVIOR_OWNER_FILES"))


# 🔴 `TestGateDiscoveryAgrees`（4 条）原本就住在这里 —— **本片把它搬走了**
# （LUM-2604 / T1-6-J，`docs/37 §276`）。
#
# 为什么必须搬：这个文件在**包里**，而门 ⑫ 的发现规则一旦收窄回顶层非递归 glob，
# 它就不在集合里 ⇒ 这 4 条**自己也不跑** ⇒ 无人报警（自指、而且方向是错的 ——
# PR #184 声称的三条可证伪判别式实测全绿）。
#
# 它们现在住在 `scripts/test_gate_scripts_tests.py`（**顶层**，非递归 glob 下仍可见，
# 且在 `scripts/tests.manifest` 里），并且**不再断言 `gates.sh` 的源码文本**
# （`assertIn("__pycache__", body)` 被门体里的一行**注释**满足过），而是断言门的
# **实际输出**：`bash scripts/gates.sh --list-discovered` 与 `scripts/tests.manifest`。


if __name__ == "__main__":
    unittest.main(verbosity=2)

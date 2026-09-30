#!/usr/bin/env python3
"""Regression tests for `t1_6_taxonomy.classify`'s 404 criterion.

Run: ``python3 scripts/test_t1_6_taxonomy.py`` (no cargo, no database, no upstream
Go tree needed).

Why these tests exist (LUM-2597).  The classifier's docstring claimed
「404 ⇒ 已挂载的 handler 主动返回的」 and justified it with
`verdict.rs:149` (`404 && empty_body && !json ⇒ Unmounted`).  That is a
non-sequent: `verdict.rs` states 「404 + empty body ⇒ unmounted」, which does
**not** entail 「non-empty 404 ⇒ the handler answered it」.  And `classify()`
never read the body at all.  Concretely
`crates/mc-http/src/routes/agents.rs::workspace_role` answers a non-member with
`not_found("workspace")` — a **non-empty** 404 body with the route mounted.  So
13 authz-side gaps were filed as `SEED_404` (owner `mc-conformance::seed`).

The rule is pinned here against **synthetic minimal fixtures** — one per 404
origin — because the real `--with-db` report cannot be regenerated on this
machine, and because a real report is not a legal positive example anyway: it
carries no observed body (`mc-conformance::report::Row` has no such field), so it
exercises only the `absent` branch.  Both directions are pinned: ① must not be
filed as ②, and ② must not be filed as ①.
"""

import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from t1_6_taxonomy import FAMILIES, classify, main  # noqa: E402


def _fx(**over):
    """一条最小的 report row（字段名照 `mc-conformance::report::Row`）。"""
    row = {
        "id": "agent_test/SomeTest@server/internal/handler/agent_test.go:105#1",
        "domain": "agent",
        "method": "GET",
        "path": "/api/agents",
        "actor": "member",
        "via": "handler",
        "status_expected": 200,
        "source": "server/internal/handler/agent_test.go:105",
        "requires": [],
        "outcome": "mismatch",
        "tier": "database",
        "status_observed": 404,
        "detail": "status 404 != expected 200",
    }
    row.update(over)
    return row


# The two bodies are the real envelopes (`mc-errors/src/http.rs::ErrorResponse`).
BODY_HANDLER_ENTITY = json.dumps({"code": "not_found", "message": "not found: agent"})
BODY_AUTHZ_WORKSPACE = json.dumps({"code": "not_found", "message": "not found: workspace"})
BODY_AUTHZ_CODE = json.dumps({"code": "workspace_not_found", "message": "workspace not found: ws-1"})


class TestThree404Origins(unittest.TestCase):
    """404 在本仓有三种来源，三条各一条，双向钉死。"""

    def test_1_handler_entity_404_is_seed_404(self):
        """① handler 查实体落空 ⇒ 装置面（seed）。body 指向业务实体。"""
        self.assertEqual(classify(_fx(body_observed=BODY_HANDLER_ENTITY)), "SEED_404")

    def test_2_authz_workspace_404_is_authz_404(self):
        """② 鉴权面先答 ⇒ 不是装置面。fixture 期望 200，拿到 workspace 作用域 404。"""
        self.assertEqual(classify(_fx(body_observed=BODY_AUTHZ_WORKSPACE)), "AUTHZ_404")

    def test_2b_distinct_workspace_not_found_code_is_authz_404(self):
        """`workspace_not_found` 是独立业务码（`mc-errors/src/lib.rs:118`），无歧义。"""
        self.assertEqual(classify(_fx(body_observed=BODY_AUTHZ_CODE)), "AUTHZ_404")

    def test_3_axum_fallback_empty_body_is_unmounted(self):
        """③ 空 body 的 404 才是 fallback ⇒ 未挂载。上一版把它当 SEED_404。"""
        self.assertEqual(classify(_fx(outcome="unmounted", body_observed="")), "UNMOUNTED")
        self.assertEqual(
            classify(_fx(outcome="mismatch", body_observed="")),
            "UNMOUNTED",
            "空 body 的 404 与 outcome 无关：verdict.rs:149 的判据就是 body 空",
        )

    def test_detail_empty_body_is_unmounted(self):
        """`body_empty: true` 与 detail 里的 `404 with empty body` 都是空 body 证据。"""
        self.assertEqual(classify(_fx(outcome="unmounted", body_empty=True)), "UNMOUNTED")
        self.assertEqual(
            classify(
                _fx(
                    outcome="unmounted",
                    detail="no route: 404 with empty body (axum fallback), expected 200",
                )
            ),
            "UNMOUNTED",
        )


class TestBothDirections(unittest.TestCase):
    """族判据既不能把 ① 误判成 ②，也不能把 ② 误判成 ①。"""

    def test_workspace_body_with_expected_404_stays_seed_404(self):
        """`status_expected == 404` 的 fixture 是在测「作用域/实体缺失」⇒ 装置面，不是鉴权面。

        同一份 body（`not found: workspace`）在两种期望下必须落到不同族 ——
        这就是 body 单独不够、必须叠 `status_expected` 的原因。
        """
        self.assertEqual(
            classify(_fx(status_expected=404, body_observed=BODY_AUTHZ_WORKSPACE)), "SEED_404"
        )

    def test_entity_body_never_becomes_authz(self):
        for expected in (200, 201, 204, 403, 404):
            with self.subTest(expected=expected):
                self.assertEqual(
                    classify(_fx(status_expected=expected, body_observed=BODY_HANDLER_ENTITY)),
                    "SEED_404",
                )

    def test_non_json_body_is_not_workspace_scope(self):
        """非 JSON 的非空 body 不许被猜成 workspace 作用域（判据不许靠字符串瞎猜）。"""
        self.assertEqual(classify(_fx(body_observed="not found: workspace")), "SEED_404")


class TestOrderAndOtherFamilies(unittest.TestCase):
    """前两刀（outcome 优先、401）不受本次改动影响。"""

    def test_outcome_wins_over_404(self):
        self.assertEqual(classify(_fx(outcome="unevaluable")), "PRECONDITION")
        self.assertEqual(classify(_fx(outcome="unmounted", body_observed=BODY_AUTHZ_WORKSPACE)), "UNMOUNTED")

    def test_requires_never_participates(self):
        """`requires` 只印证 unevaluable，不参与分族（58 条 mismatch 里有 8 条带 requires）。"""
        self.assertEqual(
            classify(_fx(requires=["daemon_token"], body_observed=BODY_HANDLER_ENTITY)),
            "SEED_404",
        )

    def test_401_and_realm_diff_unchanged(self):
        self.assertEqual(classify(_fx(status_observed=401)), "AUTH_401")
        self.assertEqual(classify(_fx(status_observed=409)), "REALM_DIFF")


class TestMissingBodyEvidenceIsLoud(unittest.TestCase):
    """🔴 分母不许静默搬家：body 证据缺失时必须报「不可派工」，不能装作判据跑过。"""

    def test_absent_body_evidence_falls_back_to_seed_404(self):
        """本仓当前的 `report.json` 就是这种：没有 body 字段。"""
        self.assertEqual(classify(_fx()), "SEED_404")

    def test_report_run_marks_seed_404_unauthoritative(self):
        doc = {"totals": {"fixtures": 2, "pass": 1, "mismatch": 1}, "fixtures": [_fx()]}
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "report.json")
            with open(path, "w", encoding="utf-8") as fh:
                json.dump(doc, fh)
            err = io.StringIO()
            argv = sys.argv
            with redirect_stderr(err), redirect_stdout(io.StringIO()):
                sys.argv = ["t1_6_taxonomy.py", path, "--json"]
                try:
                    self.assertEqual(main(), 0)
                finally:
                    sys.argv = argv
        self.assertIn("WARN", err.getvalue())
        self.assertIn("不可作为派工依据", err.getvalue())

    def test_families_cover_every_row_and_document_both_404_families(self):
        names = [n for n, _, _ in FAMILIES]
        self.assertEqual(len(names), len(set(names)))
        for required in ("UNMOUNTED", "PRECONDITION", "AUTH_401", "SEED_404", "AUTHZ_404", "REALM_DIFF"):
            self.assertIn(required, names)


if __name__ == "__main__":
    unittest.main(verbosity=2)

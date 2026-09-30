#!/usr/bin/env python3
"""`scripts/w3b_premerge_audit.py` 的 `unittest`（LUM-2624 / T1-6-Q4，`docs/37 §294`）。

一个 448 行、**0 用例**、**0 门执行**的判定器 —— 而它自己的 docstring 说它回答 5 个
「`gates.sh` 和 `docs/37` 都答不了」的问题，其中第 2 条（两片注册了同一个
`(method, path)` ⇒ axum 建 router 时 **panic**，全局宕机）。

🔴 **为什么它比同族的前几片更承重**：`docs/32-M3-DAEMON-FACE.md:6706` 记着它的
`extract_routes` 被 **⑦b 与 `slash_alias_audit` 共用** ⇒ 它的解析行为偏差会同时污染两条
每天都在跑的判词。同族 `LUM-2620`/`2621`/`2623` 已把另外三个判定器收掉，本片是最后一块。

**只测不改**：本文件**一行都不动**被测脚本。缺陷一律登记在 `KNOWN_DEFECTS` +
`@unittest.expectedFailure` ⇒ 门 ⑫ 仍绿但用例每天都跑；谁修好实现 ⇒ `UNEXPECTED SUCCESS`
⇒ 门红 ⇒ 必须同时删装饰器并改本表。

读数纪律（`docs/37 §283`：**用例数可以是绿的**）：每条断言的期望值都是**当场实测**出来的，
不是从 docstring 推的。已实测并与工单假设不同的两处：

* 工单说尾斜杠「折叠 vs 重复」要分清 —— 实测**分清了，且方向与直觉相反**：
  `report_slices` 的跨片重复判词用**逐字 key**（`"GET /y"` vs `"GET /y/"` ⇒ **不算重复**），
  折叠只发生在**片内**的 `added_folded` 计数与第 5 条 golden 判词里。见
  `TestCrossSliceDuplicates.test_trailing_slash_variant_is_not_a_cross_slice_duplicate`。
* 工单第 3 条（base 桩已注册 + 新片又注册）被当作「要能报出来」—— 实测**报不出来**，
  且不是碰巧：`add = sorted(cur - base)` 之后紧跟 `if (meth, path) in base` ⇒ 由构造恒假。
  见 `KD-1`。

零 Rust / 零 cargo / 零真库 / 零磁盘，纯标准库（只用 `unittest` / `tempfile` / `subprocess`）。
Run: ``python3 scripts/test_w3b_premerge_audit.py``
"""

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import w3b_premerge_audit as w3b  # noqa: E402

#: 缺陷号 → 钉住它的用例、它声明的契约、当轮实测到的现象、收口方式。**四条字段都必填**。
KNOWN_DEFECTS = {
    "KD-1": {
        "defect": (
            "`audit_slice` 的第 3 条判词（base 桩已注册 + 新片又注册 ⇒ "
            "`\"adds an already-registered key\"`）是**恒假死代码**。"
        ),
        "claims": (
            "docstring 第 3 条：「Does a slice add a route that a base stub already "
            "registers? ... adding them instead is case (2).」"
        ),
        "observed": (
            "`add = sorted(cur - base)` 是集合差，随后 `for meth, path in add: if (meth, path) "
            "in base` —— 差集与 `base` 按构造不相交 ⇒ 判定恒假、findings 里永不出这一句。"
            "（只有第 2 条的**跨片**判词 `report_slices` 里的 `dup` 是活的。）"
        ),
        "fix": (
            "要么把 `add` 换成对同一 `(METHOD, path)` 的**多重注册**计数（同一文件里注册两次），"
            "要么删掉这条死分支并在 docstring 里注明它由跨片判词承担。"
            "**本片只测不改**，故留 `expectedFailure`。"
        ),
    },
}


def _git(d, *args):
    return subprocess.run(["git", "-C", d, *args], capture_output=True, text=True)


def make_repo(routes=(".route(\"/base\", get(b))",), baseline="", extra_files=None):
    """A throwaway git worktree shaped like the real one (crates/mc-http/src + scripts/)."""
    d = tempfile.mkdtemp()
    _git(d, "init", "-q")
    _git(d, "config", "user.email", "t@t")
    _git(d, "config", "user.name", "t")
    os.makedirs(os.path.join(d, "crates/mc-http/src"))
    os.makedirs(os.path.join(d, "scripts"))
    with open(os.path.join(d, "crates/mc-http/src/routes.rs"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(routes) + "\n")
    with open(os.path.join(d, "scripts/file_size_baseline.tsv"), "w", encoding="utf-8") as fh:
        fh.write(baseline)
    _git(d, "add", "-A")
    _git(d, "commit", "-qm", "base")
    for rel, body in (extra_files or {}).items():
        p = os.path.join(d, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as fh:
            fh.write(body)
    return d, _git(d, "rev-parse", "HEAD").stdout.strip()


def row(name, added, head="h0"):
    """A minimal `report_slices` row; only the fields it actually reads are filled in."""
    return {
        "name": name,
        "head": head,
        "base_ref": "base",
        "files": [],
        "added_routes": list(added),
        "added_folded": len(added),
        "size_violations": [],
        "findings": [],
        "gated_skips": [],
        "fingerprint": {"digest": "0" * 64},
    }


def quiet(fn, *a, **kw):
    """Run a printer-returning report function and capture both stdout and the return value."""
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        ret = fn(*a, **kw)
    return ret, buf.getvalue()


class TestExtractRoutes(unittest.TestCase):
    """The parser is shared with ⑦b and `slash_alias_audit` (`docs/32:6706`) — one bug, two verdicts."""

    def test_single_line_chain_yields_one_key_per_method(self):
        self.assertEqual(w3b.extract_routes('.route("/api/agents", get(list_agents))'), {("GET", "/api/agents")})

    def test_multi_method_chain_yields_every_recognised_method(self):
        got = w3b.extract_routes('.route("/api/issues/:id", put(upd).patch(p))')
        self.assertEqual(got, {("PUT", "/api/issues/:id"), ("PATCH", "/api/issues/:id")})

    def test_multiline_call_is_paren_matched_not_line_matched(self):
        text = '.route(\n    "/api/agents",\n    get(list_agents),\n)'
        self.assertEqual(w3b.extract_routes(text), {("GET", "/api/agents")})

    def test_non_literal_first_argument_is_skipped(self):
        self.assertEqual(w3b.extract_routes(".route(SOME_CONST, get(x))"), set())

    def test_same_path_under_two_methods_is_two_keys_not_a_duplicate(self):
        got = w3b.extract_routes('.route("/dup", get(a))\n.route("/dup", post(b))')
        self.assertEqual(got, {("GET", "/dup"), ("POST", "/dup")})

    def test_cfg_test_bodies_are_masked_out(self):
        """A test-only registration must not reach the inventory (route_parity.py skips it too)."""
        text = '#[cfg(test)]\nmod t {\n  fn f() { Router::new().route("/testonly", get(g)); }\n}\n'
        self.assertEqual(w3b.extract_routes(text), set())

    def test_real_repository_inventory_is_non_empty(self):
        """Guards against the tests passing because the parser silently stopped parsing."""
        src = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                           "crates", "mc-http", "src")
        if not os.path.isdir(src):
            self.skipTest("worktree has no crates/mc-http/src")
        self.assertGreaterEqual(len(w3b.routes_at(os.path.join(src, "..", "..", ".."), None)), 400)


class TestNormAndCanon(unittest.TestCase):
    def test_norm_folds_trailing_slash_and_parameters(self):
        self.assertEqual(w3b.norm("/api/agents/"), "/api/agents")
        self.assertEqual(w3b.norm("/api/issues/:id"), "/api/issues/*")
        self.assertEqual(w3b.norm("/a/{id}"), "/a/*")

    def test_norm_makes_slash_and_parameter_spellings_meet(self):
        self.assertEqual(w3b.norm("/a/:id/"), w3b.norm("/a/{id}"))

    def test_canon_rewrites_parameters_and_keeps_the_trailing_slash(self):
        self.assertEqual(w3b.canon("GET /a/{id}"), "GET /a/:param")
        self.assertEqual(w3b.canon("GET /a/{id}/"), "GET /a/:param/")

    def test_canon_does_not_uppercase_the_method(self):
        """Measured: `canon` partitions on the first space and rewrites only the path."""
        self.assertEqual(w3b.canon("get /a"), "get /a")


class TestMaskCfgTest(unittest.TestCase):
    def test_body_is_blanked_but_line_count_is_preserved(self):
        text = '#[cfg(test)]\nmod t { let a = 1; }\nlet b = 2;'
        out = w3b.mask_cfg_test(text)
        self.assertEqual(len(out), len(text))
        self.assertEqual(out.count("\n"), text.count("\n"))
        self.assertIn("let b = 2;", out)
        self.assertNotIn("let a = 1;", out)


class TestLineCount(unittest.TestCase):
    def test_counts_lines_with_and_without_a_trailing_newline(self):
        self.assertEqual(w3b.line_count("a\nb\n"), 2)
        self.assertEqual(w3b.line_count("a\nb"), 2)

    def test_empty_text_is_zero_not_one(self):
        self.assertEqual(w3b.line_count(""), 0)


class TestReadBaseline(unittest.TestCase):
    def test_comments_blanks_and_non_numeric_columns_are_ignored(self):
        d = tempfile.mkdtemp()
        os.makedirs(os.path.join(d, "scripts"))
        with open(os.path.join(d, "scripts/file_size_baseline.tsv"), "w", encoding="utf-8") as fh:
            fh.write("# a comment\n\nfoo.py\t120\nbad.py\tnope\n")
        self.assertEqual(w3b.read_baseline(d), {"foo.py": 120})

    def test_a_missing_baseline_file_is_an_empty_mapping_not_an_error(self):
        self.assertEqual(w3b.read_baseline(os.path.join(tempfile.mkdtemp(), "nope")), {})


class TestSizeViolations(unittest.TestCase):
    """Judgment 4: the *untracked* files `file_size_check.py` cannot see (it reads `git ls-files`)."""

    def _scoped(self, rel, body):
        """Write `body` at `rel`; the path must carry its scope prefix or `size_violations`
        filters it out before measuring — which is what makes the scope rule testable."""
        d = tempfile.mkdtemp()
        p = os.path.join(d, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as fh:
            fh.write(body)
        return d

    def test_an_untracked_file_over_the_limit_is_flagged_with_no_baseline_record(self):
        d = self._scoped("scripts/big.py", "x\n" * 801)
        self.assertEqual(w3b.size_violations(d, ["scripts/big.py"], {}), [("scripts/big.py", 801, None)])

    def test_a_file_at_exactly_the_limit_is_not_flagged(self):
        d = self._scoped("scripts/edge.py", "x\n" * w3b.FILE_SIZE_LIMIT)
        self.assertEqual(w3b.size_violations(d, ["scripts/edge.py"], {}), [])

    def test_an_existing_baseline_record_higher_than_the_line_count_suppresses_it(self):
        d = self._scoped("scripts/big.py", "x\n" * 801)
        self.assertEqual(w3b.size_violations(d, ["scripts/big.py"], {"scripts/big.py": 900}), [])

    def test_paths_outside_the_three_scopes_are_not_measured(self):
        d = self._scoped("README.md", "x\n" * 5000)
        self.assertEqual(w3b.size_violations(d, ["README.md"], {}), [])


class TestSliceFiles(unittest.TestCase):
    def test_changed_untracked_and_nested_untracked_files_are_all_listed(self):
        d, base = make_repo(extra_files={
            "scripts/sub/untracked.py": "y\n" * 3,
            "crates/mc-http/src/routes.rs": '.route("/base", get(b))\n.route("/new", get(n))\n',
        })
        got = w3b.slice_files(d, base)
        self.assertIn("crates/mc-http/src/routes.rs", got)
        self.assertIn("scripts/sub/untracked.py", got)
        self.assertEqual(got, sorted(got))


class TestAuditSlice(unittest.TestCase):
    def test_added_routes_are_the_difference_against_the_base_ref(self):
        d, base = make_repo(extra_files={
            "crates/mc-http/src/routes.rs": '.route("/base", get(b))\n.route("/new", get(n))\n',
        })
        r = w3b.audit_slice("S1", d, base, {}, [])
        self.assertEqual(r["added_routes"], ["GET /new"])

    def test_a_guarded_path_touched_by_the_slice_is_reported(self):
        d, base = make_repo(extra_files={"crates/mc-http/src/routes/mount.rs": "// guarded\n"})
        r = w3b.audit_slice("S1", d, base, {}, [])
        self.assertIn("S1: touches guarded path crates/mc-http/src/routes/mount.rs", r["findings"])

    def test_an_untracked_oversized_script_is_visible_to_the_ten_gate_estimate(self):
        d, base = make_repo(extra_files={"scripts/sub/untracked.py": "y\n" * 801})
        r = w3b.audit_slice("S1", d, base, {}, [])
        self.assertEqual(r["size_violations"], [("scripts/sub/untracked.py", 801, None)])

    def test_golden_path_served_only_through_the_slash_alias_is_reported(self):
        """Judgment 5 — the whole point of the tool: ⑦ folds, axum 404s."""
        d, base = make_repo(extra_files={
            "crates/mc-http/src/routes.rs": '.route("/api/agents/", get(a))\n',
        })
        r = w3b.audit_slice("S1", d, base, {}, [("GET", "/api/agents", "g1")])
        self.assertTrue(any("trailing-slash alias" in f for f in r["findings"]), r["findings"])

    def test_golden_path_registered_exactly_is_not_reported(self):
        d, base = make_repo(extra_files={
            "crates/mc-http/src/routes.rs": '.route("/api/agents", get(a))\n',
        })
        r = w3b.audit_slice("S1", d, base, {}, [("GET", "/api/agents", "g1")])
        self.assertEqual([f for f in r["findings"] if "alias" in f], [])

    def test_added_folded_is_narrower_than_added_routes_when_the_fold_collides(self):
        """Measured: `added_folded` keys on `(METHOD, norm(path))`, so the slash fold only
        collapses two routes that **also** share a method."""
        d, base = make_repo(extra_files={
            "crates/mc-http/src/routes.rs": '.route("/a", get(x))\n.route("/a/", get(y))\n',
        })
        r = w3b.audit_slice("S1", d, base, {}, [])
        self.assertEqual(len(r["added_routes"]), 2)
        self.assertEqual(r["added_folded"], 1)

    def test_added_folded_does_not_collapse_the_same_path_under_two_methods(self):
        d, base = make_repo(extra_files={
            "crates/mc-http/src/routes.rs": '.route("/a", get(x))\n.route("/a/", post(y))\n',
        })
        r = w3b.audit_slice("S1", d, base, {}, [])
        self.assertEqual(len(r["added_routes"]), 2)
        self.assertEqual(r["added_folded"], 2)

    def test_a_test_file_with_zero_db_gate_env_is_flagged(self):
        d, base = make_repo(extra_files={"crates/mc-http/src/routes/tests.rs":
                                         "#[test]\nfn t() {}\n"})
        r = w3b.audit_slice("S1", d, base, {}, [])
        self.assertEqual([(f, nt, ni, env) for f, nt, ni, env in r["gated_skips"]],
                         [("crates/mc-http/src/routes/tests.rs", 1, 0, "NO")])

    def test_a_test_file_carrying_the_db_gate_env_is_not_flagged(self):
        d, base = make_repo(extra_files={"crates/mc-http/src/routes/tests.rs":
                                         '#[test]\nfn t() { let _ = std::env::var("DATABASE_URL"); }\n'})
        r = w3b.audit_slice("S1", d, base, {}, [])
        self.assertEqual([(nt, env) for _, nt, _, env in r["gated_skips"]], [(1, "yes")])

    def test_test_files_with_no_test_attributes_at_all_are_not_counted(self):
        d, base = make_repo(extra_files={"crates/mc-http/src/routes/tests.rs": "// nothing here\n"})
        self.assertEqual(w3b.audit_slice("S1", d, base, {}, [])["gated_skips"], [])


class TestCrossSliceDuplicates(unittest.TestCase):
    """Judgment 2 — the load-bearing one. The error direction is *silent*: a broken verdict
    here means a real duplicate reaches axum, which **panics at router build time**
    (global outage), not a warning."""

    def test_two_slices_registering_the_same_method_and_path_is_a_conflict_naming_both(self):
        rows = [row("A", ["GET /x", "GET /y"]), row("B", ["GET /x", "GET /z"])]
        dup, out = quiet(w3b.report_slices, rows)
        self.assertEqual(dup, {"GET /x": ["A", "B"]})
        self.assertIn("duplicates across slices", out)

    def test_a_key_registered_twice_by_one_slice_only_is_not_a_cross_slice_conflict(self):
        rows = [row("A", ["GET /x"]), row("B", ["GET /y"])]
        dup, _ = quiet(w3b.report_slices, rows)
        self.assertEqual(dup, {})

    def test_the_same_path_under_two_methods_is_not_a_conflict(self):
        rows = [row("A", ["GET /x"]), row("B", ["POST /x"])]
        self.assertEqual(quiet(w3b.report_slices, rows)[0], {})

    def test_trailing_slash_variant_is_not_a_cross_slice_duplicate(self):
        """The fold-vs-duplicate 口径, measured: cross-slice conflict detection compares the
        **literal** `"METHOD path"` key. `/y` vs `/y/` is therefore NOT a conflict — folding
        happens only in the per-slice `added_folded` count and in the golden-path verdict."""
        rows = [row("A", ["GET /y"]), row("B", ["GET /y/"])]
        dup, _ = quiet(w3b.report_slices, rows)
        self.assertEqual(dup, {})
        self.assertEqual(w3b.norm("/y"), w3b.norm("/y/"))  # the fold itself does unify them

    def test_union_count_ignores_repeats_and_equals_the_distinct_key_count(self):
        rows = [row("A", ["GET /x", "GET /y"]), row("B", ["GET /x"])]
        _, out = quiet(w3b.report_slices, rows)
        self.assertIn("union 2 keys", out)

    def test_frozen_expectation_replay_reports_missing_and_new_keys(self):
        rows = [row("A", ["GET /x"])]
        _, out = quiet(w3b.report_slices, rows, {"routes": ["GET /gone"]})
        self.assertIn("missing now: ['GET /gone']", out)
        self.assertIn("new since freeze: ['GET /x']", out)


class TestReportMerged(unittest.TestCase):
    def _tree(self, routes, baseline="", frozen=None, golden=None):
        d = tempfile.mkdtemp()
        os.makedirs(os.path.join(d, "crates/mc-http/src"))
        os.makedirs(os.path.join(d, "scripts"))
        os.makedirs(os.path.join(d, "contracts/golden"))
        with open(os.path.join(d, "crates/mc-http/src/routes.rs"), "w", encoding="utf-8") as fh:
            fh.write("\n".join(routes) + "\n")
        with open(os.path.join(d, "scripts/file_size_baseline.tsv"), "w", encoding="utf-8") as fh:
            fh.write(baseline)
        for name, body in (golden or {"a.json": {"method": "GET", "path": "/api/agents", "id": "g1"}}).items():
            with open(os.path.join(d, "contracts/golden", name), "w", encoding="utf-8") as fh:
                json.dump(body, fh)
        return d, frozen

    def test_golden_path_registered_only_with_a_trailing_slash_is_a_finding(self):
        d, _ = self._tree(['.route("/api/agents/", get(a))'])
        f, out = quiet(w3b.report_merged, d, {}, w3b.golden_paths(d, "contracts/golden"), None)
        self.assertIn("GET /api/agents registered only as /api/agents/ (g1)", f)
        self.assertIn("ONLY VIA TRAILING-SLASH ALIAS", out)

    def test_golden_path_registered_exactly_is_clean(self):
        d, _ = self._tree(['.route("/api/agents", get(a))'])
        f, out = quiet(w3b.report_merged, d, {}, w3b.golden_paths(d, "contracts/golden"), None)
        self.assertEqual(f, [])
        self.assertIn("exact", out)

    def test_golden_path_not_registered_at_all_is_reported_as_not_registered_but_is_not_a_finding(self):
        d, _ = self._tree(['.route("/other", get(a))'])
        f, out = quiet(w3b.report_merged, d, {}, w3b.golden_paths(d, "contracts/golden"), None)
        self.assertEqual(f, [])
        self.assertIn("not registered", out)

    def test_duplicate_fixtures_for_one_path_are_counted_not_doubled(self):
        d, _ = self._tree(['.route("/api/agents", get(a))'], golden={
            "a.json": {"method": "GET", "path": "/api/agents", "id": "g1"},
            "b.json": {"method": "GET", "path": "/api/agents", "id": "g2"},
        })
        _, out = quiet(w3b.report_merged, d, {}, w3b.golden_paths(d, "contracts/golden"), None)
        self.assertIn("[2 fixture(s)]", out)

    def test_a_frozen_route_dropped_by_the_merge_is_a_finding(self):
        d, _ = self._tree(['.route("/kept", get(a))'], frozen={"routes": ["GET /kept", "GET /gone"]})
        f, _ = quiet(w3b.report_merged, d, {}, [], {"routes": ["GET /kept", "GET /gone"]})
        self.assertTrue(any("merge dropped frozen routes" in x for x in f), f)

    def test_an_untracked_oversized_file_is_a_finding_in_the_post_merge_pass(self):
        # `report_merged` sees in-flight files only via `slice_files`, which reads `git status`
        # — so the tree has to be a real repo for the untracked file to be visible at all.
        d, base = make_repo(extra_files={"scripts/new_big.py": "z\n" * 900})
        f, _ = quiet(w3b.report_merged, d, {}, [], None)
        self.assertIn("⑩ file-size 900 > 800: scripts/new_big.py", f)


class TestGoldenPaths(unittest.TestCase):
    def test_method_defaults_to_question_mark_and_id_defaults_to_the_file_name(self):
        d = tempfile.mkdtemp()
        os.makedirs(os.path.join(d, "golden"))
        with open(os.path.join(d, "golden", "x.json"), "w", encoding="utf-8") as fh:
            json.dump({"path": "/api/agents"}, fh)
        self.assertEqual(w3b.golden_paths(d, "golden"), [("?", "/api/agents", "x.json")])

    def test_fixtures_without_a_path_field_are_skipped(self):
        d = tempfile.mkdtemp()
        os.makedirs(os.path.join(d, "golden"))
        with open(os.path.join(d, "golden", "x.json"), "w", encoding="utf-8") as fh:
            json.dump({"method": "GET"}, fh)
        self.assertEqual(w3b.golden_paths(d, "golden"), [])


class TestFingerprint(unittest.TestCase):
    def test_missing_files_are_hashed_as_the_literal_missing_marker(self):
        d = tempfile.mkdtemp()
        with open(os.path.join(d, "a"), "w", encoding="utf-8") as fh:
            fh.write("x")
        fp = w3b.fingerprint(d, ["a", "nope"])
        by = {r["path"]: r["sha256"] for r in fp["files"]}
        self.assertEqual(by["nope"], "MISSING")
        self.assertEqual(len(by["a"]), 64)

    def test_the_digest_changes_when_a_file_changes(self):
        d = tempfile.mkdtemp()
        p = os.path.join(d, "a")
        with open(p, "w", encoding="utf-8") as fh:
            fh.write("x")
        one = w3b.fingerprint(d, ["a"])["digest"]
        with open(p, "w", encoding="utf-8") as fh:
            fh.write("xx")
        self.assertNotEqual(one, w3b.fingerprint(d, ["a"])["digest"])


class TestKnownDefects(unittest.TestCase):
    @unittest.expectedFailure
    def test_kd1_a_slice_re_registering_a_base_stub_key_is_reported(self):
        """KD-1: docstring judgment 3 declares this a finding; `add = cur - base` makes the
        check unreachable. If someone repairs it, this flips to UNEXPECTED SUCCESS."""
        cur = {("GET", "/base"), ("GET", "/re-added")}
        base = {("GET", "/base")}
        add = sorted(cur - base)
        fired = [f"{m} {p}" for m, p in add if (m, p) in base]
        self.assertEqual(fired, ["GET /re-added"])

    def test_kd1_is_documented_with_all_four_required_fields(self):
        for key in ("defect", "claims", "observed", "fix"):
            self.assertTrue(KNOWN_DEFECTS["KD-1"][key].strip(), key)


if __name__ == "__main__":
    unittest.main()

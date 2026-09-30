#!/usr/bin/env python3
"""`scripts/route_parity.py` 的判定器用例（LUM-2606 / T1-6-K，`docs/37 §278`）。

门 ⑦ `route-parity`（`scripts/gates.sh:641`）每一轮都在报八个数字：

    upstream 456 (commit f41fae6b08fb) | local 546 registered | baseline 546
      implemented 455 real + 1 placeholder = 456 / 456
      known_gap 0   unclaimed 0   regression 0   local_only 8

这四个数不是描述性的，是**归因**：`unclaimed` 决定「还有多少缺口没人认领」、
`owners[*]` 决定「哪一队接哪一片」、`local_only` 决定「哪些本仓路由不能按上游口径判绿」。
而在本片之前，算出这八个数的 779 行**没有任何用例**（`scripts/tests.manifest` 里没有它）。

本仓已经为此付过两次钱，两次都不是「门红了」，而是**门绿着给出了错的归因**：
`SEED_404` 族的 13 条鉴权面缺口被当装置面派工（`§270`），
门 ⑫ 的发现规则只覆盖 `scripts/` 顶层（`§274`）。共同形态：判定器有语义、有分支、
有归因，但**没有任何东西在它说错时告诉它**。

本文件只**测**它 —— 不改判据（`§四`）。本轮实测到的两处缺陷把读数钉在 `docs/37 §278`，
都在本片写集之外，用例按**当时的真实行为**断言并在 docstring 里点名。

Run: ``python3 scripts/test_route_parity.py``（零 Rust / 零 cargo / 零真库 / 零磁盘）
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

import route_parity as rp  # noqa: E402

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REAL_ROUTES_DIR = os.path.join(REPO_ROOT, rp.DEFAULT_ROUTES_DIR)
REAL_UPSTREAM = os.path.join(REPO_ROOT, rp.DEFAULT_UPSTREAM)
REAL_BASELINE = os.path.join(REPO_ROOT, rp.DEFAULT_BASELINE)

# 本地树用 `rel_to=<tmp>` 提取 ⇒ 记录下来的文件名是 `src/xxx.rs`（`rel_or_abs` 的读数）。
LOCAL_MAIN = """\
use axum::Router;

pub fn routes() -> Router {
    Router::new()
        .route("/api/impl", get(show))
        .route("/api/anything", any(echo))
        .route("/api/mine", get(mine))
        .route("/api/impl", post(create))
        .route("/api/stub", get(crate::issues::not_implemented))
}
"""

MAIN_ROWS = [
    ("GET", "/api/impl", "M1", "# server/cmd/server/router.go:100"),
    ("GET", "/api/gap", "M6", "# server/cmd/server/router.go:101"),
    ("GET", "/api/nobody", "-", "# server/cmd/server/router.go:102"),
    ("POST", "/api/nobody2", "tbd", "# server/cmd/server/router.go:103"),
    ("GET", "/api/anything", "M6", "# server/cmd/server/router.go:104"),
]

OWNED_ROWS = [
    ("GET", "/api/impl", "M1", "# server/cmd/server/router.go:100"),
    ("GET", "/api/gap", "M6", "# server/cmd/server/router.go:101"),
    ("GET", "/api/anything", "M6", "# server/cmd/server/router.go:104"),
]


def _write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(text)


def _rs_tree(tmp, files):
    root = os.path.join(tmp, "src")
    for rel, body in files.items():
        _write(os.path.join(root, rel), body)
    return root


def _tree(tmp, files, rows=(), commit="f41fae6b08fb1234", baseline_keys=None):
    """Synthetic routes tree + upstream fixture (+ baseline) under `tmp`."""
    root = _rs_tree(tmp, files)
    upstream = os.path.join(tmp, "upstream-routes.tsv")
    lines = ([f"# upstream commit: {commit}", ""] if commit else [""])
    lines += ["\t".join(row) for row in rows]
    _write(upstream, "\n".join(lines) + "\n")
    baseline = None
    if baseline_keys is not None:
        baseline = os.path.join(tmp, "baseline.json")
        _write(baseline, json.dumps({"routes": sorted(baseline_keys)}))
    return root, upstream, baseline


def _extract(src, rel="a.rs"):
    """`extract_local` over a one-file synthetic tree (the temp dir dies on return)."""
    with tempfile.TemporaryDirectory() as tmp:
        return rp.extract_local(_rs_tree(tmp, {rel: src}), tmp)


def _main_tree(tmp, rows=MAIN_ROWS):
    """The canonical tree: a local-only route, a placeholder, ANY coverage, gaps."""
    root, upstream, _ = _tree(tmp, {"routes.rs": LOCAL_MAIN}, rows)
    baseline = os.path.join(tmp, "baseline.json")
    _write(baseline, json.dumps({"routes": rp.registration_keys(rp.extract_local(root, tmp).routes)}))
    return root, upstream, baseline


def _run_main(argv):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        rc = rp.main(argv)
    return rc, out.getvalue(), err.getvalue()


# --------------------------------------------------------------------------- #
# 1. mask_rust / _unescape — the layer every other reading sits on
# --------------------------------------------------------------------------- #


class TestMaskRust(unittest.TestCase):
    def test_offsets_are_stable_and_literal_bodies_are_blanked(self):
        src = 'let s = "abc";'
        masked, literals = rp.mask_rust(src)
        at = src.index('"')
        self.assertEqual(len(masked), len(src))
        self.assertEqual(literals[at], "abc")
        self.assertEqual(masked[at], '"')  # the delimiter survives
        self.assertEqual(masked[at + 1:at + 4], "   ")
        self.assertNotIn("abc", masked)

    def test_a_commented_out_route_is_not_a_registration(self):
        src = '// .route("/ghost", get(h))\nRouter::new().route("/real", get(h))\n'
        masked, literals = rp.mask_rust(src)
        self.assertEqual(masked.count(".route("), 1)
        self.assertIn("/real", literals.values())
        self.assertNotIn("ghost", "".join(literals.values()))

    def test_nested_block_comment_is_fully_consumed(self):
        src = '/* a /* b */ .route("/ghost", get(h)) */ .route("/real", get(h))\n'
        masked, _ = rp.mask_rust(src)
        self.assertEqual(masked.count(".route("), 1)

    def test_unterminated_block_comment_blanks_to_eof(self):
        masked, _ = rp.mask_rust('/* .route("/ghost", get(h))\n')
        self.assertEqual(masked.count(".route("), 0)

    def test_raw_string_without_hashes_keeps_backslashes(self):
        src = r'let p = r"/a\b";'
        masked, literals = rp.mask_rust(src)
        self.assertEqual(literals[src.index('"')], r"/a\b")
        self.assertIn(r'r"', masked)

    def test_raw_string_with_hashes_records_its_literal(self):
        src = 'let p = r#"/a "b"#;'
        masked, literals = rp.mask_rust(src)
        at = src.index('"')
        self.assertEqual(literals[at], '/a "b')
        self.assertEqual(masked[at], '"')
        self.assertTrue(masked.endswith('"#;'))  # the closing delimiter survives too
        self.assertNotIn('/a "b', masked)

    def test_raw_identifier_prefix_is_not_a_raw_string(self):
        src = "let t: r#type = r#type::default();"
        masked, literals = rp.mask_rust(src)
        self.assertEqual(literals, {})
        self.assertEqual(masked, src)

    def test_char_literal_is_blanked_but_a_lifetime_survives(self):
        src = "fn f<'a>(x: &'a str) -> char { 'x' }\n"
        masked, _ = rp.mask_rust(src)
        self.assertEqual(len(masked), len(src))
        self.assertNotIn("'x'", masked)
        self.assertIn("<'a>", masked)
        self.assertIn("&'a str", masked)

    def test_an_escaped_quote_does_not_end_the_string(self):
        src = r'let s = "a\" .route(\"/ghost\", get(h))";'
        masked, literals = rp.mask_rust(src)
        self.assertEqual(masked.count(".route("), 0)
        self.assertEqual(literals[src.index('"')], 'a" .route("/ghost", get(h))')

    def test_unescape_simple_hex_unicode_and_unknown(self):
        self.assertEqual(rp._unescape(r"a\nb\tc\rd\0e"), "a\nb\tc\rd\0e")
        self.assertEqual(rp._unescape(r"\x41"), "A")
        self.assertEqual(rp._unescape(r"\u{1F600}"), "\U0001F600")
        self.assertEqual(rp._unescape(r"\q"), r"\q")
        self.assertEqual(rp._unescape(r"\\"), "\\")
        self.assertEqual(rp._unescape("plain"), "plain")
        self.assertEqual(rp._unescape("\\"), "\\")  # dangling backslash at EOF


# --------------------------------------------------------------------------- #
# 2. paren / arg / literal helpers
# --------------------------------------------------------------------------- #


class TestScanHelpers(unittest.TestCase):
    def test_matching_paren_nested_and_eof(self):
        self.assertEqual(rp.matching_paren("(a(b)c)", 0), 7)
        self.assertEqual(rp.matching_paren("([{}])", 0), 6)
        self.assertEqual(rp.matching_paren("(abc", 0), 4)  # unterminated -> len

    def test_top_level_args_splits_and_drops_a_trailing_comma(self):
        self.assertEqual(rp.top_level_args("a, b", 0, 4), [(0, 1), (2, 4)])
        nested = "f(a, b), c"
        self.assertEqual(rp.top_level_args(nested, 0, len(nested)), [(0, 7), (8, 10)])
        trailing = ' "/a", get(h),'
        self.assertEqual(rp.top_level_args(trailing, 0, len(trailing)), [(0, 5), (6, 13)])

    def test_str_literal_at_normal_identifier_and_trailing_junk(self):
        masked, literals = rp.mask_rust('"/a"')
        self.assertEqual(rp.str_literal_at(masked, literals, 0, 4), "/a")
        masked, literals = rp.mask_rust("PREFIX")
        self.assertIsNone(rp.str_literal_at(masked, literals, 0, 6))
        masked, literals = rp.mask_rust('"/a" )')
        self.assertIsNone(rp.str_literal_at(masked, literals, 0, 6))
        masked, literals = rp.mask_rust('   "  "  ')
        self.assertEqual(rp.str_literal_at(masked, literals, 0, 9), "  ")

    def test_test_module_ranges_and_in_spans(self):
        src = ('#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n'
               '#[cfg(test)]\nuse super::*;\nfn real() {}\n')
        masked, _ = rp.mask_rust(src)
        spans = rp.test_module_ranges(masked)
        self.assertEqual(len(spans), 1)  # the `use` form has nothing to skip
        self.assertTrue(rp.in_spans(masked.index("fn t"), spans))
        self.assertFalse(rp.in_spans(masked.index("fn real"), spans))

    def test_rel_or_abs_inside_and_outside_the_root(self):
        self.assertEqual(rp.rel_or_abs("/r/a/b.rs", "/r"), "a/b.rs")
        self.assertEqual(rp.rel_or_abs("/other/b.rs", "/r"), "/other/b.rs")


# --------------------------------------------------------------------------- #
# 3. extract_local — the local half of every number on the board
# --------------------------------------------------------------------------- #


class TestExtractLocal(unittest.TestCase):
    def test_methods_any_placeholder_file_and_line(self):
        ex = _extract(LOCAL_MAIN)
        got = {(r.method, r.path): r for r in ex.routes}
        self.assertEqual(ex.files_scanned, 1)
        self.assertEqual(len(ex.routes), 5)
        self.assertIn(("GET", "/api/impl"), got)
        self.assertIn(("POST", "/api/impl"), got)
        self.assertEqual(got[("ANY", "/api/anything")].method, "ANY")
        self.assertTrue(got[("GET", "/api/stub")].placeholder)
        self.assertFalse(got[("GET", "/api/impl")].placeholder)
        self.assertEqual(got[("GET", "/api/stub")].file, "src/a.rs")
        self.assertEqual(got[("GET", "/api/stub")].line, 9)
        self.assertEqual(ex.unsupported, [])
        self.assertEqual(ex.limitations, [])

    def test_placeholder_criterion_is_word_bounded_not_a_substring(self):
        ex = _extract('Router::new().route("/a", get(handle_not_implemented_x))\n')
        self.assertFalse(ex.routes[0].placeholder)
        ex = _extract('Router::new().route("/a", get(health::placeholder))\n')
        self.assertTrue(ex.routes[0].placeholder)

    def test_cfg_test_routes_are_not_served_routes(self):
        src = ('Router::new().route("/real", get(h));\n'
               '#[cfg(test)]\nmod tests {\n'
               '    Router::new().route("/ghost", get(h));\n}\n')
        self.assertEqual([r.path for r in _extract(src).routes], ["/real"])

    def test_a_non_literal_path_is_unsupported_not_silently_skipped(self):
        ex = _extract('Router::new().route(PREFIX, get(h))\n')
        self.assertEqual(ex.routes, [])
        self.assertEqual(len(ex.unsupported), 1)
        self.assertIn("non-literal path", ex.unsupported[0])
        self.assertIn("PREFIX", ex.unsupported[0])
        ex = _extract('Router::new().route(concat!("/api/", "x"), get(h))\n')
        self.assertTrue(ex.unsupported[0].startswith("src/a.rs:1: non-literal path `concat!("))

    def test_an_arg_count_other_than_two_is_unsupported(self):
        self.assertIn("with 1 args", _extract('Router::new().route("/a")\n').unsupported[0])
        self.assertIn(
            "with 3 args", _extract('Router::new().route("/a", get(h), extra)\n').unsupported[0]
        )

    def test_a_missing_method_router_is_unsupported(self):
        ex = _extract('Router::new().route("/a", my_router())\n')
        self.assertEqual(ex.routes, [])
        self.assertIn("no method-router call in", ex.unsupported[0])
        self.assertIn('/a"', ex.unsupported[0])

    def test_the_nest_family_is_reported_as_a_limitation(self):
        src = ('Router::new()\n    .nest("/v1", sub())\n    .nest_service("/static", svc)\n'
               '    .route_service("/x", s)\n    .fallback_service(f)\n')
        ex = _extract(src)
        self.assertEqual(ex.routes, [])
        self.assertEqual(len(ex.limitations), 4)
        for api in rp.OTHER_ROUTE_APIS:
            self.assertIn(f"1x `{api}`", "\n".join(ex.limitations))
        self.assertEqual(_extract('Router::new().route("/a", get(h))\n').limitations, [])

    def test_route_service_is_not_misread_as_a_route_registration(self):
        ex = _extract('Router::new().route_service("/x", s)\n')
        self.assertEqual((ex.routes, ex.unsupported), ([], []))

    def test_comments_and_string_bodies_do_not_register_routes(self):
        src = ('// .route("/ghost1", get(h))\n'
               'let help = ".route(\\"/ghost2\\", get(h))";\n'
               'Router::new().route("/real", get(h))\n')
        self.assertEqual([r.path for r in _extract(src).routes], ["/real"])

    def test_a_raw_string_path_without_hashes_resolves(self):
        ex = _extract('Router::new().route(r"/raw_no_hash", get(h))\n')
        self.assertEqual([r.path for r in ex.routes], ["/raw_no_hash"])

    def test_a_raw_string_path_with_hashes_is_a_registered_defect(self):
        """🔴 `§278` 缺陷一：`str_literal_at` 认领支持 raw string，实际只支持无 `#` 的那种。

        `mask_rust` 保留了 raw string 的**结束定界符**（`"#` / `"###`），而
        `str_literal_at` 判「引号之后是否还有内容」时把那些 `#` 也算进去了 ⇒
        `r#"…"#` 返回 `None` ⇒ 路由被判成 `non-literal path`，而那句判词本身就是错的：
        它**是**一个字面量。修好后本用例必须改成断言该路由被提取，并同步 `§278`。
        """
        ex = _extract('Router::new().route(r#"/raw_hash"#, get(h))\n')
        self.assertEqual(ex.routes, [])
        self.assertTrue(ex.unsupported[0].startswith('src/a.rs:1: non-literal path `r#"'), ex.unsupported)
        ex = _extract('Router::new().route(r###"/raw_three"###, get(h))\n')
        self.assertEqual(ex.routes, [])
        self.assertIn("non-literal path", ex.unsupported[0])

    def test_discovery_is_recursive_and_counts_only_rs_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = _rs_tree(tmp, {
                "a.rs": 'Router::new().route("/a", get(h));\n',
                os.path.join("nested", "b.rs"): 'Router::new().route("/b", get(h));\n',
                os.path.join("nested", "c.txt"): 'Router::new().route("/c", get(h));\n',
            })
            ex = rp.extract_local(root, tmp)
        self.assertEqual(ex.files_scanned, 2)
        self.assertEqual(sorted(r.path for r in ex.routes), ["/a", "/b"])


# --------------------------------------------------------------------------- #
# 4. normalize / keys — the comparison key is the denominator
# --------------------------------------------------------------------------- #


class TestNormalizeAndKeys(unittest.TestCase):
    def test_trailing_slash_folds_by_default_and_is_kept_on_demand(self):
        self.assertEqual(rp.normalize("/x/"), "/x")
        self.assertEqual(rp.normalize("/x/", strip_trailing_slash=False), "/x/")
        self.assertEqual(rp.normalize("/"), "/")
        self.assertEqual(rp.normalize(""), "/")
        self.assertEqual(rp.normalize("x"), "/x")

    def test_params_are_name_blind(self):
        self.assertEqual(rp.normalize("/a/:id/b/{workspaceId}"), "/a/:param/b/:param")

    def test_all_four_wildcard_spellings_fold_to_one_key(self):
        for raw in ("/uploads/*", "/uploads/*name", "/uploads/{*", "/uploads/{*name}"):
            with self.subTest(raw=raw):
                self.assertEqual(rp.normalize(raw), "/uploads/:wildcard")

    def test_the_wildcard_branch_precedes_the_param_branch(self):
        """`LUM-2113`：`{*name}` 若排在 `{…}` 之后会被 `:param` 先吃掉（死代码）。"""
        self.assertEqual(rp.normalize("/a/{*name}"), "/a/:wildcard")
        self.assertEqual(rp.normalize("/a/{id}"), "/a/:param")

    def test_registration_keys_duplicates_and_slash_aliases(self):
        routes = [rp.LocalRoute("GET", "/x", "a.rs", 1), rp.LocalRoute("GET", "/x/", "a.rs", 2)]
        self.assertEqual(rp.registration_keys(routes), ["GET /x", "GET /x/"])
        keys = [("GET", "/a"), ("POST", "/b"), ("GET", "/a")]
        self.assertEqual(rp.duplicates(keys), {("GET", "/a"): [0, 2]})
        self.assertEqual(rp.duplicates([("GET", "/a")]), {})
        self.assertEqual(rp.slash_aliases([("GET", "/a"), ("GET", "/a/")]), ["GET /a + /a/"])
        self.assertEqual(rp.slash_aliases([("GET", "/a")]), [])


# --------------------------------------------------------------------------- #
# 5. upstream fixture + baseline
# --------------------------------------------------------------------------- #


class TestFixtureAndBaseline(unittest.TestCase):
    def test_read_fixture_parses_commit_owner_and_router_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "u.tsv")
            _write(path, "# upstream commit: deadbeefcafe\n\n"
                         "GET\t/api/a\tM1\t# server/cmd/server/router.go:42\n"
                         "post\t/api/b/\t\t# no router line here\n"
                         "GET\t/api/c\n")
            routes, commit = rp.read_fixture(path)
        self.assertEqual(commit, "deadbeefcafe")
        self.assertEqual(len(routes), 3)
        self.assertEqual((routes[0].method, routes[0].path, routes[0].owner), ("GET", "/api/a", "M1"))
        self.assertEqual(routes[0].router_line, 42)
        self.assertEqual(routes[0].fixture_line, 3)
        self.assertEqual((routes[1].method, routes[1].path, routes[1].owner), ("POST", "/api/b/", ""))
        self.assertEqual(routes[1].router_line, 0)
        self.assertEqual((routes[2].path, routes[2].owner), ("/api/c", ""))

    def test_read_fixture_rejects_a_short_row(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "u.tsv")
            _write(path, "GET-only-one-column\n")
            with self.assertRaises(ValueError) as ctx:
                rp.read_fixture(path)
        self.assertIn("expected `method<TAB>path<TAB>owner`", str(ctx.exception))

    def test_baseline_round_trip_shape(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "b.json")
            rp.write_baseline(path, ["GET /a"], os.path.join(tmp, "src"), tmp)
            with open(path, encoding="utf-8") as fh:
                doc = json.load(fh)
            self.assertEqual(list(doc), ["#", "why", "routes_dir", "routes"])
            self.assertEqual(doc["routes"], ["GET /a"])
            self.assertEqual(doc["routes_dir"], "src")
            self.assertEqual(rp.read_baseline(path), ["GET /a"])

    def test_read_baseline_rejects_a_non_list(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "b.json")
            for bad in ('{"routes": "GET /a"}', '{"routes": [1, 2]}'):
                with self.subTest(bad=bad):
                    _write(path, bad)
                    with self.assertRaises(ValueError):
                        rp.read_baseline(path)


# --------------------------------------------------------------------------- #
# 6. build_report — the eight numbers, i.e. the attribution
# --------------------------------------------------------------------------- #


class TestBuildReportAttribution(unittest.TestCase):
    def test_implemented_known_gap_unclaimed_local_only_and_owners(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, baseline = _main_tree(tmp)
            rep = rp.build_report(root, upstream, tmp, baseline)
        c = rep["counts"]
        self.assertEqual((c["upstream"], c["local"]), (5, 5))
        self.assertEqual(c["implemented"], 2)  # GET /api/impl + ANY covering GET /api/anything
        self.assertEqual(c["known_gap"], 1)  # GET /api/gap (owner M6)
        self.assertEqual(c["unclaimed"], 2)  # owner `-` and owner `tbd`
        self.assertEqual(c["local_only"], 4)
        self.assertEqual(c["local_only_placeholder"], 1)
        self.assertEqual((c["implemented_placeholder"], c["implemented_real"]), (0, 2))
        self.assertEqual(c["regressions"], 0)
        # `owners` is keyed by the *raw* owner string (only the empty one is renamed);
        # ties break by name, not by insertion order.
        self.assertEqual(rep["owners"], {"-": 1, "M6": 1, "tbd": 1})
        self.assertFalse(rep["ok"])  # an unclaimed gap is a red board
        self.assertEqual([r["path"] for r in rep["known_gap"]], ["/api/gap"])
        self.assertEqual(sorted(r["path"] for r in rep["unclaimed"]), ["/api/nobody", "/api/nobody2"])

    def test_unclaimed_owner_spellings_are_case_insensitive(self):
        rows = [(("GET"), f"/api/{i}", owner, "# server/cmd/server/router.go:1")
                for i, owner in enumerate(["", "-", "?", "TBD", "M1"])]
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": "fn f() {}\n"}, rows)
            rep = rp.build_report(root, upstream, tmp, None)
        self.assertEqual(rep["counts"]["unclaimed"], 4)
        self.assertEqual(rep["counts"]["known_gap"], 1)

    def test_an_any_route_covers_every_method_on_its_path(self):
        rows = [("GET", "/api/x", "M1", "# server/cmd/server/router.go:1"),
                ("DELETE", "/api/x", "M1", "# server/cmd/server/router.go:2")]
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": 'Router::new().route("/api/x", any(e))\n'}, rows)
            rep = rp.build_report(root, upstream, tmp, None)
        self.assertEqual(rep["counts"]["implemented"], 2)
        # `any(...)` has no upstream counterpart METHOD, so the registration itself
        # is still reported as local-only — that is the current criterion, not a bug.
        self.assertEqual(rep["counts"]["local_only"], 1)

    def test_a_fully_owned_unc_regressed_tree_is_ok(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, baseline = _main_tree(tmp, OWNED_ROWS)
            rep = rp.build_report(root, upstream, tmp, baseline)
        self.assertTrue(rep["ok"])
        self.assertEqual(rep["counts"]["known_gap"], 1)
        self.assertEqual(rep["counts"]["unclaimed"], 0)
        self.assertEqual(rep["counts"]["regressions"], 0)

    def test_a_route_missing_from_the_tree_is_a_regression(self):
        # `OWNED_ROWS` on purpose: the only reason this board may be red is the lost
        # route.  With a tree that also has unclaimed gaps, `ok` would stay False even
        # if the `regressions` term were dropped from it — probe P4 (`§278`) measured
        # exactly that hole, so this scenario has to isolate the term.
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, baseline = _main_tree(tmp, OWNED_ROWS)
            with open(baseline, encoding="utf-8") as fh:
                doc = json.load(fh)
            doc["routes"].append("DELETE /api/gone")
            _write(baseline, json.dumps(doc))
            rep = rp.build_report(root, upstream, tmp, baseline)
        self.assertEqual(rep["regressions"], [{"method": "DELETE", "path": "/api/gone"}])
        self.assertEqual(rep["sources"]["baseline_routes"], 6)
        self.assertFalse(rep["ok"])

    def test_no_baseline_means_no_regression_gate_and_no_note(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _main_tree(tmp)
            rep = rp.build_report(root, upstream, tmp, None)
        self.assertEqual(rep["sources"]["baseline"], "(none)")
        self.assertEqual(rep["sources"]["baseline_routes"], 0)
        self.assertEqual(rep["sources"]["baseline_note"], "")

    def test_a_missing_baseline_raises_instead_of_soft_disabling(self):
        """🔴 `§278` 缺陷二：`baseline_note`（「no baseline … drift gate off」）不可达。

        `build_report` 先无条件 `read_baseline(baseline_path)`，**下一行**才检查
        `not os.path.exists(baseline_path)` ⇒ 缺文件时先抛 `FileNotFoundError`，
        那句降级说明永远打不出来；`main()` 把它兜成 `error: …` + `rc=2`。
        """
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _main_tree(tmp)
            with self.assertRaises(FileNotFoundError):
                rp.build_report(root, upstream, tmp, os.path.join(tmp, "missing.json"))

    def test_an_unsupported_registration_makes_the_board_red(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": "Router::new().route(PREFIX, get(h))\n"}, OWNED_ROWS)
            rep = rp.build_report(root, upstream, tmp, None)
        self.assertEqual(rep["counts"]["local"], 0)
        self.assertTrue(rep["unsupported"])
        self.assertFalse(rep["ok"])

    def test_a_duplicate_registration_makes_the_board_red(self):
        src = 'Router::new().route("/api/x", get(a)).route("/api/x", get(b))\n'
        rows = [("GET", "/api/x", "M1", "# server/cmd/server/router.go:1")]
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": src}, rows)
            rep = rp.build_report(root, upstream, tmp, None)
        self.assertEqual(rep["duplicates"]["local"], ["GET /api/x"])
        self.assertFalse(rep["ok"])

    def test_a_duplicate_upstream_row_makes_the_board_red(self):
        rows = [("GET", "/api/x", "M1", "# server/cmd/server/router.go:1"),
                ("GET", "/api/x", "M2", "# server/cmd/server/router.go:2")]
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": 'Router::new().route("/api/x", get(a))\n'}, rows)
            rep = rp.build_report(root, upstream, tmp, None)
        self.assertEqual(rep["duplicates"]["upstream"], ["GET /api/x"])
        self.assertFalse(rep["ok"])

    def test_a_placeholder_is_only_split_for_implemented_routes(self):
        src = ('Router::new().route("/api/impl", get(issues::not_implemented))\n'
               'Router::new().route("/api/local", get(health::placeholder))\n')
        rows = [("GET", "/api/impl", "M1", "# server/cmd/server/router.go:1")]
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": src}, rows)
            rep = rp.build_report(root, upstream, tmp, None)
        c = rep["counts"]
        self.assertEqual((c["implemented"], c["implemented_real"], c["implemented_placeholder"]), (1, 0, 1))
        self.assertEqual((c["local_only"], c["local_only_placeholder"]), (1, 1))
        self.assertTrue(rep["implemented"][0]["placeholder"])
        self.assertTrue(rep["local_only"][0]["placeholder"])

    def test_local_only_records_the_file_and_line_of_the_registration(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _main_tree(tmp)
            rep = rp.build_report(root, upstream, tmp, None)
        mine = [r for r in rep["local_only"] if r["path"] == "/api/mine"][0]
        self.assertEqual((mine["file"], mine["line"]), ("src/routes.rs", 7))
        self.assertFalse(mine["placeholder"])

    def test_a_slash_alias_pair_is_reported_but_is_not_a_duplicate(self):
        src = 'Router::new().route("/api/x", get(a)).route("/api/x/", get(b))\n'
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": src}, OWNED_ROWS)
            rep = rp.build_report(root, upstream, tmp, None)
        self.assertEqual(rep["duplicates"]["local"], [])
        self.assertEqual(rep["slash_aliases"], ["GET /api/x + /api/x/"])

    def test_sources_record_the_scanned_shape(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, baseline = _main_tree(tmp)
            rep = rp.build_report(root, upstream, tmp, baseline)
        self.assertEqual(rep["sources"]["routes_dir"], "src")
        self.assertEqual(rep["sources"]["upstream_commit"], "f41fae6b08fb1234")
        self.assertEqual(rep["sources"]["files_scanned"], 1)
        self.assertEqual(rep["sources"]["baseline"], "baseline.json")


# --------------------------------------------------------------------------- #
# 7. render_human — what a cycle actually reads
# --------------------------------------------------------------------------- #


class TestRenderHuman(unittest.TestCase):
    def _rep(self, tmp):
        root, upstream, baseline = _main_tree(tmp)
        return rp.build_report(root, upstream, tmp, baseline)

    def test_header_counts_owner_line_and_fail_verdict(self):
        with tempfile.TemporaryDirectory() as tmp:
            text = rp.render_human(self._rep(tmp), False, False)
        lines = text.splitlines()
        self.assertIn("upstream 5 (commit f41fae6b08fb) | local 5 registered | baseline 5", lines[0])
        self.assertIn("implemented    2 real +   0 placeholder =    2 / 5", lines[1])
        self.assertIn("known_gap    1", lines[1])
        self.assertIn("unclaimed    2", lines[1])
        self.assertIn("  gaps by owner: -=1  M6=1  tbd=1", text)
        self.assertIn("!! 2 UNCLAIMED upstream routes (no owner):", text)
        self.assertIn("GET    /api/nobody   # router.go:102", text)
        self.assertIn("local-only (not upstream; keep or retire deliberately):", text)
        self.assertIn("[placeholder]", text)
        self.assertTrue(text.endswith("FAIL"))

    def test_quiet_suppresses_owners_and_local_only_but_never_the_attribution(self):
        with tempfile.TemporaryDirectory() as tmp:
            text = rp.render_human(self._rep(tmp), False, True)
        self.assertNotIn("gaps by owner", text)
        self.assertNotIn("local-only", text)
        self.assertIn("UNCLAIMED", text)
        self.assertTrue(text.endswith("FAIL"))

    def test_list_gaps_groups_by_owner(self):
        with tempfile.TemporaryDirectory() as tmp:
            text = rp.render_human(self._rep(tmp), True, True)
        self.assertIn("known gaps (owner  method  path):", text)
        self.assertIn("    [M6] 1", text)
        self.assertIn("      GET    /api/gap", text)

    def test_regression_block_names_the_lost_contract_and_the_fix(self):
        with tempfile.TemporaryDirectory() as tmp:
            rep = self._rep(tmp)
            rep["regressions"] = [{"method": "DELETE", "path": "/api/gone"}]
            text = rp.render_human(rep, False, True)
        self.assertIn("!! 1 route(s) present in the baseline are gone from the source", text)
        self.assertIn("DELETE /api/gone", text)
        self.assertIn("--write-baseline", text)

    def test_warning_blocks_for_duplicates_unsupported_limitations_and_note(self):
        with tempfile.TemporaryDirectory() as tmp:
            rep = self._rep(tmp)
            rep["duplicates"] = {"local": ["GET /dup"], "upstream": ["GET /udup"]}
            rep["unsupported"] = ["a.rs:1: non-literal path `X`"]
            rep["limitations"] = ["1x `.nest(` — cannot resolve"]
            rep["slash_aliases"] = ["GET /a + /a/"]
            rep["sources"]["baseline_note"] = "no baseline at b.json — drift gate off"
            text = rp.render_human(rep, False, True)
        self.assertIn("!! duplicate (local) route keys: GET /dup", text)
        self.assertIn("!! duplicate (upstream) route keys: GET /udup", text)
        self.assertIn("!! unresolved registrations (fix or extend the extractor):", text)
        self.assertIn("note: same route with and without trailing slash", text)
        self.assertIn("note: routes may exist that this static scan cannot see:", text)
        self.assertIn("note: no baseline at b.json", text)

    def test_an_unknown_upstream_commit_renders_as_a_question_mark(self):
        rows = [("GET", "/api/impl", "M1", "# server/cmd/server/router.go:1")]
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": LOCAL_MAIN}, rows, commit=None)
            text = rp.render_human(rp.build_report(root, upstream, tmp, None), False, True)
        self.assertIn("upstream 1 (commit ?)", text)
        self.assertTrue(text.endswith("OK: every upstream route is either implemented or owned"))


# --------------------------------------------------------------------------- #
# 8. CLI — the exit code is the gate's judgement
# --------------------------------------------------------------------------- #


class TestMainCli(unittest.TestCase):
    def test_json_output_and_zero_on_a_green_board(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": LOCAL_MAIN}, OWNED_ROWS)
            rc, out, _ = _run_main(["--json", "--routes-dir", root, "--upstream", upstream, "--no-baseline"])
        self.assertEqual(rc, 0)
        self.assertTrue(json.loads(out)["ok"])

    def test_one_when_the_board_is_not_ok(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": LOCAL_MAIN}, MAIN_ROWS)
            rc, out, _ = _run_main(["--quiet", "--routes-dir", root, "--upstream", upstream, "--no-baseline"])
        self.assertEqual(rc, 1)
        self.assertTrue(out.rstrip().endswith("FAIL"))

    def test_two_and_an_error_line_when_an_input_cannot_be_read(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": LOCAL_MAIN}, OWNED_ROWS)
            rc, _, err = _run_main(["--routes-dir", root, "--upstream", os.path.join(tmp, "nope.tsv")])
            self.assertEqual(rc, 2)
            self.assertTrue(err.startswith("error: "), err)
            rc2, _, err2 = _run_main(["--routes-dir", root, "--upstream", upstream, "--baseline", tmp])
            self.assertEqual(rc2, 2)
            self.assertIn("error: ", err2)

    def test_write_baseline_records_the_tree_then_reports(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": LOCAL_MAIN}, OWNED_ROWS)
            base = os.path.join(tmp, "deep", "baseline.json")
            rc, _, _ = _run_main(["--quiet", "--routes-dir", root, "--upstream", upstream,
                                  "--write-baseline", "--baseline", base])
            self.assertEqual(rc, 0)
            self.assertEqual(rp.read_baseline(base), rp.registration_keys(rp.extract_local(root, tmp).routes))
            rc2, out2, _ = _run_main(["--quiet", "--routes-dir", root, "--upstream", upstream,
                                      "--baseline", base])
        self.assertEqual(rc2, 0)
        self.assertIn("baseline 5", out2)

    def test_write_baseline_refuses_while_a_registration_is_unresolved(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": "Router::new().route(PREFIX, get(h))\n"}, OWNED_ROWS)
            rc, _, err = _run_main(["--routes-dir", root, "--upstream", upstream, "--write-baseline",
                                    "--baseline", os.path.join(tmp, "b.json")])
        self.assertEqual(rc, 2)
        self.assertIn("refusing to write a baseline while registrations are unresolved", err)

    def test_write_baseline_without_a_baseline_path_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = _tree(tmp, {"routes.rs": LOCAL_MAIN}, OWNED_ROWS)
            rc, _, err = _run_main(["--routes-dir", root, "--upstream", upstream,
                                    "--write-baseline", "--no-baseline"])
        self.assertEqual(rc, 2)
        self.assertIn("--write-baseline needs a baseline path", err)


# --------------------------------------------------------------------------- #
# 9. the real tree — the reading the gate actually takes (no synthetic input)
# --------------------------------------------------------------------------- #


class TestRealTree(unittest.TestCase):
    def test_the_gate_command_is_green(self):
        out = subprocess.run([sys.executable, "scripts/route_parity.py", "--quiet"],
                             cwd=REPO_ROOT, capture_output=True, text=True)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertIn("OK: every upstream route is either implemented or owned", out.stdout)
        self.assertIn("registered", out.stdout.splitlines()[0])

    def test_the_board_is_internally_consistent(self):
        rep = rp.build_report(REAL_ROUTES_DIR, REAL_UPSTREAM, REPO_ROOT, REAL_BASELINE)
        c = rep["counts"]
        self.assertTrue(rep["ok"], rep["unsupported"][:3] or rep["unclaimed"][:3])
        self.assertEqual(c["implemented"] + c["known_gap"] + c["unclaimed"], c["upstream"])
        self.assertEqual(c["implemented_real"] + c["implemented_placeholder"], c["implemented"])
        self.assertEqual(c["regressions"], 0)
        self.assertEqual(c["unclaimed"], 0)
        self.assertEqual(rep["unsupported"], [])
        self.assertEqual(rep["duplicates"]["local"], [])
        self.assertGreater(c["upstream"], 0)
        self.assertGreater(c["local"], 0)
        self.assertTrue(rep["sources"]["upstream_commit"])

    def test_the_checked_in_baseline_covers_exactly_what_the_tree_registers(self):
        """`baseline N | local N registered` 是同一件事：基线是本仓注册集合的记忆。"""
        rep = rp.build_report(REAL_ROUTES_DIR, REAL_UPSTREAM, REPO_ROOT, REAL_BASELINE)
        self.assertEqual(rep["sources"]["baseline_routes"], rep["counts"]["local"])
        live = rp.registration_keys(rp.extract_local(REAL_ROUTES_DIR, REPO_ROOT).routes)
        self.assertEqual(rp.read_baseline(REAL_BASELINE), live)

    def test_gate_seven_is_wired_to_this_script(self):
        with open(os.path.join(REPO_ROOT, "scripts", "gates.sh"), encoding="utf-8") as fh:
            self.assertIn("scripts/route_parity.py --quiet", fh.read())


if __name__ == "__main__":
    unittest.main(verbosity=2)

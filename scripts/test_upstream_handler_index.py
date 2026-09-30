#!/usr/bin/env python3
r"""`scripts/upstream_handler_index.py` 的 `unittest`（LUM-2635 / T1-6-S3）。

这个 343 行的脚本是**门 ⑨ `report.json` 的分族归因生产者**：`by_actor`（member 286 /
daemon 20 / agent 12 / token 8 / anonymous 5）、`by_via`（handler 305 / router 26）、
`tiers` 全都由它产出的 `handler-routes.tsv` 参与决定。**「一条 fixture 属于 member 还是
daemon」正是在这里定的**，而归因错 ⇒ 分族读数错 ⇒ 派工错 ⇒ 整轮返工
（`PRECONDITION` / `SEED_404` 两族都是这么被认领的）。

落地之前它 **0 用例、0 门执行**：`scripts/tests.manifest` 里没有对应行，
`gates.sh` / `.github/workflows/` 也没有任何一处引用它（门 ⑭ 实测 `surfaces=2` 不含它）。
本文件把它接进**门 ⑫**（`tests.manifest` 的集合基线逐行比对 ⇒ 谁也删不掉它）。

**判别力的单位是分支，不是断言条数**（`docs/37 §292.2` / `§293.4` 的纪律：
「对照真实输出只能在该分支被真实输入触及时才有判别力」——某片把一条分支废掉，
整个文件仍然全绿）。所以下面每一条都**造合成 lane**（tmp 目录里手写的 `router.go` +
手写的 `upstream-routes.tsv`），把五种真实上游形状逐个走一遍：

  1. 同一路由两个 handler          2. helper 与 handler 同名
  3. 匿名中间件闭包注册            4. 生成代码里的 handler（非 `h.` 接收者）
  5. 跨包同符号名                  ＋ 包常量路径 / 多行注册 / 路径字面量对不上

真实输入面另有两条（`tests/fixtures` 已提交的 456 行表 + 真实 git 历史），
它们证明「合成 lane 的断言没有跑偏」，但**不冒充分支覆盖**。

零 Rust / 零 cargo / 零真库 / 零磁盘 / 零网络：只 `python3`。
Run: ``python3 scripts/test_upstream_handler_index.py``
"""

from __future__ import annotations

import contextlib
import io
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import upstream_handler_index as uhi  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
REAL_HANDLER_TSV = os.path.join(ROOT, "docs", "fixtures", "handler-routes.tsv")
REAL_ROUTES_TSV = os.path.join(ROOT, "docs", "fixtures", "upstream-routes.tsv")


# --------------------------------------------------------------------------- #
# 合成 lane 的合成器
# --------------------------------------------------------------------------- #
def write_upstream(root: str, body: str) -> str:
    """在 `root` 下造一个「像上游 checkout 的最小目录」，返回 root。"""
    rel = os.path.join(root, uhi.ROUTER_REL)
    os.makedirs(os.path.dirname(rel), exist_ok=True)
    with open(rel, "w", encoding="utf-8") as fh:
        fh.write(body)
    return root


def row(method: str, path: str, line: int, owner: str = "M1") -> dict:
    return {"method": method, "path": path, "owner": owner, "line": line}


def parse_committed(text: str) -> list[list[str]]:
    return [ln.split("\t") for ln in text.splitlines() if ln and not ln.startswith("#")]


def header_line(text: str, prefix: str) -> str:
    for ln in text.splitlines():
        if ln.startswith(prefix):
            return ln
    raise AssertionError("header line %r not found" % prefix)


# --------------------------------------------------------------------------- #
# 1. `route_rows` —— 三个 SystemExit 分支 + 正常解析
# --------------------------------------------------------------------------- #
class TestRouteRows(unittest.TestCase):
    def _tsv(self, lines: str) -> str:
        fd, path = tempfile.mkstemp(suffix=".tsv", text=True)
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fh.write(lines)
        self.addCleanup(os.unlink, path)
        return path

    def test_parses_rows_skips_comments_and_blanks(self):
        path = self._tsv(
            "# a comment\n"
            "\n"
            "GET\t/api/a\tM1\t# router.go:12\n"
            "POST\t/api/b\tM2-A\t# router.go:13\n"
        )
        got = uhi.route_rows(path)
        self.assertEqual(
            got,
            [
                {"method": "GET", "path": "/api/a", "owner": "M1", "line": 12},
                {"method": "POST", "path": "/api/b", "owner": "M2-A", "line": 13},
            ],
        )

    def test_missing_file_is_a_named_systemexit(self):
        # 探针：这条分支若被吞掉，缺输入会读成「0 行路由」而不是「明确报错」。
        with self.assertRaises(SystemExit) as ctx:
            uhi.route_rows(os.path.join(tempfile.gettempdir(), "no-such-routes.tsv"))
        self.assertIn("gen_upstream_routes.py", str(ctx.exception))

    def test_unparsable_row_is_rejected(self):
        path = self._tsv("GET /api/a M1\n")  # 少了 tab 分隔
        with self.assertRaises(SystemExit) as ctx:
            uhi.route_rows(path)
        self.assertIn("unparsable route row", str(ctx.exception))

    def test_table_with_only_comments_is_rejected(self):
        path = self._tsv("# nothing here\n\n")
        with self.assertRaises(SystemExit) as ctx:
            uhi.route_rows(path)
        self.assertIn("no route rows", str(ctx.exception))

    def test_committed_table_is_readable_and_non_trivial(self):
        rows = uhi.route_rows(REAL_ROUTES_TSV)
        self.assertEqual(len(rows), 456)
        self.assertTrue(all(r["line"] > 0 and r["path"].startswith("/") for r in rows))


# --------------------------------------------------------------------------- #
# 2. `_split_text_args` —— 嵌套 / 引号 / 转义 / 尾逗号
# --------------------------------------------------------------------------- #
class TestSplitTextArgs(unittest.TestCase):
    def test_plain_two_args(self):
        # 注意切分**保留分隔符两侧的空白**（调用方自己 `.strip()`）—— 钉住这一点，
        # 因为 `_second_arg` 正是靠 `args[0].strip()` 才拿得到 `re.fullmatch('"..."')`。
        self.assertEqual(uhi._split_text_args(' "/api/x", h.GetX)', 0), ([' "/api/x"', " h.GetX"], 17))

    def test_nested_call_is_one_arg(self):
        args, _ = uhi._split_text_args(' mk(h.GetX), h.Other)', 0)
        self.assertEqual(args, [" mk(h.GetX)", " h.Other"])

    def test_comma_inside_string_is_not_a_separator(self):
        args, _ = uhi._split_text_args(' "/a,b", h.X)', 0)
        self.assertEqual(args, [' "/a,b"', " h.X"])

    def test_escaped_quote_does_not_end_the_string(self):
        args, _ = uhi._split_text_args(' "/a\\"b", h.X)', 0)
        self.assertEqual(args, [' "/a\\"b"', " h.X"])

    def test_raw_string_with_quotes(self):
        args, _ = uhi._split_text_args(' r#"/a"# as p, h.X)', 0)
        self.assertEqual(args, [' r#"/a"# as p', " h.X"])

    def test_braces_and_parens_balance(self):
        args, _ = uhi._split_text_args(' mw([]int{1, 2}), h.X)', 0)
        self.assertEqual(args, [" mw([]int{1, 2})", " h.X"])

    def test_unterminated_call_returns_remainder(self):
        args, end = uhi._split_text_args(' "/a", h.X', 0)
        self.assertEqual(args, [' "/a"', " h.X"])
        self.assertEqual(end, len(' "/a", h.X'))


# --------------------------------------------------------------------------- #
# 3. `classify_registration` —— 五种注册形状的归类
# --------------------------------------------------------------------------- #
class TestClassifyRegistration(unittest.TestCase):
    def test_no_second_argument(self):
        self.assertEqual(uhi.classify_registration(None), ("none", None))

    def test_anonymous_closure_is_inline(self):
        self.assertEqual(uhi.classify_registration("func(w http.ResponseWriter, r *http.Request) {"), ("inline", None))

    def test_call_expression_is_call(self):
        self.assertEqual(uhi.classify_registration("mk(h.GetX)"), ("call", None))

    def test_handler_receiver_exported_method(self):
        self.assertEqual(uhi.classify_registration("h.GetChatChannelHistory"), ("handler", "GetChatChannelHistory"))

    def test_lowercase_method_on_handler_receiver_is_not_callable(self):
        # 探针：接收者对了但名字不导出 ⇒ 不可 `testHandler.X` 调 ⇒ 不得进索引。
        self.assertEqual(uhi.classify_registration("h.buildState"), ("qualified", None))

    def test_other_receiver_is_qualified(self):
        self.assertEqual(uhi.classify_registration("health.liveHandler"), ("qualified", None))

    def test_generated_code_receiver_is_qualified(self):
        # 形状 ④：生成代码里的 handler（`gen_grpc.GetX`）不是套件的接收者。
        self.assertEqual(uhi.classify_registration("gen.GetX"), ("qualified", None))

    def test_deeply_qualified_symbol_is_qualified(self):
        self.assertEqual(uhi.classify_registration("a.b.GetX"), ("qualified", None))

    def test_bare_identifier_is_qualified(self):
        self.assertEqual(uhi.classify_registration("GetX"), ("qualified", None))


# --------------------------------------------------------------------------- #
# 4. `_second_arg` —— 定位被引用的那一行
# --------------------------------------------------------------------------- #
class TestSecondArg(unittest.TestCase):
    def setUp(self):
        self.lines = [
            "package server",                                   # 1
            "func mount() {",                                    # 2
            '\tr.Get("/api/alpha", h.GetAlpha)',                 # 3
            '\tr.Get("/api/beta", h.GetBeta)',                  # 4
            '\tr.Post(pub.Path, h.GetByConst)',                  # 5
            '\tr.Put("/api/gamma",',                             # 6
            "\t\th.GetGamma,",                                   # 7
            "\t)",                                              # 8
            '\tr.Delete("/api/delta", h.GetDelta) // trailing',  # 9
            '\tr.Patch("/api/eps", mk(h.GetEps))',              # 10
            '\tr.Get("/api/zeta", h.GetZeta)',                  # 11
        ]

    def _row(self, line: int, path: str):
        return row("GET", path, line)

    def test_cited_line_is_read(self):
        self.assertEqual(uhi._second_arg(self.lines, self._row(3, "/api/alpha")), ("h.GetAlpha", True))

    def test_trailing_comment_after_the_call(self):
        self.assertEqual(uhi._second_arg(self.lines, self._row(9, "/api/delta")), ("h.GetDelta", True))

    def test_package_constant_path_is_flagged_unverifiable(self):
        # 路径不是字面量 ⇒ 无法核对，`literal_path=False`（表头会公开这个计数）。
        self.assertEqual(uhi._second_arg(self.lines, self._row(5, "/api/const")), ("h.GetByConst", False))

    def test_registration_spanning_three_lines(self):
        self.assertEqual(uhi._second_arg(self.lines, self._row(6, "/api/gamma")), ("h.GetGamma", True))

    def test_call_expression_second_arg(self):
        self.assertEqual(uhi._second_arg(self.lines, self._row(10, "/api/eps")), ("mk(h.GetEps)", True))

    def test_literal_contradicting_the_table_is_rejected(self):
        # 表说 /api/zeta，第 11 行注册的是别的路径 ⇒ 不得采信。
        self.assertIsNone(uhi._second_arg(self.lines, self._row(11, "/api/other")))

    def test_scan_window_is_bounded_to_five_lines_after(self):
        # 窗口 = 被引用的那一行 + 其后 5 行；第 7 行上的注册**取不到**（不无限前扫）。
        body = ["\t// filler"] * 5 + ['\tr.Get("/api/far", h.GetFar)']
        self.assertEqual(uhi._second_arg(body, self._row(1, "/api/far")), ("h.GetFar", True))
        farther = ["\t// filler"] * 7 + ['\tr.Get("/api/far", h.GetFar)']
        self.assertIsNone(uhi._second_arg(farther, self._row(2, "/api/far")))
        self.assertIsNone(uhi._second_arg(body[:5], self._row(1, "/api/far")))

    def test_line_past_the_end_of_the_file(self):
        self.assertIsNone(uhi._second_arg(self.lines, self._row(99, "/api/none")))

    def test_group_root_path_literal(self):
        lines = ['\tr.Get("/", h.GetGroupRoot)']
        self.assertEqual(uhi._second_arg(lines, self._row(1, "/api/group")), ("h.GetGroupRoot", True))

    def test_registration_without_a_second_argument_is_skipped(self):
        lines = ["\tr.Use(mw)"]
        self.assertIsNone(uhi._second_arg(lines, self._row(1, "/api/any")))


# --------------------------------------------------------------------------- #
# 5. `handler_index` —— 五种形状各一条合成 lane
# --------------------------------------------------------------------------- #
class TestHandlerIndexShapes(unittest.TestCase):
    def _index(self, body: str, rows):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, True)
        return uhi.handler_index(write_upstream(tmp, body), rows)

    def test_shape_1_same_route_two_handlers_keeps_both(self):
        # 同一路径在树上出现两次，两个接收者方法各注册一次 ⇒ 两条都在索引里。
        body = (
            '\tr.Get("/api/thing", h.ListThing)\n'
            '\tr.Get("/api/thing", h.GetThing)\n'
        )
        index, joined = self._index(body, [row("GET", "/api/thing", 1), row("GET", "/api/thing", 2)])
        self.assertEqual(sorted(index), ["GetThing", "ListThing"])
        self.assertEqual(len(joined), 2)
        self.assertEqual({j["kind"] for j in joined}, {"handler"})

    def test_shape_2_helper_and_handler_share_a_name(self):
        # `health.GetThing`（包级函数）与 `h.GetThing`（套件方法）同名 ⇒ 只有后者可调用。
        body = '\tr.Get("/api/thing", health.GetThing)\n\tr.Post("/api/thing", h.GetThing)\n'
        index, joined = self._index(body, [row("GET", "/api/thing", 1), row("POST", "/api/thing", 2)])
        self.assertEqual(list(index), ["GetThing"])
        self.assertEqual(joined[0]["kind"], "qualified")
        self.assertEqual(joined[1]["kind"], "handler")

    def test_shape_3_anonymous_middleware_closure(self):
        # 匿名闭包不是可调用 handler ⇒ 记 inline / 标签 `-`，索引里不得出现。
        # （第 4 行那个 `h.*` 是必需的：树里一个可调用注册都没有时脚本会响亮失败。）
        body = (
            '\tr.Get("/api/guard", func(w http.ResponseWriter, r *http.Request) {\n'
            "\t\tmw.ServeHTTP(w, r)\n"
            "\t})\n"
            '\tr.Get("/api/plain", h.GetPlain)\n'
        )
        index, joined = self._index(
            body, [row("GET", "/api/guard", 1), row("GET", "/api/plain", 4)]
        )
        self.assertEqual(list(index), ["GetPlain"])
        self.assertEqual(joined[0]["kind"], "inline")
        self.assertIsNone(joined[0]["handler"])

    def test_shape_4_generated_code_handler_is_kept_out_of_the_index(self):
        body = '\tr.Get("/api/gen", pb.GetGen)\n\tr.Get("/api/real", h.GetReal)\n'
        index, joined = self._index(body, [row("GET", "/api/gen", 1), row("GET", "/api/real", 2)])
        self.assertEqual(list(index), ["GetReal"])
        self.assertEqual([j["kind"] for j in joined], ["qualified", "handler"])

    def test_shape_5_same_symbol_name_in_two_packages(self):
        # 两个包各有 `GetX` ⇒ 都不是套件接收者 ⇒ 一条都不进索引。
        body = (
            '\tr.Get("/api/a", apiv1.GetX)\n'
            '\tr.Get("/api/b", apiv2.GetX)\n'
            '\tr.Get("/api/c", h.GetC)\n'
        )
        index, joined = self._index(
            body, [row("GET", "/api/a", 1), row("GET", "/api/b", 2), row("GET", "/api/c", 3)]
        )
        self.assertEqual(list(index), ["GetC"])
        self.assertEqual([j["expr"] for j in joined], ["apiv1.GetX", "apiv2.GetX", "h.GetC"])
        self.assertEqual([j["kind"] for j in joined], ["qualified", "qualified", "handler"])

    def test_joined_record_carries_row_expr_kind_and_literal_flag(self):
        body = '\tr.Get("/api/a", h.GetA)\n\tr.Get(constP, h.GetB)\n'
        _index, joined = self._index(body, [row("GET", "/api/a", 1, "M4"), row("GET", "/api/b", 2, "M5")])
        self.assertEqual(
            [(j["row"]["owner"], j["expr"], j["kind"], j["handler"], j["literal_path"]) for j in joined],
            [("M4", "h.GetA", "handler", "GetA", True), ("M5", "h.GetB", "handler", "GetB", False)],
        )

    def test_unreadable_registration_aborts_with_a_regeneration_hint(self):
        # 路由表与 checkout  disagree ⇒ 必须响亮失败，而不是静默少一行。
        body = '\tr.Get("/api/a", h.GetA)\n'
        with self.assertRaises(SystemExit) as ctx:
            self._index(body, [row("GET", "/api/missing", 2)])
        self.assertIn("gen_upstream_routes.py", str(ctx.exception))
        self.assertIn("/api/missing", str(ctx.exception))

    def test_tree_without_any_callable_registration_aborts(self):
        body = '\tr.Get("/api/a", apiv1.GetA)\n'
        with self.assertRaises(SystemExit) as ctx:
            self._index(body, [row("GET", "/api/a", 1)])
        self.assertIn("no handler registrations", str(ctx.exception))

    def test_missing_router_file_is_an_io_error(self):
        with self.assertRaises(OSError):
            uhi.handler_index(tempfile.mkdtemp(), [row("GET", "/api/a", 1)])


# --------------------------------------------------------------------------- #
# 6. `render_handler_routes` —— 表头计数 / 排序 / 标签
# --------------------------------------------------------------------------- #
class TestRenderHandlerRoutes(unittest.TestCase):
    def setUp(self):
        self.joined = [
            {"row": row("POST", "/api/z", 3, "M1"), "expr": "h.Zeta", "kind": "handler", "handler": "Zeta", "literal_path": True},
            {"row": row("GET", "/api/a", 1, "M2"), "expr": "h.Alpha", "kind": "handler", "handler": "Alpha", "literal_path": True},
            {"row": row("GET", "/api/const", 2, "M3"), "expr": "h.ByConst", "kind": "handler", "handler": "ByConst", "literal_path": False},
            {"row": row("GET", "/api/q", 4, "M4"), "expr": "health.live", "kind": "qualified", "handler": None, "literal_path": True},
            {"row": row("GET", "/api/c", 5, "M5"), "expr": "mk(h.C)", "kind": "call", "handler": None, "literal_path": True},
            {"row": row("GET", "/api/i", 6, "M6"), "expr": "func(w, r) {", "kind": "inline", "handler": None, "literal_path": True},
        ]

    def render(self) -> str:
        return uhi.render_handler_routes(self.joined, "deadbeef")

    def test_header_publishes_every_kind_and_the_constant_path_count(self):
        head = header_line(self.render(), "# rows:")
        self.assertIn("6 routes", head)
        self.assertIn("3 callable handler methods", head)
        self.assertIn("1 otherwise qualified", head)
        self.assertIn("1 call expressions", head)
        self.assertIn("1 inline closures", head)
        self.assertIn("1 of 6 rows cite a path written as a package constant", head)
        self.assertIn("upstream commit: deadbeef", self.render())

    def test_rows_are_sorted_handler_first_then_by_name(self):
        labels = [c[0] for c in parse_committed(self.render())]
        # 排序键 = (kind 序, 标签, method, path)：handler → qualified → 其余折成 `-`。
        self.assertEqual(labels, ["Alpha", "ByConst", "Zeta", "health.live", "-", "-"])

    def test_only_qualified_rows_carry_their_expression_verbatim(self):
        # call / inline 两类在表里都折成 `-`（测试无法按 `testHandler.X` 调它们）。
        table = parse_committed(self.render())
        self.assertEqual([c[0] for c in table if c[0] == "-"], ["-", "-"])
        self.assertEqual(
            [c[1:] for c in table if c[0] == "-"],
            [
                ["GET", "/api/c", "M5", "# router.go:5"],
                ["GET", "/api/i", "M6", "# router.go:6"],
            ],
        )
        self.assertEqual([c[0] for c in table if "." in c[0]], ["health.live"])

    def test_each_row_carries_back_the_owner_and_line(self):
        table = {c[0]: c for c in parse_committed(self.render())}
        self.assertEqual(table["Alpha"][1:], ["GET", "/api/a", "M2", "# router.go:1"])

    def test_render_is_deterministic_under_input_reordering(self):
        shuffled = list(reversed(self.joined))
        self.assertEqual(uhi.render_handler_routes(shuffled, "deadbeef"), self.render())

    def test_render_is_repeatable(self):
        self.assertEqual(self.render(), self.render())

    def test_empty_join_still_renders_a_header(self):
        text = uhi.render_handler_routes([], "x")
        self.assertIn("0 routes", text)
        self.assertTrue(text.endswith("# handler\tmethod\tpath\towner\t# router.go:N\n"))


# --------------------------------------------------------------------------- #
# 7. CLI —— 三种模式 + 两个错误码
# --------------------------------------------------------------------------- #
class TestCli(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.root = write_upstream(os.path.join(self.dir, "up"), '\tr.Get("/api/a", h.GetA)\n')
        self.tsv = os.path.join(self.dir, "handler-routes.tsv")
        self.rows_tsv = os.path.join(self.dir, "upstream-routes.tsv")
        with open(self.rows_tsv, "w", encoding="utf-8") as fh:
            fh.write("GET\t/api/a\tM1\t# router.go:1\n")
        self.saved = (uhi.ROUTES_TSV, uhi.HANDLER_ROUTES_TSV, uhi.route_rows.__defaults__)
        uhi.ROUTES_TSV = self.rows_tsv
        uhi.HANDLER_ROUTES_TSV = self.tsv
        # 🔴 `route_rows(path=ROUTES_TSV)` 的默认值在**定义时**求值 ⇒ 改模块变量
        # 对 `handler_index()` 内部的 `route_rows()` 调用**无效**。必须改 `__defaults__`，
        # 否则 CLI 的合成 lane 会去读真实的 456 行表（本片第一版就踩了，症状是
        # 「合成 checkout 里根本没有 /api/agent-activity-30d」这条 SystemExit）。
        uhi.route_rows.__defaults__ = (self.rows_tsv,)
        self.addCleanup(self._restore)

    def _restore(self):
        uhi.ROUTES_TSV, uhi.HANDLER_ROUTES_TSV, uhi.route_rows.__defaults__ = self.saved

    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = uhi.main(list(argv))
        return rc, out.getvalue(), err.getvalue()

    def _read(self, path: str) -> str:
        with open(path, encoding="utf-8") as fh:
            return fh.read()

    def test_write_then_check_round_trips_byte_for_byte(self):
        rc, out, _ = self.run_main("--upstream", self.root, "--write-handler-routes", self.tsv)
        self.assertEqual(rc, 0)
        self.assertIn("1 routes, 1 handlers", out)
        first = self._read(self.tsv)
        rc, out, _ = self.run_main("--upstream", self.root, "--write-handler-routes", self.tsv)
        self.assertEqual(rc, 0)
        self.assertEqual(self._read(self.tsv), first, "两次产出必须逐字相同")
        rc, out, _ = self.run_main("--upstream", self.root, "--check-handler-routes", self.tsv)
        self.assertEqual(rc, 0)
        self.assertIn("matches the router", out)

    def test_check_detects_drift(self):
        self.run_main("--upstream", self.root, "--write-handler-routes", self.tsv)
        with open(self.tsv, "a", encoding="utf-8") as fh:
            fh.write("Injected\tGET\t/api/x\tM9\t# router.go:99\n")
        rc, _out, err = self.run_main("--upstream", self.root, "--check-handler-routes", self.tsv)
        self.assertEqual(rc, 1)
        self.assertIn("drifted", err)

    def test_check_on_a_missing_file_is_exit_1_not_a_traceback(self):
        rc, _out, err = self.run_main(
            "--upstream", self.root, "--check-handler-routes", os.path.join(self.dir, "nope.tsv")
        )
        self.assertEqual(rc, 1)
        self.assertIn("is missing", err)

    def test_list_handlers_prints_the_census(self):
        rc, out, _ = self.run_main("--upstream", self.root, "--list-handlers")
        self.assertEqual(rc, 0)
        self.assertIn("GetA\tGET\t/api/a\trouter.go:1", out)
        self.assertIn("1 handlers, 1 routes", out)

    def test_a_directory_that_is_not_a_checkout_is_exit_2(self):
        rc, _out, err = self.run_main("--upstream", self.dir, "--list-handlers")
        self.assertEqual(rc, 2)
        self.assertIn("is not a multica checkout", err)

    def test_missing_upstream_argument_is_a_usage_error(self):
        with self.assertRaises(SystemExit) as ctx:
            self.run_main("--list-handlers")
        self.assertNotEqual(ctx.exception.code, 0)


# --------------------------------------------------------------------------- #
# 8. 真实输入面 —— 已提交的 456 行表与自己的判据自洽（不是分支覆盖的替代品）
# --------------------------------------------------------------------------- #
class TestCommittedTable(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(REAL_HANDLER_TSV, encoding="utf-8") as fh:
            cls.text = fh.read()
        cls.rows = parse_committed(cls.text)
        # (method, path) 不是主键 —— 同一路径可以注册多次 ⇒ 用 multimap。
        cls.routes: dict[tuple[str, str], set[int]] = {}
        for r in uhi.route_rows(REAL_ROUTES_TSV):
            cls.routes.setdefault((r["method"], r["path"]), set()).add(r["line"])

    def test_shape_is_handler_method_path_owner_line(self):
        self.assertEqual(len(self.rows), 456)
        for cells in self.rows:
            self.assertEqual(len(cells), 5)
            self.assertRegex(cells[4], r"^# router\.go:\d+$")

    def test_every_row_label_obeys_the_published_rule(self):
        # handler 行 ⇒ `h.<Name>` 分类；qualified 行 ⇒ 逐字是那个表达式；其余 ⇒ `-`。
        for label, method, path, _owner, cite in self.rows:
            line = int(cite.split(":")[1])
            if label == "-":
                continue
            if re.fullmatch(r"[A-Z][A-Za-z0-9_]*", label):
                self.assertEqual(uhi.classify_registration("h." + label), ("handler", label))
            else:
                self.assertEqual(uhi.classify_registration(label)[0], "qualified", label)
            key = (method, path)
            self.assertIn(key, self.routes, "表里的 (method, path) 必须来自已提交的路由表")
            self.assertIn(line, self.routes[key], "行号必须与路由表一致：%s" % label)

    def test_header_counts_match_the_rows_they_describe(self):
        head = header_line(self.text, "# rows:")
        counts = {
            k: int(v)
            for v, k in re.findall(r"(\d+) (?:otherwise )?(routes|qualified|call expressions|inline closures)", head)
        }
        handlers = [c for c in self.rows if re.fullmatch(r"[A-Z][A-Za-z0-9_]*", c[0])]
        qualified = [c for c in self.rows if c[0] != "-" and c not in handlers]
        dashes = [c for c in self.rows if c[0] == "-"]
        self.assertEqual(counts["routes"], len(self.rows))
        self.assertEqual(counts["call expressions"] + counts["inline closures"], len(dashes))
        self.assertEqual(counts["qualified"], len(qualified))
        # 表头的 451 数的是**行**（451 条路由），不是**互异 handler 名**（436 个）——
        # 两者不等正是「同一路由两个 handler」形状在真实数据里的体现。
        self.assertEqual(len(handlers), 451)
        self.assertEqual(len({c[0] for c in handlers}), 436)
        self.assertIn("451 callable handler methods", head)
        self.assertIn("18 of 456 rows cite a path written as a package constant", head)

    def test_sort_order_of_the_committed_table_is_the_published_one(self):
        keys = [
            (0 if re.fullmatch(r"[A-Z][A-Za-z0-9_]*", c[0]) else (1 if c[0] != "-" else 3), c[0], c[1], c[2])
            for c in self.rows
        ]
        # qualified 行以表达式排序，call/inline 统一折成 `-` ⇒ 用「折后标签」复核单调性。
        folded = [(k[0], "-" if k[0] == 3 else k[1], k[2], k[3]) for k in keys]
        self.assertEqual(folded, sorted(folded))

    def test_module_is_importable_by_its_documented_name(self):
        out = subprocess.run(
            [sys.executable, "-c", "import sys; sys.path.insert(0, %r); import upstream_handler_index as m; print(m.HANDLER_RECV)" % HERE],
            capture_output=True,
            text=True,
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout.strip(), "h")


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""门 ④ 判定器 `scripts/schema_drift.py`（799 行）的 `unittest` 覆盖（LUM-2621 / T1-6-Q2）。

**只测不改**：本文件一行也不改被测脚本（它离门 ⑩ 的 800 行硬上限**只剩 1 行**），
发现缺陷**只登记**（`KNOWN_DEFECTS` + `@unittest.expectedFailure` + 双向守卫），
修不修交回 owner 裁决（与 `LUM-2608` / `LUM-2617` 同族同纪律）。

🔴 **为什么它是绿的却是危险的**：`scripts/tests.manifest` 在本片之前没有它
⇒ 门 ④（`scripts/gates.sh:442`，`MULTICA_TEST_DATABASE_URL=… python3
scripts/schema_drift.py --quiet`）每天执行一个 **799 行、0 用例**的判定器，而门 ⑩ 只看行数，
「0 测试」不在它的四条判词里 ⇒ **一个下一次必然撞门的文件今天是绿的**。

**为什么这些用例不需要真库**：门 ④ 需要 `MULTICA_TEST_DATABASE_URL`，但判红逻辑本身全是
**纯函数** —— `load_snapshot` / `object_map` / `load_deviations` / `load_apply_exceptions` /
`_up_sql_files` / `merged_migrations` / `diff_snapshots` / `check_registry` 一个字节都不碰
数据库。因此本片是**零 Rust / 零 cargo / 零真库 / 零磁盘**，且**不跑门 ④**。

🔴 **「用例测的是真实函数」怎么证明**（`LUM-2614` 家族：import 失败后的空壳也会绿）：
`TestTheRealFunctionsAreUnderTest` 一族直接打真函数 —— 真 `contracts/upstream-schema.json`
（2146 个对象）、真 `migrations/`（566 个文件，跨 `upstream/` + `compat/` 两个目录）、
真 `contracts/schema-deviations.tsv`（45 条登记）、真
`contracts/upstream-apply-exceptions.tsv`，以及 `main([])` 的 **rc=2**（无 URL ⇒ 不起库）。
另外 `TestImportIsNotAnEmptyShell` 断言被测模块确实带那些符号。

Run: ``python3 scripts/test_schema_drift.py``
"""

import ast
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))  # scripts/ is not a package

import schema_drift as sd  # noqa: E402
from schema_snapshot import SNAPSHOT_FORMAT, SchemaToolError  # noqa: E402

#: 缺陷号 → 钉住它的用例、它声明的契约、当轮实测到的现象、收口方式（结构照 `LUM-2608` 的表）。
#: 四条字段都必填（有用例检查）—— 只写「有个已知缺陷」而不写「怎么算收口」，那张表就退化成散文。
KNOWN_DEFECTS = {
    "KD-1": {
        "case": "TestKnownDefects.test_kd1_check_registry_is_a_pure_read_of_rows_against_items",
        "claim": "`check_registry(items, rows)` 的 docstring 承诺返回 "
        "`(unregistered items, stale errors, stale warnings)` —— 它是「把 rows 读成三个列表」的"
        "**纯函数**，两次调用同一组入参必须给同一组出参",
        "observed": "它**就地改写** `row.covered` / `row.example` ⇒ 第二次调用时 `covered` 从上一轮"
        "的 `1` 起步 ⇒ 一条**已经不再匹配任何差异**的行第二次读成 `covered=1` ⇒ stale 列表为空 "
        "⇒ 过期行静默消失（今天 `main()` 只调一次，所以还看不出问题）",
        "close": "让 `check_registry` 返回新对象（覆盖计数放进返回值而不是行里），或接受"
        "「必须只调一次」并在 docstring 里写死；两种都可以，删掉装饰器即可",
    },
    "KD-2": {
        "case": "TestKnownDefects.test_kd2_an_indented_comment_is_a_comment_in_both_loaders",
        "claim": "模块 docstring：「Registry rows are TSV, `#` comments allowed」；"
        "`load_deviations` 用 `line.lstrip().startswith('#')` 实现 ⇒ **缩进的注释行也是注释**。"
        "`load_apply_exceptions` 应当同一条规则",
        "observed": "它用 `line.startswith('#')`（**不 lstrip**）⇒ 一行缩进的 `# 注释` 被当成数据行 "
        "⇒ `malformed row (want 6 tab-separated columns)` ⇒ `SchemaToolError` ⇒ `main()` **rc=2**。"
        "同一个模块、同一种「注释」写法，两个解析器两套规则",
        "close": "把那一行改成 `line.lstrip().startswith('#')`（与 `load_deviations` 对齐）后删掉装饰器",
    },
}


def _marked_expected_failures():
    """Every `test_*` in this module carrying `@unittest.expectedFailure`, as `Class.method`."""
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


class _Fixtures(unittest.TestCase):
    """夹具只落在 `tempfile` 里 ⇒ 收尾 `git status` 必然干净（不需要 `git add` 探针）。"""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    def write(self, name, text):
        p = self.tmp / name
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8")
        return p

    def snapshot_doc(self, objects=None, fmt=SNAPSHOT_FORMAT):
        if objects is None:
            objects = [{"kind": "table", "key": "issue", "def": {"relkind": "r"}}]
        return {"format": fmt, "objects": objects}


class TestImportIsNotAnEmptyShell(_Fixtures):
    """🔴 `LUM-2614` 家族：import 成功不等于测到了东西。这里钉「被测符号真的在」。"""

    def test_the_module_under_test_exposes_every_parser_the_ticket_names(self):
        for name in (
            "load_snapshot",
            "object_map",
            "load_deviations",
            "load_apply_exceptions",
            "merged_migrations",
            "diff_snapshots",
            "check_registry",
            "_up_sql_files",
            "dir_migrations",
        ):
            self.assertTrue(callable(getattr(sd, name, None)), name)

    def test_the_constants_the_loaders_validate_against_are_still_the_five_and_four(self):
        self.assertEqual(sd.DEVIATION_COLUMNS, ("对象", "类型", "差异摘要", "原因", "承接 issue"))
        self.assertEqual(sd.CATEGORIES, ("missing", "extra", "differs", "apply-exception"))

    def test_this_file_imports_only_the_standard_library_and_the_two_scripts_it_tests(self):
        """门 ⑫ 的隐含前提：本文件不 import 任何 crate、不需要 cargo、不需要真库。"""
        tree = ast.parse(Path(__file__).read_text(encoding="utf-8"))
        imported = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported.update(a.name.split(".")[0] for a in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported.add(node.module.split(".")[0])
        self.assertEqual(
            sorted(imported),
            ["ast", "json", "os", "pathlib", "schema_drift", "schema_snapshot", "subprocess",
             "sys", "tempfile", "unittest"],
        )


class TestLoadSnapshot(_Fixtures):
    """输入解析器之一：JSON 快照。"""

    def test_a_well_formed_snapshot_loads_and_keeps_its_meta(self):
        p = self.write("s.json", json.dumps(self.snapshot_doc()))
        doc = sd.load_snapshot(p)
        self.assertEqual(doc["format"], SNAPSHOT_FORMAT)
        self.assertEqual(len(doc["objects"]), 1)

    def test_a_missing_file_is_a_tool_error_not_a_traceback(self):
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_snapshot(self.tmp / "nope.json")
        self.assertIn("is missing", str(ctx.exception))

    def test_broken_json_is_a_tool_error_naming_the_path(self):
        p = self.write("s.json", "{not json")
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_snapshot(p)
        self.assertIn("not valid JSON", str(ctx.exception))

    def test_a_foreign_format_is_refused_rather_than_diffed_across(self):
        p = self.write("s.json", json.dumps(self.snapshot_doc(fmt=SNAPSHOT_FORMAT + 99)))
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_snapshot(p)
        self.assertIn("snapshot format", str(ctx.exception))

    def test_an_empty_object_list_is_refused(self):
        p = self.write("s.json", json.dumps(self.snapshot_doc(objects=[])))
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_snapshot(p)
        self.assertIn("no objects", str(ctx.exception))

    def test_an_object_row_without_kind_or_key_is_refused(self):
        p = self.write("s.json", json.dumps(self.snapshot_doc(objects=[{"key": "issue"}])))
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_snapshot(p)
        self.assertIn("malformed object row", str(ctx.exception))


class TestObjectMap(_Fixtures):
    """🔴 key 是 `(kind, key)` **二元组**，不是扁平字符串 —— 断言按二元组构造。"""

    def test_keys_are_kind_key_tuples(self):
        doc = self.snapshot_doc(
            objects=[
                {"kind": "table", "key": "issue", "def": {"relkind": "r"}},
                {"kind": "column", "key": "issue.id", "def": {"table": "issue"}},
            ]
        )
        om = sd.object_map(doc)
        self.assertEqual(sorted(om), [("column", "issue.id"), ("table", "issue")])
        self.assertIn(("column", "issue.id"), om)

    def test_a_row_without_a_def_maps_to_an_empty_dict_not_to_none(self):
        om = sd.object_map(self.snapshot_doc(objects=[{"kind": "table", "key": "issue"}]))
        self.assertEqual(om, {("table", "issue"): {}})

    def test_the_same_name_under_two_kinds_stays_two_entries(self):
        """`issue` 作为表和作为函数是**两个对象**；扁平 key 会塌成一行。"""
        om = sd.object_map(
            self.snapshot_doc(
                objects=[
                    {"kind": "table", "key": "issue", "def": {"relkind": "r"}},
                    {"kind": "function", "key": "issue", "def": {"language": "sql"}},
                ]
            )
        )
        self.assertEqual(len(om), 2)


class TestLoadDeviations(_Fixtures):
    """输入解析器之二：登记 TSV。列数 / 空列 / 类别前缀 / `allow_missing` 两态。"""

    ROW = "issue.id\tcolumn\textra: 本地增列\tcompat 补丁\tLUM-1\n"

    def rows(self, text):
        return sd.load_deviations(self.write("d.tsv", text))

    def test_a_well_formed_row_loads_with_all_five_fields(self):
        [row] = self.rows(self.ROW)
        self.assertEqual(
            (row.key, row.kind, row.summary, row.reason, row.issue, row.line, row.covered),
            ("issue.id", "column", "extra: 本地增列", "compat 补丁", "LUM-1", 1, 0),
        )

    def test_blank_lines_and_comments_are_ignored_including_indented_ones(self):
        [row] = self.rows("\n# a comment\n  # indented comment\n\n" + self.ROW)
        self.assertEqual(row.key, "issue.id")

    def test_line_numbers_count_every_physical_line_not_just_the_data_ones(self):
        [row] = self.rows("# c\n\n# c2\n" + self.ROW)
        self.assertEqual(row.line, 4)

    def test_a_missing_file_is_a_tool_error(self):
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_deviations(self.tmp / "nope.tsv")
        self.assertIn("is missing", str(ctx.exception))

    def test_allow_missing_true_degrades_to_an_empty_registry(self):
        self.assertEqual(sd.load_deviations(self.tmp / "nope.tsv", allow_missing=True), [])

    def test_wrong_column_count_reports_the_line_number_and_what_it_found(self):
        p = self.write("d.tsv", "# c\n" + self.ROW + "issue.a\tcolumn\textra: x\treason\n")
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_deviations(p)
        self.assertIn(f"{p}:3:", str(ctx.exception))
        self.assertIn("found 4", str(ctx.exception))

    def test_a_sixth_column_is_also_refused(self):
        p = self.write("d.tsv", self.ROW.rstrip("\n") + "\textra\n")
        with self.assertRaises(SchemaToolError):
            sd.load_deviations(p)

    def test_object_kind_and_summary_must_not_be_empty(self):
        for bad in (
            "\tcolumn\textra: x\treason\tLUM-1\n",
            "issue.id\t\textra: x\treason\tLUM-1\n",
            "issue.id\tcolumn\t\treason\tLUM-1\n",
        ):
            with self.subTest(bad=bad):
                with self.assertRaises(SchemaToolError) as ctx:
                    self.rows(bad)
                self.assertIn("must not be empty", str(ctx.exception))

    def test_the_summary_prefix_must_be_one_of_the_four_categories(self):
        p = self.write("d.tsv", "issue.id\tcolumn\tadded: x\treason\tLUM-1\n")
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_deviations(p)
        self.assertIn("must start with one of", str(ctx.exception))

    def test_reason_and_issue_are_both_mandatory(self):
        for text, needle in (
            ("issue.id\tcolumn\textra: x\t\tLUM-1\n", "原因 is empty"),
            ("issue.id\tcolumn\textra: x\treason\t\n", "承接 issue is empty"),
        ):
            with self.subTest(needle=needle):
                with self.assertRaises(SchemaToolError) as ctx:
                    self.rows(text)
                self.assertIn(needle, str(ctx.exception))

    def test_duplicate_entries_are_kept_as_two_rows_and_both_match_one_difference(self):
        """🔴 **实测**：没有去重、没有告警。两条一模一样的登记覆盖同一个差异 ⇒ 门仍绿。
        这是读数（今天的行为），不是「应该」—— 重复登记只是冗余，不构成判红理由。"""
        rows = self.rows(self.ROW + self.ROW)
        self.assertEqual([r.line for r in rows], [1, 2])
        [item] = [sd.DiffItem("column", "issue.id", "extra", "extra: x")]
        unreg, stale_err, stale_warn = sd.check_registry([item], rows)
        self.assertEqual((unreg, stale_err, stale_warn), ([], [], []))
        self.assertEqual([r.covered for r in rows], [1, 1])


class TestDeviationRowMatching(_Fixtures):
    """`DeviationRow.matches` —— 登记怎么匹配一条差异：key / glob / kind / 类别 四个维度。"""

    @staticmethod
    def row(key, kind, summary, line=1):
        return sd.DeviationRow(key, kind, summary, "reason", "LUM-1", line)

    def test_an_exact_key_matches_and_a_different_one_does_not(self):
        item = sd.DiffItem("column", "issue.id", "extra", "extra: x")
        self.assertTrue(self.row("issue.id", "column", "extra: x").matches(item))
        self.assertFalse(self.row("issue.name", "column", "extra: x").matches(item))

    def test_the_glob_form_documented_in_the_docstring_matches_a_qualified_column_key(self):
        """docstring 说 `<table>.*` 在实践里就是 `issue.*`；真快照的列 key 正是 `issue.identifier`。"""
        item = sd.DiffItem("column", "issue.identifier", "extra", "extra: x")
        self.assertTrue(self.row("issue.*", "column", "extra: x").matches(item))

    def test_a_table_glob_does_not_match_the_table_itself_only_its_children(self):
        """`fnmatch("issue", "issue.*")` 为假 —— 记下来，别把两种 key 混着用断言。"""
        table = sd.DiffItem("table", "issue", "missing", "missing: x")
        column = sd.DiffItem("column", "issue.id", "missing", "missing: x")
        row = self.row("issue.*", "*", "missing: x")
        self.assertFalse(row.matches(table))
        self.assertTrue(row.matches(column))

    def test_kind_must_match_or_be_the_star_wildcard(self):
        item = sd.DiffItem("column", "issue.id", "extra", "extra: x")
        self.assertFalse(self.row("issue.id", "table", "extra: x").matches(item))
        self.assertTrue(self.row("issue.id", "*", "extra: x").matches(item))

    def test_the_category_must_match_too(self):
        """同一对象、同一列，`missing:` 的登记不覆盖 `extra:` 的差异（方向不同）。"""
        item = sd.DiffItem("column", "issue.id", "extra", "extra: x")
        self.assertFalse(self.row("issue.id", "column", "missing: y").matches(item))

    def test_the_category_property_reads_the_prefix_not_the_whole_summary(self):
        self.assertEqual(self.row("k", "table", "differs: same object").category, "differs")


class TestLoadApplyExceptions(_Fixtures):
    """输入解析器之三：生成的例外表。**6 列**（比登记多一列 `statement`）、按对象去重。"""

    HEADER = "# contracts/upstream-apply-exceptions.tsv\n"
    OK = "032_x.up.sql\t3\textension-unavailable\tno pg_bigm\tindex:idx_a\tCREATE INDEX ...\n"

    def items(self, text):
        return sd.load_apply_exceptions(self.write("e.tsv", text))

    def test_a_well_formed_row_becomes_one_apply_exception_item(self):
        [item] = self.items(self.HEADER + self.OK)
        self.assertEqual(item.ident, ("idx_a", "index", "apply-exception"))
        self.assertEqual(item.source, "032_x.up.sql:3")
        self.assertEqual(item.detail, ["extension-unavailable at 032_x.up.sql:3"])
        self.assertIn("no pg_bigm", item.summary)

    def test_comments_and_blank_lines_are_ignored(self):
        self.assertEqual(len(self.items(self.HEADER + "\n" + self.OK)), 1)

    def test_a_missing_file_is_a_tool_error(self):
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_apply_exceptions(self.tmp / "nope.tsv")
        self.assertIn("is missing", str(ctx.exception))

    def test_five_columns_is_refused_even_though_only_five_are_read(self):
        p = self.write("e.tsv", "f.up.sql\t3\textension-unavailable\treason\textension:pg_bigm\n")
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_apply_exceptions(p)
        self.assertIn("want 6 tab-separated columns", str(ctx.exception))

    def test_a_row_naming_no_object_is_refused_so_no_skip_can_escape_the_registry(self):
        p = self.write("e.tsv", "f.up.sql\t3\tcat\treason\t\tstatement\n")
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_apply_exceptions(p)
        self.assertIn("lists no object", str(ctx.exception))

    def test_an_unparsable_object_token_is_refused(self):
        p = self.write("e.tsv", "f.up.sql\t3\tcat\treason\t:noname\tstatement\n")
        with self.assertRaises(SchemaToolError) as ctx:
            sd.load_apply_exceptions(p)
        self.assertIn("cannot parse object", str(ctx.exception))

    def test_several_objects_on_one_line_become_several_items_in_file_order(self):
        [a, b] = self.items(
            "f.up.sql\t3\tcat\treason\tindex:idx_a,index:idx_b\tstatement\n"
        )
        self.assertEqual([i.ident for i in (a, b)], [("idx_a", "index", "apply-exception"),
                                                     ("idx_b", "index", "apply-exception")])

    def test_the_same_object_twice_is_one_item_that_records_both_sources(self):
        """去重是真的：第二条**不新增 item**，只在 `detail` 上追加一行来源。"""
        [item] = self.items(
            "f.up.sql\t3\tcat\treason\textension:pg_bigm\tstmt\n"
            "g.up.sql\t9\tcat\treason\textension:pg_bigm\tstmt2\n"
        )
        self.assertEqual(item.detail, ["cat at f.up.sql:3", "also skipped at g.up.sql:9"])
        self.assertEqual(item.source, "f.up.sql:3")

    def test_the_statement_fallback_key_is_carried_through(self):
        [item] = self.items(
            "076_x.up.sql\t101\tcat\treason\tstatement:076_x.up.sql:101\tstmt\n"
        )
        self.assertEqual(item.ident, ("076_x.up.sql:101", "statement", "apply-exception"))


class TestUpSqlDiscovery(_Fixtures):
    """`_up_sql_files` / `dir_migrations` / `merged_migrations` —— merge 的核心语义。"""

    def test_recursive_false_sees_only_the_files_directly_under_the_directory(self):
        self.write("001_a.up.sql", "select 1;")
        self.write("compat/002_b.up.sql", "select 1;")
        d = Path(self.tmp)
        self.assertEqual([p.name for p in sd._up_sql_files(d, recursive=False)], ["001_a.up.sql"])
        self.assertEqual([p.name for p in sd.dir_migrations(d)], ["001_a.up.sql"])

    def test_recursive_true_sees_both_directories(self):
        self.write("001_a.up.sql", "select 1;")
        self.write("compat/002_b.up.sql", "select 1;")
        files = sd._up_sql_files(Path(self.tmp), recursive=True)
        self.assertEqual([p.name for p in files], ["001_a.up.sql", "002_b.up.sql"])

    def test_down_files_are_never_part_of_the_apply_set(self):
        self.write("001_a.up.sql", "select 1;")
        self.write("001_a.down.sql", "drop table a;")
        self.assertEqual([p.name for p in sd.dir_migrations(Path(self.tmp))], ["001_a.up.sql"])

    def test_an_empty_directory_is_an_error_not_an_empty_apply_set(self):
        """判词写死：空 apply set 会让**每个**上游对象都以错误的原因显示为 missing。"""
        empty = self.tmp / "empty"
        empty.mkdir()
        with self.assertRaises(SchemaToolError) as ctx:
            sd._up_sql_files(empty, recursive=True)
        self.assertIn("no *.up.sql migrations", str(ctx.exception))

    def test_merged_migrations_orders_by_stem_across_subdirectories(self):
        self.write("compat/535_z.up.sql", "select 1;")
        self.write("upstream/001_a.up.sql", "select 1;")
        files = sd.merged_migrations(Path(self.tmp))
        self.assertEqual([p.stem for p in files], ["001_a.up", "535_z.up"])
        self.assertEqual([p.parent.name for p in files], ["upstream", "compat"])

    def test_a_duplicate_stem_across_two_directories_is_rejected_not_last_one_wins(self):
        """docstring：`a duplicate stem is an error rather than a silent "last one wins"`。"""
        self.write("upstream/001_a.up.sql", "select 1;")
        self.write("compat/001_a.up.sql", "select 1;")
        with self.assertRaises(SchemaToolError) as ctx:
            sd.merged_migrations(Path(self.tmp))
        self.assertIn("duplicate migration version", str(ctx.exception))

    def test_the_two_same_named_files_under_the_two_real_directories_are_merged_in_this_repo(self):
        """真仓库读数：`upstream/` 560 + `compat/` 6 = 566，stem 唯一且有序。"""
        files = sd.merged_migrations(ROOT / "migrations")
        stems = [p.stem for p in files]
        self.assertEqual(len(files), 566)
        self.assertEqual(len(set(stems)), len(stems))
        self.assertEqual(stems, sorted(stems))
        self.assertEqual(len({p.parent.name for p in files}), 2)


class TestDiffSnapshots(_Fixtures):
    """`diff_snapshots` —— 四个方向的差异 + 子对象折叠 + 纯列序差异。"""

    def test_identical_snapshots_produce_no_items(self):
        om = sd.object_map(self.snapshot_doc())
        self.assertEqual(sd.diff_snapshots(om, dict(om)), [])

    def test_a_table_only_upstream_is_missing_and_a_table_only_here_is_extra(self):
        mine = sd.object_map(self.snapshot_doc([{"kind": "table", "key": "extra_t", "def": {}}]))
        theirs = sd.object_map(self.snapshot_doc([{"kind": "table", "key": "missing_t", "def": {}}]))
        items = sd.diff_snapshots(mine, theirs)
        self.assertEqual(sorted(i.ident for i in items), [("extra_t", "table", "extra"),
                                                         ("missing_t", "table", "missing")])

    def test_a_table_present_on_both_sides_with_a_different_def_is_differs(self):
        mine = sd.object_map(self.snapshot_doc([{"kind": "table", "key": "t", "def": {"relkind": "r"}}]))
        theirs = sd.object_map(self.snapshot_doc([{"kind": "table", "key": "t", "def": {"relkind": "v"}}]))
        [item] = sd.diff_snapshots(mine, theirs)
        self.assertEqual(item.ident, ("t", "table", "differs"))
        self.assertEqual(item.detail, ["relkind: repo='r' upstream='v'"])

    def test_the_children_of_a_missing_table_are_folded_into_its_row(self):
        """docstring：columns/constraints/indexes/triggers 归到缺失表那一行，不逐条列。"""
        theirs = {
            ("table", "t"): {},
            ("column", "t.c"): {"table": "t"},
            ("index", "t.i"): {"table": "t"},
            ("constraint", "t.k"): {"table": "t"},
        }
        items = sd.diff_snapshots({}, theirs)
        self.assertEqual(len(items), 1)
        self.assertEqual(items[0].ident, ("t", "table", "missing"))
        self.assertEqual(items[0].folded, {"column": 1, "constraint": 1, "index": 1})
        self.assertIn("+1 column", items[0].folded_note())

    def test_a_child_of_a_table_present_on_both_sides_is_listed_on_its_own(self):
        mine = {("table", "t"): {}, ("column", "t.c"): {"table": "t", "type": "int"}}
        theirs = {("table", "t"): {}, ("column", "t.c"): {"table": "t", "type": "text"}}
        [item] = sd.diff_snapshots(mine, theirs)
        self.assertEqual(item.ident, ("t.c", "column", "differs"))

    def test_a_non_child_kind_is_never_folded(self):
        """`function` 不在 `CHILD_KINDS` 里 ⇒ 缺失表的函数仍逐条列出。"""
        theirs = {("function", "t.f"): {"table": "t"}}
        [item] = sd.diff_snapshots({}, theirs)
        self.assertEqual(item.ident, ("t.f", "function", "missing"))

    def test_a_reordered_column_is_a_difference_but_is_flagged_position_only(self):
        mine = {("column", "t.c"): {"table": "t", "position": 2, "type": "int"}}
        theirs = {("column", "t.c"): {"table": "t", "position": 1, "type": "int"}}
        [item] = sd.diff_snapshots(mine, theirs)
        self.assertTrue(item.position_only)
        self.assertEqual(item.detail_note(), "column order only (attnum)")

    def test_a_real_field_change_alongside_a_move_is_not_position_only(self):
        mine = {("column", "t.c"): {"table": "t", "position": 2, "type": "int"}}
        theirs = {("column", "t.c"): {"table": "t", "position": 1, "type": "text"}}
        [item] = sd.diff_snapshots(mine, theirs)
        self.assertFalse(item.position_only)

    def test_the_two_snapshots_are_the_repo_side_and_the_upstream_side_in_that_order(self):
        """`main()` 传的是 `diff_snapshots(object_map(snapshot), object_map(upstream))`
        ⇒ `theirs` 恒是上游；判词的方向（missing/extra）依赖这个次序，别弄反。"""
        mine = {("table", "only_here"): {}}
        theirs = {("table", "only_there"): {}}
        by_key = {i.key: i.category for i in sd.diff_snapshots(mine, theirs)}
        self.assertEqual(by_key, {"only_here": "extra", "only_there": "missing"})


class TestCheckRegistry(_Fixtures):
    """`check_registry` 的**三个方向**分开读：未登记 / 过期（apply-exception 硬错）/ 过期（其余告警）。"""

    @staticmethod
    def item(key="t.c", kind="column", category="missing"):
        return sd.DiffItem(kind, key, category, f"{category}: x")

    def test_a_difference_with_no_matching_row_is_unregistered(self):
        unreg, stale_err, stale_warn = sd.check_registry([self.item()], [])
        self.assertEqual(len(unreg), 1)
        self.assertEqual((stale_err, stale_warn), ([], []))

    def test_a_row_that_matched_records_how_much_it_covered_and_one_example(self):
        row = sd.DeviationRow("t.*", "column", "missing: x", "r", "LUM-1", 4)
        unreg, _, _ = sd.check_registry([self.item("t.a"), self.item("t.b")], [row])
        self.assertEqual(unreg, [])
        self.assertEqual((row.covered, row.example, row.line), (2, "t.a", 4))

    def test_an_apply_exception_row_that_matched_nothing_is_a_hard_error(self):
        """docstring：`apply-exception:` 的行是生成的，必须与例外表严格同步 ⇒ 过期即 rc=1。"""
        row = sd.DeviationRow("extension", "*", "apply-exception: x", "r", "LUM-1", 7)
        _, stale_err, stale_warn = sd.check_registry([], [row])
        self.assertEqual([(r.key, r.line) for r in stale_err], [("extension", 7)])
        self.assertEqual(stale_warn, [])

    def test_a_row_of_any_other_category_that_matched_nothing_is_only_a_warning(self):
        row = sd.DeviationRow("gone", "column", "missing: x", "r", "LUM-1", 9)
        _, stale_err, stale_warn = sd.check_registry([], [row])
        self.assertEqual(stale_err, [])
        self.assertEqual([r.key for r in stale_warn], ["gone"])

    def test_both_skip_directions_the_docstring_promises_are_reachable(self):
        """「a skip with no registered row fails」= 未登记；「a registered row with no skip
        fails」= 过期硬错。两条都从同一组入参推出来，不是靠 mock。"""
        items = [self.item("ext.pg_bigm", "extension", "apply-exception")]
        row = sd.DeviationRow("extension", "*", "apply-exception: x", "r", "LUM-1", 3)
        unreg, stale_err, _ = sd.check_registry(items, [])
        self.assertEqual(len(unreg), 1)
        unreg, stale_err, _ = sd.check_registry([], [row])
        self.assertEqual(len(stale_err), 1)


class TestRenderDeviations(_Fixtures):
    """`--emit-deviations` 的骨架行：`原因` / `承接 issue` **故意留空**，粘回去必定被拒。"""

    def test_the_skeleton_names_the_category_and_leaves_the_last_two_columns_empty(self):
        text = sd.render_deviations([sd.DiffItem("column", "t.c", "missing", "missing: x")])
        self.assertIn("t.c\tcolumn\tmissing:\t\t", text.splitlines())

    def test_a_skeleton_row_pasted_back_into_the_registry_is_rejected(self):
        """docstring 承诺的那一条，今天是真的（`原因 is empty`）。"""
        text = sd.render_deviations([sd.DiffItem("column", "t.c", "missing", "missing: x")])
        body = [ln for ln in text.splitlines() if not ln.startswith("#")][0]
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "d.tsv"
            p.write_text(body + "\n", encoding="utf-8")
            with self.assertRaises(SchemaToolError) as ctx:
                sd.load_deviations(p)
        self.assertIn("原因 is empty", str(ctx.exception))

    def test_rows_come_out_sorted_by_category_kind_key(self):
        items = [
            sd.DiffItem("table", "z", "missing", "missing: x"),
            sd.DiffItem("column", "a", "extra", "extra: x"),
            sd.DiffItem("column", "b", "missing", "missing: x"),
        ]
        body = [ln for ln in sd.render_deviations(items).splitlines() if not ln.startswith("#")]
        self.assertEqual(body, ["a\tcolumn\textra:\t\t", "b\tcolumn\tmissing:\t\t",
                                "z\ttable\tmissing:\t\t"])


class TestDiffItemShape(_Fixtures):
    def test_to_json_keeps_every_field_the_report_prints(self):
        item = sd.DiffItem("column", "t.c", "differs", "differs: x", ["a: repo=1 upstream=2"],
                           folded={"index": 2}, source="f.up.sql:3", position_only=True)
        self.assertEqual(
            item.to_json(),
            {
                "object": "t.c",
                "kind": "column",
                "category": "differs",
                "summary": "differs: x",
                "detail": ["a: repo=1 upstream=2"],
                "folded": {"index": 2},
                "source": "f.up.sql:3",
                "position_only": True,
            },
        )

    def test_ident_is_key_kind_category(self):
        self.assertEqual(sd.DiffItem("column", "t.c", "missing", "x").ident,
                         ("t.c", "column", "missing"))

    def test_a_long_definition_is_clipped_with_an_ellipsis(self):
        long = "x" * 400
        self.assertTrue(sd._clip(long).endswith("..."))
        self.assertEqual(len(sd._clip(long)), 160)
        self.assertEqual(sd._clip("short"), "short")


class TestTheRealFunctionsAreUnderTest(_Fixtures):
    """🔴 门 ④ 本片**不跑**（需要 `MULTICA_TEST_DATABASE_URL`）⇒ 用例必须证明它测的是
    **真数据上的真实函数**，而不是 import 失败后的空壳（`LUM-2614` 家族）。"""

    def test_the_real_upstream_snapshot_loads_and_maps_to_2146_objects(self):
        doc = sd.load_snapshot(ROOT / "contracts" / "upstream-schema.json")
        om = sd.object_map(doc)
        self.assertEqual(len(om), 2146)
        self.assertIn(("table", "issue"), om)
        self.assertEqual(sd.diff_snapshots(om, dict(om)), [])  # 真快照自比 = 零差异

    def test_the_real_registry_loads_and_every_row_carries_a_reason_and_an_issue(self):
        rows = sd.load_deviations(ROOT / "contracts" / "schema-deviations.tsv")
        self.assertGreater(len(rows), 40)
        self.assertTrue(all(r.reason and r.issue for r in rows))
        self.assertTrue(all(r.category in sd.CATEGORIES for r in rows))

    def test_the_real_registry_has_no_stale_row_in_either_direction_right_now(self):
        """把真例外项 + 真快照自比喂进去：没有未登记，也没有过期硬错（两个方向的存量读数）。"""
        rows = sd.load_deviations(ROOT / "contracts" / "schema-deviations.tsv")
        exceptions = sd.load_apply_exceptions(
            ROOT / "contracts" / "upstream-apply-exceptions.tsv"
        )
        unreg, stale_err, _ = sd.check_registry(exceptions, rows)
        self.assertEqual(stale_err, [])
        self.assertTrue(all(i.category == "apply-exception" for i in exceptions))

    def test_the_real_exceptions_file_yields_one_item_per_object_with_a_source(self):
        items = sd.load_apply_exceptions(ROOT / "contracts" / "upstream-apply-exceptions.tsv")
        self.assertGreater(len(items), 0)
        self.assertTrue(all(":" in i.source for i in items))

    def test_the_real_migration_set_is_the_merged_upstream_plus_compat_numbering(self):
        files = sd.merged_migrations(ROOT / "migrations")
        upstream_only = sd.dir_migrations(ROOT / "migrations" / "upstream")
        self.assertEqual(len(upstream_only), 560)
        self.assertEqual(len(files) - len(upstream_only), 6)

    def test_main_without_a_database_url_is_rc2_and_says_so(self):
        """真 `main()`：不起库、不连服务器，只走参数解析的第一道判红。"""
        env = dict(os.environ)
        env.pop("MULTICA_TEST_DATABASE_URL", None)
        proc = subprocess.run(
            [sys.executable, str(SCRIPTS / "schema_drift.py"), "--quiet"],
            capture_output=True, text=True, env=env, cwd=str(ROOT),
        )
        self.assertEqual(proc.returncode, 2)
        self.assertIn("no database URL", proc.stderr)
        self.assertEqual(proc.stdout, "")


class TestKnownDefectRegistry(unittest.TestCase):
    """登记表与装饰器**双向**机器校验（照 `LUM-2608` 的做法，理由见那里的 docstring）。"""

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

    def test_the_registry_is_json_serialisable_so_it_can_be_cited_by_docs(self):
        self.assertEqual(json.loads(json.dumps(KNOWN_DEFECTS)), KNOWN_DEFECTS)
        for defect_id in KNOWN_DEFECTS:
            self.assertRegex(defect_id, r"^KD-\d+$")


class TestKnownDefects(unittest.TestCase):
    """KD-1 / KD-2 —— 断言在**正确**的那一侧；今天的行为另有绿用例钉住。"""

    def test_kd1_today_check_registry_accumulates_coverage_into_the_rows(self):
        """KD-1 的**今天行为**（绿）：`covered` / `example` 是就地改写的字段。"""
        row = sd.DeviationRow("t.c", "column", "missing: x", "r", "LUM-1", 1)
        self.assertEqual((row.covered, row.example), (0, ""))
        sd.check_registry([sd.DiffItem("column", "t.c", "missing", "missing: x")], [row])
        self.assertEqual((row.covered, row.example), (1, "t.c"))

    @unittest.expectedFailure
    def test_kd1_check_registry_is_a_pure_read_of_rows_against_items(self):
        """🔴 KD-1：`check_registry` 第二次调用会把**过期行读成已覆盖**。

        `main()` 今天只调一次，所以门 ④ 仍绿；但 `covered` 留在行里意味着任何第二次读
        （重试、增量报告、未来的 `--watch`）都会得到「stale = 0」的空结论。断言在
        **纯读**这一侧：换一组 items 重算，覆盖计数必须从头来。
        """
        row = sd.DeviationRow("t.c", "column", "missing: x", "r", "LUM-1", 1)
        matched = sd.DiffItem("column", "t.c", "missing", "missing: x")
        sd.check_registry([matched], [row])
        # 第二遍：这条差异**已经不在** items 里了（表被补上了），行应当变回过期。
        _, stale_err, stale_warn = sd.check_registry([], [row])
        self.assertEqual(row.covered, 0)
        self.assertEqual([r.key for r in stale_warn], ["t.c"])

    def test_kd2_today_an_indented_comment_is_a_hard_parse_error_in_the_exceptions_loader(self):
        """KD-2 的**今天行为**（绿）：与 xfail 那条互为对照，缺一条就只剩单边。"""
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "e.tsv"
            p.write_text("  # indented\n", encoding="utf-8")
            with self.assertRaises(SchemaToolError) as ctx:
                sd.load_apply_exceptions(p)
        self.assertIn("malformed row", str(ctx.exception))
        # 同一个模块的另一个解析器接受它 —— 这正是 KD-2 说的不对称。
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "d.tsv"
            p.write_text("  # indented\n", encoding="utf-8")
            self.assertEqual(sd.load_deviations(p), [])

    @unittest.expectedFailure
    def test_kd2_an_indented_comment_is_a_comment_in_both_loaders(self):
        """🔴 KD-2：一个模块两种「注释」规则 —— 缩进的 `#` 行在例外表里是 rc=2 的硬失败。

        `load_deviations` 用 `lstrip()`，`load_apply_exceptions` 不用；而例外表本身
        **就是一个满屏 `#` 注释的生成文件**。断言在「注释就是注释」这一侧。
        """
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "e.tsv"
            p.write_text("  # indented comment\n\n", encoding="utf-8")
            self.assertEqual(sd.load_apply_exceptions(p), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)

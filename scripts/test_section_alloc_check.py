#!/usr/bin/env python3
"""`scripts/section_alloc_check.py` 的 `unittest`（LUM-2623 / T1-6-Q3，`docs/37 §293`）。

门 ⑬（`scripts/gates.sh:172` 的 `ALL_GATES`、`.github/workflows/ci.yml:90` 的 **`fast` 必过项**）
每天都在跑这个 234 行的判定器，而本文件落地之前它 **0 用例、0 门执行**：
`scripts/tests.manifest` 里没有对应行 ⇒ 门 ⑫ 只消费它的**退出码**，
门 ⑩（`file_size_check.py`）又因为它**不在 `file_size_baseline.tsv` 里**而只能按行数判它。
它是本仓 `ci.yml` 直接引用的四个判定器里**最后一个**没有测试的（`LUM-2606`/`LUM-2608`/`LUM-2617`/
`LUM-2620`/`LUM-2621` 已收）。

**只测不改**：缺陷一律登记在 `KNOWN_DEFECTS` + `@unittest.expectedFailure` ⇒ 门 ⑫ 仍绿，
但用例每天都跑；谁修好实现 ⇒ `UNEXPECTED SUCCESS` ⇒ 门红 ⇒ 必须同时删装饰器并改本表。

判词就是「cycle 自己这一轮写的号段对不对」——台账双向一致性（R1 文件→台账 / R2 台账→文件）、
撞号（R3 台账行数）与出现次数（R4 第 4 列）。四条各钉一遍，外加两个前置判据
（缺文件 / 空台账必须判红，「没有东西可校验」不许读成绿）。

读数纪律（`§283`：**用例数可以是绿的**）：每条断言都先实测过，含「今天是什么行为」。
本片 0 路由 ⇒ 门 ⑦ 的八个数字必须**逐字不变**，验收证据只能来自门 ⑫ 的用例数与门 ⑬ 的
`defects=0`。

零 Rust / 零 cargo / 零真库 / 零容器 / 零磁盘，纯标准库。
Run: ``python3 scripts/test_section_alloc_check.py``
"""

import ast
import contextlib
import io
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import section_alloc_check as sac  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
REAL_DOC = os.path.join(ROOT, "docs", "37-M3-W3C-PREFLIGHT.md")
REAL_LEDGER = os.path.join(ROOT, "docs", "section-alloc.tsv")

#: 缺陷号 → 钉住它的用例、它声明的契约、当轮实测到的现象、收口方式。**四条字段都必填**。
KNOWN_DEFECTS = {
    "KD-1": {
        "case": "TestKnownDefects.test_kd1_headings_inside_a_fenced_code_block_are_not_sections",
        "claim": "`## §NNN` 只在**真实标题**处算段号；写在 ``` 围栏里的示例行不是标题 ⇒ 只有它、而台账登记了该段号时应判 R2",
        "observed": "`read_doc_sections` 对每行做 `SECTION_RE.match`，**从不跟踪围栏状态** ⇒ 围栏里孤立的 `## §300` 被读成真段号（`sections=1`），台账那一行反而被判绿（`defects=[]`、rc=0）",
        "close": "`read_doc_sections` 记住 ``` / ~~~ 的开关并跳过围栏内的行，然后删掉装饰器",
    },
    "KD-2": {
        "case": "TestKnownDefects.test_kd2_a_brand_new_duplicate_cannot_be_laundered_by_the_count_column",
        "claim": "撞号在**提交前**就红（本文件 docstring：「两个片各自分配同一个空号，就表现为两行同号」⇒ R3）；第 4 列只该用于**如实登记既有的** §226，不该成为新增撞号的洗白口",
        "observed": "R3 只数**台账行数**、R4 只比**次数** ⇒ 文件里 `## §300` 出现 2 次、台账写 `300…2` 时四条判词**全部判绿**（`defects=[]`、rc=0）；没有任何「允许次数>1 的段号」白名单能把它与 §226 区分开",
        "close": "给 R3/R4 加一条：次数 > 1 的段号必须在一个显式的历史例外名单里（今天只有 §226），不在名单内即红；然后删掉装饰器",
    },
    "KD-3": {
        "case": "TestKnownDefects.test_kd3_non_canonical_section_numbers_are_a_format_defect",
        "claim": "`read_ledger` 的 docstring 承诺「段号不是十进制数字 ⇒ 判**缺陷**，而不是静默忽略：台账漂了必须看得见」⇒ 非规范写法（`0300` 前导零、`0` 号段）应当是一条**格式缺陷**",
        "observed": "`num.isdigit()` 接受 `0300` 与 `0` ⇒ 它们进 `rows` 后与文件里的 `\"300\"` 是**不同字符串** ⇒ 同一次台账漂移被报成 R1 + R2 两条「段不存在」，而不是一条「段号写法不规范」；`0` 号段则被完全接受（`defects=[]`）",
        "close": "在 `read_ledger` 里判 `num == str(int(num))` 且 `int(num) >= 1`，不满足即缺陷（保留行以免 R1/R2 重复刷屏）；然后删掉装饰器",
    },
    "KD-4": {
        "case": "TestKnownDefects.test_kd4_the_r4_message_names_the_row_that_was_actually_compared",
        "claim": "R4 的消息必须指名它拿来比较的**那一行**的登记次数（`ledger_declared` 是 dict ⇒ 同一个段号有多行时被覆盖）",
        "observed": "`ledger_declared = {r[\"num\"]: r[\"count\"] …}` 让**最后一行**胜出，而消息里的 `lines` 列出**全部**同号行 ⇒ 两行分别登记 1 与 5、文件里出现 1 次时，消息说「台账行 1,2 登记 5 次」（行 1 明明登记 1 次）",
        "close": "R4 按行遍历并逐行比较（撞号交给 R3 报），消息里只列被比较的那一行；然后删掉装饰器",
    },
    "KD-5": {
        "case": "TestKnownDefects.test_kd5_out_of_order_section_numbers_are_reported",
        "claim": "`docs/37` 的 `## §` 必须**按段号单调**排列（台账纪律，`LUM-2623` 工单 §2 明写）⇒ 乱序应当有判词",
        "observed": "四条判词全是**集合/计数**比较，`read_doc_sections` 返回的**顺序**被完全丢弃 ⇒ 文件写 `## §300` 后接 `## §200`（两段都在台账里）判绿；**仓库现状本身就不单调**：`## §226` 的第二次出现在 `docs/37:20884`，夹在 §225 与 §227 之间",
        "close": "在 `check()` 里对 `read_doc_sections` 的段号序列做单调性检查（并把 §226 的既有两处按历史例外登记），然后删掉装饰器",
    },
}

MISSING_DEFECT_FIELDS = ("case", "claim", "observed", "close")


@contextlib.contextmanager
def fixture(doc: str, ledger: str, missing: tuple[str, ...] = ()):
    """在一对临时文件上跑 `check()`：把模块级 `DOC` / `LEDGER` 指过去，跑完复原。

    `missing` 里给的文件名（`"doc"` / `"ledger"`）**不落盘** ⇒ 用来测「缺文件判红」。
    """
    tmp = Path(tempfile.mkdtemp())
    doc_p, led_p = tmp / "37-M3-W3C-PREFLIGHT.md", tmp / "section-alloc.tsv"
    if "doc" not in missing:
        doc_p.write_text(doc, encoding="utf-8")
    if "ledger" not in missing:
        led_p.write_text(ledger, encoding="utf-8")
    old = (sac.DOC, sac.LEDGER)
    sac.DOC, sac.LEDGER = doc_p, led_p
    try:
        yield tmp
    finally:
        sac.DOC, sac.LEDGER = old


def check(doc: str, ledger: str, missing: tuple[str, ...] = ()) -> tuple[list[str], dict]:
    with fixture(doc, ledger, missing):
        return sac.check()


def main(argv: list[str], doc: str, ledger: str, missing: tuple[str, ...] = ()) -> tuple[int, str, str]:
    """跑 `main(argv)` 并抓 stdout / stderr（`check()` 读的就是被 patch 的那两个路径）。"""
    out, err = io.StringIO(), io.StringIO()
    with fixture(doc, ledger, missing):
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = sac.main(argv)
    return rc, out.getvalue(), err.getvalue()


def sec(line: str) -> str | None:
    """`SECTION_RE.match(line)` 的段号，或 None（不把 `Optional[Match]` 的收窄留给用例作者）。"""
    m = sac.SECTION_RE.match(line)
    return m.group(1) if m else None


def ledger_row(num: str, holder: str = "LUM-1", summary: str = "摘要", count: str | None = None) -> str:
    row = f"{num}\t{holder}\t{summary}"
    return row if count is None else f"{row}\t{count}"


class TestSectionRegex(unittest.TestCase):
    """`SECTION_RE` 本身：三种真实写法都收，`§1730` 不许被读成 `§173`。"""

    def test_it_matches_the_three_heading_spellings_that_really_exist_in_this_repo(self):
        for line in ("## §173", "## §173. 标题", "## §173 标题"):
            self.assertEqual(sec(line), "173", line)

    def test_it_also_matches_the_colon_and_the_list_separator_spellings(self):
        for line in ("## §173、标题", "## §173：标题", "## §173: title"):
            self.assertEqual(sec(line), "173", line)

    def test_it_tolerates_extra_space_around_the_section_sign(self):
        for line in ("##§173", "##  §  173  标题"):
            self.assertEqual(sec(line), "173", line)

    def test_a_four_digit_section_is_not_truncated_to_three(self):
        self.assertEqual(sec("## §1730 标题"), "1730")

    def test_a_letter_right_after_the_digits_is_not_a_section(self):
        self.assertIsNone(sec("## §173a 标题"))

    def test_it_only_anchors_at_the_two_hash_level(self):
        self.assertIsNone(sac.SECTION_RE.match("### §173"))
        self.assertIsNone(sac.SECTION_RE.match("# §173"))
        self.assertIsNone(sac.SECTION_RE.match("#### §173"))

    def test_a_bare_hash_level_two_without_the_section_sign_is_not_a_section(self):
        self.assertIsNone(sac.SECTION_RE.match("## 173 标题"))

    def test_an_indented_section_line_is_not_a_section(self):
        self.assertIsNone(sac.SECTION_RE.match("  ## §173 标题"))

    def test_the_pattern_is_compiled_from_the_module_level_name_the_gate_docstring_names(self):
        self.assertIsInstance(sac.SECTION_RE, re.Pattern)
        self.assertEqual(sac.SECTION_RE.pattern, r"^##\s*§\s*(\d+)(?=[\s.、:：]|$)")


class TestReadDocSections(unittest.TestCase):
    def test_it_returns_one_based_line_numbers(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "x.md"
            p.write_text("前言\n\n## §300 标题\n", encoding="utf-8")
            self.assertEqual(sac.read_doc_sections(p), [(3, "300")])

    def test_it_keeps_duplicate_occurrences_because_r4_counts_them(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "x.md"
            p.write_text("## §226 a\n## §227 b\n## §226 c\n", encoding="utf-8")
            got = sac.read_doc_sections(p)
        self.assertEqual(got, [(1, "226"), (2, "227"), (3, "226")])
        self.assertEqual(len(got), 3, "R4 依赖重复出现被保留；去重会让 R4 永远判绿")

    def test_it_returns_an_empty_list_for_a_file_without_any_section(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "x.md"
            p.write_text("只有散文\n", encoding="utf-8")
            self.assertEqual(sac.read_doc_sections(p), [])

    def test_it_ignores_a_section_sign_that_is_not_at_the_start_of_the_line(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "x.md"
            p.write_text("正文里提到 ## §300 不是标题\n", encoding="utf-8")
            self.assertEqual(sac.read_doc_sections(p), [])

    def test_crlf_line_endings_still_yield_the_section(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "x.md"
            p.write_bytes(b"## \xc2\xa7300 a\r\n## \xc2\xa7301 b\r\n")
            self.assertEqual(sac.read_doc_sections(p), [(1, "300"), (2, "301")])


class TestReadLedger(unittest.TestCase):
    def _read(self, text: str):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "section-alloc.tsv"
            p.write_text(text, encoding="utf-8")
            return sac.read_ledger(p)

    def test_a_three_field_row_defaults_the_count_to_one(self):
        rows, defects = self._read(ledger_row("300") + "\n")
        self.assertEqual(defects, [])
        self.assertEqual(rows[0]["count"], 1)
        self.assertEqual(rows[0]["num"], "300")
        self.assertEqual(rows[0]["holder"], "LUM-1")
        self.assertEqual(rows[0]["summary"], "摘要")
        self.assertEqual(rows[0]["line"], 1)

    def test_a_four_field_row_keeps_the_declared_count(self):
        rows, defects = self._read(ledger_row("300", count="2") + "\n")
        self.assertEqual(defects, [])
        self.assertEqual(rows[0]["count"], 2)

    def test_comments_and_blank_lines_are_skipped(self):
        rows, defects = self._read("# 注释\n\n   \n" + ledger_row("300") + "\n# 尾注释\n")
        self.assertEqual(defects, [])
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["line"], 4, "行号是物理行号：注释与空行都占号")

    def test_a_leading_space_before_the_hash_still_counts_as_a_comment(self):
        rows, defects = self._read("   # 缩进注释\n" + ledger_row("300") + "\n")
        self.assertEqual(defects, [])
        self.assertEqual(len(rows), 1)

    def test_two_fields_is_a_format_defect_and_the_row_is_dropped(self):
        rows, defects = self._read("300\tLUM-1\n")
        self.assertEqual(rows, [])
        self.assertEqual(len(defects), 1)
        self.assertIn("需要 3 或 4 个 TAB 分隔字段", defects[0])

    def test_five_fields_is_a_format_defect(self):
        rows, defects = self._read("300\tLUM-1\t摘要\t1\t\n")
        self.assertEqual(rows, [])
        self.assertIn("需要 3 或 4 个 TAB 分隔字段", defects[0])

    def test_a_non_numeric_section_number_is_a_format_defect(self):
        rows, defects = self._read(ledger_row("§300") + "\n")
        self.assertEqual(rows, [])
        self.assertIn("段号必须是十进制数字", defects[0])

    def test_a_non_numeric_count_is_a_format_defect(self):
        rows, defects = self._read(ledger_row("300", count="两次") + "\n")
        self.assertEqual(rows, [])
        self.assertIn("出现次数必须是 >=1 的十进制数字", defects[0])

    def test_a_zero_count_is_a_format_defect(self):
        rows, defects = self._read(ledger_row("300", count="0") + "\n")
        self.assertEqual(rows, [])
        self.assertIn("出现次数必须是 >=1 的十进制数字", defects[0])

    def test_the_defect_message_carries_the_line_number(self):
        _, defects = self._read(ledger_row("300") + "\n" + ledger_row("301", count="x") + "\n")
        self.assertIn(":2:", defects[0])

    def test_a_number_defect_message_quotes_the_offending_field(self):
        _, defects = self._read(ledger_row("300") + "\n" + ledger_row("§301") + "\n")
        self.assertIn("§301", defects[0])
        self.assertIn("段号必须是十进制数字", defects[0])

    def test_two_rows_for_the_same_number_are_both_kept_so_r3_can_see_them(self):
        rows, defects = self._read(ledger_row("300", "LUM-1", "a") + "\n" + ledger_row("300", "LUM-2", "b") + "\n")
        self.assertEqual(defects, [])
        self.assertEqual([r["holder"] for r in rows], ["LUM-1", "LUM-2"])
        self.assertEqual([r["line"] for r in rows], [1, 2])

    def test_crlf_line_endings_do_not_break_the_count_column(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "section-alloc.tsv"
            p.write_bytes(b"300\tLUM-1\t\xe6\x91\x98\xe8\xa6\x81\t2\r\n")
            rows, defects = sac.read_ledger(p)
        self.assertEqual(defects, [])
        self.assertEqual(rows[0]["count"], 2)

    def test_surrounding_whitespace_in_the_fields_is_stripped(self):
        rows, defects = self._read(" 300 \t LUM-1 \t 摘要 \t 2 \n")
        self.assertEqual(defects, [])
        self.assertEqual((rows[0]["num"], rows[0]["holder"], rows[0]["count"]), ("300", "LUM-1", 2))


class TestRuleR1FileToLedger(unittest.TestCase):
    def test_a_section_in_the_file_with_no_ledger_row_is_red(self):
        defects, rd = check("## §300 a\n## §302 b\n", ledger_row("300") + "\n")
        self.assertEqual(rd["R1"], 1)
        self.assertEqual(rd["R2"], 0)
        self.assertTrue(any(d.startswith("R1 段号 302") for d in defects), defects)

    def test_a_fully_registered_file_is_green(self):
        defects, rd = check("## §300 a\n## §301 b\n", ledger_row("300") + "\n" + ledger_row("301") + "\n")
        self.assertEqual(defects, [])
        self.assertEqual(rd["R1"], 0)

    def test_the_r1_message_names_the_number_and_how_many_places_it_appears(self):
        defects, _ = check("## §300 a\n## §300 b\n", ledger_row("301") + "\n")
        msg = [d for d in defects if d.startswith("R1 段号 300")][0]
        self.assertIn("2 处", msg)

    def test_removing_the_ledger_row_of_an_existing_section_flips_green_to_red(self):
        doc, ledger = "## §300 a\n", ledger_row("300") + "\n"
        self.assertEqual(check(doc, ledger)[0], [])
        defects, rd = check(doc, "")
        self.assertEqual(rd["R1"], 1)


class TestRuleR2LedgerToFile(unittest.TestCase):
    def test_a_ledger_row_with_no_section_in_the_file_is_red(self):
        defects, rd = check("## §300 a\n", ledger_row("300") + "\n" + ledger_row("999") + "\n")
        self.assertEqual(rd["R2"], 1)
        self.assertTrue(any(d.startswith("R2 台账行") for d in defects), defects)

    def test_the_r2_message_names_the_ledger_line_number(self):
        defects, _ = check("## §300 a\n", ledger_row("300") + "\n" + ledger_row("999") + "\n")
        msg = [d for d in defects if d.startswith("R2 台账行")][0]
        self.assertIn("台账行 2", msg)

    def test_the_check_is_bidirectional_so_a_ledger_cannot_drift_into_prose(self):
        """工单里的正向对照：台账注入一行 999 ⇒ 红。"""
        doc, ledger = "## §300 a\n", ledger_row("300") + "\n"
        self.assertEqual(check(doc, ledger)[0], [])
        self.assertEqual(check(doc, ledger + ledger_row("999") + "\n")[0] != [], True)
        self.assertEqual(check(doc, ledger)[0], [], "撤销后必须回到绿（台账是手工维护的）")


class TestRuleR3LedgerUniqueness(unittest.TestCase):
    def test_two_ledger_rows_for_the_same_number_are_red(self):
        defects, rd = check("## §300 a\n", ledger_row("300", "LUM-1", "a") + "\n" + ledger_row("300", "LUM-2", "b") + "\n")
        self.assertEqual(rd["R3"], 1)
        msg = [d for d in defects if d.startswith("R3 撞号")][0]
        self.assertIn("台账行 1,2", msg)
        self.assertIn("LUM-1 / LUM-2", msg)

    def test_three_rows_for_the_same_number_report_one_defect_naming_all_three_lines(self):
        ledger = "".join(ledger_row("300", f"LUM-{i}") + "\n" for i in (1, 2, 3))
        defects, rd = check("## §300 a\n", ledger)
        self.assertEqual(rd["R3"], 1)
        self.assertIn("台账行 1,2,3", [d for d in defects if d.startswith("R3")][0])

    def test_a_registered_historic_duplicate_is_about_the_ledger_not_the_file(self):
        """R3 数的是**台账行数**；文件里同一段号出现两次由 R4 管（第 4 列）。"""
        doc = "## §226 a\n## §226 b\n"
        self.assertEqual(check(doc, ledger_row("226", count="2") + "\n")[1]["R3"], 0)
        self.assertEqual(check(doc, ledger_row("226", count="2") + "\n")[1]["R4"], 0)


class TestRuleR4OccurrenceCount(unittest.TestCase):
    def test_a_file_side_duplicate_registered_once_is_red(self):
        defects, rd = check("## §300 a\n## §300 b\n", ledger_row("300") + "\n")
        self.assertEqual(rd["R4"], 1)
        msg = [d for d in defects if d.startswith("R4 段号 300")][0]
        self.assertIn("出现 2 次", msg)
        self.assertIn("登记 1 次", msg)

    def test_a_file_side_duplicate_registered_twice_is_green(self):
        defects, rd = check("## §300 a\n## §300 b\n", ledger_row("300", count="2") + "\n")
        self.assertEqual(defects, [])
        self.assertEqual(rd["R4"], 0)

    def test_a_count_larger_than_the_file_side_occurrences_is_red(self):
        _, rd = check("## §300 a\n", ledger_row("300", count="3") + "\n")
        self.assertEqual(rd["R4"], 1)

    def test_r4_is_the_only_rule_that_catches_a_duplicate_with_no_count_column(self):
        """docstring 承诺的形态：没有第 4 列 ⇒ R1–R3 全绿，只有 R4 抓得住。"""
        doc = "## §300 a\n## §300 b\n"
        _, rd = check(doc, ledger_row("300") + "\n")
        self.assertEqual((rd["R1"], rd["R2"], rd["R3"]), (0, 0, 0))
        self.assertEqual(rd["R4"], 1, "R4 是唯一判词；它失明时门对自己的历史失败模式失明")

    def test_a_three_way_duplicate_registered_once_reports_one_defect(self):
        _, rd = check("## §300 a\n## §300 b\n## §300 c\n", ledger_row("300") + "\n")
        self.assertEqual(rd["R4"], 1)

    def test_r4_does_not_double_report_the_missing_side(self):
        """R4 只看两侧都有的段号：0 的那侧交给 R1 / R2，避免重复刷屏。"""
        _, rd = check("## §300 a\n", ledger_row("999") + "\n")
        self.assertEqual(rd["R4"], 0)
        self.assertEqual((rd["R1"], rd["R2"]), (1, 1))


class TestPreconditions(unittest.TestCase):
    def test_a_missing_document_is_red_not_green(self):
        defects, rd = check("", ledger_row("300") + "\n", missing=("doc",))
        self.assertTrue(any("缺文件" in d and "只能判红" in d for d in defects), defects)
        self.assertEqual(rd, {})

    def test_a_missing_ledger_is_red_not_green(self):
        defects, _ = check("## §300 a\n", "", missing=("ledger",))
        self.assertTrue(any("缺文件" in d for d in defects), defects)
        self.assertEqual(len([d for d in defects if "R1" in d or "R2" in d]), 0, "前置判红后不再报集合类缺陷")

    def test_both_missing_is_red(self):
        defects, _ = check("", "", missing=("doc", "ledger"))
        self.assertEqual(len([d for d in defects if "缺文件" in d]), 2)

    def test_an_empty_ledger_is_red(self):
        defects, rd = check("## §300 a\n", "")
        self.assertTrue(any("是空的（0 行）" in d for d in defects), defects)
        self.assertEqual(rd["ledger_rows"], 0)

    def test_a_ledger_of_only_comments_counts_as_empty_and_is_red(self):
        defects, rd = check("## §300 a\n", "# 只有注释\n")
        self.assertEqual(rd["ledger_rows"], 0)
        self.assertTrue(any("是空的（0 行）" in d for d in defects), defects)

    def test_a_file_with_no_sections_and_a_non_empty_ledger_is_red_on_r2(self):
        defects, rd = check("只有散文\n", ledger_row("300") + "\n")
        self.assertEqual(rd["R2"], 1)
        self.assertEqual(len(defects), 1)

    def test_both_files_empty_is_red_exactly_once(self):
        defects, rd = check("", "")
        self.assertEqual(len(defects), 1)
        self.assertEqual(rd["ledger_rows"], 0)


class TestReadings(unittest.TestCase):
    def test_the_readings_distinguish_sections_from_distinct_numbers(self):
        _, rd = check("## §300 a\n## §301 b\n## §300 c\n", ledger_row("300", count="2") + "\n" + ledger_row("301") + "\n")
        self.assertEqual(rd["sections"], 3)
        self.assertEqual(rd["distinct"], 2)
        self.assertEqual(rd["ledger_rows"], 2)

    def test_doc_dups_lists_only_the_numbers_that_really_repeat(self):
        _, rd = check("## §300 a\n## §300 b\n## §301 c\n", ledger_row("300", count="2") + "\n" + ledger_row("301") + "\n")
        self.assertEqual(rd["doc_dups"], ["300"])

    def test_doc_dups_is_empty_for_a_clean_file(self):
        _, rd = check("## §300 a\n", ledger_row("300") + "\n")
        self.assertEqual(rd["doc_dups"], [])

    def test_every_rule_counter_is_present_in_the_readings(self):
        _, rd = check("## §300 a\n", ledger_row("300") + "\n")
        for key in ("R1", "R2", "R3", "R4"):
            self.assertIn(key, rd)
            self.assertEqual(rd[key], 0)

    def test_the_four_counters_sum_to_the_number_of_defects_in_a_clean_shaped_failure(self):
        """四个计数器只数「存在性/唯一性/次数」类缺陷；格式缺陷不计入 ⇒ 断言它们可加。"""
        doc = "## §300 a\n## §300 b\n## §301 c\n"
        ledger = ledger_row("300", count="1") + "\n" + ledger_row("302") + "\n" + ledger_row("400", "L", "s", "x") + "\n"
        defects, rd = check(doc, ledger)
        self.assertEqual((rd["R1"], rd["R2"], rd["R3"], rd["R4"]), (1, 1, 0, 1))
        self.assertEqual(len(defects), 4, "R1 + R2 + R4 三条，加一条台账格式缺陷（`400 L s x` 被丢弃）")

    def test_the_defect_list_of_that_same_fixture_names_each_rule_exactly_once(self):
        doc = "## §300 a\n## §300 b\n## §301 c\n"
        ledger = ledger_row("300") + "\n" + ledger_row("302") + "\n"
        defects, rd = check(doc, ledger)
        self.assertEqual([d.split()[0] for d in defects].count("R1"), 1)
        self.assertEqual([d.split()[0] for d in defects].count("R2"), 1)
        self.assertEqual([d.split()[0] for d in defects].count("R4"), 1)


class TestMain(unittest.TestCase):
    def test_a_clean_pair_exits_zero_with_the_one_line_summary(self):
        rc, out, err = main(["--quiet"], "## §300 a\n", ledger_row("300") + "\n")
        self.assertEqual(rc, 0)
        self.assertEqual(out.strip(), "section-alloc: OK — sections=1 numbers=1 ledger=1 defects=0")
        self.assertEqual(err, "")

    def test_a_defective_pair_exits_one_and_prints_the_summary_as_fail(self):
        rc, out, err = main(["--quiet"], "## §300 a\n", ledger_row("999") + "\n")
        self.assertEqual(rc, 1)
        self.assertIn("section-alloc: FAIL — 2 defect(s)", out)
        self.assertEqual(err, "", "--quiet 不打逐条缺陷")

    def test_quiet_keeps_the_per_defect_lines_out_of_stdout(self):
        _, verbose, verr = main([], "## §300 a\n", ledger_row("999") + "\n")
        self.assertIn("R1 段号 300", verr)
        self.assertIn("R2 台账行", verr)
        self.assertIn("section-alloc:", verbose)
        self.assertIn("vs", verbose)
        self.assertIn("readings: sections=1 distinct=1 ledger_rows=1", verbose)
        self.assertIn("R1(file->ledger)=1", verbose)
        self.assertIn("R4(count)=0", verbose)

    def test_verbose_names_a_registered_duplicate_as_not_a_defect(self):
        _, verbose, _ = main([], "## §226 a\n## §226 b\n", ledger_row("226", count="2") + "\n")
        self.assertIn("既有重复段号（由台账第 4 列如实登记，不是缺陷）: §226", verbose)
        self.assertIn("section-alloc: OK", verbose)

    def test_verbose_omits_the_duplicate_line_when_there_is_none(self):
        _, verbose, _ = main([], "## §300 a\n", ledger_row("300") + "\n")
        self.assertNotIn("既有重复段号", verbose)

    def test_verbose_says_nothing_about_readings_when_the_precondition_failed(self):
        rc, out, err = main([], "## §300 a\n", "", missing=("ledger",))
        self.assertEqual(rc, 1)
        self.assertNotIn("readings:", out)
        self.assertIn("缺文件", err)

    def test_the_summary_line_of_a_missing_file_failure_falls_back_to_zero_readings(self):
        rc, out, _ = main(["--quiet"], "", "", missing=("doc", "ledger"))
        self.assertEqual(rc, 1)
        self.assertIn("FAIL", out)

    def test_an_unknown_flag_exits_two(self):
        with self.assertRaises(SystemExit) as ctx:
            with fixture("## §300 a\n", ledger_row("300") + "\n"):
                with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    sac.main(["--write-ledger"])
        self.assertEqual(ctx.exception.code, 2)

    def test_help_names_the_script_and_says_it_is_read_only(self):
        buf = io.StringIO()
        with self.assertRaises(SystemExit) as ctx:
            with fixture("## §300 a\n", ledger_row("300") + "\n"):
                with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(io.StringIO()):
                    sac.main(["--help"])
        self.assertEqual(ctx.exception.code, 0)
        self.assertIn("section_alloc_check.py", buf.getvalue())
        self.assertIn("--quiet", buf.getvalue())

    def test_the_module_exposes_no_write_mode_in_its_source(self):
        """docstring 的承诺：「本脚本是**只读**的：没有 `--write-*` 模式」。"""
        source = (Path(HERE) / "section_alloc_check.py").read_text(encoding="utf-8")
        # `--write-*` 在 docstring 与 argparse description 里被**提到**（说明它不存在）；
        # 断言的是没有任何一个 `--write-` 被**注册**成 flag。
        self.assertNotIn('"--write', source)
        self.assertNotIn("'--write", source)
        # 唯一的 `os.open` 是 BrokenPipeError 兜底里的 devnull，不是写被校验的文件。
        for verb in (".write_text(", ".write_bytes(", "os.remove", "os.rename", "unlink(", "shutil.", "os.replace"):
            self.assertNotIn(verb, source, f"只读脚本里出现了写动作：{verb}")

    def test_the_gate_runs_the_script_exactly_as_ci_does(self):
        """门 ⑬ 的命令唯一实现在 `gates.sh`；这里跑真实仓库，确认它今天判绿。"""
        proc = subprocess.run(
            [sys.executable, os.path.join(HERE, "section_alloc_check.py"), "--quiet"],
            capture_output=True, text=True, cwd=ROOT,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("defects=0", proc.stdout)


class TestRealRepoInvariant(unittest.TestCase):
    """真实仓库今天必须判绿，且 `§226` 必须仍以「第 4 列 = 2」的形式被如实登记。"""

    def setUp(self):
        self.defects, self.readings = sac.check()

    def test_the_real_document_and_ledger_are_green_today(self):
        self.assertEqual(self.defects, [])

    def test_the_real_pair_has_the_shape_the_gate_summary_prints(self):
        """摘要三元组必须等于**独立重数**出来的值 —— 2026-09-30 14:30 cycle 把字面量换成了这个。

        原来这里钉的是字面量 `(224, 223, 223)`。那是 §293 落盘那天的**读数**，不是不变式：
        并发的 `LUM-2624` 合入后 `docs/37` 多一个 `## §294` ⇒ 真实值变成 `(225, 224, 224)`
        ⇒ 门 ⑫ 判红，而**两片单独都是绿的**（`§294.5` 记的就是这件事）。

        字面量读数的问题是：它把「今天」当成了「永远」，于是**每一次正常新增段号都会打红它**，
        而这类红会逼着人把数字改成新读数 —— 那就等于把断言退化成「打印当前值」。

        现在的判据是**自洽**的：另起一份实现（不调 `sac` 的计数器）把 `## §` 标题与台账首列
        各数一遍，要求与门打印的三元组一致。这条仍然会因「门的计数器算错」而红，
        但不会因「仓库今天多了两段」而红。
        """
        doc_nums = []
        with open(REAL_DOC, encoding="utf-8") as fh:
            for line in fh.read().splitlines():
                got = sec(line)
                if got:
                    doc_nums.append(got)
        led_nums = []
        with open(REAL_LEDGER, encoding="utf-8") as fh:
            for line in fh.read().splitlines():
                if line.strip() and not line.lstrip().startswith("#"):
                    led_nums.append(line.split("\t")[0])
        self.assertEqual(
            (self.readings["sections"], self.readings["distinct"], self.readings["ledger_rows"]),
            (len(doc_nums), len(set(doc_nums)), len(led_nums)),
        )

    def test_section_226_is_the_one_and_only_registered_duplicate(self):
        self.assertEqual(self.readings["doc_dups"], ["226"])

    def test_the_226_ledger_row_really_declares_two_occurrences(self):
        with open(REAL_LEDGER, encoding="utf-8") as fh:
            rows = [ln for ln in fh.read().splitlines() if ln.startswith("226\t")]
        self.assertEqual(len(rows), 1, "台账里 226 只能有一行")
        self.assertEqual(rows[0].split("\t")[3], "2")

    def test_the_226_heading_really_appears_twice_in_the_document(self):
        with open(REAL_DOC, encoding="utf-8") as fh:
            lines = fh.read().splitlines()
        hits = [i + 1 for i, ln in enumerate(lines) if sec(ln) == "226"]
        self.assertEqual(len(hits), 2, hits)
        # 第二处夹在 §225 与 §227 之间 ⇒ 仓库现状本身就不是单调的（KD-5 的现场证据）。
        self.assertTrue(hits[1] > 20000 and hits[0] < 21000, hits)

    def test_every_ledger_row_points_at_a_section_that_really_exists(self):
        self.assertEqual(self.readings["R1"], 0)
        self.assertEqual(self.readings["R2"], 0)

    def test_the_ledger_is_the_same_size_as_the_distinct_number_count(self):
        self.assertEqual(self.readings["distinct"], self.readings["ledger_rows"])

    def test_the_last_ledger_row_is_the_highest_number(self):
        with open(REAL_LEDGER, encoding="utf-8") as fh:
            nums = [int(ln.split("\t")[0]) for ln in fh.read().splitlines() if ln.strip() and not ln.lstrip().startswith("#")]
        self.assertEqual(nums, sorted(nums))
        # 末行 = 仓库当前最高段号。同样是**自洽**判据，不是「今天 = 295」——
        # 下一个 cycle 加 §296 时这条不该红（见上面那条的 docstring：同一次仲裁）。
        doc_nums = []
        with open(REAL_DOC, encoding="utf-8") as fh:
            for line in fh.read().splitlines():
                got = sec(line)
                if got:
                    doc_nums.append(int(got))
        self.assertEqual(nums[-1], max(doc_nums), "台账末行 = docs/37 里出现的最高段号")

    def test_the_document_carries_a_section_for_every_ledger_number(self):
        with open(REAL_DOC, encoding="utf-8") as fh:
            doc_nums = {n for n in (sec(ln) for ln in fh.read().splitlines()) if n}
        with open(REAL_LEDGER, encoding="utf-8") as fh:
            led_nums = {ln.split("\t")[0] for ln in fh.read().splitlines() if ln.strip() and not ln.lstrip().startswith("#")}
        self.assertEqual(doc_nums, led_nums)


class TestKnownDefects(unittest.TestCase):
    def test_every_known_defect_names_a_case_that_exists(self):
        for kd, meta in KNOWN_DEFECTS.items():
            for field in MISSING_DEFECT_FIELDS:
                self.assertTrue(meta.get(field), f"{kd} 缺字段 {field}")
            self.assertIn(meta["case"], ALL_TEST_IDS)

    @unittest.expectedFailure
    def test_kd1_headings_inside_a_fenced_code_block_are_not_sections(self):
        """围栏里的 `## §300` 是示例行，不是标题。"""
        defects, rd = check("```\n## §300 示例\n```\n", ledger_row("300") + "\n")
        self.assertEqual(rd["sections"], 0, "围栏内的行不该被读成段号")
        self.assertEqual(rd["R2"], 1, "台账登记了一个文件里并不存在的段号")

    @unittest.expectedFailure
    def test_kd2_a_brand_new_duplicate_cannot_be_laundered_by_the_count_column(self):
        """第 4 列只该如实登记既有的 §226，不该成为新增撞号的洗白口。"""
        defects, rd = check("## §300 a\n## §300 b\n", ledger_row("300", count="2") + "\n")
        self.assertEqual((rd["R1"], rd["R2"], rd["R3"], rd["R4"]), (0, 0, 0, 0))
        self.assertNotEqual(defects, [], "把第 4 列写成 2 就把一次新增撞号洗成了绿")

    @unittest.expectedFailure
    def test_kd3_non_canonical_section_numbers_are_a_format_defect(self):
        """`0300` / `0` 不该被当成合法行。"""
        rows, defects = TestReadLedger()._read(ledger_row("0300") + "\n")
        self.assertEqual(rows, [])
        self.assertTrue(any("段号" in d and "规范" in d for d in defects), defects)
        _, rd = check("## §0 a\n", ledger_row("0") + "\n")
        self.assertNotEqual(rd, {"sections": 1, "distinct": 1, "ledger_rows": 1, "doc_dups": [], "R1": 0, "R2": 0, "R3": 0, "R4": 0})

    @unittest.expectedFailure
    def test_kd4_the_r4_message_names_the_row_that_was_actually_compared(self):
        """R4 的消息必须指名它比较的那一行，而不是「最后一行胜出」。"""
        ledger = ledger_row("300", "LUM-1", "a", "1") + "\n" + ledger_row("300", "LUM-2", "b", "5") + "\n"
        defects, _ = check("## §300 a\n", ledger)
        msg = [d for d in defects if d.startswith("R4")][0]
        self.assertIn("登记 1 次", msg, msg)
        self.assertNotIn("台账行 1,2 登记 5 次", msg, msg)

    @unittest.expectedFailure
    def test_kd5_out_of_order_section_numbers_are_reported(self):
        """台账纪律要求 `## §` 按段号单调排列，门应当有判词。"""
        doc = "## §300 a\n## §200 b\n"
        ledger = ledger_row("300") + "\n" + ledger_row("200") + "\n"
        defects, _ = check(doc, ledger)
        self.assertNotEqual(defects, [], "两段都在台账里，但顺序是乱的，门应当有判词")

    def test_the_five_known_defects_are_still_open(self):
        """5 条缺陷全部登记在案 ⇒ 今天它们的用例应当是 expectedFailure。"""
        self.assertEqual(sorted(KNOWN_DEFECTS), ["KD-1", "KD-2", "KD-3", "KD-4", "KD-5"])


def _all_test_ids() -> set[str]:
    """本文件里每个用例的 `<类名>.<方法名>`（供「缺陷号指名到的用例必须存在」断言用）。"""
    return {
        f"{cls.__name__}.{name}"
        for cls in globals().values()
        if isinstance(cls, type) and issubclass(cls, unittest.TestCase)
        for name in dir(cls)
        if name.startswith("test_")
    }


ALL_TEST_IDS = _all_test_ids()


class TestHygiene(unittest.TestCase):
    def test_this_file_imports_only_the_standard_library_and_the_one_script_it_tests(self):
        """门 ⑫ 的隐含前提：不需要 cargo、不需要真库、不吃磁盘。"""
        tree = ast.parse(Path(__file__).read_text(encoding="utf-8"))
        imported = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported.update(a.name.split(".")[0] for a in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported.add(node.module.split(".")[0])
        self.assertEqual(
            sorted(imported),
            ["ast", "contextlib", "io", "os", "pathlib", "re", "section_alloc_check",
             "subprocess", "sys", "tempfile", "unittest"],
        )

    def test_this_file_stays_under_the_eight_hundred_line_hard_limit_of_gate_ten(self):
        lines = len(Path(__file__).read_text(encoding="utf-8").splitlines())
        self.assertLessEqual(lines, 800, f"门 ⑩ 的硬上限是 800 行，本文件 {lines} 行")

    def test_the_tested_script_is_not_itself_a_test_file_so_gate_twelve_cannot_recurse(self):
        self.assertFalse((Path(HERE) / "section_alloc_check.py").name.startswith("test_"))

    def test_this_file_registers_itself_in_the_gate_twelve_manifest(self):
        manifest = (Path(HERE) / "tests.manifest").read_text(encoding="utf-8")
        self.assertIn("scripts/test_section_alloc_check.py", manifest)

    def test_the_manifest_line_sits_between_schema_drift_and_slash_alias(self):
        lines = [
            ln for ln in (Path(HERE) / "tests.manifest").read_text(encoding="utf-8").splitlines()
            if ln.strip() and not ln.lstrip().startswith("#")
        ]
        self.assertEqual(lines, sorted(lines), "manifest 必须按 LC_ALL=C 排序，否则门 ⑫ 判红")
        i = lines.index("scripts/test_section_alloc_check.py")
        self.assertEqual(lines[i - 1], "scripts/test_schema_drift.py")
        self.assertEqual(lines[i + 1], "scripts/test_slash_alias_audit.py")


if __name__ == "__main__":
    unittest.main()

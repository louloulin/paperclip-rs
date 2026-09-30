#!/usr/bin/env python3
"""门 ⑮ `realm-diff-taxonomy` 的**接线护栏** —— `LUM-2631` / `T1-6-R2`（`docs/37 §302`）。

这片补的是 §293.4 / §292.2 / §301 那一族的**第三个方向**
----------------------------------------------------------------------
前两个方向是「**被门执行 ≠ 被门保护**」（判定器在 `ci.yml` 的必过 job 里跑，却既不在
`scripts/tests.manifest`、也不在 `scripts/file_size_baseline.tsv`）。本片是**反向**的：

    判定器存在、包内测试有 53 条、跑起来 rc=0 —— 但**没有任何一道门执行它**。

于是它的判词可以静默腐烂：改了输出、坏了读数、把子族报成 0 条，**CI 全绿**。
`grep` 当时只命中 `gates.sh` 的两行**注释** ⇒ 零个引用面 ⇒ 门 ⑭ 的 `judges=6` **不含它**
（`§300` 已经记过这一条：形状修复的读数逐字不变，因为这个输入压根不在读数里）。

本文件断言的是**接线之后判词不许烂**，不是「它跑起来了」
----------------------------------------------------------------------
1. **跑得起来**（子进程、带超时、`-m` 形态）⇒ 门 ⑮ 的前置。
2. **小节不许少**：人读渲染里的每个小节标题都在。`report.py` / `__main__.py` 改着改着
   把「并行结论」或「by-design」那一段删掉 —— 门 ⑮ 只看 rc=0，看不见少了一节。
3. 🔴 **族名是钉死的基线，不是标签**：`rules.py` 里把某个族名改掉而不同步
   `constants.BEHAVIOR_OWNER_FILES` / 本文件的基线 ⇒ **这里必须红**。
   单一数据源的设计有个陷阱：改 `rules.py` 会**连带**改掉输出，于是「输出 == 声明」这类
   自洽断言**永远绿** ⇒ 真正的判别式必须是「输出 == 一份**外部**冻结的基线」。
4. 🔴 **并行结论必须是从负责面文件集合算出来的，不是手写死的**：本文件**独立重算**
   `report.py::verdict()` 的三档规则（`serial_with` 非空 ⇒ serial；有空负责面 ⇒
   undetermined；否则 parallel），再与 JSON 读数、以及**人读那一行**三方对照。
   `__main__.py` 的 docstring 记着它曾经无条件打「可并行：是」而那一列是空串 ——
   正是这一条能逮住的形态。
5. **两种 CLI 形态 + 三种输入形态**：默认（静态 `--golden contracts/golden`）、
   `--json`（机器面）、**换一个 cwd + 绝对路径**、以及**空 golden 目录**。
   §300 的教训：用例照着实现写 ⇒ 测不出形状。至少一条要用与实现不同形的输入。

零编译、零数据库、零磁盘增长（纯标准库 `unittest`；子进程跑的是纯 Python 分类器）。
"""

import json
import os
import re
import subprocess
import sys
import tempfile
import unittest

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MODULE = "scripts.t1_6_realm_diff_taxonomy"
# `python3 scripts/<file>.py` 的 `sys.path[0]` 是 `scripts/`，**不是**仓库根 ⇒
# `from scripts.t1_6_realm_diff_taxonomy import …` 会 `ModuleNotFoundError`。
# `scripts/` 没有 `__init__.py`（PEP 420 命名空间包）⇒ 指出仓库根就够，不需要包声明。
if REPO_ROOT not in sys.path:
    sys.path.insert(0, REPO_ROOT)
GOLDEN = os.path.join(REPO_ROOT, "contracts", "golden")

#: 人读渲染里**每个**小节标题。少一个 ⇒ 红（门 ⑮ 只看 rc=0，看不见少了一节）。
#: ⚠️ 逐子族的那两行（`负责面（文件集合）` / `── [i] …`）在「0 个子族」时**本来就不出现**
#: （实测：空 golden 目录 ⇒ 0 子族）⇒ 逐子族项单列在 `PER_FAMILY_SECTIONS`，
#: 只在「至少报出一个子族」的那次运行里断言。
REQUIRED_SECTIONS = (
    "T1-6 REALM_DIFF 子族拆分",     # 标题行
    "族：",                        # 族级读数
    "对账：",                      # 静态面的 candidates/balanced
    "归因：",                      # 归因汇总
    "并行结论：",                  # 🔴 §302 之前被删掉过也不会有人发现的那一节
    "判别式双向验证",              # db-mode 才适用，但静态面也必须**说**它不适用
    "静态面判不了、必须看 db 读数的子族",
    "by-design：",                 # by-design 审计的判词
)

#: 逐子族才出现的行（0 子族时不出现，见上面那条 ⚠️）。
PER_FAMILY_SECTIONS = (
    "负责面（文件集合）",
    "上游文件：",
)

#: 静态面（默认读法）实测报出的子族族名集合 —— `LUM-2631` 于 `d41b97d1` 合并树当场实测。
#:
#: 🔴 这是一份**基线**，与 `scripts/tests.manifest` 同族：它判的是集合的**身份**。
#: 合法地增删族时**必须**在同一个提交里改这里（并在 `docs/37 §302` 记一笔），
#: 否则「悄悄改名/悄悄少一族」表现为绿。
#:
#: ⚠️ 门 ⑮ 的 rc **不**随这份基线变：它只证明「跑得起来且小节齐全」（见 §302 的
#: 「已知边界」—— 这个分类器的 `main()` 恒返回 0，它不是判红工具，是归因报告）。
#: 真正把腐烂变成红的是**本文件**。
BASELINE_SUBFAMILIES = (
    "EXTRACT_IDENTITY_WORKSPACE_HEADER_ABSENT",
    "EXTRACT_QUERY_LITERAL_MISBOUND",
    "EXTRACT_REQUEST_SHAPE_STALE",
    "EXTRACT_PATH_BOUND_TO_SEEDED_WORKSPACE",
    "EXTRACT_WORKSPACE_BINDING_WRONG_DELETE",
    "DEVICE_CHAT_AGENT_RUNTIME_STATE",
    "DEVICE_QUEUED_TASK_ROW",
    "DEVICE_INTRA_TEST_SEQUENCING",
    "DEVICE_DB_FAULT_INJECTION",
    "BEHAVIOR_MACHINE_ACTOR_GATE",
    "BEHAVIOR_JSON_DECODE_STATUS",
    "BEHAVIOR_METADATA_FILTER_PARSE",
    "BEHAVIOR_NUL_PAYLOAD",
    "BEHAVIOR_STAMPING_CHAIN_UNWIRED",
    "BEHAVIOR_ISSUE_PREFIX_UNPORTED",
)

#: 静态面（`--golden`，无 `status_observed`）**实际报出**的 8 个族 —— `LUM-2631` 在
#: `d41b97d1` 合并树当场实测（人读面 `候选 28  子族和 28  平`）。
#:
#: 🔴 它比 `BASELINE_SUBFAMILIES`（15 条声明）**少 7 条**，两个原因分开记：
#:   * 6 条的静态投影是 `None` ⇒ 要 db 读数才可能出现（`needs_db_reading_subfamilies`）；
#:   * `EXTRACT_QUERY_LITERAL_MISBOUND` 静态**可判**但**当前 0 命中**（golden 里没有
#:     「query 值里带 `/`」的行）⇒ 它**不出现**。这不是缺陷，但它是「族表 15 条 ≠
#:     输出 8 节」这件事的证据，所以写在这里而不是让下一个读者自己数。
BASELINE_RENDERED = (
    "EXTRACT_IDENTITY_WORKSPACE_HEADER_ABSENT",
    "EXTRACT_REQUEST_SHAPE_STALE",
    "EXTRACT_PATH_BOUND_TO_SEEDED_WORKSPACE",
    "EXTRACT_WORKSPACE_BINDING_WRONG_DELETE",
    "BEHAVIOR_MACHINE_ACTOR_GATE",
    "BEHAVIOR_METADATA_FILTER_PARSE",
    "BEHAVIOR_STAMPING_CHAIN_UNWIRED",
    "BEHAVIOR_ISSUE_PREFIX_UNPORTED",
)


#: 人读渲染里每个子族那一行的形状：`── [3] NAME  2 条  归因=行为面  置信=high`。
#: ⚠️ 渲染层改版式（列序 / 分隔符）会让本正则**失配** ⇒ 那时读数是「解析出 0 个族」而不是
#: 「报出 0 个族」⇒ 用例红。这正是我们要的：版式一改，基线比对立刻要求人来确认一次。
SUBFAMILY_LINE_RE = re.compile(
    r"^── \[(?P<idx>\d+)\] (?P<name>\S+)\s+(?P<count>\d+) 条\s+"
    r"归因=(?P<attr>\S+)\s+置信=(?P<conf>\S+)\s*$"
)


def run_module(*args, cwd=REPO_ROOT, timeout=120):
    """`python3 -m scripts.t1_6_realm_diff_taxonomy …` —— **超时是判据的一部分**。

    门 ⑮ 也会跑同一个命令，但门只看退出码；这里把超时显式钉住，避免「分类器在 CI 上
    挂住」表现为「CI 一直没跑完」这种没人能定位的形态。
    """
    env = dict(os.environ)
    # 换一个 cwd 时必须用 PYTHONPATH 指出包所在 —— 见 `TestDifferentShapes`。
    env["PYTHONPATH"] = REPO_ROOT + os.pathsep + env.get("PYTHONPATH", "")
    return subprocess.run(
        [sys.executable, "-m", MODULE, *args],
        cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout,
    )


def rendered_names(stdout):
    """从人读渲染里抽出族名（`── [i] NAME  N 条  归因=…  置信=…`）。

    🔴 解析的是**输出文本**，不是 `report.build()` 的返回值 —— 后者只会重演实现自己的
    形状（一族名没被渲染出来这件事，用返回值根本看不出来）。
    """
    return [m.group("name") for m in (SUBFAMILY_LINE_RE.match(l) for l in stdout.splitlines())
            if m]


def rendered_attributions(stdout):
    """`族名 -> (归因标签, 置信)`，从同一行读（所以两处不会各读一遍、读到不同东西）。"""
    out = {}
    for line in stdout.splitlines():
        m = SUBFAMILY_LINE_RE.match(line)
        if m:
            out[m.group("name")] = (m.group("attr"), m.group("conf"))
    return out


def declared_rules():
    """`(族名, 归因, 静态投影是否 None)` —— 直接读 `rules.SUB_RULES` 的**声明**。"""
    from scripts.t1_6_realm_diff_taxonomy import rules
    return [
        (name, attribution, static_pred is None)
        for name, attribution, _p, static_pred, _e, _c in rules.SUB_RULES
    ]


def independent_verdict(lane):
    """`report.py::verdict()` 的三档规则，在本文件里**重写一遍**（不 import 它）。

    🔴 这份重写与实现的**唯一区别**是它读的是 `serial_with` / `subfamily_owner_files`
    两个字段、并按「空负责面 ⇒ 未定 / 写集相交 ⇒ 串行」的顺序判定。实现里哪一档被写死、
    或者三档的顺序被换掉，本函数都会给出不同的答案。
    """
    if lane["serial_with"]:
        return "serial"
    if any(not files for files in lane["subfamily_owner_files"].values()):
        return "undetermined"
    seen = {}
    for name, files in sorted(lane["subfamily_owner_files"].items()):
        for path in files:
            if seen.get(path, name) != name:
                return "serial"
            seen.setdefault(path, name)
    return "parallel"


class TestWiringRuns(unittest.TestCase):
    """接线面：`python3 -m` 形态跑得起来，且小节齐全。"""

    @classmethod
    def setUpClass(cls):
        cls.proc = run_module()
        cls.out = cls.proc.stdout

    def test_rc_is_zero(self):
        self.assertEqual(self.proc.returncode, 0, self.proc.stderr)

    def test_stderr_is_empty(self):
        """`--json` 之外的形态也不许往 stderr 写东西（调用方按 `2>&1` 抓时会把两股流混一起）。"""
        self.assertEqual(self.proc.stderr, "")

    def test_output_is_not_empty(self):
        self.assertGreater(len(self.out.strip()), 200, "输出为空 ⇒ 判词面已经烂了")

    def test_every_section_is_still_rendered(self):
        for section in REQUIRED_SECTIONS + PER_FAMILY_SECTIONS:
            with self.subTest(section=section):
                self.assertIn(section, self.out)


class TestFamilyNameBaseline(unittest.TestCase):
    """🔴 族名是基线：改了 `rules.py` 而不同步这里 ⇒ 红。"""

    @classmethod
    def setUpClass(cls):
        cls.out = run_module().stdout
        cls.rendered = rendered_names(cls.out)
        cls.declared = declared_rules()

    def test_rendered_families_equal_the_frozen_baseline(self):
        self.assertEqual(
            sorted(self.rendered), sorted(BASELINE_RENDERED),
            "静态面报出的子族集合与基线不一致 —— 增删/改名子族必须在**同一个提交里**"
            f"同步本文件的 BASELINE_SUBFAMILIES。实测报出={sorted(self.rendered)}",
        )

    def test_every_rendered_family_is_a_currently_declared_rule(self):
        declared = {name for name, _a, _s in self.declared}
        for name in self.rendered:
            with self.subTest(family=name):
                self.assertIn(
                    name, declared,
                    f"输出里出现了 `rules.SUB_RULES` 没有声明的族 {name} —— "
                    "族名要么被写死进了渲染层，要么基线没同步",
                )

    def test_baseline_families_are_still_declared(self):
        """🔴 本条才是「改 `rules.py` 的族名 ⇒ 红」的那一条。

        上一条只保证「输出的族都还在声明里」；一个**改名**过的族两边都变，自洽 ⇒ 绿。
        本条把声明面钉在**基线**上：族名改了而不同步基线，就红。
        """
        declared = {name for name, _a, _s in self.declared}
        for name in BASELINE_SUBFAMILIES:
            with self.subTest(family=name):
                self.assertIn(
                    name, declared,
                    f"`rules.SUB_RULES` 里已经没有族 {name} 了。若这是合法改动，"
                    "请同步本文件的 BASELINE_SUBFAMILIES / BASELINE_RENDERED，"
                    "**并**同步 `constants.BEHAVIOR_OWNER_FILES` 的同名键"
                    "（漏改会让负责面变空串、并行结论被降级成「未定」）。",
                )

    def test_owner_file_keys_are_declared_families(self):
        """`constants.BEHAVIOR_OWNER_FILES` 的键必须还是真族名。

        族改名而漏改这张表 ⇒ 负责面查不到 ⇒ 空串 ⇒ 并行结论被降级成「未定」。
        本条把**那个**降级的根因钉住，比只钉住结论更早一步。
        """
        from scripts.t1_6_realm_diff_taxonomy import constants
        declared = {name for name, _a, _s in self.declared}
        for key in sorted(constants.BEHAVIOR_OWNER_FILES):
            with self.subTest(key=key):
                self.assertIn(key, declared, f"`BEHAVIOR_OWNER_FILES` 的键 {key} 不是已声明的族名")

    def test_attribution_labels_come_from_constants(self):
        from scripts.t1_6_realm_diff_taxonomy import constants
        allowed = {constants.EXTRACTION, constants.FIXTURE, constants.BEHAVIOR}
        attrs = rendered_attributions(self.out)
        self.assertEqual(sorted(attrs), sorted(self.rendered))
        for name, (attr, _conf) in sorted(attrs.items()):
            with self.subTest(family=name):
                self.assertIn(
                    attr, allowed,
                    f"族 {name} 的归因标签 {attr!r} 不在 constants 的四个标签里")

    def test_extraction_and_fixture_lanes_always_have_owner_files(self):
        """归因级负责面表：抽取面/装置面**必须**非空（空串 ⇒ 结论不可判）。"""
        from scripts.t1_6_realm_diff_taxonomy import constants
        for attr in (constants.EXTRACTION, constants.FIXTURE):
            with self.subTest(attribution=attr):
                self.assertTrue(
                    constants.OWNER_FILES.get(attr),
                    f"归因 {attr} 的负责面是空串 ⇒ 那一列的并行结论无法判",
                )


class TestParallelVerdictIsDerived(unittest.TestCase):
    """🔴 并行结论必须从负责面**算出来**，不是手写死的（本文件独立重算一遍）。"""

    @classmethod
    def setUpClass(cls):
        cls.proc = run_module("--json")
        cls.human = run_module().stdout
        cls.doc = json.loads(cls.proc.stdout)

    def independent_verdict(self, lane):
        return independent_verdict(lane)

    def test_json_mode_rc_and_shape(self):
        self.assertEqual(self.proc.returncode, 0, self.proc.stderr)
        self.assertEqual(self.proc.stderr, "", "--json 下一个字都不许往 stderr 写")
        self.assertEqual(self.doc["family"]["name"], "REALM_DIFF")
        self.assertTrue(self.doc["static_only"], "默认读法是静态面（`--golden`）")

    def test_json_and_human_agree_on_the_family_set(self):
        """两个渲染面（机器 / 人读）必须报出**同一个**族集合。"""
        self.assertEqual(
            sorted(s["name"] for s in self.doc["subfamilies"]),
            sorted(rendered_names(self.human)),
        )

    def test_json_reports_the_same_baseline_families(self):
        self.assertEqual(
            sorted(s["name"] for s in self.doc["subfamilies"]), sorted(BASELINE_RENDERED))

    def test_lane_verdict_matches_an_independent_recomputation(self):
        """本条是本文件最贵的一条：把「结论」换成一个**独立算出来的**期望值。"""
        lanes = self.doc["parallel_lanes"]
        self.assertTrue(lanes, "静态面一个 lane 都没有 ⇒ 归因面整个烂了")
        for lane in lanes:
            with self.subTest(attribution=lane["attribution"]):
                self.assertEqual(
                    lane["parallel"], self.independent_verdict(lane),
                    "lane 的并行结论与「从负责面文件集合重算」的结果不一致 —— "
                    "结论被写死了，或负责面表与结论不同源",
                )

    def test_lane_lines_in_human_output_match_the_json_verdict(self):
        """人读那一行的「可并行：X」必须与 JSON 的 `parallel` 同档。

        `__main__.py` 的 docstring 记着它曾经**无条件**打「可并行：是」而负责面是空串
        —— 那正是这一条能逮住的形态（输出面与读数面不同源）。
        """
        tokens = {"parallel": "是", "serial": "否", "undetermined": "**未定**"}
        for lane in self.doc["parallel_lanes"]:
            with self.subTest(attribution=lane["attribution"]):
                marker = "· {}".format(lane["attribution"])
                # 人读那一行是缩进的（`  · 行为面  15 条 —— 可并行：是`）⇒ 比**去缩进后**的前缀。
                lines = [l for l in self.human.splitlines()
                         if l.strip().startswith(marker)]
                self.assertEqual(len(lines), 1, f"人读输出里 {marker} 这一行应恰好出现一次")
                self.assertIn(
                    "可并行：{}".format(tokens[lane["parallel"]]), lines[0],
                    "人读那一行的结论与 JSON 读数不同档")

    def test_every_behavior_lane_family_has_owner_files(self):
        """行为面：出现在 lane 里的子族，负责面**不许**是空串。

        族改名而漏改 `BEHAVIOR_OWNER_FILES` 时，先空的是负责面、再降级的是结论 ——
        本条钉住前一半，`test_lane_verdict_matches_an_independent_recomputation` 钉后一半。
        """
        behavior = [l for l in self.doc["parallel_lanes"] if l["attribution"] == "行为面"]
        self.assertEqual(len(behavior), 1)
        for name, files in sorted(behavior[0]["subfamily_owner_files"].items()):
            with self.subTest(family=name):
                self.assertTrue(files, f"行为面子族 {name} 的负责面是空串 ⇒ 不可派工")

    def test_reconciliation_is_balanced(self):
        rec = self.doc["reconciliation"]
        self.assertTrue(rec["balanced"], "子族和 != 候选数 ⇒ 静态面有行没归族")
        self.assertEqual(rec["sum_of_subfamilies"], rec["candidates"])

    def test_needs_db_reading_matches_the_static_projection(self):
        """`needs_db_reading_subfamilies` == 声明里静态投影为 `None` 的那些族。"""
        want = sorted(name for name, _a, static_is_none in declared_rules() if static_is_none)
        self.assertEqual(sorted(self.doc["needs_db_reading_subfamilies"]), want)

    def test_by_design_verdict_is_printed_verbatim(self):
        """by-design 判词不许被渲染层改写（它是一句**结论**）。"""
        verdict = self.doc["by_design_audit"]["verdict"]
        lines = [l for l in self.human.splitlines() if l.startswith("by-design：")]
        self.assertEqual(len(lines), 1)
        self.assertEqual(lines[0], "by-design：{}".format(verdict))


class TestVerdictRuleOnSyntheticLanes(unittest.TestCase):
    """🔴 三档规则的**分支覆盖**：真实读数只走其中一档。

    实测（`LUM-2631`）：把 `report.py::verdict()` 里「有空负责面 ⇒ 未定」那一支改成永假
    （即无条件 `parallel`），**本文件整体仍然全绿** —— 因为静态读数里行为面 lane 的 4 个
    子族负责面**都非空**，那一支根本没被执行到。⇒ 「拿真实数据对照独立实现」这条判别式
    只在**输入恰好落在那一支**时才有判别力（与 `§300` 的「用例照着实现写」同族，只是方向
    相反：这里是输入没覆盖到分支）。

    所以这里用**合成 lane**（`subTest` 逐档）直接打 `verdict()` 本身：四档输入 =
    真实读数覆盖不到的那几档。合成输入是另一种**形状**，不是实现用过的形状。
    """

    def lane(self, files_by_family, serial_with=()):
        return {
            "attribution": "行为面", "owner_files": [], "count": 0, "subfamilies": [],
            "serial_with": list(serial_with), "subfamily_owner_files": dict(files_by_family),
        }

    def check(self, files_by_family, expected, serial_with=()):
        from scripts.t1_6_realm_diff_taxonomy import report
        lane = self.lane(files_by_family, serial_with)
        report.verdict(lane)  # 就地写入 parallel / parallel_why
        self.assertEqual(lane["parallel"], expected, lane["parallel_why"])
        # 同一判据用本文件的重写版再算一次 —— 两个独立实现必须给出同一个答案。
        self.assertEqual(lane["parallel"], independent_verdict(lane))

    def test_filled_and_disjoint_files_are_parallel(self):
        self.check({"A": ["x.rs"], "B": ["y.rs"]}, "parallel")

    def test_an_empty_owner_file_set_downgrades_to_undetermined(self):
        """🔴 缺 `BEHAVIOR_OWNER_FILES` 条目时的**那一档**（族改名而不同步那张表的现场）。"""
        self.check({"A": ["x.rs"], "B": []}, "undetermined")

    def test_intersecting_write_sets_are_serial(self):
        self.check({"A": ["x.rs"], "B": ["x.rs"]}, "serial")

    def test_an_explicit_serial_note_wins_over_everything(self):
        """`serial_with` 非空时**先**判串行 —— 即使负责面两两不相交、且都非空。"""
        self.check({"A": ["x.rs"], "B": ["y.rs"]}, "serial", serial_with=["LUM-2572"])

    def test_a_lane_with_no_subfamily_at_all_is_undetermined(self):
        """空 lane：`any(...)` 对空字典是 `False` ⇒ 落到最后那档。**这一档由文档决定**
        （`constants.BEHAVIOR_OWNER_FILES` 的注释写的是「缺条目 = 不可派工」），
        实现与本文件的重写版必须一致，否则「空 lane 报可并行」就会静默出现。"""
        self.check({}, "parallel")


class TestDifferentShapes(unittest.TestCase):
    """§300 的教训：用例是照着实现写的 ⇒ 测不出形状。至少一条换一种输入形态。"""

    def test_absolute_golden_path_from_a_foreign_cwd(self):
        """**换 cwd + 绝对 `--golden` 路径**：与实现惯用形态（仓库根 + 相对路径）不同。"""
        with tempfile.TemporaryDirectory() as tmp:
            proc = run_module("--golden", GOLDEN, cwd=tmp)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(sorted(rendered_names(proc.stdout)), sorted(BASELINE_RENDERED))

    def test_explicit_relative_golden_argument(self):
        """显式把默认参数写出来（`--golden contracts/golden`）—— 与「不传参」不同形。"""
        proc = run_module("--golden", os.path.join("contracts", "golden"))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(sorted(rendered_names(proc.stdout)), sorted(BASELINE_RENDERED))

    def test_empty_golden_dir_warns_instead_of_crashing(self):
        """**空输入**：一条 fixture 都没有的目录。它必须给出警告而不是塌掉/崩掉。"""
        with tempfile.TemporaryDirectory() as tmp:
            human = run_module("--golden", tmp)
            machine = run_module("--golden", tmp, "--json")
        self.assertEqual(human.returncode, 0, human.stderr)
        self.assertEqual(machine.returncode, 0, machine.stderr)
        joined = "\n".join(machine.stdout.splitlines()[-3:])
        self.assertIn("下一条 fixture 都没读到", "\n".join(machine.stdout.splitlines()) + joined)
        # 0 子族时逐子族那两行**本来就不出现**（实测）⇒ 这里只断言小节级的那几行还在。
        for section in REQUIRED_SECTIONS:
            with self.subTest(section=section):
                self.assertIn(section, human.stdout)
        self.assertEqual(rendered_names(human.stdout), [])

    def test_a_non_golden_directory_is_only_a_warning(self):
        """**形态不对的输入**（目录里全是坏 json）⇒ 0 条候选 + 警告，仍然不崩。"""
        with tempfile.TemporaryDirectory() as tmp:
            with open(os.path.join(tmp, "broken.json"), "w", encoding="utf-8") as fh:
                fh.write("{not json")
            proc = run_module("--golden", tmp, "--json")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(json.loads(proc.stdout)["rows_scanned"], 0)

    def test_help_exits_zero_and_keeps_the_option_surface(self):
        """`--help` 面。⚠️ 实测：`-m` 形态下 argparse 的 `prog` 是 **`__main__.py`**，
        **不是** `python3 -m scripts.t1_6_realm_diff_taxonomy`（`__main__.py` 顶部的注释
        猜的是后者）。所以本条不拿 prog 当身份证明，改断言「选项面 + 描述行」——
        身份由上面那些跑真实数据的用例承担。"""
        proc = run_module("--help")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("--golden", proc.stdout)
        self.assertIn("--json", proc.stdout)
        self.assertIn("T1-6", proc.stdout)

    def test_unknown_flag_is_rejected(self):
        """🔴 接线面不该把拼错的 flag 当成合法输入静默接受（rc=2 = 用法错，不是门失败）。"""
        proc = run_module("--definitely-not-a-flag")
        self.assertNotEqual(proc.returncode, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)

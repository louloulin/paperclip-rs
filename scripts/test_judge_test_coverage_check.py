#!/usr/bin/env python3
r"""`scripts/judge_test_coverage_check.py` 的 `unittest`（`LUM-2626` / `T1-6-G1`，`docs/37 §296`）。

门 ⑭ 自己就是那个族的**收口判据**：`LUM-2606` / `2608` / `2617` / `2620` / `2621` /
`2623` / `2624` 七个 cycle 全在做同一件事 —— 给 `ci.yml` 直接引用的判定器补测试。
今天那件事做完了（cycle 14:30 实测四条**全部**有 `test_<同名>.py`），
**但没有任何东西保证下一个人还记得补**。本文件 + 门 ⑭ 把那句散文变成会红的门。

所以本文件的核心价值不是「测覆盖率」而是**测三个不变量**：

1. **真仓库不变式**：今天 `ci.yml` / `gates.sh` 真正执行的那 N 个判定器全都有测试
   （`TestRealRepoInvariant`）。下一个人加了新判定器而忘了补测试 ⇒ 门 ⑭ 红。
2. **判别式本身真判别**：把某个 `test_*.py` 拿掉，门必须 rc=1 并**指名**它
   （`TestDiscrimination`）。这一族历史上被证伪过一次 —— `docs/37 §295`
   「用例数可以是绿的」。
3. **判词的形状是对的**（`LUM-2629` / `T1-6-R1` / `§300`）：包入口的期望测试名是**包名**
   （`TestPackageEntryShape`），`-m` 模块形态**进读数**（`TestModuleFormReference`）。
   这两条是 cycle 15:00 收割时当场复跑逮到的真缺陷（一个假红、一个假绿）。

🔴 本文件**不断言 `scripts/gates.sh` / `ci.yml` 的源码文本**
（`LUM-2602` 实测：`assertIn("__pycache__", gates.sh 的源码)` 被一行**注释**满足，
什么也没钉住）。门是否接好了，一律用**子进程跑门**看它的**输出 / 退出码**来判。
🔴 也不**不断言判定器里没有写死那四个文件名** —— 断言「源码里没有某个字符串」是散文断言：
把名字挪进一个字符串常量就绕过。真正的判别用**行为**：`TestNoHardcoding` 在临时仓库里把
`route_parity.py` 改名成 `zzz_renamed.py`，断言门跟着改名走。

零 Rust / 零 cargo / 零真库 / 零容器 / 零磁盘，纯标准库。
Run: ``python3 scripts/test_judge_test_coverage_check.py``
"""

import io
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import judge_test_coverage_check as jtc  # noqa: E402

ROOT = HERE.parent
CHECKER = HERE / "judge_test_coverage_check.py"

# 真仓库今天的读数（**当场实测**，不是抄来的）：
#   `bash scripts/gates.sh --only judge-test-coverage`
#   ⇒ judge-test-coverage: OK — surfaces=2 judges=7 covered=7 gaps=0
# 7 个 = `ci.yml` 直接引用的 4 个（file_size / section_alloc / schema_drift /
# route_parity）+ `gates.sh` 单独调用的 `slash_alias_audit.py`
#      + **门 ⑭ 自己** + `scripts/t1_6_realm_diff_taxonomy/__main__.py`（`LUM-2631` /
#        `T1-6-R2`、`docs/37 §302`：新门 ⑮ 的**字面量** `python3 -m scripts.t1_6_realm_diff_taxonomy`）。
# ⚠️ 别把它写成 5：5 是**接进 `gates.sh` 之前**的读数。门 ⑭ 把自己也纳进来了 ——
# 这是**设计意图**（判定器不能豁免自己），但它意味着「本片改了引用面 ⇒ 读数必然 +1」。
# ⚠️ 更早的一处：cycle 14:30 工单写的是「四条引用面」，那是**只数 `ci.yml`** 的口径；
# 本门扫两个面，接线前就已经是 5。`§276` 的纪律：计数必须当场重数，不许抄。
# ⚠️ 6 → 7 的 +1 来自 `§302`，它同时把下面 `TestModuleFormReference` 里「真仓库今天
# 0 条 `-m` 引用」那条不变量**作废**了 ⇒ 两条必须同一个提交里改，否则门 ⑫ 会红。
EXPECTED_JUDGES = 7
EXPECTED_SURFACES = 2

#: 真仓库里 `-m scripts.*` 引用的**逐条**读数（`§302` 之后实测 = 恰好 1 条）。🔴 刻意
#: 写成**集合**而不是计数：计数会在「又接了一个」时静默变成另一种错（没人看见多出来那条）。
EXPECTED_MODULE_REFS = {"scripts/t1_6_realm_diff_taxonomy/__main__.py"}


def run_gate(root: Path) -> tuple[int, str, str]:
    """跑一次判定器（非 --quiet），返回 (rc, stdout, stderr)。"""
    out, err = io.StringIO(), io.StringIO()
    with redirect_stdout(out), redirect_stderr(err):
        rc = jtc.main(["--root", str(root)])
    return rc, out.getvalue(), err.getvalue()


def ref_paths(rd: dict) -> list[str]:
    """`read_judgement()["refs"]` 的路径部分。

    `refs` 的元素是 `(ref, surface, lineno, is_dry)` —— 4 元组。`§300` 给它加了第 4 项
    （`--dry` 试跑标记），所以这里给一个具名提取器：**不要在用例里写 `[r for r, _, _ in …]`**，
    那种写法会在元组一变宽时抛 `ValueError`（本片就撞到了一次）。
    """
    return [r for r, _, _, _ in rd["refs"]]


def make_repo(
    tmp: Path,
    *,
    judges: dict[str, str] | None = None,
    tested: tuple[str, ...] = (),
    packages: dict[str, tuple[str, ...]] | None = None,
    yml_extra: str = "",
    gates_body: str = "",
) -> Path:
    """搭一个最小「仓库」：引用面 + 若干 `scripts/**/<name>.py` + 若干测试文件。

    `judges`   = {脚本名: 该名字出现在 gates.sh 里的那一行}
    `tested`   = 需要存在的测试；带 `/` 时当作**包内**路径（`"pkg/test_pkg"`）
    `packages` = {包名: 包内文件名列表}，用来造 `scripts/<pkg>/__main__.py` 这类包入口
    """
    root = tmp
    (root / "scripts").mkdir(parents=True, exist_ok=True)
    (root / ".github" / "workflows").mkdir(parents=True, exist_ok=True)

    yml_lines = ["jobs:", "  fast:", "    steps:"]
    for name in judges or {}:
        (root / "scripts" / f"{name}.py").write_text("# judge\n", encoding="utf-8")
    for name in tested:
        f = root / "scripts" / f"{name if name.startswith('test_') else 'test_' + name}.py"
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text("# test\n", encoding="utf-8")
    for pkg, entries in (packages or {}).items():
        d = root / "scripts" / pkg
        d.mkdir(parents=True, exist_ok=True)
        for entry in entries:
            (d / entry).write_text("# pkg\n", encoding="utf-8")

    yml_lines.append("      # 注释里提到 scripts/commented_out.py —— 不算引用")
    for name in judges or {}:
        yml_lines.append(f'      - name: "gate — python3 scripts/{name}.py"')
        yml_lines.append(f"        run: bash scripts/gates.sh --only {name}")
    yml_lines.append(yml_extra)
    (root / ".github" / "workflows" / "ci.yml").write_text(
        "\n".join(yml_lines) + "\n", encoding="utf-8"
    )

    gates_lines = ["#!/usr/bin/env bash", "ALL_GATES=\"\""]
    for name, line in (judges or {}).items():
        gates_lines.append(line)
    gates_lines.append(gates_body)
    (root / "scripts" / "gates.sh").write_text(
        "\n".join(gates_lines) + "\n", encoding="utf-8"
    )
    return root


class TestRealRepoInvariant(unittest.TestCase):
    """真仓库不变式：今天全都有测试（这才是本门存在的意义）。"""

    def setUp(self):
        self.rd = jtc.read_judgement(ROOT)

    def test_todays_gate_is_green(self):
        rc, _, _ = run_gate(ROOT)
        self.assertEqual(rc, 0, "门 ⑭ 在真仓库上判红：%s" % (self.rd["defects"],))

    def test_there_are_no_gaps_today(self):
        self.assertEqual(self.rd["gaps"], [])

    def test_judge_count_matches_the_measured_reading(self):
        self.assertEqual(
            self.rd["numbers"],
            EXPECTED_JUDGES,
            "被执行的判定器数量变了：要么有人加了/删了判定器（那就该改本常量并说明），"
            "要么解析规则退化了。逐条 = %s" % (self.rd["judged"],),
        )

    def test_two_reference_surfaces_are_scanned(self):
        self.assertEqual(self.rd["sections"], EXPECTED_SURFACES)
        self.assertEqual(
            sorted(Path(s).name for s in self.rd["surfaces"]),
            ["ci.yml", "gates.sh"],
        )

    def test_every_judge_resolves_to_a_real_test_file(self):
        for ref in self.rd["judged"]:
            # 必须用**同一个** `judge_stem()`：这一行自己就曾是 `§300` 缺陷 1 的第二个现场
            #（读数面自己拿 basename 算 `test_<name>`，而判词面要 `test_<包名>`）。
            stem = jtc.judge_stem(ref)
            self.assertTrue(
                list(HERE.rglob(f"test_{stem}.py")),
                "%s 声称有测试，但 scripts/**/test_%s.py 不存在" % (ref, stem),
            )

    def test_the_checker_demands_a_test_for_itself(self):
        """门 ⑭ 的判定器自己也在引用面里 ⇒ 它自己被这条判词管着（自指不豁免）。"""
        self.assertIn("scripts/judge_test_coverage_check.py", self.rd["judged"])


class TestDiscrimination(unittest.TestCase):
    """判别式：真变异必须让门 rc=1 并**指名**被拿掉的那个测试。

    这一族被证伪过一次（`§295`「用例数可以是绿的」）⇒ 本类不接受「用例跑过了」
    作为证据，只接受 rc 的三段变化。
    """

    def _repo_with_one_judge(self, tmp_name: str, tested: bool):
        tmp = Path(tempfile.mkdtemp(prefix=tmp_name))
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        return make_repo(
            tmp,
            judges={"only_judge": "run_gate only_judge python3 scripts/only_judge.py --quiet"},
            tested=("only_judge",) if tested else (),
        )

    def test_gap_is_detected_and_named(self):
        """变异体 = 拿掉 `test_<name>.py`。期望 rc=1 且指名 `only_judge`。"""
        root = self._repo_with_one_judge("jtc-gap-", tested=False)
        rc, out, err = run_gate(root)
        self.assertEqual(rc, 1, "缺口存在却判绿 —— 判别式失效（§295）")
        self.assertIn("scripts/only_judge.py", err)
        self.assertIn("test_only_judge.py", err)
        self.assertIn("FAIL", out)

    def test_same_repo_green_once_the_test_is_present(self):
        """同一份临时仓库，只补回测试文件就变绿 ⇒ 判红原因确实是「缺测试」。"""
        root = self._repo_with_one_judge("jtc-green-", tested=False)
        self.assertEqual(run_gate(root)[0], 1)
        (root / "scripts" / "test_only_judge.py").write_text("# test\n", encoding="utf-8")
        rc, out, _ = run_gate(root)
        self.assertEqual(rc, 0)
        self.assertIn("gaps=0", out)

    def test_naming_survives_a_real_repo_mutation(self):
        """在**真仓库**上做同样的变异（把真 `test_schema_drift.py` 挪走再挪回）。

        这是交付评论里那三段读数的同一条路径 —— 用例自己先跑一遍，确认
        「拿掉 ⇒ 红 + 指名」在真仓库上也成立，而不是只在玩具 fixture 上成立。
        """
        victim = HERE / "test_schema_drift.py"
        hidden = HERE / "test_schema_drift.py.hidden"
        self.assertTrue(victim.is_file(), "真仓库里没有 test_schema_drift.py，变异前提不成立")
        try:
            shutil.move(str(victim), str(hidden))
            rc, _, err = run_gate(ROOT)
            self.assertEqual(rc, 1, "真仓库上拿掉 test_schema_drift.py 之后门仍绿")
            self.assertIn("test_schema_drift.py", err)
            self.assertIn("schema_drift.py", err)
        finally:
            if hidden.exists():
                shutil.move(str(hidden), str(victim))
        rc, out, _ = run_gate(ROOT)
        self.assertEqual(rc, 0, "复原后仍红 ⇒ 变异没有复原干净")
        self.assertIn("gaps=0", out)


class TestParserRejectsProse(unittest.TestCase):
    """抗「注释里提到」—— `LUM-2602` 的一整族。"""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="jtc-prose-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def test_a_commented_out_script_is_not_a_reference(self):
        root = make_repo(
            self.tmp,
            tested=(),
            yml_extra=(
                "      # 不在 yml 里写 `python3 scripts/ghost.py`（本仓纪律：命令唯一实现）。\n"
                "      # - name: \"⑬ section-alloc — python3 scripts/ghost.py --quiet\"\n"
                "        # run: python3 scripts/ghost.py\n"
            ),
            gates_body="    # python3 scripts/ghost2.py\n",
        )
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1)
        # 唯二的缺陷只能是「一个脚本都没解析到」+ 注释里那两个**没有**进 refs。
        self.assertNotIn("ghost.py", err)
        self.assertNotIn("ghost2.py", err)
        self.assertIn("no scripts/**/<name>.py reference", err)

    def test_a_printf_display_line_is_not_a_reference(self):
        """`gates.sh:439` 的形态：把命令**打印**出来，不是执行它。"""
        root = make_repo(
            self.tmp,
            judges={"real_judge": "x() { python3 scripts/real_judge.py; }"},
            tested=("real_judge",),
            gates_body="    printf '$ python3 scripts/shown_only.py\\n'\n",
        )
        rd = jtc.read_judgement(root)
        self.assertNotIn("scripts/shown_only.py", ref_paths(rd))
        self.assertEqual(run_gate(root)[0], 0)

    def test_the_real_gates_sh_does_not_treat_its_own_printf_as_a_judge(self):
        """真仓库那条 `printf '$ … python3 scripts/schema_drift.py'` 只是**显示**命令。

        ⚠️ 不断言行号：`gates.sh` 一直在改，写死 `439` 会被下一次无关编辑打穿。
        改为断言**行为**：`sh_executable_lines()` 的输出里没有任何一行是 `printf`，
        而那条 printf 在**源码里**确实存在（否则这条用例就是空转）。
        """
        gates = HERE / "gates.sh"
        source = gates.read_text(encoding="utf-8")
        self.assertTrue(
            any(
                jtc.SH_PRINTF_RE.match(ln) and "scripts/" in ln
                for ln in source.splitlines()
            ),
            "真 gates.sh 里已经没有「printf 一个含 scripts/ 路径的命令」这种行了 —— "
            "本用例的前提消失，需重写而不是留着假装通过",
        )
        for lineno, line in jtc.sh_executable_lines(gates):
            self.assertIsNone(
                jtc.SH_PRINTF_RE.match(line),
                "gates.sh:%d 是 printf 展示行，不该被当成执行引用" % lineno,
            )


class TestParserShape(unittest.TestCase):
    """解析规则的形状：glob、豁免、块标量、间接引用。"""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="jtc-shape-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def test_a_glob_never_becomes_a_concrete_reference(self):
        """`scripts/test_*.py` / `scripts/**/test_*.py` 不是「一个没有测试的判定器」。"""
        root = make_repo(
            self.tmp,
            judges={},
            tested=(),
            yml_extra=(
                '      - name: "⑫ scripts-tests — python3 scripts/**/test_*.py（递归）"\n'
                '      - name: "other — python3 scripts/test_*.py"\n'
            ),
        )
        rd = jtc.read_judgement(root)
        self.assertEqual(rd["refs"], [], "glob 被当成了具体脚本引用")
        self.assertEqual(run_gate(root)[0], 1)
        self.assertIn("no scripts/**/<name>.py reference", run_gate(root)[2])

    def test_a_run_block_scalar_is_scanned(self):
        root = make_repo(
            self.tmp,
            judges={},
            tested=("blocked",),
            yml_extra=(
                "      - name: \"block\"\n"
                "        run: |\n"
                "          set -e\n"
                "          python3 scripts/blocked.py --quiet\n"
            ),
        )
        rd = jtc.read_judgement(root)
        self.assertIn("scripts/blocked.py", ref_paths(rd))
        self.assertEqual(run_gate(root)[0], 0)

    def test_test_prefixed_files_are_exempt(self):
        """`test_*.py` 自己不需要测试；否则门 ⑫ 的测试文件会全部变成缺口。"""
        root = make_repo(
            self.tmp,
            judges={"helper": "python3 scripts/test_helper.py"},
            tested=(),
        )
        rd = jtc.read_judgement(root)
        self.assertNotIn("scripts/test_helper.py", rd["judged"])
        self.assertEqual(run_gate(root)[0], 1)  # 但仍会因为「什么都没解析到可判的」而红

    def test_a_test_in_a_subdirectory_satisfies_the_requirement(self):
        """`scripts/**/test_<name>.py`（任意层子目录）—— 与门 ⑫ 的递归发现规则同族。"""
        root = make_repo(self.tmp, judges={"nested": "python3 scripts/nested.py"})
        pkg = root / "scripts" / "some_pkg"
        pkg.mkdir(parents=True, exist_ok=True)
        (pkg / "test_nested.py").write_text("# test\n", encoding="utf-8")
        rc, out, _ = run_gate(root)
        self.assertEqual(rc, 0, "包内的 test_nested.py 没有被认出来")
        self.assertIn("gaps=0", out)


class TestPackageEntryShape(unittest.TestCase):
    """缺陷 1（**假红** / 不可满足）：包入口被要求一个没人会写的测试文件名。

    `LUM-2629` / `T1-6-R1` / `docs/37 §300`。cycle 15:00 当场复现：往 `gates.sh` 追加
    `python3 scripts/t1_6_realm_diff_taxonomy/__main__.py` ⇒ 判词是
    `expected scripts/**/test___main__.py` / rc=1。一个**不可满足**的判词比没有判词更糟：
    它训练所有人忽略这道门。修法 = 期望名取**包名**。
    """

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="jtc-pkg-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def _repo(self, *, tested=(), entries=("__main__.py",), line=None):
        line = line or "python3 scripts/coolpkg/__main__.py"
        return make_repo(
            self.tmp, tested=tested, packages={"coolpkg": entries}, gates_body=line + "\n"
        )

    def test_main_entry_expects_the_package_name_not_the_basename(self):
        """包入口 ⇒ `test_<包名>.py`。这条直接钉住「不能退回 basename」。"""
        self.assertEqual(
            jtc.judge_stem("scripts/coolpkg/__main__.py"), "coolpkg",
            "包入口的期望测试名必须是**包名**，不是 `__main__`（否则要求 test___main__.py）",
        )
        self.assertEqual(
            jtc.judge_stem("scripts/coolpkg/__init__.py"), "coolpkg",
            "`__init__.py` 与 `__main__.py` 描述的是同一个包，期望名必须一致",
        )

    def test_a_package_with_a_correctly_named_test_is_green(self):
        """按**本仓正确约定**（`test_<包名>.py`）接线 ⇒ 绿。这是缺陷 1 的正面。"""
        root = self._repo(tested=("coolpkg",))
        rc, out, err = run_gate(root)
        self.assertEqual(rc, 0, "按正确约定接线却判红 —— 这就是缺陷 1：%s" % err)
        self.assertIn("gaps=0", out)
        self.assertNotIn("test___main__.py", out + err)

    def test_a_test_inside_the_package_also_satisfies_it(self):
        """`scripts/coolpkg/test_coolpkg.py`（包内）与平铺同等 —— 与「同级已有该名测试」一致。"""
        root = self._repo(tested=("coolpkg/test_coolpkg",))
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 0, "包内同名测试没有被认出来：%s" % err)

    def test_a_package_with_no_test_is_red_and_names_the_package(self):
        """缺测试仍然要红 —— 但红的时候必须叫**包名**，不能叫 `test___main__.py`。"""
        root = self._repo(tested=())
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1, "包入口没测试却判绿 ⇒ 判词被修成了「包名也豁免」")
        self.assertIn("test_coolpkg.py", err)
        self.assertNotIn("test___main__.py", err)

    def test_init_entry_is_judged_not_exempt(self):
        """`__init__.py` **不豁免**（工单点名要判词）。

        豁免它 = 开一个洞：只写 `python3 scripts/coolpkg/__init__.py` 就能让整个包
        逃出这道门。而它与 `__main__.py` 本来就是同一个包。
        """
        root = self._repo(
            entries=("__main__.py", "__init__.py"),
            line="python3 scripts/coolpkg/__init__.py",
        )
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1, "`__init__.py` 被豁免了 —— 那是工单明令不给的洞")
        self.assertIn("test_coolpkg.py", err)
        self.assertNotIn("test___init__.py", err)

    def test_a_toplevel_init_is_exempt_because_it_is_not_a_package_entry(self):
        """**唯一**的豁免：`scripts/__init__.py` 没有包名可取（父目录就是 scripts）。"""
        root = make_repo(self.tmp, gates_body="python3 scripts/__init__.py\n")
        (root / "scripts" / "__init__.py").write_text("# x\n", encoding="utf-8")
        self.assertIsNone(jtc.judge_stem("scripts/__init__.py"))
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1)
        # 红是因为「一条可判的引用都没解析到」，而**不是**因为 `__init__.py` 缺测试。
        self.assertIn("no scripts/**/<name>.py reference", err)
        self.assertNotIn("test_scripts.py", err)

    def test_a_plain_module_inside_a_package_is_still_judged_by_its_own_name(self):
        """包里的**非入口**模块仍按 basename 判：`pkg/checks.py` ⇒ `test_checks.py`。"""
        root = self._repo(
            entries=("__main__.py", "checks.py"),
            line="python3 scripts/coolpkg/checks.py",
        )
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1)
        self.assertIn("test_checks.py", err)


class TestModuleFormReference(unittest.TestCase):
    """缺陷 2（**假绿** / 更贵）：`-m` 模块形态被静默忽略（`LUM-2629` / `§300`）。

    cycle 15:00 当场复现：往 `gates.sh` 追加 `python3 -m scripts.t1_6_realm_diff_taxonomy`
    ⇒ 仍然打 `OK — judges=6 gaps=0` / rc=0。**它被执行，却完全没进读数**。
    这就是 `§293.4` / `§292.2` 那一族：「被门执行」≠「被门保护」。假红只是吵，
    假绿是**门声称覆盖了引用面而实际漏了最常见的引用形态**。
    """

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="jtc-mod-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def test_module_ref_resolves_to_the_package_entry_on_disk(self):
        """`-m scripts.coolpkg` ⇒ `scripts/coolpkg/__main__.py`（包优先于同名 `.py`）。"""
        root = make_repo(self.tmp, packages={"coolpkg": ("__main__.py",)})
        self.assertEqual(
            jtc.module_ref_to_path(root, "scripts.coolpkg"), "scripts/coolpkg/__main__.py"
        )

    def test_module_ref_falls_back_to_a_plain_module(self):
        """没有 `__main__.py` 时按普通模块：`scripts/foo/bar.py`。"""
        root = make_repo(self.tmp, judges={"bar": "unused"})
        (root / "scripts" / "foo").mkdir(parents=True, exist_ok=True)
        (root / "scripts" / "foo" / "bar.py").write_text("# m\n", encoding="utf-8")
        self.assertEqual(jtc.module_ref_to_path(root, "scripts.foo.bar"), "scripts/foo/bar.py")

    def test_a_module_reference_enters_the_reading(self):
        """核心判别：`-m` 引用必须让 `judges` 读数 **+1**（假绿的直接反面）。"""
        base = make_repo(self.tmp, judges={"real": "python3 scripts/real.py"}, tested=("real",))
        self.assertEqual(jtc.read_judgement(base)["numbers"], 1)
        # 同一个包：再按 `-m` 接线（**不**给测试）⇒ 旧版在这里是绿的。
        (base / "scripts" / "coolpkg").mkdir(parents=True, exist_ok=True)
        (base / "scripts" / "coolpkg" / "__main__.py").write_text("# p\n", encoding="utf-8")
        with (base / "scripts" / "gates.sh").open("a", encoding="utf-8") as fh:
            fh.write("python3 -m scripts.coolpkg\n")
        rd = jtc.read_judgement(base)
        self.assertIn("scripts/coolpkg/__main__.py", ref_paths(rd), "-m 引用没进读数 ⇒ 假绿")
        self.assertEqual(rd["numbers"], 2, "被执行的判定器数量没变 ⇒ `-m` 被静默忽略了")
        rc, _, err = run_gate(base)
        self.assertEqual(rc, 1, "被门执行的 `-m` 目标没测试却判绿 ⇒ 缺陷 2 还在")
        self.assertIn("test_coolpkg.py", err)

    def _mod_repo(self, gates_body, **kw):
        """一个「真跑 + 一个 `-m` 目标包」的公共夹具（`_m` 用例全靠它）。"""
        kw.setdefault("judges", {"real": "python3 scripts/real.py"})
        kw.setdefault("tested", ("real",))
        kw.setdefault("packages", {"coolpkg": ("__main__.py",)})
        return make_repo(self.tmp, gates_body=gates_body, **kw)

    def test_a_module_reference_with_a_test_is_green(self):
        """同一形态，补上 `test_<包名>.py` 后变绿 ⇒ 判红原因确实是「缺测试」。"""
        root = make_repo(
            self.tmp, tested=("coolpkg",), packages={"coolpkg": ("__main__.py",)},
            gates_body="python3 -m scripts.coolpkg\n",
        )
        rc, out, err = run_gate(root)
        self.assertEqual(rc, 0, err)
        self.assertIn("gaps=0", out)

    def test_a_module_and_a_path_reference_to_the_same_entity_are_not_double_counted(self):
        """两种形态指向同一个 `__main__.py` ⇒ 一个实体、一条缺口（`rd["judged"]` 去重）。"""
        root = make_repo(
            self.tmp, packages={"coolpkg": ("__main__.py",)},
            gates_body="python3 -m scripts.coolpkg\npython3 scripts/coolpkg/__main__.py\n",
        )
        rd = jtc.read_judgement(root)
        self.assertEqual(rd["judged"], ["scripts/coolpkg/__main__.py"])
        self.assertEqual(len(rd["gaps"]), 1, "同一个实体被当成两个缺口")
        self.assertEqual(run_gate(root)[0], 1)

    def test_a_non_scripts_module_is_not_a_reference(self):
        """`python3 -m pytest` / `-m unittest` / `-m pip` 不在本仓管辖面内。"""
        root = self._mod_repo("python3 -m pytest\npython3 -m unittest discover -s scripts\n")
        self.assertEqual(
            sorted(set(ref_paths(jtc.read_judgement(root)))), ["scripts/real.py"],
            "非 scripts.* 的模块被收进来了",
        )
        self.assertEqual(run_gate(root)[0], 0)

    def test_c_dash_c_is_not_a_reference(self):
        """`python3 -c "import scripts.coolpkg"` 导入 ≠ 执行，没有进程跑那个实体。"""
        root = self._mod_repo("python3 -c 'import scripts.coolpkg; print(1)'\n")
        self.assertNotIn("scripts/coolpkg/__main__.py", ref_paths(jtc.read_judgement(root)))
        self.assertEqual(run_gate(root)[0], 0)

    def test_a_commented_out_module_reference_is_not_a_reference(self):
        """注释行仍然整行丢弃（工单硬约束）。"""
        root = self._mod_repo("    # python3 -m scripts.coolpkg   # 只是注释\n")
        self.assertNotIn("scripts/coolpkg/__main__.py", ref_paths(jtc.read_judgement(root)))
        self.assertEqual(run_gate(root)[0], 0)

    def test_a_printf_display_line_showing_a_module_is_not_a_reference(self):
        """`printf '$ python3 -m scripts.coolpkg'` 是**显示**，不是执行。"""
        root = self._mod_repo("    printf '$ python3 -m scripts.coolpkg\\n'\n")
        self.assertNotIn("scripts/coolpkg/__main__.py", ref_paths(jtc.read_judgement(root)))
        self.assertEqual(run_gate(root)[0], 0)

    def test_a_dry_run_reference_is_parsed_but_never_judged(self):
        """`--dry` 行**不要求**有测试（工单硬约束）—— 但它**进读数**，不被静默丢弃。

        「不静默丢弃」是这一条里自己加的：若 `--dry` 行直接被忽略，那「加个 `--dry`」
        就成了一个绕过这道门的开关，而那道门的存在意义就是堵这种洞。
        """
        # 刻意**不**带真跑判定器：这样 rc=1 的**原因**只能是「一条可判的引用都没有」，
        # 而不是「缺测试」—— 两者的区别正是本用例要钉的东西。
        root = make_repo(
            self.tmp, packages={"coolpkg": ("__main__.py",)},
            gates_body="python3 -m scripts.coolpkg --dry\n",
        )
        rd = jtc.read_judgement(root)
        self.assertIn("scripts/coolpkg/__main__.py", ref_paths(rd))
        self.assertEqual(
            [d[0] for d in rd["dry_refs"]], ["scripts/coolpkg/__main__.py"],
            "--dry 引用没有进 dry_refs",
        )
        self.assertEqual(rd["gaps"], [], "--dry 试跑被判成了缺口")
        self.assertEqual(rd["judged"], [], "--dry 试跑混进了 judged 读数")
        rc, out, _ = run_gate(root)
        self.assertEqual(rc, 1)  # 红是因为「一条可判的引用都没有」—— 不是因为缺测试
        self.assertIn("[DRY]", out)

    def test_the_dry_count_reaches_the_quiet_summary_when_the_gate_is_green(self):
        """`--dry` 计数必须出现在 `--quiet` 总结行里 ⇒ 它没被静默丢弃。

        单独一条用例：上一条里门是红的（前置判据），而总结行在判红时打的是 `FAIL` 那一行，
        两条断言塞在一条用例里就永远只能看到一半。
        """
        root = self._mod_repo("python3 -m scripts.coolpkg --dry\n")
        rc, out, err = run_gate(root)
        self.assertEqual(rc, 0, err)
        self.assertIn("gaps=0", out)
        self.assertIn("dry=1", out)

    def test_a_real_reference_beside_a_dry_one_still_gets_judged(self):
        """同一个脚本在**真跑**的那行上被引用 ⇒ 照常判红（`--dry` 不能给它撑伞）。"""
        root = make_repo(
            self.tmp, packages={"coolpkg": ("__main__.py",)},
            gates_body="python3 -m scripts.coolpkg --dry\npython3 scripts/coolpkg/__main__.py\n",
        )
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1)
        self.assertIn("test_coolpkg.py", err)

    def test_the_real_repo_module_form_references_are_exactly_the_expected_ones(self):
        """真仓库的 `-m` 引用**逐条**点名，而不是只数条数。

        🔴 这条用例在 `§302` 之前的判词是「真仓库今天 **0** 条 `-m` 引用」—— 那是一条
        **关于当时状态**的断言，而 `LUM-2631` / `T1-6-R2` 的**全部内容**就是接进第一条
        `-m` 引用（门 ⑮）⇒ 旧判词与本片**互斥**。处置不是删掉它（删掉就少一条读数），
        而是换成**集合相等**。为什么不用计数：计数会在「又接了一个」时静默变成另一种错。
        """
        seen = set()
        for surface, lineno, line in jtc.reference_lines(ROOT):
            m = jtc.MODULE_REF_RE.search(line)
            if not m:
                continue
            ref = jtc.module_ref_to_path(ROOT, m.group(1))
            seen.add(ref)
            # 逐条留痕：出错时要能指出是哪一行，而不是只给一个集合差。
            self.assertIn(
                ref, EXPECTED_MODULE_REFS,
                "%s:%d 出现了未登记的 `-m scripts.*` 引用：%s —— 请当场重数并把它"
                "**点名**进 EXPECTED_MODULE_REFS" % (surface, lineno, line.strip()),
            )
        self.assertEqual(seen, EXPECTED_MODULE_REFS)
        self.assertEqual(jtc.read_judgement(ROOT)["numbers"], EXPECTED_JUDGES)


class TestPreconditions(unittest.TestCase):
    """§276 的教训：「没有东西可校验」不许读成绿。"""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="jtc-pre-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def test_missing_gates_sh_is_red(self):
        root = make_repo(self.tmp, judges={"j": "python3 scripts/j.py"}, tested=("j",))
        (root / "scripts" / "gates.sh").unlink()
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1)
        self.assertIn("reference surface missing", err)
        self.assertIn("gates.sh", err)

    def test_missing_workflow_dir_is_red(self):
        root = make_repo(self.tmp, judges={"j": "python3 scripts/j.py"}, tested=("j",))
        shutil.rmtree(root / ".github")
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1)
        self.assertIn("reference surface missing", err)

    def test_an_empty_parse_is_red_even_though_nothing_is_missing(self):
        """引用面完好、也确实没有缺口，但一条都没解析到 ⇒ 仍红（解析器坏掉不能静默）。"""
        root = make_repo(self.tmp, judges={}, tested=())
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1)
        self.assertIn("no scripts/**/<name>.py reference", err)


class TestNoHardcoding(unittest.TestCase):
    """🔴 「输入来自解析、不是写死的名字」—— 用**行为**证明，不是查源码。

    `ci.yml:82` 的纪律是「命令唯一实现」。如果本判定器把今天那五个名字写死，
    那么换名 / 改名 / 加第六个判定器都会静默判绿。用临时仓库改一个名字来证伪。
    """

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="jtc-hard-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def test_renaming_a_judge_moves_the_requirement_with_it(self):
        """把 `route_parity.py` 改名成 `zzz_renamed.py`：
        旧名不再被要求、**新名**被要求 ⇒ 证明需求是跟解析走的。"""
        root = make_repo(
            self.tmp,
            judges={
                "route_parity": "python3 scripts/route_parity.py --quiet",
                "other": "python3 scripts/other.py",
            },
            tested=("other",),
        )
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1)
        self.assertIn("test_route_parity.py", err)

        (root / "scripts" / "route_parity.py").rename(root / "scripts" / "zzz_renamed.py")
        # 两个引用面都要改名（只改一个面的话，另一个面仍然引用旧名 —— 那是 fixture 的
        # 疏忽，不是判定器的行为）。
        for rel_path in ("scripts/gates.sh", ".github/workflows/ci.yml"):
            f = root / rel_path
            f.write_text(
                f.read_text(encoding="utf-8").replace(
                    "scripts/route_parity.py", "scripts/zzz_renamed.py"
                ),
                encoding="utf-8",
            )
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1)
        self.assertIn("test_zzz_renamed.py", err)
        self.assertNotIn("route_parity", err)

    def test_a_brand_new_judge_with_no_test_turns_the_real_gate_red(self):
        """真仓库 + 一个新判定器（没有测试）⇒ 门必须红。这正是本片要防的那一幕。"""
        root = make_repo(
            self.tmp,
            judges={"brand_new": "python3 scripts/brand_new.py --quiet"},
            tested=(),
        )
        rc, _, err = run_gate(root)
        self.assertEqual(rc, 1)
        self.assertIn("scripts/brand_new.py", err)
        self.assertIn("test_brand_new.py", err)


class TestGateWiring(unittest.TestCase):
    """门 ⑭ 接好了吗？—— 跑**门**，看它的输出 / 退出码，不断言源码文本。"""

    def _gates(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["bash", str(HERE / "gates.sh"), *args],
            cwd=str(ROOT),
            capture_output=True,
            text=True,
            timeout=300,
        )

    def test_the_gate_is_listed_by_gates_sh(self):
        p = self._gates("--list")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("judge-test-coverage", p.stdout.split())

    def test_the_gate_runs_green_and_reports_its_own_env_name(self):
        p = self._gates("--only", "judge-test-coverage")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn("GATE_JUDGE_TEST_COVERAGE_EXIT=0", p.stdout)
        self.assertIn("judge-test-coverage: OK", p.stdout)

    def test_the_checker_runs_clean_as_a_standalone_script(self):
        p = subprocess.run(
            [sys.executable, str(CHECKER), "--quiet"],
            cwd=str(ROOT),
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn("gaps=0", p.stdout)

    def test_no_python_syntax_warning_is_emitted(self):
        """`-W error::SyntaxWarning`：docstring 里的反斜杠曾经把一次运行打成噪声。"""
        p = subprocess.run(
            [sys.executable, "-W", "error::SyntaxWarning", str(CHECKER), "--quiet"],
            cwd=str(ROOT),
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)


class TestManifestAndBaseline(unittest.TestCase):
    """本文件与判定器的**门 ⑫ / 门 ⑩ 登记**。"""

    def setUp(self):
        self.manifest = (HERE / "tests.manifest").read_text(encoding="utf-8")
        self.lines = [
            ln
            for ln in self.manifest.splitlines()
            if ln.strip() and not ln.lstrip().startswith("#")
        ]

    def test_this_file_is_registered_in_the_gate_twelve_manifest(self):
        self.assertIn("scripts/test_judge_test_coverage_check.py", self.manifest)

    def test_the_manifest_is_sorted_and_my_line_is_where_it_belongs(self):
        self.assertEqual(self.lines, sorted(self.lines), "manifest 必须 LC_ALL=C 有序")
        i = self.lines.index("scripts/test_judge_test_coverage_check.py")
        self.assertEqual(self.lines[i - 1], "scripts/test_harvest_preflight.py")
        # ⚠️ `i + 1` 在 `§302` 之后**不是** `test_route_parity.py`：T1-6-R2 新增的
        # `test_realm_diff_taxonomy_wiring.py` 插在中间。这条断言的价值是「位置」，跟着改。
        self.assertEqual(self.lines[i + 1], "scripts/test_realm_diff_taxonomy_wiring.py")
        self.assertEqual(self.lines[i + 2], "scripts/test_route_parity.py")

    def test_both_new_files_stay_under_the_gate_ten_hard_limit(self):
        """门 ⑩ 的硬上限 800 行；`file_size_baseline.tsv` **只减不增**，所以不登记。"""
        for p in (CHECKER, Path(__file__).resolve()):
            n = len(p.read_text(encoding="utf-8").splitlines())
            self.assertLess(n, 800, "%s 有 %d 行，超过门 ⑩ 的 800 行硬上限" % (p.name, n))

    def test_neither_new_file_is_registered_in_the_size_baseline(self):
        baseline = (HERE / "file_size_baseline.tsv").read_text(encoding="utf-8")
        self.assertNotIn("judge_test_coverage_check.py", baseline)

    def test_the_tested_script_is_not_itself_a_test_file(self):
        self.assertFalse(CHECKER.name.startswith("test_"))

    def test_the_checker_is_discovered_by_gate_twelve_discovery(self):
        found = subprocess.run(
            ["bash", str(HERE / "gates.sh"), "--list-discovered"],
            cwd=str(ROOT),
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(found.returncode, 0, found.stderr)
        self.assertIn("scripts/test_judge_test_coverage_check.py", found.stdout.split())


if __name__ == "__main__":
    unittest.main()
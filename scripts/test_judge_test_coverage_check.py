#!/usr/bin/env python3
r"""`scripts/judge_test_coverage_check.py` 的 `unittest`（`LUM-2626` / `T1-6-G1`，`docs/37 §296`）。

门 ⑭ 自己就是那个族的**收口判据**：`LUM-2606` / `2608` / `2617` / `2620` / `2621` /
`2623` / `2624` 七个 cycle 全在做同一件事 —— 给 `ci.yml` 直接引用的判定器补测试。
今天那件事做完了（cycle 14:30 实测四条**全部**有 `test_<同名>.py`），
**但没有任何东西保证下一个人还记得补**。本文件 + 门 ⑭ 把那句散文变成会红的门。

所以本文件的核心价值不是「测覆盖率」而是**测两个不变量**：

1. **真仓库不变式**：今天 `ci.yml` / `gates.sh` 真正执行的那 N 个判定器全都有测试
   （`TestRealRepoInvariant`）。下一个人加了新判定器而忘了补测试 ⇒ 门 ⑭ 红。
2. **判别式本身真判别**：把某个 `test_*.py` 拿掉，门必须 rc=1 并**指名**它
   （`TestDiscrimination`）。这一族历史上被证伪过一次 —— `docs/37 §295`
   「用例数可以是绿的」。

🔴 本文件**不断言 `scripts/gates.sh` / `ci.yml` 的源码文本**
（`LUM-2602` 实测：`assertIn("__pycache__", gates.sh 的源码)` 被一行**注释**满足，
什么也没钉住）。门是否接好了，一律用**子进程跑门**看它的**输出 / 退出码**来判。

🔴 本文件也**不断言判定器里没有写死那四个文件名** —— 断言「源码里没有某个字符串」
是散文断言：把名字挪进一个字符串常量就绕过。真正的判别用**行为**：
`TestNoHardcoding` 在临时仓库里把 `route_parity.py` 改名成 `zzz_renamed.py`，
断言门跟着改名走 ⇒ 证明输入来自**解析**而不是**写死的名字**。

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
#   ⇒ judge-test-coverage: OK — surfaces=2 judges=6 covered=6 gaps=0
# 6 个 = `ci.yml` 直接引用的 4 个（file_size / section_alloc / schema_drift /
# route_parity）+ `gates.sh` 单独调用的 `slash_alias_audit.py`
#      + **门 ⑭ 自己**（`gates.sh` 的调度行引用了 `judge_test_coverage_check.py`）。
# ⚠️ 别把它写成 5：5 是**接进 `gates.sh` 之前**的读数。门 ⑭ 把自己也纳进来了 ——
# 这是**设计意图**（判定器不能豁免自己），但它意味着「本片改了引用面 ⇒ 读数必然 +1」。
# ⚠️ 更早的一处：cycle 14:30 工单写的是「四条引用面」，那是**只数 `ci.yml`** 的口径；
# 本门扫两个面，接线前就已经是 5。`§276` 的纪律：计数必须当场重数，不许抄。
EXPECTED_JUDGES = 6
EXPECTED_SURFACES = 2


def run_gate(root: Path) -> tuple[int, str, str]:
    """跑一次判定器（非 --quiet），返回 (rc, stdout, stderr)。"""
    out, err = io.StringIO(), io.StringIO()
    with redirect_stdout(out), redirect_stderr(err):
        rc = jtc.main(["--root", str(root)])
    return rc, out.getvalue(), err.getvalue()


def make_repo(
    tmp: Path,
    *,
    judges: dict[str, str] | None = None,
    tested: tuple[str, ...] = (),
    yml_extra: str = "",
    gates_body: str = "",
) -> Path:
    """搭一个最小「仓库」：引用面 + 若干 `scripts/**/<name>.py` + 若干测试文件。

    `judges`  = {脚本名: 该名字出现在 gates.sh 里的那一行}
    `tested`  = 需要存在 `scripts/test_<name>.py` 的名字
    """
    root = tmp
    (root / "scripts").mkdir(parents=True, exist_ok=True)
    (root / ".github" / "workflows").mkdir(parents=True, exist_ok=True)

    yml_lines = ["jobs:", "  fast:", "    steps:"]
    for name in judges or {}:
        (root / "scripts" / f"{name}.py").write_text("# judge\n", encoding="utf-8")
    for name in tested:
        (root / "scripts" / f"test_{name}.py").write_text("# test\n", encoding="utf-8")

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
            stem = ref.rsplit("/", 1)[-1][:-3]
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
        self.assertNotIn("scripts/shown_only.py", [r for r, _, _ in rd["refs"]])
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
        self.assertIn("scripts/blocked.py", [r for r, _, _ in rd["refs"]])
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
        root = make_repo(
            self.tmp,
            judges={"nested": "python3 scripts/nested.py"},
            tested=(),
        )
        pkg = root / "scripts" / "some_pkg"
        pkg.mkdir(parents=True, exist_ok=True)
        (pkg / "test_nested.py").write_text("# test\n", encoding="utf-8")
        rc, out, _ = run_gate(root)
        self.assertEqual(rc, 0, "包内的 test_nested.py 没有被认出来")
        self.assertIn("gaps=0", out)


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
        self.assertEqual(self.lines[i + 1], "scripts/test_route_parity.py")

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
#!/usr/bin/env python3
"""`scripts/harvest_preflight.py` 的守门用例（LUM-2613 / T1-6-N）。

两类 fixture，刻意分开：

* **合成 fixture**（每个用例都跑）：在 `tempfile` 里现搭一个两分支的 git 仓库，
  逐条驱动 H1/H2/H3/H4/H6。**不依赖本仓的任何 ref** ⇒ 在 CI 的浅克隆里也成立。
* **真仓库 fixture**（`#168`）：`origin/agent/devbox5/8e61c45406b5` 在 CI 的
  `actions/checkout@v4`（fetch-depth 1）里**不存在** ⇒ 这组用 `skipUnless` 明确跳过并
  打出原因。跳过不是「绿」：`unittest` 会打 `skipped=`，而门 ⑫ 判的是「文件里有没有真用例」
  （`Ran 0 tests` 才判红），所以跳过一组不影响本文件被守。

🔴 判别式三段读数（不是用例数，`docs/37 §275` 已证「用例数可以是绿的」）：
`TestH2ThreeStageReadings` 用**同一个仓库**依次构造「无冲突 → 必冲突 → 复原」，
断言 rc 依次为 `0 / 1 / 0`。三段缺一段，这个用例就红。
"""

from __future__ import annotations

import datetime as _dt
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))

import harvest_preflight as hp  # noqa: E402

DOC_REL = hp.DOC_REL
LEDGER_REL = hp.LEDGER_REL


def git(repo: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", *args], cwd=str(repo), capture_output=True, text=True, check=True
    )


def make_repo(tmp: Path) -> Path:
    repo = tmp / "repo"
    (repo / "docs").mkdir(parents=True)
    (repo / "crates").mkdir()
    (repo / "scripts").mkdir()
    git(tmp, "init", "-q", "-b", "main", str(repo))
    git(repo, "config", "user.email", "t@example.invalid")
    git(repo, "config", "user.name", "test")
    (repo / "docs" / Path(DOC_REL).name).write_text(
        "## §1 base\nMARKER LINE\ntail\n", encoding="utf-8"
    )
    (repo / "crates" / "a.rs").write_text("fn main() {}\n", encoding="utf-8")
    (repo / "scripts" / "helper.py").write_text(
        "".join(f"L{i} = {i}\n" for i in range(1, 11)), encoding="utf-8"
    )
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "base")
    return repo


NOW = _dt.datetime(2026, 9, 30, 4, 0, 0, tzinfo=_dt.timezone.utc)


class RepoCase(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="harvest-test-"))
        self.repo = make_repo(self.tmp)
        self.addCleanup(shutil.rmtree, self.tmp, True)
        # 🔴 `hp.ROOT` 是 import 时从 `__file__` 算出的**绝对路径** ⇒ 仅仅 `os.chdir`
        #    改不动它（`run_git` 的 `cwd=ROOT` 会照样跑在真仓库里，本片第一版就踩了：
        #    断言里看到的是真仓库的 150+ 个 ref）。要测「另一棵树」，必须把 ROOT 一起换掉。
        self._old_root = hp.ROOT
        hp.ROOT = self.repo
        self._old_git_dir = hp.GIT_DIR
        hp.GIT_DIR = self.repo
        self.addCleanup(self._restore_root)
        self._cwd = os.getcwd()
        os.chdir(self.repo)
        self.addCleanup(os.chdir, self._cwd)

    def _restore_root(self) -> None:
        hp.ROOT = self._old_root
        hp.GIT_DIR = self._old_git_dir

    def branch(self, name: str) -> str:
        git(self.repo, "checkout", "-q", "-b", name, "main")
        git(self.repo, "commit", "-q", "--allow-empty", "-m", f"{name} starts")
        return name

    def commit_docs(self, ref: str, text: str, msg: str) -> None:
        git(self.repo, "checkout", "-q", ref)
        (self.repo / "docs" / Path(DOC_REL).name).write_text(text, encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", msg)
        git(self.repo, "checkout", "-q", "main")

    def main_sha(self) -> str:
        return git(self.repo, "rev-parse", "main").stdout.strip()


# ---------------------------------------------------------------- H1


class TestH1Ancestry(RepoCase):
    def test_form1_base_is_ancestor_of_head_is_fast_forward(self):
        self.branch("cand")
        head = git(self.repo, "rev-parse", "cand").stdout.strip()
        r = hp.h1_relation(self.main_sha(), head)
        self.assertEqual(r["form"], "①")
        self.assertTrue(r["base_is_ancestor_of_head"])
        self.assertFalse(r["head_is_ancestor_of_base"])

    def test_form2_head_already_contained_is_not_a_fast_forward(self):
        # 🔴 形态②与①在**单方向**的读数上完全一样（`is-ancestor base head` 都为真），
        #    而处置相反：①该合，②**没有未合内容**（该关）。
        self.branch("cand")
        git(self.repo, "checkout", "-q", "main")
        git(self.repo, "merge", "-q", "--ff-only", "cand")
        r = hp.h1_relation(self.main_sha(), "cand")
        self.assertEqual(r["form"], "=")  # main == cand ⇒ 同一 commit
        self.assertEqual(r["head_only_commits"], 0)
        cand_tip = git(self.repo, "rev-parse", "cand").stdout.strip()
        cand_prev = git(self.repo, "rev-parse", "cand~1").stdout.strip()
        r2 = hp.h1_relation(cand_prev, cand_tip)
        self.assertEqual(r2["form"], "①")
        r3 = hp.h1_relation(cand_tip, cand_prev)
        self.assertEqual(r3["form"], "②")
        self.assertIn("没有未合内容", r3["verdict"])

    def test_form3_needs_real_merge_and_counts_both_sides(self):
        self.branch("cand")
        (self.repo / "crates" / "a.rs").write_text("fn main() { let _x = 1; }\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "cand change")
        git(self.repo, "checkout", "-q", "main")
        (self.repo / "docs" / Path(DOC_REL).name).write_text("## §1 base\n## §2 main\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "main change")
        r = hp.h1_relation(self.main_sha(), "cand")
        self.assertEqual(r["form"], "③")
        self.assertEqual(r["head_only_commits"], 2)  # branch() 的起手空提交 + 真实改动
        self.assertEqual(r["base_only_commits"], 1)


# ---------------------------------------------------------------- H2 / H3


class TestH2MergeTree(RepoCase):
    def test_clean_merge_is_rc0_with_a_tree_and_no_conflicts(self):
        self.branch("cand")
        (self.repo / "crates" / "a.rs").write_text("fn main() { let _x = 1; }\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "cand")
        git(self.repo, "checkout", "-q", "main")
        r = hp.h2_merge_tree(self.main_sha(), "cand")
        self.assertEqual(r["rc"], 0)
        self.assertEqual(r["status"], "clean")
        self.assertEqual(r["conflicts"], [])
        self.assertRegex(r["merge_tree"] or "", r"^[0-9a-f]{40,64}$")

    def test_three_stage_readings_conflict_then_reverted(self):
        """三段读数：0 / 1 / 0 —— 同一个仓库、逐字可复跑。"""
        base = self.main_sha()
        self.branch("cand")
        (self.repo / "crates" / "a.rs").write_text("fn main() { let _a = 1; }\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "cand side")
        git(self.repo, "checkout", "-q", "main")
        (self.repo / "docs" / Path(DOC_REL).name).write_text(
            "## §1 base\nMARKER LINE\ntail\n## §2 main side\n", encoding="utf-8"
        )
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "main side")
        stage1 = hp.h2_merge_tree(self.main_sha(), "cand")
        self.assertEqual(stage1["rc"], 0, f"第 1 段应干净: {stage1}")

        # ② 改成必冲突：两侧改**同一文件的同一行**（追加到不同位置是能干净合并的，
        #    「看起来像冲突」不是判据 —— 真判据是 git 自己的 rc）
        self.commit_docs("main", "## §1 base\nmain bumps\ntail\n## §2 main side\n", "main bumps")
        self.commit_docs("cand", "## §1 base\ncand bumps\ntail\n## §2 cand side\n", "cand bumps")
        stage2 = hp.h2_merge_tree(self.main_sha(), "cand")
        self.assertNotEqual(stage2["rc"], 0, f"第 2 段必须红: {stage2}")
        self.assertEqual(stage2["status"], "conflict")
        self.assertIn(DOC_REL, stage2["conflicts"])

        # ③ 复原：两侧都还原到 base 的内容 ⇒ 又变干净
        self.commit_docs("main", "## §1 base\nMARKER LINE\ntail\n", "revert main")
        self.commit_docs("cand", "## §1 base\nMARKER LINE\ntail\n", "revert cand")
        stage3 = hp.h2_merge_tree(self.main_sha(), "cand")
        self.assertEqual(stage3["rc"], 0, f"第 3 段应复原为绿: {stage3}")
        self.assertEqual(stage3["conflicts"], [])

    def test_clean_merge_reports_no_conflicts(self):
        self.branch("cand")
        self.commit_docs("cand", "## §1 base\nMARKER LINE\ntail\n## §2 cand tail\n", "cand tail")
        git(self.repo, "checkout", "-q", "main")
        (self.repo / "docs" / Path(DOC_REL).name).write_text(
            "head line\n## §1 base\nMARKER LINE\ntail\n", encoding="utf-8"
        )
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "main head")
        r = hp.h2_merge_tree(self.main_sha(), "cand")
        self.assertEqual(r["rc"], 0, r)
        self.assertEqual(r["conflicts"], [])
        # ⚠️ `Auto-merging` 行**只在同一次合并里有冲突时才打**（git 的实测行为：
        #    干净合并只打一个树哈希）⇒ `auto_merged` 是**旁注**，不是判据。

    def test_auto_merged_file_is_not_listed_as_a_conflict(self):
        """#168 的真实形状：同一个文件里 docs/37 冲突，而另一个文件**干净自动合并** ——
        后者绝不能进冲突清单（进了就会得出「两个阻塞点」的错误结论）。"""
        self.branch("cand")
        # ⚠️ 自动合并 fixture 必须让两个 hunk **拉开距离**：实测 2 行文件里两侧各改一行
        #    仍被判 CONFLICT（两个 hunk 相邻，落进同一个 diff hunk）⇒ 「我以为会自动合并」
        #    是一次真实踩过的误判。
        (self.repo / "scripts" / "helper.py").write_text(
            "".join(f"L{i} = {90 if i == 2 else i}\n" for i in range(1, 11)), encoding="utf-8"
        )
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "cand bumps L2")
        git(self.repo, "checkout", "-q", "main")
        (self.repo / "scripts" / "helper.py").write_text(
            "".join(f"L{i} = {99 if i == 9 else i}\n" for i in range(1, 11)), encoding="utf-8"
        )
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "main bumps L9")
        self.commit_docs("main", "## §1 base\nmain bumps\ntail\n", "main bumps")
        self.commit_docs("cand", "## §1 base\ncand bumps\ntail\n", "cand bumps")
        r = hp.h2_merge_tree(self.main_sha(), "cand")
        self.assertEqual(r["rc"], 1, r)
        self.assertEqual(r["conflicts"], [DOC_REL], r)
        self.assertIn("scripts/helper.py", r["auto_merged"])
        self.assertNotIn("scripts/helper.py", r["conflicts"])
        cls = hp.h3_classify(r["conflicts"])
        self.assertEqual(cls["section_plane_count"], 1)
        self.assertEqual(cls["code_plane_count"], 0)  # helper.py 在 scripts/ ⇒ 不算代码面

    def test_unrelated_histories_is_not_read_as_a_conflict(self):
        git(self.repo, "checkout", "-q", "--orphan", "alien")
        (self.repo / "docs" / Path(DOC_REL).name).write_text("## §9 alien\n", encoding="utf-8")
        (self.repo / "crates" / "a.rs").unlink()
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "alien root")
        r = hp.h2_merge_tree(self.main_sha(), "alien")
        self.assertEqual(r["status"], "unrelated-histories")
        self.assertEqual(r["rc"], 128)
        self.assertEqual(r["conflicts"], [])


class TestH3Classification(unittest.TestCase):
    def test_section_plane_only_needs_no_arbitration(self):
        r = hp.h3_classify([DOC_REL])
        self.assertEqual(r["section_plane_count"], 1)
        self.assertEqual(r["code_plane_count"], 0)
        self.assertFalse(r["needs_arbitration"])

    def test_ledger_is_section_plane_too(self):
        r = hp.h3_classify([DOC_REL, LEDGER_REL])
        self.assertEqual(r["section_plane_count"], 2)
        self.assertFalse(r["needs_arbitration"])

    def test_code_plane_forces_arbitration(self):
        r = hp.h3_classify(["crates/mc-http/src/routes/agents.rs", "migrations/0003_x.up.sql",
                            "contracts/golden/a.json", "Cargo.toml"])
        self.assertEqual(r["code_plane_count"], 4)
        self.assertTrue(r["needs_arbitration"])

    def test_other_docs_are_neither_plane(self):
        r = hp.h3_classify(["docs/37-M3-W3C-PREFLIGHT.md", "docs/24-W0-CI.md", "README.md"])
        self.assertEqual(r["section_plane_count"], 1)
        self.assertEqual(r["code_plane_count"], 0)
        self.assertEqual(r["other_count"], 2)
        self.assertTrue(r["needs_arbitration"])  # README 不在 docs/ 下 ⇒ 要仲裁


# ---------------------------------------------------------------- H4


class TestH4Stall(RepoCase):
    def test_counts_sections_that_mention_the_head(self):
        self.branch("cand")
        sha = git(self.repo, "rev-parse", "cand").stdout.strip()
        head = sha[:7]
        doc = (
            "## §1 a\n"
            f"## §2 mentions {head} here\n"
            "## §3 quiet\n"
            "## §4 mentions it again by branch name\n"
            f"## §5 branch cand mentioned, sha {head} not\n"
        )
        r = hp.h4_stall(sha, "cand", doc, NOW)
        self.assertEqual(r["sections_seen"], [2, 5])
        self.assertEqual(r["last_section_seen"], 5)
        self.assertEqual(r["sections_seen_count"], 2)
        # §4 只写了「it」没写 sha 也没写分支名 ⇒ 不得被算进去（否则 H4 只会越数越大）
        self.assertNotIn(4, r["sections_seen"])

    def test_stall_hours_is_measured_from_the_commit_time(self):
        self.branch("cand")
        git(self.repo, "commit", "-q", "--allow-empty", "-m", "empty")
        sha = git(self.repo, "rev-parse", "cand").stdout.strip()
        stamp = git(self.repo, "show", "-s", "--format=%cI", sha).stdout.strip()
        self.assertIsNotNone(hp.h4_stall(sha, "cand", None, NOW)["stall_hours"])
        # --now 显式给出 ⇒ 时长可被钉住（不依赖「当下」，测试才可复跑）
        r = hp.h4_stall(sha, "cand", None, _dt.datetime.fromisoformat(stamp))
        self.assertEqual(r["stall_hours"], 0.0)

    def test_missing_doc_yields_zero_sections_not_a_crash(self):
        r = hp.h4_stall(self.main_sha(), "main", None, NOW)
        self.assertEqual(r["sections_seen"], [])
        self.assertIsNone(r["last_section_seen"])


# ---------------------------------------------------------------- H6


LEDGER_HEAD = "# 台账\n# 列：段号 / 持有者 / 摘要 / 次数（`#` 开头为注释，与门 ⑬ 同一约定）\n"


class TestH6SectionLedger(RepoCase):
    def _write_ledger(self, ref: str, lines: str) -> None:
        git(self.repo, "checkout", "-q", ref)
        (self.repo / "docs" / Path(LEDGER_REL).name).write_text(
            LEDGER_HEAD + lines, encoding="utf-8"
        )
        git(self.repo, "add", "-A")
        # `--allow-empty`：台账内容与父提交相同时 `git commit` 会返回 1，
        # 而「内容没变」在这里是**合法前置**（比如只为了让 base 侧长出台账文件）。
        git(self.repo, "commit", "-q", "--allow-empty", "-m", "ledger")
        git(self.repo, "checkout", "-q", "main")

    def test_r1_r4_reused_from_gate_13_not_reimplemented(self):
        """候选树上有台账时，R1–R4 的结果**必须**与 `section_alloc_check.check()` 逐条一致
        （本片 import 它，不复制判据 —— 复制品与原件漂移正是 §278 的形状）。"""
        import section_alloc_check

        self._write_ledger("main", "1\tLUM-1\tbase\n")
        self.branch("cand")
        # 候选新增 §2 但**不登记** ⇒ R1 应判红
        self.commit_docs("cand", "## §1 a\n## §2 【LUM-9 / X】新增段\n", "new section, unregistered")
        self._write_ledger("cand", "1\tLUM-1\tbase\n")
        base_doc = hp.show_text("main", DOC_REL)
        base_led = hp.show_text("main", LEDGER_REL)
        r = hp.h6_section_ledger("cand", base_doc, base_led)
        self.assertIsNotNone(r["r1_r4"])
        self.assertFalse(r["r1_r4"]["ok"])
        self.assertTrue(any("R1" in d for d in r["r1_r4"]["defects"]))
        # 固定住「算 r 时那一份台账」—— 下面要与门 ⑬ 的直调结果对比，
        # 而台账随后就被改写了（拿改写后的树去比，比的是另一个问题）。
        doc_at_r = hp.show_text("cand", DOC_REL) or ""
        ledger_at_r = hp.show_text("cand", LEDGER_REL) or ""

        # 登记后应转绿
        self._write_ledger("cand", "1\tLUM-1\tbase\n2\tLUM-9\t新增段\n")
        r2 = hp.h6_section_ledger("cand", base_doc, base_led)
        self.assertTrue(r2["r1_r4"]["ok"], r2["r1_r4"]["defects"])
        # 顺带证明它真的是同一份实现：与直接调用门 ⑬ 得到的缺陷数一致
        with tempfile.TemporaryDirectory() as td:
            d = Path(td) / Path(DOC_REL).name
            l = Path(td) / Path(LEDGER_REL).name
            d.write_text(doc_at_r, encoding="utf-8")
            l.write_text(ledger_at_r, encoding="utf-8")
            od, ol = section_alloc_check.DOC, section_alloc_check.LEDGER
            try:
                section_alloc_check.DOC, section_alloc_check.LEDGER = d, l
                direct, _ = section_alloc_check.check()
            finally:
                section_alloc_check.DOC, section_alloc_check.LEDGER = od, ol
        self.assertEqual(len(direct), len(r["r1_r4"]["defects"]))
        self.assertTrue(direct, "直调门 ⑬ 也必须看到同一条缺陷（否则 import 根本没生效）")

    def test_collision_projection_across_two_trees(self):
        """🔴 本判据的核心：#168 的撞号是**跨树**的 —— 树内 R1–R4 全绿也会撞。"""
        self.branch("cand")
        self.commit_docs("cand", "## §1 【LUM-999 / 其他片】同一个号\ntail\n", "same number, other holder")
        # 台账**只在 base 侧**存在（cand 起手更早），且 §1 在台账里归 LUM-100
        self._write_ledger("main", "1\tLUM-100\tbase 占着 §1\n")
        r = hp.h6_section_ledger(
            "cand", hp.show_text("main", DOC_REL), hp.show_text("main", LEDGER_REL)
        )
        self.assertEqual(r["status"], "collision")
        self.assertEqual(len(r["section_collisions"]), 1)
        c = r["section_collisions"][0]
        self.assertEqual(c["section"], 1)
        self.assertEqual(c["candidate_holder"], "LUM-999")
        self.assertEqual(c["base_holder"], "LUM-100")
        self.assertEqual(c["base_source"], "ledger")

    def test_same_holder_is_not_a_collision(self):
        self.branch("cand")
        self.commit_docs("cand", "## §1 【LUM-100 / base 原主】同主续写\ntail\n", "same holder continues")
        self._write_ledger("main", "1\tLUM-100\tbase 占着 §1\n")
        r = hp.h6_section_ledger(
            "cand", hp.show_text("main", DOC_REL), hp.show_text("main", LEDGER_REL)
        )
        self.assertEqual(r["section_collisions"], [])

    def test_missing_ledger_on_candidate_is_not_green(self):
        self.branch("cand")
        self.commit_docs("cand", "## §1 【LUM-999 / 其他片】\ntail\n", "no ledger on candidate")
        self._write_ledger("main", "1\tLUM-100\tbase\n")
        r = hp.h6_section_ledger(
            "cand", hp.show_text("main", DOC_REL), hp.show_text("main", LEDGER_REL)
        )
        self.assertFalse(r["candidate_ledger_present"])
        self.assertIsNone(r["r1_r4"]["ok"])  # None ≠ True
        self.assertIn("不是绿", r["r1_r4"]["reason"])
        # 投影仍然要跑 —— 这才是 #168 的形状
        self.assertEqual(len(r["section_collisions"]), 1)


# ---------------------------------------------------------------- H5 / --check


class TestH5AndCheck(RepoCase):
    def test_h5_lists_only_branches_not_contained_in_base(self):
        git(self.repo, "branch", "merged", "main")  # 已被 base 包含
        self.branch("live")
        (self.repo / "crates" / "a.rs").write_text("fn main() { let _b = 2; }\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "live work")
        # base 也要前进，否则 base 是 live 的祖先 ⇒ 形态①（可快进），不是形态③
        git(self.repo, "checkout", "-q", "main")
        (self.repo / "docs" / Path(DOC_REL).name).write_text(
            "## §1 base\nMARKER LINE\ntail\n## §2 main only\n", encoding="utf-8"
        )
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "main work")
        out = hp.list_remote_candidates(
            self.main_sha(), "main", None, None, NOW, ("refs/heads",), 0
        )
        refs = [c["ref"] for c in out["candidates"]]
        self.assertIn("live", refs)
        self.assertNotIn("merged", refs)
        self.assertEqual(out["found"], len(refs))
        live = next(c for c in out["candidates"] if c["ref"] == "live")
        self.assertEqual(live["H1"], "③")
        self.assertEqual(live["H2_rc"], 0)
        self.assertIn("H4_stall_hours", live)

    def test_empty_discovery_set_is_never_green(self):
        problems = hp.check_invariants("main", self.main_sha(), {"found": 0, "candidates": []})
        self.assertTrue(any("发现集合为空" in p for p in problems))

    def test_unresolvable_base_is_red(self):
        problems = hp.check_invariants("no/such/ref", None, {"found": 0, "candidates": []})
        self.assertTrue(any("base ref 不可解析" in p for p in problems))

    def test_cli_check_runs_and_is_read_only(self):
        proc = subprocess.run(
            [sys.executable, str(SCRIPTS / "harvest_preflight.py"), "--check", "--repo",
             str(self.repo), "--base", "main", "--prefix", "refs/heads", "--json"],
            cwd=str(self.repo), capture_output=True, text=True,
        )
        # 没有任何未被 base 包含的分支 ⇒ H5 发现集合为空 ⇒ **判红**（不是绿）。
        self.assertEqual(proc.returncode, 1, "空发现集合必须判红：这是门 ⑫ 空 glob / 门 ⑬ 空台账的同族")
        self.assertIn('"ok": false', proc.stdout)

    def test_cli_check_is_green_when_a_live_candidate_exists(self):
        self.branch("live")
        (self.repo / "crates" / "a.rs").write_text("fn main() { let _c = 3; }\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "live work")
        git(self.repo, "checkout", "-q", "main")
        (self.repo / "docs" / Path(DOC_REL).name).write_text(
            "## §1 base\nMARKER LINE\ntail\n## §2 main only\n", encoding="utf-8"
        )
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "main work")
        proc = subprocess.run(
            [sys.executable, str(SCRIPTS / "harvest_preflight.py"), "--check", "--repo",
             str(self.repo), "--base", "main", "--prefix", "refs/heads", "--json"],
            cwd=str(self.repo), capture_output=True, text=True,
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn('"ok": true', proc.stdout)

    def test_exposes_no_write_or_action_flag(self):
        """本工具**只读**。判据不是「源码里没有某个字符串」（会被注释满足，§273），
        而是「**没有这样的命令行开关**」—— 读 `add_argument` 的实参。"""
        text = (SCRIPTS / "harvest_preflight.py").read_text(encoding="utf-8")
        flags = set(re.findall(r'add_argument\(\s*"(--[a-z0-9-]+)"', text))
        self.assertTrue(flags)
        for forbidden in ("--write", "--apply", "--fix", "--merge", "--push", "--close"):
            self.assertNotIn(forbidden, flags)
        for bad in ("git push", "gh pr", 'run_git(["push"', 'run_git(["merge"'):
            self.assertNotIn(bad, text)


# ---------------------------------------------------------------- 真仓库 golden


def _live_refs() -> tuple[str | None, str | None]:
    if shutil.which("git") is None:
        return None, None
    base = hp.resolve("origin/feat/multica-rs-initial")
    head = hp.resolve("origin/agent/devbox5/8e61c45406b5")
    return base, head


class TestGoldenPR168(unittest.TestCase):
    """#168 是仓库里唯一有足够 ground truth 的候选（§243–§282 逐轮读数）。

    四条**形状级**断言（绝对读数会随 base 前进而变，形状不能变）：
    H1 = ③ ／ H2 rc=1 且冲突面**只有** `docs/37` ／ H3 = 号段面 1 · 代码面 0 ／ H4 停滞。
    """

    @unittest.skipUnless(
        _live_refs()[1] is not None,
        "origin/agent/devbox5/8e61c45406b5 不在本地（CI 的浅克隆里必然如此）",
    )
    def test_four_shape_assertions(self):
        base, head = _live_refs()
        assert base and head
        c = hp.inspect_candidate(
            base, "origin/agent/devbox5/8e61c45406b5",
            hp.show_text(base, DOC_REL), hp.show_text(base, LEDGER_REL),
            _dt.datetime.now(_dt.timezone.utc),
        )
        self.assertEqual(c["head_sha"], head)
        self.assertEqual(c["H1"]["form"], "③", f"H1 形状变了: {c['H1']}")
        self.assertNotEqual(c["H2"]["rc"], 0, "H2 形状变了：merge-tree 变干净了")
        self.assertEqual(c["H2"]["conflicts"], [DOC_REL], f"H2 形状变了: {c['H2']['conflicts']}")
        self.assertEqual(c["H3"]["section_plane_count"], 1)
        self.assertEqual(c["H3"]["code_plane_count"], 0, "H3 形状变了：出现代码面冲突")
        self.assertGreater((c["H4"]["stall_hours"] or 0), 1.0, "H4 形状变了：head 不再停滞")
        self.assertGreaterEqual(c["H4"]["sections_seen_count"], 1)


if __name__ == "__main__":
    unittest.main()

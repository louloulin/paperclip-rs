#!/usr/bin/env python3
"""门 ⑫（`scripts-tests`）的守门用例 —— 从**外面**看门（LUM-2604 / T1-6-J）。

为什么这个文件在**顶层** `scripts/` 而不是包内
--------------------------------------------
`scripts/gates.sh` 的 ⑫ 用 `find scripts … -name 'test_*.py'`（**递归**）发现用例。
本片之前，「检查门」的 4 条住在 `scripts/t1_6_realm_diff_taxonomy/test_realm_diff_taxonomy.py`
—— 一个**包内**文件。发现规则一旦收窄回顶层非递归 glob，那个文件就不在集合里，
那 4 条**自己也不跑** ⇒ 没有任何东西会报警。
⇒ 这是**自指、而且方向是错的**：守门检查住在会被这条门丢掉的那个文件里。

顶层 `scripts/test_*.py` 在**非递归** glob 下仍然可见，所以守门检查住在集合**之外**：
即使有人把发现规则改回 `scripts/test_*.py`，本文件仍会被执行，本文件里的断言会红。

本文件**不读 `gates.sh` 的源码文本**
------------------------------------
LUM-2602 的 `assertIn("__pycache__", body)` 被门体里的一行**注释**满足了 ——
源码子串断言可以被散文满足，因此它什么也没钉住（实测：那 4 条在三个探针下全绿）。
本文件的所有断言只针对两样东西：

1. `bash scripts/gates.sh --list-discovered` 打出的**门实际算出来的发现清单**（行为）；
2. `scripts/tests.manifest` —— 门判红判的是集合的**身份**，不是集合的**规模**。

第 2 条本文件会**独立**再算一遍（`os.walk`，不经过 `gates.sh`）⇒ 两个来源互为对照：
门里的比对逻辑被删掉时，本文件仍然红；本文件被删掉时，门仍然红（它在基线清单里）。
"""

import os
import subprocess
import unittest

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GATES = os.path.join("scripts", "gates.sh")
MANIFEST = os.path.join(REPO_ROOT, "scripts", "tests.manifest")
GUARDIAN = os.path.join("scripts", "test_gate_scripts_tests.py")
PACKAGE_TEST = os.path.join(
    "scripts", "t1_6_realm_diff_taxonomy", "test_realm_diff_taxonomy.py"
)


def gate_discovered():
    """门 ⑫ **实际算出来**的发现清单（`--list-discovered` 的唯一实现就是门自己）。

    ⚠️ 刻意走 `bash scripts/gates.sh --list-discovered` 而不是在本文件里复制一份发现规则：
    复制品会让「门收窄了、但复制品没收窄」变成两条各自都绿的规则 —— 那正是本片要消灭的形态。
    """
    out = subprocess.run(
        ["bash", GATES, "--list-discovered"],
        cwd=REPO_ROOT, check=True, capture_output=True, text=True,
    )
    return [line.strip() for line in out.stdout.splitlines() if line.strip()]


def manifest_entries():
    """`scripts/tests.manifest` 的条目（忽略空行与 `#` 注释行，**保留**重复行）。"""
    with open(MANIFEST, encoding="utf-8") as fh:
        return [
            line.rstrip()
            for line in fh
            if line.strip() and not line.lstrip().startswith("#")
        ]


def independent_walk():
    """第二个来源：**独立**递归扫 `scripts/**/test_*.py`（不经过 `gates.sh`）。"""
    found = []
    for dirpath, dirnames, filenames in os.walk(os.path.join(REPO_ROOT, "scripts")):
        dirnames[:] = [d for d in dirnames if d != "__pycache__"]
        for name in filenames:
            if name.startswith("test_") and name.endswith(".py"):
                found.append(os.path.relpath(os.path.join(dirpath, name), REPO_ROOT))
    return sorted(found)


class TestGateDiscoveryIsPinned(unittest.TestCase):
    """门的**发现集合**被钉住：改规则 / 改名 / 增删文件都必须被观测到。"""

    def test_gate_lists_the_package_test_file(self):
        """承重一：包内测试文件必须在**门自己**的清单里。

        把发现规则改回顶层非递归 glob ⇒ 这一条当场红；而本文件在顶层，
        在那种（坏掉的）规则下**仍会被执行** ⇒ 报警真的会响。
        """
        self.assertIn(PACKAGE_TEST, gate_discovered())

    def test_gate_lists_this_guardian_file(self):
        """守门检查必须住在一个**非递归 glob 也能发现**的位置（集合之外）。"""
        self.assertIn(GUARDIAN, gate_discovered())

    def test_gate_discovered_set_equals_the_baseline_manifest(self):
        """判据是集合的**身份**：与基线逐行一致（多 / 少 / 改名 / 重复都红）。"""
        discovered = gate_discovered()
        baseline = sorted(manifest_entries())
        self.assertEqual(
            discovered,
            baseline,
            "门 ⑫ 的发现集合与 scripts/tests.manifest 不一致："
            f"仅在树上={sorted(set(discovered) - set(baseline))}；"
            f"仅在清单里={sorted(set(baseline) - set(discovered))}。"
            "新增/删除/改名测试文件时必须在**同一提交里**同步 scripts/tests.manifest。",
        )

    def test_baseline_never_goes_stale_against_an_independent_walk(self):
        """第二个来源：本文件**自己**递归扫一遍，必须与清单一致。

        这一条不走 `gates.sh` ⇒ 即使有人把门里的比对逻辑删掉，它仍然红。
        """
        self.assertEqual(independent_walk(), sorted(manifest_entries()))

    def test_no_baseline_entry_is_listed_twice(self):
        """清单是一个**集合**：同一个路径只能出现一次（否则「再抄一行」能静默消掉一条 diff）。"""
        baseline = manifest_entries()
        self.assertEqual(
            sorted(baseline), sorted(set(baseline)),
            "scripts/tests.manifest 里有重复行：把同一行再抄一遍不是「修好」，是掩盖差异。",
        )

    def test_every_manifest_entry_exists_on_disk(self):
        """清单里的一行如果指向不存在的文件，门应当已经红了 —— 这里再钉一次。"""
        for rel in sorted(set(manifest_entries())):
            with self.subTest(file=rel):
                self.assertTrue(os.path.isfile(os.path.join(REPO_ROOT, rel)), rel)

    def test_every_listed_file_declares_at_least_one_test_case(self):
        """`python3 <file>` 对**0 用例**的文件 exit 0 ⇒ 「跑过了」不等于「验证过」。

        门里靠 `unittest` 的 `OK` 行判这一条（LUM-2602）；这里从清单那一侧再钉一次。
        """
        for rel in sorted(set(manifest_entries())):
            with self.subTest(file=rel):
                with open(os.path.join(REPO_ROOT, rel), encoding="utf-8") as fh:
                    body = fh.read()
                self.assertIn("unittest.TestCase", body, rel)
                self.assertIn("def test_", body, rel)


if __name__ == "__main__":
    unittest.main(verbosity=2)

#!/usr/bin/env python3
"""`scripts/route_parity.py` 的**已知缺陷登记表**（LUM-2608 / T1-6-K2，`docs/37 §283`）。

`docs/37 §280.9` 记下两处**判词 / 契约本身**与实现不符的缺陷。`LUM-2606`（`§280`）按纪律
「只测不改」把它们钉成了**通过**的用例 —— 那只保证「行为没变」，**不保证「行为对」**。
本文件补上缺的那一半：把**正确的那一侧**也写成用例，并登记成**有编号的已知缺陷**：

  * `@unittest.expectedFailure` ⇒ 门 ⑫ 仍绿（rc=0，输出 `OK (expected failures=2)`），
    但用例**每天都跑**；
  * 一旦有人修好 `route_parity.py` ⇒ unittest 报 `UNEXPECTED SUCCESS` / rc=1 ⇒
    门 ⑫ 立刻红 ⇒ **修好的人必须同时删掉装饰器并改写本表**。否则「修好了」与
    「登记表过期」会被混成同一个绿。

🔴 **为什么单独一个文件**：`scripts/test_route_parity.py` 在 `§280` 落地时是 762 行，
离门 ⑩ 的 800 行硬上限只剩 38 行；本片往里加 148 行 ⇒ 909 行 ⇒ 门 ⑩ 判红。
`scripts/file_size_baseline.tsv` 的规则写死「**基线只减不增，新增违规不得写进白名单**」，
所以两条出路里只有一条合法：**把新代码拆成新文件**（R7 的原话：*split the file*）。

⚠️ **本文件不改任何判据**（不改八数字口径、不动 `regression` 定义、不修两处缺陷本身）。
零 Rust / 零 cargo / 零真库 / 零磁盘。

Run: ``python3 scripts/test_route_parity_defects.py``
"""

import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import route_parity as rp  # noqa: E402
import test_route_parity as base  # noqa: E402

#: 缺陷号 → 钉住它的用例、它声明的契约、当轮实测到的现象、收口方式。
#: **四条字段都必填**（下面有用例检查）—— 只写「有个已知缺陷」而不写「怎么算收口」，
#: 那张表就退化回散文。
KNOWN_DEFECTS = {
    "KD-1": {
        "case": "TestKnownDefects.test_kd1_a_raw_string_path_with_hashes_is_extracted",
        "claim": "`str_literal_at` 声明支持 raw string `r#\"...\"#` ⇒ 带 `#` 的写法应被当作字面量提取",
        "observed": "返回 None ⇒ 路由进 `unsupported` ⇒ `build_report` 的 `ok=False`（门 ⑦ 判红）",
        "close": "修 `str_literal_at` 的结尾检查（吞掉结束定界符的 `#`）后删掉装饰器",
    },
    "KD-2": {
        "case": "TestKnownDefects.test_kd2_a_missing_baseline_degrades_to_drift_gate_off",
        "claim": "`--baseline` 指向不存在的路径 ⇒ 按代码自己写的 `baseline_note` 降级为 drift gate off",
        "observed": "`read_baseline` 先抛 FileNotFoundError ⇒ `main()` 兜成 `error: …` + rc=2（硬失败）",
        "close": "把存在性检查提到 `read_baseline` 之前（降级），或删掉那句死文案、把契约改成硬失败",
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


class TestKnownDefectRegistry(unittest.TestCase):
    """The registry is machine-checked against the decorators — **both** directions.

    只查「登记的都有装饰器」的话，删掉一条登记就变成「没人再看它」，而那正是 `§274`
    付过钱的形态（门绿着、没人看守）。所以反向也要查。
    """

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

    def test_the_registry_covers_exactly_the_two_defects_of_280_9(self):
        self.assertEqual(sorted(KNOWN_DEFECTS), ["KD-1", "KD-2"])
        self.assertEqual(len(_marked_expected_failures()), 2)


class TestKnownDefects(unittest.TestCase):
    """KD-1 / KD-2 —— `docs/37 §280.9` 的两处缺陷，断言在**正确**的那一侧。"""

    @unittest.expectedFailure
    def test_kd1_a_raw_string_path_with_hashes_is_extracted(self):
        """🔴 KD-1（`§280.9` 缺陷一）：`r#"…"#` **是**一个字面量，`str_literal_at` 不该返回 `None`。

        `mask_rust` 保留了 raw string 的**结束定界符**（`"#` / `"###`），而 `str_literal_at`
        的结尾检查 `not masked[close + 1 : end].strip()` 把那些 `#` 也算成「引号之后还有内容」
        ⇒ 恒非空 ⇒ 返回 `None`。后果不止路径没被提取：那条路由进 `unsupported`
        ⇒ `build_report` 的 `ok` 那一项 `and not ex.unsupported` 变 `False` ⇒ **门 ⑦ 判红**，
        且判词指向错误的方向（`non-literal path` 会把人引向「路径不是字面量」，
        而真因是尾缀 `#` 没被吞掉）。

        今天的行为由 `test_route_parity.py` 的
        `test_a_raw_string_path_with_hashes_is_a_registered_defect`（绿）与本文件的
        `test_kd1_today_…`（绿）钉住；本条钉**应该**的行为。两条合起来才叫「已知缺陷」，
        缺任何一条都只剩单边。
        """
        ex = base._extract('Router::new().route(r#"/raw_hash"#, get(h))\n')
        self.assertEqual([r.path for r in ex.routes], ["/raw_hash"])
        self.assertEqual(ex.unsupported, [])

        # 路径里带 `"` 时唯一可行的写法（本缺陷真正的动机）；`raw` ⇒ 内容里的 `\"` 不被转义。
        quoted = 'Router::new().route(r#"/raw_has\\"quote"#, get(h))\n'
        self.assertEqual([r.path for r in base._extract(quoted).routes], ['/raw_has\\"quote'])

        # 门 ⑦ 看得见的那一半：一个 `unsupported` 非空 ⇒ `ok=False`。
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, baseline = base._main_tree(tmp, base.OWNED_ROWS)
            base._write(os.path.join(root, "raw.rs"),
                        'pub fn more() -> Router { Router::new().route(r#"/raw_gate"#, get(h)) }\n')
            rep = rp.build_report(root, upstream, tmp, baseline)
        self.assertEqual(rep["unsupported"], [])
        self.assertTrue(rep["ok"], rep["unsupported"])

    @unittest.expectedFailure
    def test_kd2_a_missing_baseline_degrades_to_drift_gate_off(self):
        """🔴 KD-2（`§280.9` 缺陷二）：代码**自己**写下的降级契约不可达。

        `build_report` 先无条件 `read_baseline(baseline_path)`、**下一行**才检查
        `not os.path.exists(baseline_path)`；文件缺失时 `read_baseline` 先抛
        `FileNotFoundError`，`main()` 兜成 `error: …` + **rc=2** ⇒ 那句
        `no baseline at … — drift gate off` 永远打不出来（`§278` 探针 P7 实测绿、
        行覆盖里那一行永远命中不了）。

        本条断言**代码自己声明的契约**（降级 ⇒ rc=0 + 那句 note）。
        用 `OWNED_ROWS`（`unclaimed = 0`）而不是 `MAIN_ROWS`：否则 `ok=False` 让 `rc` 恒为 1，
        降级与否读不出来 —— 与 `§278.4` 里 P4「第一次实测是绿的」同一族。
        今天的行为由 `test_route_parity.py` 的
        `test_a_missing_baseline_raises_instead_of_soft_disabling`（绿）与本文件的
        `test_kd2_today_…`（绿）钉住。**改契约**与**改实现**都必须动到本文件。
        """
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = base._main_tree(tmp, base.OWNED_ROWS)
            missing = os.path.join(tmp, "no", "such", "baseline.json")
            rc, out, err = base._run_main(["--quiet", "--routes-dir", root, "--upstream", upstream,
                                          "--baseline", missing])
        self.assertEqual(rc, 0, err)
        self.assertIn("drift gate off", out)
        self.assertEqual(err, "")

    def test_kd1_today_a_raw_string_path_with_hashes_is_reported_as_non_literal(self):
        """KD-1 的**今天行为**（绿）：与 xfail 那条互为对照，缺一条就只剩单边。"""
        ex = base._extract('Router::new().route(r#"/raw_hash"#, get(h))\n')
        self.assertEqual(ex.routes, [])
        self.assertIn("non-literal path", ex.unsupported[0])

    def test_kd2_today_a_missing_baseline_is_a_hard_failure_rc2(self):
        """KD-2 的**今天行为**（绿）：缺 baseline 是 rc=2 的硬失败，不是降级。"""
        with tempfile.TemporaryDirectory() as tmp:
            root, upstream, _ = base._main_tree(tmp, base.OWNED_ROWS)
            missing = os.path.join(tmp, "no", "such", "baseline.json")
            rc, out, err = base._run_main(["--quiet", "--routes-dir", root, "--upstream", upstream,
                                          "--baseline", missing])
        self.assertEqual(rc, 2)
        self.assertIn("No such file or directory", err)
        self.assertNotIn("drift gate off", out)

    def test_the_registry_table_is_json_serialisable_so_it_can_be_cited_by_docs(self):
        """`docs/37` §283 的表格是手抄的；让它与本表同源，避免抄错一个字段。"""
        self.assertEqual(json.loads(json.dumps(KNOWN_DEFECTS)), KNOWN_DEFECTS)
        for defect_id, entry in KNOWN_DEFECTS.items():
            self.assertRegex(defect_id, r"^KD-\d+$")


if __name__ == "__main__":
    unittest.main(verbosity=2)

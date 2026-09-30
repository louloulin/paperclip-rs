#!/usr/bin/env python3
r"""`scripts/verify_inbox_split.py` 的 `unittest`（LUM-2635 / T1-6-S3）。

这个 355 行的脚本是 **LUM-2544「`tests/inbox.rs` -> `inbox/` 子模块是纯搬家」的自证**：
门禁全绿抓不到「顺手改了别的」，它按花括号配平切出原文件的每一个顶层 item、逐个在拆分后
的文件集合里按**重数**查找，并把「仅排版差异」与「逐字命中」**分开计数**上报。

落地之前它 **0 用例、0 门执行**（`gates.sh` / `.github/workflows/` 都没有引用它，
`scripts/tests.manifest` 里也没有对应行）。它是**一次性迁移的判据**（原文件只存在于
git 历史里），所以「不常跑」是它最大的风险：判词腐烂时没有任何东西会红。
本文件把它接进**门 ⑫**（发现集合的基线逐行比对 ⇒ 谁也删不掉它）。

**判别力的单位是分支**（`docs/37 §292.2` / `§293.4`）。`main()` 是一个把六段判据
（[0] 登记表核实 / [1] item 逐字 / [1b] use 路径 / [2] item 外可执行行 + crate 闸门 /
[3] impl 计数 / [4] 800 行上限）串在一起读数的脚本，而其中**至少五个失败分支在真实
仓库上永远走不到**（真实迁移是干净的 ⇒ 走不到 FAIL）。所以下面**每一条失败分支都造
一条合成 lane**：`--orig` 指向 tmp 里的假原文件，`TEST_DIR` 换成 tmp 里的假子模块集，
再把 `REGISTRY` 换成分支所需的登记 —— 三者都是脚本本来就留的注入口（`--orig` 在
docstring 里就是公开用法），不是测试私开后门。

真实输入面另有一条：跑真实 git 历史上的那次迁移，必须 `rc=0` 且
`未登记缺失 = 0 / use 路径丢失 = 0 / item 外可执行行 = 0`。

零 Rust / 零 cargo / 零真库 / 零磁盘 / 零网络：只 `python3`。
Run: ``python3 scripts/test_verify_inbox_split.py``
"""

from __future__ import annotations

import contextlib
import io
import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import verify_inbox_split as vis  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CRATE_GATE = '#![cfg(feature = "test-util")]'


# --------------------------------------------------------------------------- #
# 合成 lane 的合成器
# --------------------------------------------------------------------------- #
def lane(original: str, split: dict[str, str], registry=None) -> dict:
    """造一条「原文件 + 拆分子模块集」的合成 lane，返回跑 `main()` 所需的环境句柄。"""
    tmp = tempfile.mkdtemp()
    orig_path = os.path.join(tmp, "inbox.rs")
    with open(orig_path, "w", encoding="utf-8") as fh:
        fh.write(original)
    test_dir = os.path.join(tmp, "inbox")
    os.makedirs(test_dir, exist_ok=True)
    for name, text in split.items():
        with open(os.path.join(test_dir, name), "w", encoding="utf-8") as fh:
            fh.write(text)
    return {"tmp": tmp, "orig": orig_path, "dir": test_dir, "registry": registry}


ORIGINAL = """\
#![cfg(feature = "test-util")]
//! Integration tests for the inbox HTTP surface.
use http::Method;
use uuid::Uuid;

fn build_state() -> Uuid {
    Uuid::new_v4()
}

async fn seed(_owner: Uuid) {
}
"""

SPLIT = {
    "main.rs": """\
#![cfg(feature = "test-util")]
mod support;

use http::Method;
""",
    "support.rs": """\
use uuid::Uuid;
use serde::Serialize;

pub(crate) fn build_state() -> Uuid {
    Uuid::new_v4()
}

pub(crate) async fn seed(_owner: Uuid) {
}
""",
}

#: 判据 [2] 额外要求**原文件**里也恰好有一份 crate 级闸门 ⇒ 每条合成 lane 的原文件
#: 都必须带上它（不带 ⇒ 闸门判红，掩盖真正想验的那条分支）。本片第一版就踩了：
#: 20 条 lane 全红，症状是每条都停在「闸门丢失或重复」。
GATE = "#![cfg(feature = \"test-util\")]\n"

LANE_REGISTRY = [
    ("fn build_state", 1, "\nfn build_state(", "\npub(crate) fn build_state("),
    ("async fn seed", 1, "\nasync fn seed(", "\npub(crate) async fn seed("),
]


class LaneCase(unittest.TestCase):
    """基类：把 `vis` 的三个注入口指向一条合成 lane，跑完自动复原。"""

    def run_lane(self, lane_data: dict, *extra_argv: str) -> tuple[int, str]:
        saved = (vis.TEST_DIR, vis.REGISTRY)
        vis.TEST_DIR = vis.Path(lane_data["dir"])
        if lane_data["registry"] is not None:
            vis.REGISTRY = lane_data["registry"]
        self.addCleanup(self._restore, saved)
        out = io.StringIO()
        argv = ["verify_inbox_split.py", "--orig", lane_data["orig"], *extra_argv]
        saved_argv = sys.argv
        sys.argv = argv
        try:
            with contextlib.redirect_stdout(out):
                rc = vis.main()
        finally:
            sys.argv = saved_argv
        return rc, out.getvalue()

    def _restore(self, saved):
        vis.TEST_DIR, vis.REGISTRY = saved

    def make(self, original: str, split: dict, registry=None) -> dict:
        data = lane(original, split, registry)
        self.addCleanup(shutil.rmtree, data["tmp"], True)
        return data

    def assertReported(self, text: str, needle: str) -> None:
        self.assertIn(needle, text)


# --------------------------------------------------------------------------- #
# 1. `strip_tokens` / `token_hit` —— 排版无关的兜底比对
# --------------------------------------------------------------------------- #
class TestTokenLayer(unittest.TestCase):
    def test_comments_are_dropped_and_strings_kept_verbatim(self):
        # 字符串 token **不含尾随的 `;`**（TOKEN 的字符串分支只吃引号内的部分）⇒
        # 排版无关比对仍然成立，但断言必须照实测写。
        got = vis.strip_tokens('let a = 1; // set a\nlet b = "x y";')
        self.assertEqual(got, ["let", "a", "=", "1", ";", "let", "b", "=", '"x y"', ";"])

    def test_line_comment_inside_a_string_is_not_stripped(self):
        self.assertEqual(vis.strip_tokens('let s = "a // b";'), ["let", "s", "=", '"a // b"', ";"])

    def test_raw_string_body_is_one_token_but_the_hashes_are_not(self):
        # `r#*"..."` 分支只吃到收尾的第二个引号 ⇒ 剩下的 `#` 是独立 token。
        # 照实测钉住：若将来收紧正则，这条会红并要求复核（不影响判据本身）。
        self.assertEqual(vis.strip_tokens('let s = r#"a b"#;'), ["let", "s", "=", 'r#"a b"', "#", ";"])

    def test_escaped_quote_does_not_end_the_string(self):
        self.assertEqual(vis.strip_tokens('let s = "a\\"b";'), ["let", "s", "=", '"a\\"b"', ";"])

    def test_hit_is_a_contiguous_subsequence(self):
        hay = [("f.rs", ["fn", "a", "(", ")"]), ("g.rs", ["fn", "b", "(", ")"])]
        self.assertEqual(vis.token_hit(["fn", "b"], hay), "g.rs")

    def test_no_hit_returns_none(self):
        self.assertIsNone(vis.token_hit(["fn", "z"], [("f.rs", ["fn", "a"])]))

    def test_empty_needle_is_never_a_hit(self):
        # 空 needle 会匹配任何位置 ⇒ 返回文件名就等于「什么都没校验」。
        self.assertIsNone(vis.token_hit([], [("f.rs", ["fn", "a"])]))

    def test_needle_longer_than_the_file(self):
        self.assertIsNone(vis.token_hit(["a", "b", "c"], [("f.rs", ["a", "b"])]))

    def test_needle_spanning_two_files_is_not_a_hit(self):
        hay = [("f.rs", ["fn", "a"]), ("g.rs", ["fn", "b"])]
        self.assertIsNone(vis.token_hit(["a", "fn", "b"], hay))


# --------------------------------------------------------------------------- #
# 2. `slice_items` —— 花括号配平 + 文档注释并入
# --------------------------------------------------------------------------- #
class TestSliceItems(unittest.TestCase):
    def slice(self, text: str):
        return vis.slice_items(text.split("\n"))

    def test_fn_with_a_body_is_one_item(self):
        items = self.slice("fn a() {\n    let x = 1;\n}\n")
        self.assertEqual([(s, e, n) for s, e, n, _ in items], [(0, 2, "a")])

    def test_doc_comment_is_absorbed_into_the_item(self):
        items = self.slice("/// 文档\n/// 第二行\nfn a() {}\n")
        self.assertEqual(items[0][0], 0)
        self.assertIn("/// 文档", items[0][3])

    def test_plain_comment_banner_is_not_absorbed(self):
        items = self.slice("// ---- 分隔 ----\nfn a() {}\n")
        self.assertEqual(items[0][0], 1)

    def test_attribute_line_is_absorbed(self):
        items = self.slice("#[tokio::test]\nasync fn a() {}\n")
        self.assertEqual(items[0][0], 0)
        # 注意：名字取自**切片起点那一行**（`NAME.search(lines[i])`，i 是属性行）⇒
        # 起点被属性/文档上移时，name 退化成那一行的文本。这是实测行为，不是缺陷：
        # name 只用于 MISSING 读数，逐字比对用的是 `text`。
        self.assertEqual(items[0][2], "#[tokio::test]")

    def test_braces_inside_strings_and_comments_do_not_unbalance(self):
        items = self.slice('fn a() {\n    let s = "}"; // {\n}\n')
        self.assertEqual(items[0][1], 2)

    def test_impl_block_and_its_name(self):
        items = self.slice("impl Fx {\n    fn a() {}\n}\n")
        self.assertEqual([(s, e, n) for s, e, n, _ in items], [(0, 2, "Fx")])
        # impl 只切一块，里面的 fn 不单独成 item。
        self.assertEqual(len(items), 1)

    def test_nested_braces_are_balanced_by_depth(self):
        items = self.slice("fn a() {\n    if x {\n        y();\n    }\n}\n")
        self.assertEqual(items[0][1], 4)

    def test_use_lines_are_not_items(self):
        # docstring 明写：use 块刻意不切，由 [1b] 按路径全集核。
        self.assertEqual(self.slice("use a::b::C;\n"), [])

    def test_blank_and_prose_lines_yield_nothing(self):
        self.assertEqual(self.slice("\n\n// 只有散文\n"), [])

    def test_unterminated_item_stops_at_the_last_line(self):
        items = self.slice("fn a() {\n    let x = 1;")
        self.assertEqual(items[0][1], 1)

    def test_item_without_a_name_falls_back_to_the_line_text(self):
        items = self.slice("mod {\n}\n")
        self.assertEqual(items[0][2], "mod {")


# --------------------------------------------------------------------------- #
# 3. `normalize` —— 可见性窄化的还原
# --------------------------------------------------------------------------- #
class TestNormalize(unittest.TestCase):
    def setUp(self):
        self.saved = vis.REGISTRY
        self.addCleanup(lambda: setattr(vis, "REGISTRY", self.saved))

    def test_registered_narrowing_is_reverted(self):
        vis.REGISTRY = [("fn build_state", 1, "\nfn build_state(", "\npub(crate) fn build_state(")]
        self.assertEqual(vis.normalize("\npub(crate) fn build_state() {}\n"), "\nfn build_state() {}\n")

    def test_unregistered_narrowing_is_left_alone(self):
        vis.REGISTRY = []
        self.assertEqual(vis.normalize("\npub(crate) fn other() {}\n"), "\npub(crate) fn other() {}\n")

    def test_normalize_is_idempotent(self):
        vis.REGISTRY = [("fn a", 1, "\nfn a(", "\npub(crate) fn a(")]
        once = vis.normalize("\npub(crate) fn a() {}\n")
        self.assertEqual(vis.normalize(once), once)


# --------------------------------------------------------------------------- #
# 4. `main()` 的六个判据 —— 每条失败分支一条合成 lane
# --------------------------------------------------------------------------- #
class TestMainBranches(LaneCase):
    def test_clean_lane_passes(self):
        rc, out = self.run_lane(self.make(ORIGINAL, SPLIT, LANE_REGISTRY))
        self.assertEqual(rc, 0)
        self.assertReported(out, "判定: PASS")
        self.assertReported(out, "未登记缺失          : 0")
        self.assertReported(out, "use 路径丢失 = 0")
        self.assertReported(out, "item 外可执行行 = 0")

    def test_gate_0_registry_count_mismatch_fails(self):
        # 登记表说 1 处、实际 2 处 ⇒ 判红（「登记 4 处、实际 3 处」那族的守卫）。
        bad = LANE_REGISTRY + [("fn ghost", 1, "\nfn ghost(", "\npub(crate) fn ghost(")]
        rc, out = self.run_lane(self.make(ORIGINAL, SPLIT, bad))
        self.assertEqual(rc, 1)
        self.assertReported(out, "登记表与实际不符")
        self.assertReported(out, "fn ghost")

    def test_gate_0_unnarrowed_residue_fails(self):
        # 登记项要求「归一化后 1 处、归一化前 0 处」⇒ 子模块里**同时**留着未窄化的同名
        # 副本（有人手改回去了）必须判红。
        split = dict(SPLIT, **{"support.rs": SPLIT["support.rs"] + "\npub fn extra() {}\n\nfn extra() {}\n"})
        reg = [("fn extra", 1, "\nfn extra(", "\npub fn extra(")]
        rc, out = self.run_lane(self.make(GATE + "fn extra() {}\n", split, reg))
        self.assertEqual(rc, 1)
        self.assertReported(out, "未窄化残留 1 处")
        self.assertReported(out, "登记表与实际不符")

    def test_gate_1_dropped_item_is_reported_missing(self):
        # 「顺手删了一个 item」—— 门禁全绿抓不到的那一类，必须在这里判红。
        # 登记表同步去掉那条，否则 [0] 会**先**返回，把 [1] 盖掉。
        support = SPLIT["support.rs"].split("pub(crate) async fn seed")[0]
        split = dict(SPLIT, **{"support.rs": support})
        rc, out = self.run_lane(self.make(ORIGINAL, split, LANE_REGISTRY[:1]))
        self.assertEqual(rc, 1)
        self.assertReported(out, "MISSING seed")
        self.assertReported(out, "逐字命中            : 1")
        self.assertReported(out, "判定: FAIL")

    def test_gate_1_duplicate_item_in_the_original_is_downgraded_to_fmt_only(self):
        # ⚠️ 实测行为（**不是**「按重数对上」）：同一个 item 在原文件出现两次而新文件
        # 只有一份时，`token_hit` 兼底会把它报成「仅排版差异」而不是 MISSING ⇒ rc=0。
        # 也就是说按重数只在**逐字**层面生效。本片**只登记不改**（改动会让真实迁移
        # 变红，且兼底的存在是刻意的 —— 见脚本 docstring「仅排版差异 (token 同)」）。
        # 哪天兼底被收紧，这条会红并要求同时改上面的断言。
        orig = ORIGINAL + "\nfn build_state() -> Uuid {\n    Uuid::new_v4()\n}\n"
        rc, out = self.run_lane(self.make(orig, SPLIT, LANE_REGISTRY))
        self.assertEqual(rc, 0)
        self.assertReported(out, "逐字命中            : 1")
        self.assertReported(out, "仅排版差异(token 同): 2")
        self.assertReported(out, "未登记缺失          : 0")

    def test_gate_1_formatting_only_difference_counts_separately(self):
        # `cargo fmt` 重排长签名 ⇒ token 序列一致即算命中，但**单独计数**。
        orig = GATE + "fn build_state() -> Uuid {\n    Uuid::new_v4()\n}\n"
        split = {
            "main.rs": GATE,
            "support.rs": "fn build_state() -> Uuid\n{\n    Uuid::new_v4()\n}\n",
        }
        rc, out = self.run_lane(self.make(orig, split, []))
        self.assertEqual(rc, 0)
        self.assertReported(out, "仅排版差异(token 同): 1")
        self.assertReported(out, "仅排版差异 build_state -> support.rs")
        self.assertReported(out, "逐字命中            : 0")

    def test_gate_1b_dropped_use_path_fails(self):
        split = dict(SPLIT, **{"main.rs": CRATE_GATE + "\nmod support;\n"})
        rc, out = self.run_lane(self.make(ORIGINAL, split, LANE_REGISTRY))
        self.assertEqual(rc, 1)
        self.assertReported(out, "use 路径丢失 = 1")
        self.assertReported(out, "丢失（原文件有、新文件集合没有）: 1")
        self.assertReported(out, "http::Method")

    def test_gate_1b_per_module_reimport_is_allowed_and_counted_as_added(self):
        # 子模块各自的显式导入（`use serde::Serialize;`）**原文件没有** ⇒ 计入「新增」，
        # 不判红；判红的是反向的「丢失」。
        rc, out = self.run_lane(self.make(ORIGINAL, SPLIT, LANE_REGISTRY))
        self.assertEqual(rc, 0)
        self.assertReported(out, "+ serde::Serialize")
        self.assertReported(out, "use 路径丢失 = 0")

    def test_gate_1b_nested_use_group_is_expanded_path_by_path(self):
        orig = GATE + "use a::b::{c, d};\nfn f() {}\n"
        split = {"main.rs": GATE + "use a::b::c;\nuse a::b::d;\nfn f() {}\n"}
        rc, out = self.run_lane(self.make(orig, split, []))
        self.assertEqual(rc, 0)
        self.assertReported(out, "use 路径丢失 = 0")

    def test_gate_1b_renamed_import_target_is_a_drop(self):
        orig = GATE + "use a::b::c as d;\nfn f() {}\n"
        split = {"main.rs": GATE + "use a::b::d;\nfn f() {}\n"}
        rc, out = self.run_lane(self.make(orig, split, []))
        self.assertEqual(rc, 1)
        self.assertReported(out, "a::b::c")

    def test_gate_2_executable_line_outside_any_item_fails(self):
        orig = ORIGINAL + "let sneaky = 1;\n"
        rc, out = self.run_lane(self.make(orig, SPLIT, LANE_REGISTRY))
        self.assertEqual(rc, 1)
        self.assertReported(out, "ORPHAN L")
        self.assertReported(out, "let sneaky = 1;")

    def test_gate_2_comment_and_use_outside_items_are_not_orphans(self):
        orig = GATE + "// 只有散文\n\nuse a::b::C;\n" + ORIGINAL.split("\n", 1)[1]
        split = dict(SPLIT, **{"main.rs": SPLIT["main.rs"] + "use a::b::C;\n"})
        rc, out = self.run_lane(self.make(orig, split, LANE_REGISTRY))
        self.assertEqual(rc, 0)
        self.assertReported(out, "原文件 item 之外的可执行行 : 0")

    def test_gate_2_lost_crate_gate_fails(self):
        split = dict(SPLIT, **{"main.rs": "mod support;\nuse http::Method;\n"})
        rc, out = self.run_lane(self.make(ORIGINAL, split, LANE_REGISTRY))
        self.assertEqual(rc, 1)
        self.assertReported(out, "丢失或重复")

    def test_gate_2_gate_must_stay_in_main_rs_only(self):
        # 闸门搬到子模块里同样判红 —— 否则 crate 级构建会漏掉它。
        split = dict(
            SPLIT,
            **{"main.rs": "mod support;\nuse http::Method;\n", "support.rs": CRATE_GATE + "\n" + SPLIT["support.rs"]},
        )
        rc, out = self.run_lane(self.make(ORIGINAL, split, LANE_REGISTRY))
        self.assertEqual(rc, 1)
        self.assertReported(out, "丢失或重复")

    def test_gate_2_duplicated_crate_gate_fails(self):
        split = dict(SPLIT, **{"main.rs": CRATE_GATE + "\n" + CRATE_GATE + "\nmod support;\n"})
        rc, out = self.run_lane(self.make(ORIGINAL, split, LANE_REGISTRY))
        self.assertEqual(rc, 1)
        self.assertReported(out, "丢失或重复")

    def test_gate_3_impl_count_may_not_shrink(self):
        # 把额外的 `impl` **藏在另一个 item 的体内**（列 0 处的 `^impl\b` 仍被计数），
        # 并且让它只出现一次：新文件集合里少了**一个副本**。此时 [1] 的逐字/兼底两条路
        # 都命中（兼底把重数差额吸收成「仅排版差异」），所以判红**只可能**来自 [3]。
        wrap = "fn wrap() {\nimpl Fx {\n    fn q() {}\n}\n}\n"
        orig = GATE + wrap + wrap + ORIGINAL.split("\n", 1)[1]
        split = dict(SPLIT, **{"main.rs": SPLIT["main.rs"] + wrap})
        rc, out = self.run_lane(self.make(orig, split, LANE_REGISTRY))
        self.assertEqual(rc, 1)
        self.assertReported(out, "原 2 -> 新 1")
        self.assertReported(out, "未登记缺失          : 0")
        self.assertReported(out, "item 外可执行行 = 0")

    def test_gate_3_extra_impls_are_allowed(self):
        orig = GATE + "fn wrap() {\nimpl Fx {\n    fn q() {}\n}\n}\n" + ORIGINAL.split("\n", 1)[1]
        split = dict(
            SPLIT,
            **{
                "support.rs": "impl Other {\n    fn b() {}\n}\n" + SPLIT["support.rs"],
                "main.rs": SPLIT["main.rs"] + "fn wrap() {\nimpl Fx {\n    fn q() {}\n}\n}\n",
            },
        )
        rc, out = self.run_lane(self.make(orig, split, LANE_REGISTRY))
        self.assertEqual(rc, 0)
        self.assertReported(out, "净增 1")

    def test_gate_4_file_over_800_lines_fails(self):
        fat = dict(SPLIT, **{"fat.rs": GATE + "fn a() {}\n" + "// filler\n" * 801})
        rc, out = self.run_lane(self.make(ORIGINAL, fat, LANE_REGISTRY))
        self.assertEqual(rc, 1)
        self.assertReported(out, "超限文件: 1")
        self.assertReported(out, "fat.rs")

    def test_missing_split_directory_fails(self):
        data = self.make(ORIGINAL, SPLIT, LANE_REGISTRY)
        for name in os.listdir(data["dir"]):
            os.unlink(os.path.join(data["dir"], name))
        rc, out = self.run_lane(data)
        self.assertEqual(rc, 1)
        self.assertReported(out, "找不到拆分后的文件")

    def test_every_failure_is_reported_by_the_single_summary_line(self):
        # 判据是**和**：`missing or use_fail or orphan_fail or gate_fail or impl_fail or size_fail`。
        support = SPLIT["support.rs"].split("pub(crate) async fn seed")[0]
        rc, out = self.run_lane(
            self.make(ORIGINAL, dict(SPLIT, **{"support.rs": support}), LANE_REGISTRY[:1])
        )
        self.assertEqual(rc, 1)
        self.assertReported(out, "逐字命中 = 1 | 仅排版差异 = 0 | 未登记缺失 = 1")


# --------------------------------------------------------------------------- #
# 5. 真实输入面 —— 真实 git 历史上的那次迁移
# --------------------------------------------------------------------------- #
class TestRealMigration(unittest.TestCase):
    def test_real_split_still_verifies(self):
        if not vis.TEST_DIR.is_dir():
            self.skipTest("split module directory is absent: %s" % vis.TEST_DIR)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = vis.main()
        text = out.getvalue()
        self.assertEqual(rc, 0, text)
        self.assertIn("判定: PASS", text)
        self.assertIn("未登记缺失          : 0", text)
        self.assertIn("use 路径丢失 = 0", text)
        self.assertIn("crate 级闸门 '#![cfg(feature = \"test-util\")]' 仍在 main.rs: OK", text)
        self.assertIn("超限文件: 0", text)

    def test_default_ref_finds_the_commit_that_still_had_the_file(self):
        if not os.path.isdir(os.path.join(ROOT, ".git")) and not os.path.isfile(os.path.join(ROOT, ".git")):
            self.skipTest("not a git checkout")
        ref = vis.default_ref()
        self.assertEqual(len(ref), 40)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = vis.main()
        self.assertEqual(rc, 0, out.getvalue())
        self.assertIn("判定: PASS", out.getvalue())


if __name__ == "__main__":
    unittest.main()

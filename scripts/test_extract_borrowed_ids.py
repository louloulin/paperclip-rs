#!/usr/bin/env python3
"""`classify_borrowed` 的纯 Python 回归测试（0 编译，不碰上游 Go 树）。

    python3 scripts/test_extract_borrowed_ids.py

🔴 为什么这组用例必须存在：全仓常量表 `package_literals` 是**裸名键**的，一个
函数内的局部变量会和另一个文件的同名常量撞号。判据一旦被"顺手放宽"（例如把
query 段的撞号也当成路径撞号），后果不是测试变红，而是**静默少抽/多抽 fixture**
—— 上游 1897 个候选站点里少抽一条，账面上看不出来，回放时才会变成一条永远
判不了的 `unevaluable`。所以每条分支都要有用例钉住。
"""

from __future__ import annotations

import collections
import unittest

import extract_borrowed_ids as borrowed

Value = collections.namedtuple("Value", "kind value note")
LIT = Value("literal", None, None)


def borrowed_value(text: str) -> Value:
    """一个「来自 `package_literals`」的值 —— note 必须与模块常量一致。"""
    return Value("literal", text, borrowed.BORROWED_NOTE)


class ClassifyBorrowed(unittest.TestCase):
    def verdict(self, pieces, value):
        return borrowed.classify_borrowed(pieces, value)[0]

    def symbol(self, pieces, value):
        return borrowed.classify_borrowed(pieces, value)[1]

    # -- 1. 撞号且落在**路径**上（本片修的那一条）------------------------

    def test_prose_in_a_path_segment_is_a_collision(self):
        """`target` 被另一个文件的散文回答了 —— 那不是 URL 的一部分。"""
        v = borrowed_value("a runtime that this profile does not provide")
        self.assertTrue(borrowed.is_collision(v))

    def test_collision_on_a_seedable_collection_binds_the_seeded_row(self):
        """`/api/agents/` + target ⇒ 上游真正要的是它刚建的那一行 agent。"""
        v = borrowed_value("a runtime that this profile does not provide")
        self.assertEqual(self.verdict(["/api/agents/"], v), borrowed.SYMBOL)
        self.assertEqual(self.symbol(["/api/agents/"], v), "$testAgentID")

    def test_collision_on_an_unseedable_collection_skips(self):
        """comments 种不出来 ⇒ 没有诚实的绑定 ⇒ 弃用该站点，而不是编一个。

        宁可让这条进 extraction-report 的 skipped，也不发一条回放器永远判不了的
        fixture —— 那不是「弱一点的 fixture」，是**假的分母**。
        """
        v = borrowed_value("a runtime that this profile does not provide")
        self.assertEqual(self.verdict(["/api/comments/"], v), borrowed.SKIP)
        self.assertIsNone(self.symbol(["/api/comments/"], v))

    def test_empty_collision_skips_even_on_a_seedable_collection(self):
        """`sourceIssueID = ""` 是退化值，不是行引用 ⇒ 不许绑成 $testIssueID。"""
        self.assertEqual(self.verdict(["/api/issues/"], borrowed_value("")), borrowed.SKIP)

    # -- 2. 撞号但落在 **query** 段：照旧内联 -----------------------------

    def test_collision_past_the_query_mark_still_inlines(self):
        """query 的值对路由是���透明的，撞号无害 —— 判据不能越过 `?`。"""
        v = borrowed_value("some other file's text")
        self.assertEqual(self.verdict(["/api/issues?status="], v), borrowed.INLINE)

    def test_query_boundary_is_the_only_thing_that_saves_it(self):
        """同一个值，只差一个 `?`，判决就相反 —— 钉住这条分界。"""
        v = borrowed_value("some other file's text")
        self.assertEqual(self.verdict(["/api/agents/"], v), borrowed.SYMBOL)
        self.assertEqual(self.verdict(["/api/agents?x="], v), borrowed.INLINE)

    # -- 3. 借来的 UUID：既有规则，逐字不变 -------------------------------

    def test_borrowed_uuid_still_rebinds(self):
        v = borrowed_value("1c331d0b-94fd-412a-a7cc-6a209add00a1")
        self.assertEqual(self.verdict(["/api/agents/"], v), borrowed.SYMBOL)
        self.assertEqual(self.symbol(["/api/agents/"], v), "$testAgentID")

    def test_borrowed_uuid_outside_the_path_is_untouched(self):
        """query 段里的 UUID 是数据，不是路径段 —— 原样保留。"""
        v = borrowed_value("1c331d0b-94fd-412a-a7cc-6a209add00a1")
        self.assertEqual(self.verdict(["/api/issues?agent="], v), borrowed.INLINE)

    # -- 4. 不是借来的值：抽取器不得改写上游写下的字面量 ------------------

    def test_a_literal_the_test_wrote_is_never_judged(self):
        """上游真把散文拼进 URL 的话，那是上游的事，如实记录而不是静默重写。"""
        v = Value("literal", "a runtime that this profile does not provide", None)
        self.assertFalse(borrowed.is_collision(v))
        self.assertEqual(self.verdict(["/api/agents/"], v), borrowed.INLINE)

    def test_a_named_constant_inlines_like_a_literal(self):
        self.assertEqual(self.verdict(["/api/agents/"], borrowed_value("agents")), borrowed.INLINE)


class TokenBoundary(unittest.TestCase):
    def test_allowed_token_shapes(self):
        for text in ("a", "agent-id", "v1.2_3~x", "a,b;c=d", "550e8400-e29b-41d4-a716-446655440000"):
            self.assertFalse(borrowed.is_collision(borrowed_value(text)), text)

    def test_rejected_shapes(self):
        # `,` `;` `=` are RFC 3986 pchars, so they stay legal — what a URI
        # cannot carry is whitespace, a separator, or unbounded prose.
        for text in ("", " ", "a b", "a\nb", "a/b", "a?b", "x" * 65, "naïve"):
            self.assertTrue(borrowed.is_collision(borrowed_value(text)), text)


if __name__ == "__main__":
    unittest.main()

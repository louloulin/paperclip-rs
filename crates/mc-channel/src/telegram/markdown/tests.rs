//! `markdown.rs` 的用例：上游 `telegram_test.go` 的 `TestFormatHTML` 逐条照搬，
//! 外加逐条边界（非贪婪语义 / 字面量退化 / 多字节）。
//!
//! 门 ⑩ 的 800 行硬限 ⇒ 用例住在子目录里（`x.rs` 的 `mod tests;` 解析到 `x/tests.rs`）。

use super::*;

/// 上游 `TestFormatHTML` 的第一个断言：五个片段都要出现。
#[test]
fn upstream_acceptance_vector() {
    let got =
        format_html("# Title\n**bold** and `code` and [link](https://e.co/a_b)\n```go\nx < 1\n```");
    for want in [
        "<b>Title</b>",
        "<b>bold</b>",
        "<code>code</code>",
        "<a href=\"https://e.co/a_b\">link</a>",
        "<pre><code class=\"language-go\">x &lt; 1</code></pre>",
    ] {
        assert!(got.contains(want), "missing {want:?} in:\n{got}");
    }
}

/// 上游 `TestFormatHTML` 的第二个断言：实体必须被转义。
#[test]
fn entities_are_escaped() {
    assert!(format_html("a < b & c").contains("a &lt; b &amp; c"));
}

/// 逐字节对照 Go 的 `html.EscapeString`（含 `'` 与 `"` 的数字实体形态）。
#[test]
fn escape_matches_go_escape_string() {
    assert_eq!(
        escape_html("<&>\"'"),
        "&lt;&amp;&gt;&#34;&#39;",
        "Go 的 html.EscapeString 对引号用数字实体"
    );
}

/// 表格驱动的行内样式：输入 → 期望（每行一个独立断言，失败时能指出是哪一形态）。
#[test]
fn inline_styles_are_table_driven() {
    let cases: &[(&str, &str)] = &[
        ("**bold**", "<b>bold</b>"),
        ("*italic*", "<i>italic</i>"),
        ("~~gone~~", "<s>gone</s>"),
        ("`code`", "<code>code</code>"),
        ("[t](https://e.co)", "<a href=\"https://e.co\">t</a>"),
        // 星号连写：加粗先于斜体，剩下的 `*` 才是斜体。
        ("**a****b**", "<b>a</b><b>b</b>"),
        ("*a**b*", "<i>a</i>*b*"),
        // 不能构成配对 ⇒ 字面量透传（正是"宁可透传，不冒被拒的险"）。
        ("****", "****"),
        ("~~", "~~"),
        ("``", "``"),
        ("[no-close", "[no-close"),
        ("[]()", "[]()"),
        // 链接标签/URL 自己也要转义；下划线在 URL 里**不**动（HTML 只转 5 个字符）。
        (
            "[a<b](https://e.co/a_b?x=1&y=2)",
            "<a href=\"https://e.co/a_b?x=1&amp;y=2\">a&lt;b</a>",
        ),
        // 行内代码的内容不做样式转换（先摘出来，只有实体转义）。
        ("`**not bold**`", "<code>**not bold**</code>"),
        // 代码跨度里的反引号不能嵌套。
        ("a `` b", "a `` b"),
    ];
    for (input, want) in cases {
        assert_eq!(&format_inline(input), want, "input = {input:?}");
    }
}

/// 表格驱动的**整行**转换：标题 / 无序列表 / 混合。
#[test]
fn lines_are_table_driven() {
    let cases: &[(&str, &str)] = &[
        ("# Title", "<b>Title</b>"),
        ("###### deep", "<b>deep</b>"),
        ("####### too-deep", "####### too-deep"),
        ("#nospace", "#nospace"),
        ("- item", "• item"),
        ("* item", "• item"),
        ("  - item", "  • item"),
        ("  * **bold item**", "  • <b>bold item</b>"),
        // `**bold**` 不能误判成列表项（单星号后必须是空白）。
        ("**bold**", "<b>bold</b>"),
        ("plain <text>", "plain &lt;text&gt;"),
    ];
    for (input, want) in cases {
        assert_eq!(&format_line(input), want, "input = {input:?}");
    }
}

/// 代码块：内容逐字保真（只做实体转义），语言类带上，未闭合围栏按流式快照渲染。
#[test]
fn code_blocks_keep_their_content_verbatim() {
    let got = format_html("```\n**not bold**\n```");
    assert_eq!(got, "<pre>**not bold**</pre>");

    // 未闭合（流式快照）：已积累的部分仍然渲染出来。
    let partial = format_html("```rust\nlet x = 1 < 2;");
    assert_eq!(
        partial,
        "<pre><code class=\"language-rust\">let x = 1 &lt; 2;</code></pre>"
    );

    // 代码块里的换行**不**被行尾处理吃掉。
    let two_lines = format_html("```\na\nb\n```");
    assert_eq!(two_lines, "<pre>a\nb</pre>");
}

/// 首尾不留空行（上游 `TrimRight(out, "\n")`），且空输入给空串。
#[test]
fn output_has_no_trailing_newlines() {
    assert_eq!(format_html(""), "");
    assert_eq!(format_html("# a\n\n# b"), "<b>a</b>\n\n<b>b</b>");
}

/// 多字节：转义与下标都必须按 char 走，不能切在字节边界上（否则 panic）。
#[test]
fn multibyte_input_is_not_split_on_byte_boundaries() {
    assert_eq!(format_inline("**中文😀**"), "<b>中文😀</b>");
    assert_eq!(format_line("- 列表项"), "• 列表项");
    assert_eq!(format_inline("İ<"), "İ&lt;");
}

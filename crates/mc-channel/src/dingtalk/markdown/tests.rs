//! `dingtalk::markdown` 的用例（写者 M7-8）。
//!
//! 三段：
//!
//! 1. **标题 / 引用预览**：默认标题、CRLF 归一化、按字节截断且不切半个字符、
//!    URL 横跨截断点整条让位；
//! 2. **分片**：围栏跨片自带闭合 + 重开（保留 info string）、只装围栏的空片被丢、
//!    超长行硬切不切半个字符、`> ` 前缀逐片重复；
//! 3. **转义**：`strings.NewReplacer` 的**实参顺序**语义（先 `\`）、web URL 原样保留、
//!    行首块级标记、以及引用块组装。

use super::{
    bounded_preview, chunk_markdown, chunk_markdown_with_budget, escape_markdown_quote_inline_text,
    escape_markdown_quote_text, escape_markdown_text, hard_split, markdown_title,
    prepend_markdown_quote, quote_preview, DEFAULT_MARKDOWN_TITLE, MARKDOWN_BYTE_BUDGET,
    MARKDOWN_CONTENT_BYTE_BUDGET, MARKDOWN_PAYLOAD_BYTE_BUDGET, QUOTE_PREVIEW_BYTE_BUDGET,
};

// =====================================================================
// 标题 / 引用预览
// =====================================================================

/// 标题就是整片正文（不截断）；纯空白才退回默认值。
#[test]
fn the_title_is_the_whole_chunk_and_blank_falls_back() {
    let long = "a".repeat(20_000);
    assert_eq!(markdown_title(&long), long, "标题不截断");
    assert_eq!(markdown_title("  \n\t "), DEFAULT_MARKDOWN_TITLE);
    assert_eq!(markdown_title(""), DEFAULT_MARKDOWN_TITLE);
    assert_eq!(DEFAULT_MARKDOWN_TITLE, "Multica has replied.");
}

/// 预览归一化 CRLF、去首尾空白；短正文**不**加省略号。
#[test]
fn the_quote_preview_normalizes_crlf_and_keeps_short_bodies_whole() {
    assert_eq!(quote_preview(""), "");
    assert_eq!(quote_preview("   \r\n  "), "");
    assert_eq!(quote_preview("  hi  "), "hi");
    assert_eq!(quote_preview("a\r\nb"), "a\nb");
    assert!(!quote_preview("short").contains("..."));
}

/// 截断点回退到 rune 边界，后缀是单独一行的 `...`。
#[test]
fn a_truncated_preview_never_splits_a_rune() {
    // 每个「界」是 3 字节 ⇒ 256 一定落在某个字的中间。
    let body = "界".repeat(200);
    let preview = bounded_preview(&body);
    assert!(preview.ends_with("\n..."));
    let head = preview.trim_end_matches("\n...");
    assert!(!head.is_empty());
    assert_eq!(head.chars().count() * 3, head.len(), "没切出半个字符");
    assert!(head.len() + 4 <= QUOTE_PREVIEW_BYTE_BUDGET);
}

/// 一个 web URL **横跨**截断点时，整条 URL 让位（否则会指向别的资源）。
#[test]
fn a_url_straddling_the_boundary_is_dropped_whole() {
    let url = "https://example.test/very/long/path";
    let filler = "a".repeat(QUOTE_PREVIEW_BYTE_BUDGET - 8);
    let body = format!("{filler}\n{url}\nmore text after the link");
    let preview = bounded_preview(&body);
    assert!(!preview.contains("example.test"), "{preview}");
    assert!(preview.ends_with("\n..."));

    // 整条 URL 都在预算内 ⇒ 原样保留（含 query）。
    let body = format!("{url}?x=1&y=2 more");
    assert_eq!(bounded_preview(&body), body);
}

// =====================================================================
// 分片
// =====================================================================

/// 预算以内不分片；预算为 0 也返回原文（上游的"只切一次"分支）。
#[test]
fn a_body_within_budget_is_not_split() {
    assert_eq!(chunk_markdown(""), vec![String::new()]);
    assert_eq!(chunk_markdown("short"), vec!["short".to_string()]);
    assert_eq!(
        chunk_markdown_with_budget("abcd", 0),
        vec!["abcd".to_string()]
    );
    // 生产常量：14_000 片预算 / 14_000−4 正文预算 / 15_000 载荷上限。
    assert_eq!(MARKDOWN_BYTE_BUDGET, 14_000);
    assert_eq!(MARKDOWN_CONTENT_BYTE_BUDGET, 13_996);
    assert_eq!(MARKDOWN_PAYLOAD_BYTE_BUDGET, 15_000);
}

/// 围栏代码块：每片自带闭合围栏、下一片重开**同一行**围栏（含 info string）。
#[test]
fn an_open_fence_is_closed_per_chunk_and_reopened_with_its_info_string() {
    let body = format!("```sh\n{}", "echo hi\n".repeat(8));
    let chunks = chunk_markdown_with_budget(&body, 40);
    assert_eq!(chunks.len(), 3, "{chunks:?}");
    let echoes: Vec<usize> = chunks
        .iter()
        .map(|chunk| {
            assert!(chunk.starts_with("```sh\n"), "{chunk:?}");
            assert!(chunk.ends_with("\n```"), "{chunk:?}");
            chunk.matches("echo hi").count()
        })
        .collect();
    assert_eq!(echoes, vec![3, 3, 2], "三片覆盖全部 8 行且互不重复");
    for chunk in &chunks {
        assert!(chunk.len() <= 40, "{} > 40", chunk.len());
    }
}

/// 只装围栏 / 空行的片会被丢掉（它只会渲染成一个空代码块）。
#[test]
fn a_fence_only_chunk_is_dropped() {
    // 开围栏之后紧跟一行就超预算 ⇒ 那个"只有开围栏"的片不该发出去。
    let body = format!("```\n{}\n```\n", "x".repeat(200));
    let chunks = chunk_markdown_with_budget(&body, 32);
    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(
            chunk.trim_matches(['`', '\n']).contains('x'),
            "空代码块被发出去了：{chunk:?}"
        );
    }
}

/// 超长单行硬切：切点回退到 rune 边界，`> ` 前缀逐片重复。
#[test]
fn an_oversized_quote_line_is_hard_split_on_rune_boundaries() {
    let body = format!("> {}", "界".repeat(40));
    let chunks = chunk_markdown_with_budget(&body, 32);
    assert!(chunks.len() > 1);
    let mut rebuilt = String::new();
    for chunk in &chunks {
        assert!(chunk.starts_with("> "), "{chunk:?}");
        assert!(chunk.len() <= 28, "{} > 28", chunk.len());
        rebuilt.push_str(chunk.trim_start_matches("> "));
    }
    assert_eq!(rebuilt, "界".repeat(40));
}

/// `hard_split` 是总函数：预算小于一个码位也不会死循环 / 负数切片 / 半字符。
#[test]
fn hard_split_is_total() {
    // `budget` 小于一个码位的最大字节数时被抬到 4（上游 `utf8.UTFMax` 的兜底）。
    assert_eq!(hard_split("", 0), Vec::<String>::new());
    assert_eq!(
        hard_split("界界", 0),
        vec!["界".to_string(), "界".to_string()]
    );
    assert_eq!(hard_split("abcdef", 4), vec!["abcd", "ef"]);
    assert_eq!(hard_split("abcdef", 100), vec!["abcdef".to_string()]);
}

// =====================================================================
// 转义
// =====================================================================

/// 替换按**实参顺序**走：`\` 先被翻倍，之后插进来的反斜杠**不再**被翻倍。
#[test]
fn escaping_is_a_single_pass_in_argument_order() {
    assert_eq!(escape_markdown_text(r"\*"), r"\\\*");
    assert_eq!(escape_markdown_text("a|b"), r"a\|b");
    assert_eq!(escape_markdown_text("(x)"), "(x)");
    assert_eq!(escape_markdown_quote_inline_text(r"\`"), r"\\\`");
    assert_eq!(escape_markdown_quote_inline_text("_"), r"\_");
    // 引用用比答案**少**六个字符的集合（`#+->|!` 只在行首保护）。
    assert_eq!(escape_markdown_quote_inline_text("a|b"), "a|b");
    assert_eq!(escape_markdown_quote_inline_text("a-b"), "a-b");
}

/// 引用里 web URL **原样**保留（`DingTalk` 要按它自己的拼写自动成链）。
#[test]
fn a_web_url_survives_the_quote_escape_verbatim() {
    let body = "see https://example.test/a*b?x=1&y=2 and `code`";
    let escaped = escape_markdown_quote_text(body);
    assert!(
        escaped.contains("https://example.test/a*b?x=1&y=2"),
        "{escaped}"
    );
    assert!(escaped.contains(r"\`code\`"), "{escaped}");
}

/// 行首的块级标记加反斜杠；行中间的同名标点不动。
#[test]
fn only_leading_block_markers_are_escaped() {
    assert_eq!(escape_markdown_quote_text("# head"), r"\# head");
    assert_eq!(escape_markdown_quote_text("  - item"), r"  \- item");
    assert_eq!(escape_markdown_quote_text("a - b"), "a - b");
    assert_eq!(escape_markdown_quote_text("> quote"), r"\> quote");
}

/// 引用块组装：空引用原样返回；非空 ⇒ `> …` + 水平线 + 答案；换行是 hard break。
#[test]
fn the_quote_block_wraps_the_answer_behind_a_rule() {
    assert_eq!(prepend_markdown_quote("answer", ""), "answer");
    assert_eq!(prepend_markdown_quote("answer", "   "), "answer");

    let rendered = prepend_markdown_quote("answer", "line1\nline2");
    assert_eq!(rendered, "> line1  \n> line2\n\n---\n\nanswer");
    // 引用的换行被渲染成 hard break（`  \n> `），而不是软换行。
    assert!(rendered.contains("line1  \n> line2"));
}

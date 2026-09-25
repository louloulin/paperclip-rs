//! `DingTalk` 出站的 **Markdown 面**：正文分片、正文/引用转义、引用预览
//! （上游 `internal/integrations/dingtalk/markdown.go` 286 行 + `outbound_send.go` 的
//! `escapeMarkdown*` / `prependMarkdownQuote` 三函数）。
//!
//! - **写者**：M7-8（`docs/60-M7-PLAN.md` §3.3；上游 `outbound_send.go` 的转义函数与本文件
//!   同属本片 ⇒ 按"分片/转义"而不是按上游文件切）。
//!
//! # 上游为什么要自己分片
//!
//! `DingTalk` 文档给出的硬上限是 **`msgParam` 15000 字节**，而 `msgParam` 是
//! `{"title":…,"text":…}` 的**序列化后** JSON —— 转义（`"`→`\"`、`<`→`\u003c` 等）会让
//! 正文显著膨胀。所以判据不是"正文字节数"，而是"**序列化后**的载荷字节数"
//! （[`MARKDOWN_PAYLOAD_BYTE_BUDGET`]），而分片预算要比它**更保守**
//! （[`MARKDOWN_BYTE_BUDGET`]）—— 两条都逐字照上游，不许"更聪明地"改。
//!
//! # 代码块跨片要能自己站起来
//!
//! 在代码块中间切开会把后面那半的渲染弄坏，所以每片**自带**一条合成闭合围栏
//! （[`SYNTHETIC_FENCE_CLOSE`]），下一片重开同一行围栏（**保留 info string** —— 上游逐字：
//! `DingTalk` 按语言标签高亮，裸围栏重开会丢掉高亮）。这道"合成前缀"的字节要**先**从正文预算里
//! 扣掉，否则最终 wire 上的正文会超限。
//!
//! # 引用（`> …`）为什么是 Markdown 而不是平台字段
//!
//! `DingTalk` 的出站机器人消息**没有**原生的回复/引用字段（上游注释逐字）⇒ 引用被渲染成
//! Markdown 引用块 + 一条水平线，并且与答案在**同一片**里。
//!
//! # 凭据面
//!
//! 本文件**没有**任何凭据字段，也没有 `tracing::*`。

use crate::dingtalk::inbound::card::web_url_spans;

/// 一片正文的字节预算（上游 `markdownByteBudget = 14000`）。
pub const MARKDOWN_BYTE_BUDGET: usize = 14_000;

/// **序列化后**的 `msgParam` 字节上限（上游 `markdownPayloadByteBudget = 15000`）。
pub const MARKDOWN_PAYLOAD_BYTE_BUDGET: usize = 15_000;

/// 引用预览（用户看到的那段源消息摘录）的字节预算（上游 `quotePreviewByteBudget = 256`）。
///
/// 它**只是**展示策略，不是平台对标题的限制（上游逐字）。
pub const QUOTE_PREVIEW_BYTE_BUDGET: usize = 256;

/// 跨片时追加的合成闭合围栏（上游 `markdownSyntheticFenceCloseBytes = len("\n```")`）。
pub const SYNTHETIC_FENCE_CLOSE: &str = "\n```";

/// 扣掉合成围栏后的**正文**预算（上游 `markdownContentByteBudget`）。
pub const MARKDOWN_CONTENT_BYTE_BUDGET: usize = MARKDOWN_BYTE_BUDGET - SYNTHETIC_FENCE_CLOSE.len();

/// 重开围栏那条合成前缀的字节上限（上游 `maxMarkdownFenceInfoBytes = 256`）：
/// 一段超长的 info string 不该把整片预算吃光，也不该把预算算成负数。
pub const MAX_MARKDOWN_FENCE_INFO_BYTES: usize = 256;

/// 一片只有空白时用的默认标题（上游 `defaultMarkdownTitle`，逐字）。
pub const DEFAULT_MARKDOWN_TITLE: &str = "Multica has replied.";

/// 标题 = 整片正文（上游 `markdownTitle`：`DingTalk` 的文本引用回调会把它当"标题"回传，
/// 所以**不**截断；只有纯空白才退回默认值）。
#[must_use]
pub fn markdown_title(body: &str) -> String {
    if body.trim().is_empty() {
        return DEFAULT_MARKDOWN_TITLE.to_string();
    }
    body.to_string()
}

/// 引用预览（上游 `quotePreview`）：只影响出站回复里显示的那段源消息摘录，
/// **从不**约束答案标题，也**从不**约束入站的选中上下文。
#[must_use]
pub fn quote_preview(body: &str) -> String {
    let normalized = body.replace("\r\n", "\n");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    bounded_preview(trimmed)
}

/// 按字节上限截断预览（上游 `boundedPreview`）。
///
/// 两个细节都是**语义**而不是美化：
/// 1. 截断点回退到 UTF-8 边界（否则会切出半个字符）；
/// 2. 若一个 web URL **横跨**截断点，就把截断点退到该 URL 的起点 —— 一段被切短的地址
///    会指向**另一个**资源，那比不给链接更糟。
#[must_use]
pub fn bounded_preview(body: &str) -> String {
    const SUFFIX: &str = "\n...";
    if body.len() <= QUOTE_PREVIEW_BYTE_BUDGET {
        return body.to_string();
    }
    let mut limit = QUOTE_PREVIEW_BYTE_BUDGET - SUFFIX.len();
    while limit > 0 && !is_rune_start(body.as_bytes()[limit]) {
        limit -= 1;
    }
    for (start, end) in web_url_spans(body) {
        if start < limit && end > limit {
            limit = start;
            break;
        }
    }
    let prefix = body[..limit].trim_end_matches([' ', '\t', '\r', '\n']);
    if prefix.is_empty() {
        return "...".to_string();
    }
    format!("{prefix}{SUFFIX}")
}

/// 一个字节是否是 UTF-8 起始字节（Go `utf8.RuneStart`）。
fn is_rune_start(byte: u8) -> bool {
    byte & 0xC0 != 0x80
}

/// 按 [`MARKDOWN_BYTE_BUDGET`] 分片（上游 `chunkMarkdown`）。
#[must_use]
pub fn chunk_markdown(body: &str) -> Vec<String> {
    chunk_markdown_with_budget(body, MARKDOWN_BYTE_BUDGET)
}

/// 用显式预算分片（上游 `chunkMarkdownWithBudget`；用例压小预算好钉住边界）。
#[must_use]
pub fn chunk_markdown_with_budget(body: &str, byte_budget: usize) -> Vec<String> {
    chunk_markdown_with_first_budget(body, byte_budget, byte_budget)
}

/// 首片与后续片用**不同**预算（上游 `chunkMarkdownWithFirstBudget`）：引用前缀只占首片。
///
/// 逐行推进；一行自己就超预算时硬切（切点回退到 rune 边界）。代码围栏状态跨行维护，
/// 收片时补闭合围栏、下一片重开。
#[must_use]
pub fn chunk_markdown_with_first_budget(
    body: &str,
    first_budget: usize,
    later_budget: usize,
) -> Vec<String> {
    let mut byte_budget = first_budget;
    if body.len() <= byte_budget {
        return vec![body.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut fence_open = false;
    // `fence_info` 是当前代码块的**开围栏整行**（例如 ```` ```go ````）。
    let mut fence_info = String::new();

    // `flush` 需要同时改这几个局部状态 ⇒ 用闭包会与借用检查器打架，直接内联成宏更直白。
    macro_rules! flush {
        ($reopen:expr) => {{
            if !current.is_empty() {
                let mut text = current.clone();
                if fence_open {
                    text.push_str(SYNTHETIC_FENCE_CLOSE);
                }
                // 只装围栏 / 空行的片会渲染成一个空代码块 ⇒ 丢它（上游 `isBlankChunk`）。
                if !is_blank_chunk(&text) {
                    chunks.push(text);
                    byte_budget = later_budget;
                }
                current.clear();
                if $reopen && fence_open {
                    current.push_str(&fence_info);
                    current.push('\n');
                }
            }
        }};
    }

    for line in split_keep_newline(body) {
        if line.len() > Budget::content(byte_budget) {
            flush!(true);
            let mut line = line;
            let mut quote_prefix = "";
            if !fence_open && line.starts_with("> ") {
                quote_prefix = "> ";
                line = &line[quote_prefix.len()..];
            }
            while !line.is_empty() {
                let mut piece_budget =
                    Budget::piece(Budget::content(byte_budget), quote_prefix.len());
                if fence_open {
                    piece_budget = Budget::fence_piece(byte_budget, fence_info.len());
                }
                let mut cut = piece_budget.min(line.len());
                if cut < line.len() {
                    while cut > 0 && !is_rune_start(line.as_bytes()[cut]) {
                        cut -= 1;
                    }
                }
                let piece = &line[..cut];
                line = &line[cut..];
                let rendered = if fence_open {
                    format!("{fence_info}\n{piece}{SYNTHETIC_FENCE_CLOSE}")
                } else {
                    piece.to_string()
                };
                chunks.push(format!("{quote_prefix}{rendered}"));
                byte_budget = later_budget;
            }
            continue;
        }
        if current.len() + line.len() > Budget::content(byte_budget) {
            flush!(true);
        }
        if is_fence_line(line) {
            if fence_open {
                fence_open = false;
                fence_info.clear();
            } else {
                fence_open = true;
                fence_info = continuation_fence(line);
            }
        }
        current.push_str(line);
    }
    flush!(false);
    // 收尾那次 `flush` 的预算重置没有后续读者（上游 `resetBudget` 在最后一片之后也是空操作）；
    // 显式读一次，免得它被当成死写告警。
    let _ = byte_budget;
    chunks
}

/// 预算算术（Go 用有符号 `int`，负数随后被 `utf8.UTFMax` 兜住 ⇒ 这里显式表达同一条规则）。
struct Budget;

impl Budget {
    /// 片预算 → 正文预算（扣掉合成闭合围栏）。
    fn content(byte_budget: usize) -> usize {
        byte_budget.saturating_sub(SYNTHETIC_FENCE_CLOSE.len())
    }

    /// 普通硬切的片预算（扣掉要重复的 `> ` 前缀）。
    fn piece(content_byte_budget: usize, quote_prefix_len: usize) -> usize {
        content_byte_budget
            .saturating_sub(quote_prefix_len)
            .max(utf8_max())
    }

    /// 代码块内硬切的片预算（要装下重开的围栏行 + 换行 + 闭合围栏）。
    fn fence_piece(byte_budget: usize, fence_info_len: usize) -> usize {
        byte_budget
            .saturating_sub(fence_info_len)
            .saturating_sub(1 + SYNTHETIC_FENCE_CLOSE.len())
            .max(utf8_max())
    }
}

/// `utf8.UTFMax` = 4：任何预算都不该小于一个码位的最大字节数（否则会切出半个字符）。
const fn utf8_max() -> usize {
    4
}

/// 重开围栏用的那一行（上游 `continuationFence`：info string 太长 ⇒ 退回裸围栏）。
fn continuation_fence(line: &str) -> String {
    let fence = line.trim_end_matches(['\r', '\n']);
    if fence.len() > MAX_MARKDOWN_FENCE_INFO_BYTES {
        return "```".to_string();
    }
    fence.to_string()
}

/// 按行切**保留**行尾 `\n`（上游 `splitKeepNewline`：`strings.Join` 能逐字还原；
/// `SplitAfter` 在末尾会多出一个 `""`，丢掉它免得正文平白多一个空行）。
fn split_keep_newline(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// 这一行是否开/闭一个围栏代码块（首个非空白内容就是 ```` ``` ````）。
fn is_fence_line(line: &str) -> bool {
    line.trim_start_matches([' ', '\t']).starts_with("```")
}

/// 这一片是否**没有**可渲染内容（每行不是空行就是围栏）。
fn is_blank_chunk(text: &str) -> bool {
    for line in text.split('\n') {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("```") {
            return false;
        }
    }
    true
}

/// 不切出半个字符的硬切（上游 `hardSplit`）。
///
/// ⚠️ 生产调用点（[`chunk_markdown_with_first_budget`] 的硬切分支）内联了同样的算术；
/// 这个函数保留成**总函数**是上游的取舍：预算算错时宁可多切一片，也不要死循环 /
/// 负数切片 / 半个字符。
#[must_use]
pub fn hard_split(text: &str, budget: usize) -> Vec<String> {
    let budget = budget.max(utf8_max());
    let mut remainder = text;
    let mut pieces = Vec::new();
    while remainder.len() > budget {
        let mut cut = budget;
        while cut > 0 && !is_rune_start(remainder.as_bytes()[cut]) {
            cut -= 1;
        }
        if cut == 0 {
            cut = budget;
        }
        pieces.push(remainder[..cut].to_string());
        remainder = &remainder[cut..];
    }
    if !remainder.is_empty() {
        pieces.push(remainder.to_string());
    }
    pieces
}

// =====================================================================
// 转义（上游 `outbound_send.go`）
// =====================================================================

/// 按**实参顺序**做一次性替换（Go `strings.NewReplacer` 的语义）。
///
/// 顺序很关键：先替换 `\` → `\\` 就不会把后一步插进来的反斜杠再转义一遍
/// （逐个 `str::replace` 会，那是**行为差异**，不是风格差异）。
fn replace_in_order(text: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'outer: while !rest.is_empty() {
        for (from, to) in pairs {
            if rest.starts_with(from) {
                out.push_str(to);
                rest = &rest[from.len()..];
                continue 'outer;
            }
        }
        let character = rest.chars().next().expect("非空");
        out.push(character);
        rest = &rest[character.len_utf8()..];
    }
    out
}

/// 引用正文的**行内**转义（上游 `escapeMarkdownQuoteInlineText`）。
///
/// 转义方括号本身就已经挡住了 Markdown 链接；括号**故意**保持字面 —— `DingTalk` 会自动
/// 把 URL 变链接，此时它可能把尾部的转义符一起吞进链接里（上游逐字）。
#[must_use]
pub fn escape_markdown_quote_inline_text(text: &str) -> String {
    replace_in_order(
        text,
        &[
            ("\\", "\\\\"),
            ("`", "\\`"),
            ("*", "\\*"),
            ("_", "\\_"),
            ("[", "\\["),
            ("]", "\\]"),
        ],
    )
}

/// 整段引用正文的转义（上游 `escapeMarkdownQuoteText`）：行内转义 **+** 只保护**行首**的块级标记。
///
/// 行首标记只在行首需要保护；普通标点前面**不加**可见的反斜杠 —— 受限的渲染器不会消费那些转义，
/// 用户会真的看到 `\`。
#[must_use]
pub fn escape_markdown_quote_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    let mut start = 0usize;
    for (span_start, span_end) in web_url_spans(text) {
        escaped.push_str(&escape_markdown_quote_inline_text(&text[start..span_start]));
        // web URL **原样**保留：`DingTalk` 要按它自己的拼写自动成链。
        escaped.push_str(&text[span_start..span_end]);
        start = span_end;
    }
    escaped.push_str(&escape_markdown_quote_inline_text(&text[start..]));

    let mut lines: Vec<String> = Vec::new();
    for line in escaped.split('\n') {
        let trimmed = line.trim_start_matches([' ', '\t']);
        let first = trimmed.chars().next();
        if first.is_some_and(|character| "#+->".contains(character)) {
            let indent = line.len() - trimmed.len();
            lines.push(format!("{}\\{trimmed}", &line[..indent]));
        } else {
            lines.push(line.to_string());
        }
    }
    lines.join("\n")
}

/// 普通答案正文的转义（上游 `escapeMarkdownText`）：比引用多保护 `#+ -!>|` 六个字符。
#[must_use]
pub fn escape_markdown_text(text: &str) -> String {
    replace_in_order(
        text,
        &[
            ("\\", "\\\\"),
            ("`", "\\`"),
            ("*", "\\*"),
            ("_", "\\_"),
            ("[", "\\["),
            ("]", "\\]"),
            ("#", "\\#"),
            ("+", "\\+"),
            ("-", "\\-"),
            ("!", "\\!"),
            (">", "\\>"),
            ("|", "\\|"),
        ],
    )
}

/// 把触发消息渲染成 `DingTalk` 支持的引用块（上游 `prependMarkdownQuote`）。
///
/// 引用为空 ⇒ 原样返回。引用里源代码的换行与单独一行的省略号都渲染成 **hard break**
/// （`"  \n> "`），而答案本身接在一条水平线之后。
#[must_use]
pub fn prepend_markdown_quote(text: &str, quote: &str) -> String {
    let quote = quote_preview(quote);
    if quote.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + quote.len() + 16);
    out.push_str("> ");
    out.push_str(&escape_markdown_quote_text(&quote).replace('\n', "  \n> "));
    out.push_str("\n\n---\n\n");
    out.push_str(text);
    out
}

#[cfg(test)]
mod tests;

//! 标准 Markdown → Slack `mrkdwn` 的转换器（上游 `internal/integrations/slack/mrkdwn.go`，134 行）。
//!
//! - **写者**：M7-3（`docs/60-M7-PLAN.md` §3.3）。
//! - **出处**：上游是 Nous Research Hermes Agent 的 `format_message`（MIT）的 Go 移植；
//!   本文件是那份 Go 代码的 Rust 移植（同一份 MIT 许可与同一串保护位序，见上游文件头的声明）。
//!
//! # 为什么必须转（上游注释逐字）
//!
//! Slack 渲染自己的 `mrkdwn` 方言，不是标准 Markdown：加粗是**一个**星号（不是两个），斜体是
//! `_下划线_`，链接是 `<url|label>`，标题与 `~~删除线~~` **不支持**。agent 输出标准 Markdown，
//! 不转换就会在 Slack 里看到字面的 `**` / `##` / `[text](url)`。
//!
//! # 十三个步骤（顺序有语义，**别重排**）
//!
//! ```text
//! 1 栅栏码块 → 保护        5 引用标记 → 保护      9  **粗** → *粗*
//! 2 行内码 → 保护          6 & < > 转义           10 *斜* → _斜_
//! 3 [text](url) → <url|text>（图片链接**不动**）   11 ~~删~~ → ~删~
//! 4 既有 Slack 实体 → 保护 7 ## 标题 → *标题*      12 按**逆序**还原保护位
//!                          8 ***粗斜*** → *_粗斜_*
//! ```
//!
//! 保护位（`\x00SL<n>\x00`）是**故意**的：后续每一趟都会重写整段文本，不把"不该被改的片段"
//! 藏起来就会被打第二遍（栅栏里的 `**` 变成粗体、既有的 `<@U123>` 被转义）。
//!
//! # 手写扫描器（**不引 `regex`**）
//!
//! 依赖面在 M7-0 anchor 一次定死（`docs/60` §3.1 的"零新外部包"），所以上游的十条正则在这里
//! 是手写扫描：每条 `find_*` 的语义逐条对齐正则（含**左最先**与**非重叠**扫描、`\s` 只含
//! ASCII 空白、`.` 不跨行、`#{1,6}` 的贪婪回退）。`docs/32` §10 登记了这条形态差异。

use std::collections::HashMap;

/// 把标准 Markdown 转成 Slack `mrkdwn`（上游 `formatMrkdwn`）。
#[must_use]
pub fn format_mrkdwn(content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }
    let mut stash = Placeholders::default();
    // 1) 栅栏码块，然后 2) 行内码。
    let text = rewrite(content, find_fenced, |m| stash.stash(m));
    let text = rewrite(&text, find_inline_code, |m| stash.stash(m));

    // 3) `[text](url)` → `<url|text>`；图片链接（`!` 前缀）原样留着
    //    （Slack 不渲染 Markdown 内联图片）。
    let text = rewrite(&text, find_md_link, |m| match split_md_link(m) {
        Some(Link::Image) | None => m.to_string(),
        Some(Link::Link { label, url }) => {
            let url = url.trim();
            let url = if url.starts_with('<') && url.ends_with('>') {
                url.trim_start_matches('<').trim_end_matches('>').trim()
            } else {
                url
            };
            stash.stash(format!("<{url}|{label}>"))
        }
    });

    // 4) 既有的 Slack 实体 / 手写链接，5) 引用标记。
    let text = rewrite(&text, find_slack_entity, |m| stash.stash(m));
    let text = rewrite(&text, find_blockquote, |m| stash.stash(m));

    // 6) 转义 Slack 控制字符（**先反解再转义**，避免把输入二次转义）。
    let text = replace_patterns(&text, &[("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">")]);
    let text = replace_patterns(&text, &[("&", "&amp;"), ("<", "&lt;"), (">", "&gt;")]);

    // 7) 标题（`## Title`）→ `*Title*`（顺带剥掉标题里多余的粗体）。
    let text = rewrite(&text, find_header, |m| {
        let inner = header_inner(m).trim();
        let inner = rewrite(inner, find_bold, |bold| bold_inner(bold).to_string());
        stash.stash(format!("*{inner}*"))
    });

    // 8) `***粗斜***` → `*_粗斜_*`，9) `**粗**` → `*粗*`，
    // 10) `*斜*` → `_斜_`，11) `~~删~~` → `~删~`。
    let text = rewrite(
        &text,
        |t, from| find_delimited(t, from, "***"),
        |m| stash.stash(format!("*_{}_*", delimiter_inner(m, 3))),
    );
    let text = rewrite(
        &text,
        |t, from| find_delimited(t, from, "**"),
        |m| stash.stash(format!("*{}*", delimiter_inner(m, 2))),
    );
    let text = rewrite(&text, find_italic, |m| {
        stash.stash(format!("_{}_", delimiter_inner(m, 1)))
    });
    let text = rewrite(
        &text,
        |t, from| find_delimited(t, from, "~~"),
        |m| {
            let inner = delimiter_inner(m, 2);
            stash.stash(format!("~{inner}~"))
        },
    );

    // 12) 按**插入序的逆序**还原（嵌套的保护位因此能解开）。
    stash.restore(&text)
}

// =====================================================================
// 保护位
// =====================================================================

/// 保护位表：键 `\x00SL<n>\x00`（与上游 `mrkdwnPlaceholders` 逐字同形）。
#[derive(Default)]
struct Placeholders {
    /// 插入序（还原时**逆序**遍历）。
    order: Vec<String>,
    values: HashMap<String, String>,
}

impl Placeholders {
    /// 把一个片段藏进保护位，返回它的键。
    fn stash(&mut self, value: impl Into<String>) -> String {
        let key = format!("\u{0}SL{}\u{0}", self.order.len());
        self.order.push(key.clone());
        self.values.insert(key.clone(), value.into());
        key
    }

    /// 逆序还原。
    fn restore(&self, text: &str) -> String {
        let mut out = text.to_string();
        for key in self.order.iter().rev() {
            if let Some(value) = self.values.get(key) {
                out = out.replace(key.as_str(), value);
            }
        }
        out
    }
}

// =====================================================================
// 扫描骨架
// =====================================================================

/// 一趟扫描重写：等价于 Go 的 `ReplaceAllStringFunc`（**左最先 + 非重叠**）。
fn rewrite<F, G>(text: &str, mut find: F, mut replace: G) -> String
where
    F: FnMut(&str, usize) -> Option<(usize, usize)>,
    G: FnMut(&str) -> String,
{
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while cursor <= text.len() {
        let Some((start, end)) = find(text, cursor) else {
            break;
        };
        // 防御：匹配器不得回退（回退会让 `text[cursor..start]` 的切片语义反转）。
        if start < cursor || end > text.len() {
            break;
        }
        out.push_str(&text[cursor..start]);
        out.push_str(&replace(&text[start..end]));
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// 按**参数序**做一次不重叠的多模式替换（等价于 Go 的 `strings.NewReplacer`：在目标串上从左到
/// 右扫，每个位置按实参序试第一个命中的模式）。
fn replace_patterns(text: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0usize;
    'outer: while pos < text.len() {
        for (pattern, replacement) in pairs {
            if text[pos..].starts_with(pattern) {
                out.push_str(replacement);
                pos += pattern.len();
                continue 'outer;
            }
        }
        let ch = text[pos..].chars().next().unwrap_or('\u{0}');
        out.push(ch);
        pos += ch.len_utf8();
    }
    out
}

/// ASCII 空白（Go 的 `\s`：`[\t\n\f\r ]`；**不含** `\v`，也不含 Unicode 空白）。
fn is_space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

/// 从 `from` 起（含）的第一个**行首**；`from` 落在行中 ⇒ 跳到下一行。
fn line_start_at_or_after(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut pos = from;
    loop {
        if pos > text.len() {
            return None;
        }
        if pos == 0 || bytes[pos - 1] == b'\n' {
            return Some(pos);
        }
        {
            let rel = text[pos..].find('\n')?;
            pos = pos + rel + 1;
        }
    }
}

/// `^`（行首）锚定的模式共用的巡行脚手架：在 `from` 之后的每个行首试 `probe`。
fn find_line_anchored<F>(text: &str, from: usize, mut probe: F) -> Option<(usize, usize)>
where
    F: FnMut(&str, usize) -> Option<usize>,
{
    let mut line = line_start_at_or_after(text, from)?;
    loop {
        let bytes = text.as_bytes();
        if line == 0 || bytes[line - 1] == b'\n' {
            if let Some(end) = probe(text, line) {
                return Some((line, end));
            }
        }
        {
            let rel = text[line..].find('\n')?;
            line = line + rel + 1;
        }
    }
}

// =====================================================================
// 十趟的匹配器
// =====================================================================

/// `(?s)(```(?:[^\n]*\n)?.*?```)` —— 栅栏码块（左最先、闭合栅栏最短）。
fn find_fenced(text: &str, from: usize) -> Option<(usize, usize)> {
    let mut search = from;
    while search <= text.len() {
        let start = text[search..].find("```")? + search;
        let after = start + 3;
        // 贪婪的可选"信息行"（`[^\n]*\n`）：先按它成功来算闭合位。
        if let Some(nl_rel) = text[after..].find('\n') {
            let nl = after + nl_rel;
            if let Some(rel) = text[nl + 1..].find("```") {
                return Some((start, nl + 1 + rel + 3));
            }
        }
        // 回退：可选组不参与，闭合栅栏就在紧跟其后。
        if let Some(rel) = text[after..].find("```") {
            return Some((start, after + rel + 3));
        }
        search = start + 1;
    }
    None
}

/// 反引号定界的行内码（内容至少一个非反引号字符，可跨行）。
fn find_inline_code(text: &str, from: usize) -> Option<(usize, usize)> {
    let mut search = from;
    while search < text.len() {
        let open = text[search..].find('`')? + search;
        let rel = text[open + 1..].find('`')?;
        let close = open + 1 + rel;
        if close > open + 1 {
            return Some((open, close + 1));
        }
        search = open + 1;
    }
    None
}

/// `(!?)\[([^\]]+)\]\(([^()]*(?:\([^()]*\)[^()]*)*)\)` —— Markdown 链接（含图片前缀）。
fn find_md_link(text: &str, from: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut search = from;
    while search < text.len() {
        let bracket = text[search..].find('[')? + search;
        let start = if bracket > 0 && bytes[bracket - 1] == b'!' {
            bracket - 1
        } else {
            bracket
        };
        if start < from {
            search = bracket + 1;
            continue;
        }
        // 标签：`[^\]]+`（非空、首个 `]` 收尾）。
        let label_rel = text[bracket + 1..].find(']')?;
        let label_end = bracket + 1 + label_rel;
        if label_end == bracket + 1 || bytes.get(label_end + 1) != Some(&b'(') {
            search = bracket + 1;
            continue;
        }
        // URL：`[^()]*(\([^()]*\)[^()]*)*` 后必须紧跟闭合 `)`。
        if let Some(end) = parse_link_url(text, label_end + 1) {
            return Some((start, end));
        }
        search = bracket + 1;
    }
    None
}

/// 链接目标（`(` 之后）的解析：返回闭合 `)` 之后的索引。
fn parse_link_url(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut pos = open + 1;
    loop {
        while pos < bytes.len() && !matches!(bytes[pos], b'(' | b')') {
            pos += 1;
        }
        if pos < bytes.len() && bytes[pos] == b'(' {
            // 一层平衡括号（正则里只允许一层）。
            let mut inner = pos + 1;
            while inner < bytes.len() && !matches!(bytes[inner], b'(' | b')') {
                inner += 1;
            }
            if inner < bytes.len() && bytes[inner] == b')' {
                pos = inner + 1;
                continue;
            }
            return None;
        }
        break;
    }
    (pos < bytes.len() && bytes[pos] == b')').then_some(pos + 1)
}

/// 一条 Markdown 链接的两种形态。
enum Link<'a> {
    /// `![alt](url)`：Slack 不渲染，**原样留着**。
    Image,
    /// `[text](url)`。
    Link { label: &'a str, url: &'a str },
}

/// 把一条已匹配的链接切成形态 + 两段（`rewrite` 的回调里用）。
fn split_md_link(matched: &str) -> Option<Link<'_>> {
    let bytes = matched.as_bytes();
    let bracket = matched.find('[')?;
    let label_end = matched[bracket + 1..].find(']')? + bracket + 1;
    let url_end = matched.rfind(')')?;
    let url = matched.get(label_end + 2..url_end)?;
    Some(if bracket > 0 && bytes[0] == b'!' {
        Link::Image
    } else {
        Link::Link {
            label: &matched[bracket + 1..label_end],
            url,
        }
    })
}

/// `(<(?:[@#!]|(?:https?|mailto|tel):)[^>\n]+>)` —— 既有的 Slack 实体 / 手写链接。
fn find_slack_entity(text: &str, from: usize) -> Option<(usize, usize)> {
    let mut search = from;
    while search < text.len() {
        let open = text[search..].find('<')? + search;
        let rest = &text[open + 1..];
        let prefixed = match rest.as_bytes().first() {
            Some(b'@' | b'#' | b'!') => true,
            _ => ["http://", "https://", "mailto:", "tel:"]
                .iter()
                .any(|scheme| rest.starts_with(scheme)),
        };
        if prefixed {
            let mut pos = 0usize;
            let bytes = rest.as_bytes();
            while pos < bytes.len() && !matches!(bytes[pos], b'>' | b'\n') {
                pos += 1;
            }
            if pos >= 1 && pos < bytes.len() && bytes[pos] == b'>' {
                return Some((open, open + 1 + pos + 1));
            }
        }
        search = open + 1;
    }
    None
}

/// `(?m)^(>+\s)` —— 引用标记（只吃**一个**空白字符，与上游的 `\s` 单字符一致）。
fn find_blockquote(text: &str, from: usize) -> Option<(usize, usize)> {
    find_line_anchored(text, from, |text, line| {
        let bytes = text.as_bytes();
        let mut pos = line;
        while pos < bytes.len() && bytes[pos] == b'>' {
            pos += 1;
        }
        (pos > line && pos < bytes.len() && is_space(bytes[pos])).then_some(pos + 1)
    })
}

/// `(?m)^#{1,6}\s+(.+)$` —— 标题（`\s+` **贪婪**、可跨行；`$` 是行尾）。
fn find_header(text: &str, from: usize) -> Option<(usize, usize)> {
    find_line_anchored(text, from, |text, line| {
        let bytes = text.as_bytes();
        let mut hashes = line;
        while hashes < bytes.len() && hashes - line < 6 && bytes[hashes] == b'#' {
            hashes += 1;
        }
        if hashes == line {
            return None;
        }
        // `\s+`（贪婪，含 `\n`）：一直吃到第一个非空白字符。
        let body = hashes;
        let mut pos = body;
        while pos < bytes.len() && is_space(bytes[pos]) {
            pos += 1;
        }
        if pos == body || pos >= bytes.len() {
            return None;
        }
        // `(.+)$`：至少一个非 `\n` 字符，到行尾为止。
        let mut end = pos;
        while end < bytes.len() && bytes[end] != b'\n' {
            end += 1;
        }
        (end > pos).then_some(end)
    })
}

/// 标题的已匹配段 → 内部文本（`#` 与后面的空白之后、行尾之前）。
fn header_inner(matched: &str) -> &str {
    let bytes = matched.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() && bytes[pos] == b'#' {
        pos += 1;
    }
    while pos < bytes.len() && is_space(bytes[pos]) {
        pos += 1;
    }
    matched.get(pos..).unwrap_or_default()
}

/// 单 `*` 的斜体：`\*(\S(?:[^*\n]*?\S)?)\*`。
fn find_italic(text: &str, from: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut search = from;
    while search < text.len() {
        let open = text[search..].find('*')? + search;
        let first = open + 1;
        if first < bytes.len() && !is_space(bytes[first]) {
            let mut pos = first + 1;
            loop {
                if pos >= bytes.len() || bytes[pos] == b'\n' {
                    break;
                }
                if bytes[pos] == b'*' {
                    // `\S` 收尾（或内容只有一个字符）。
                    if pos == first + 1 || !is_space(bytes[pos - 1]) {
                        return Some((open, pos + 1));
                    }
                    break;
                }
                pos += 1;
            }
        }
        search = open + 1;
    }
    None
}

/// `~~(.+?)~~` / `**(.+?)**` / `***(.+?)***` 共用：定界符之间的内容**不跨行**、非空、取最短。
fn find_delimited(text: &str, from: usize, delimiter: &str) -> Option<(usize, usize)> {
    let mut search = from;
    while search <= text.len() {
        let open = text[search..].find(delimiter)? + search;
        let after = open + delimiter.len();
        {
            let rel = text[after..].find(delimiter)?;
            let close = after + rel;
            if close > after && !text[after..close].contains('\n') {
                return Some((open, close + delimiter.len()));
            }
        }
        search = open + 1;
    }
    None
}

/// 定界符已匹配段 → 内容（去掉两侧各 `delimiter_len` 字节的定界符）。
fn delimiter_inner(matched: &str, delimiter_len: usize) -> &str {
    matched
        .get(delimiter_len..matched.len().saturating_sub(delimiter_len))
        .unwrap_or_default()
}

/// 标题里要剥掉的粗体（上游 `reInnerBold` = `\*\*(.+?)\*\*`）。
fn bold_inner(matched: &str) -> &str {
    delimiter_inner(matched, 2)
}

/// `\*\*(.+?)\*\*` 的匹配器（标题内部用；与 `find_delimited(text, from, "**")` 同义）。
fn find_bold(text: &str, from: usize) -> Option<(usize, usize)> {
    find_delimited(text, from, "**")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上游 `TestFormatMrkdwn` 的逐条移植。
    #[test]
    fn format_mrkdwn_table() {
        let cases: &[(&str, &str, &str)] = &[
            ("plain unchanged", "just a reply", "just a reply"),
            ("empty", "", ""),
            ("bold", "**bold**", "*bold*"),
            ("italic star to underscore", "*italic*", "_italic_"),
            ("underscore italic kept", "_italic_", "_italic_"),
            ("bold italic", "***both***", "*_both_*"),
            ("strikethrough", "~~gone~~", "~gone~"),
            ("header to bold", "## Title", "*Title*"),
            ("header strips inner bold", "### **Big**", "*Big*"),
            (
                "markdown link",
                "see [docs](https://x.com/a)",
                "see <https://x.com/a|docs>",
            ),
            (
                "image link untouched",
                "![alt](https://x.com/i.png)",
                "![alt](https://x.com/i.png)",
            ),
            (
                "inline code protected",
                "use `**not bold**` here",
                "use `**not bold**` here",
            ),
            (
                "existing slack mention untouched",
                "hi <@U123>",
                "hi <@U123>",
            ),
            (
                "ampersand and angles escaped",
                "a & b < c > d",
                "a &amp; b &lt; c &gt; d",
            ),
            ("blockquote preserved", "> quoted line", "> quoted line"),
        ];
        for (name, input, want) in cases {
            assert_eq!(format_mrkdwn(input), *want, "{name}");
        }
    }

    /// 上游 `TestFormatMrkdwn_FencedCodeProtected`。
    #[test]
    fn fenced_code_is_protected_while_outside_text_converts() {
        let input = "before\n```\n**stars** and [x](y) stay literal\n```\nafter **bold**";
        let want = "before\n```\n**stars** and [x](y) stay literal\n```\nafter *bold*";
        assert_eq!(format_mrkdwn(input), want);
    }

    /// 上游 `TestFormatMrkdwn_LinkInsideBold`：链接先被藏起来，于是粗体那趟能安全地套在它外面。
    #[test]
    fn link_inside_bold_survives_the_bold_pass() {
        assert_eq!(
            format_mrkdwn("**see [docs](https://x.com)**"),
            "*see <https://x.com|docs>*"
        );
    }

    /// 栅栏的三种形态：带语言信息行、无信息行、无闭合（第三个不成立 ⇒ 原样）。
    #[test]
    fn fenced_code_shapes() {
        assert_eq!(format_mrkdwn("```rust\n**a**\n```"), "```rust\n**a**\n```");
        assert_eq!(format_mrkdwn("```code```"), "```code```");
        // 没有闭合栅栏 ⇒ `reFenced` **不**命中，于是后面的粗体那趟照转（Go 同形）。
        assert_eq!(format_mrkdwn("```**a**"), "```*a*");
        // 栅栏外的粗体照转，栅栏内的不转（同一段文本里）。
        assert_eq!(format_mrkdwn("**out** ```**in**```"), "*out* ```**in**```");
    }

    /// 转义：先反解再转义 —— 净效果是「把 agent 已经写好的实体规范化成转义形态」，
    /// 于是 `&amp;` / `&lt;` 往返不变，而 `&amp;lt;` 也只反解一层（上游同样两趟）。
    #[test]
    fn escaping_unescapes_once_then_escapes() {
        assert_eq!(
            format_mrkdwn("&amp;lt;"),
            "&amp;lt;",
            "反解一层后又转义回去"
        );
        assert_eq!(
            format_mrkdwn("&lt;b&gt;"),
            "&lt;b&gt;",
            "已转义的实体往返不变"
        );
        assert_eq!(format_mrkdwn("a&b"), "a&amp;b");
        assert_eq!(format_mrkdwn("a &amp; b"), "a &amp; b");
    }

    /// 既有 Slack 实体与手写链接不被转义，且引用标记被保护（`>` 不会变成 `&gt;`）。
    #[test]
    fn slack_entities_and_blockquotes_are_protected() {
        assert_eq!(
            format_mrkdwn("<https://x.com|label> and <#C1> and <!here>"),
            "<https://x.com|label> and <#C1> and <!here>"
        );
        // 引用标记只保护**行首**的 `>+\s`：行中的 `>` 会被转义（上游同形）。
        assert_eq!(format_mrkdwn("> a > b"), "> a &gt; b");
        // 引用里的粗体照转（保护位只包住 `> ` 标记本身）。
        assert_eq!(format_mrkdwn("> **bold**"), "> *bold*");
    }

    /// 标题的贪婪 `\s+`（可跨行）与 `#{1,6}` 的上界。
    #[test]
    fn header_greedy_whitespace_and_hash_bound() {
        assert_eq!(format_mrkdwn("# one"), "*one*");
        assert_eq!(format_mrkdwn("###### six"), "*six*");
        // 七个 `#` 不是标题（`#{1,6}` 的回退都会失败）。
        assert_eq!(format_mrkdwn("####### seven"), "####### seven");
        // `#` 不在行首就不是标题。
        assert_eq!(format_mrkdwn("a # tag"), "a # tag");
        // `#` 后没有空白就不是标题。
        assert_eq!(format_mrkdwn("#nospace"), "#nospace");
    }

    /// 链接目标里的一层平衡括号（`[^()]*(\([^()]*\)[^()]*)*`）。
    #[test]
    fn link_url_allows_one_paren_layer() {
        assert_eq!(
            format_mrkdwn("[d](https://x.com/a(b)c)"),
            "<https://x.com/a(b)c|d>"
        );
        // `<>` 包住的 URL 会被剥掉外层尖括号。
        assert_eq!(
            format_mrkdwn("[d](<https://x.com/a>)"),
            "<https://x.com/a|d>"
        );
    }

    /// 斜体的最小匹配与边界（不跨行、不以空白收尾、不与 `*` 冲突）。
    #[test]
    fn italic_boundaries() {
        assert_eq!(format_mrkdwn("a * b *c*"), "a * b _c_");
        assert_eq!(format_mrkdwn("*a\nb*"), "*a\nb*", "斜体不跨行");
        assert_eq!(format_mrkdwn("* trailing *"), "* trailing *");
        assert_eq!(format_mrkdwn("**"), "**");
    }

    /// 手写扫描器的等价性单测：`line_start_at_or_after` 与 `replace_patterns`。
    #[test]
    fn scanner_helpers() {
        assert_eq!(line_start_at_or_after("a\nb", 0), Some(0));
        assert_eq!(
            line_start_at_or_after("a\nb", 1),
            Some(2),
            "行中 ⇒ 跳到下一行"
        );
        assert_eq!(
            line_start_at_or_after("a\n", 2),
            Some(2),
            "末尾（行尾之后）也是行首"
        );
        assert_eq!(
            line_start_at_or_after("a\nb", 3),
            None,
            "文本不以换行结尾 ⇒ 末尾不是行首"
        );
        assert_eq!(line_start_at_or_after("abc", 1), None);
        assert_eq!(
            replace_patterns("&amp;lt;", &[("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">")]),
            "&lt;",
            "单趟替换：反解的产物不再被反解"
        );
    }

    /// `rewrite` 的左最先 + 非重叠语义（与 `ReplaceAllStringFunc` 一致）。
    #[test]
    fn rewrite_is_leftmost_and_non_overlapping() {
        // `aa` 在 `aaaa` 上只匹配两次（非重叠）。
        assert_eq!(
            rewrite(
                "aaaa",
                |t, from| t[from..].find("aa").map(|r| (from + r, from + r + 2)),
                |m| format!("[{m}]")
            ),
            "[aa][aa]"
        );
    }
}

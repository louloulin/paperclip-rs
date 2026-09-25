//! agent 的标准 Markdown → Telegram **HTML parse mode**（上游
//! `internal/integrations/telegram/markdown.go`，119 行）。
//!
//! - **写者**：M7-6（`LUM-1771`；`docs/60-M7-PLAN.md` §3.3）。
//!
//! # 为什么是 HTML 而不是 MarkdownV2（上游逐字的决定记录）
//!
//! `MarkdownV2` 要求**所有**正文里的 18 个标点都转义（漏一个整条 `sendMessage` 就被拒），
//! 而 HTML 只有 `&` / `<` / `>` 需要转义，唯一的失败模式是"出现了不认识的标签"。
//! 所以转换是**逐行**的、**故意保守**的：认不出的 Markdown 按转义后的纯文本透传，
//! 而不是冒"整条消息被拒"的险。
//!
//! # 与上游的一处形态差异（登记 `docs/32` §18 的偏离）
//!
//! 上游用 `regexp`（7 条模式）；本 crate 的依赖集在 M7-0 之后**冻结**
//! （`docs/60` §2.2：不得新增三方依赖），所以这里把七条模式**手写**成扫描器。
//! 匹配语义逐条照 `regexp` 的**非贪婪**语义复刻（含"分组 1 吃掉前导字符"这类细节），
//! 并用上游 `telegram_test.go` 的用例逐条钉住，另加边界用例（`****` / `*a**b*` 等）。
//!
//! # 输出形态（本文件是**纯函数**，没有端口、没有 IO）
//!
//! [`format_html`] 是唯一入口：它按行处理（代码块必须逐字保真，只做实体转义），
//! 首尾不留空行（上游 `strings.TrimRight(out, "\n")`）。

/// HTML 转义：与 Go 的 `html.EscapeString` **逐字节同表**
/// （`&`→`&amp;`、`'`→`&#39;`、`<`→`&lt;`、`>`→`&gt;`、`"`→`&#34;`）。
///
/// 顺序无关（一次遍历），且**不**碰 `\u{0}` —— 占位符靠这一条活过转义。
#[must_use]
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            other => out.push(other),
        }
    }
    out
}

/// 行内代码的占位哨兵（上游 `"\x00CODE\x00"`，逐字）。
const CODE_TOKEN: &str = "\u{0}CODE\u{0}";

/// 链接的占位哨兵（上游 `"\x00LINK\x00"`，逐字）。
const LINK_TOKEN: &str = "\u{0}LINK\u{0}";

/// 把 Markdown 渲染成 Telegram HTML（上游 `formatHTML`）。
///
/// 代码块**先**处理（内容必须逐字保真，只做实体转义），其余逐行转换；未闭合的围栏
/// 按"流式快照"渲染已积累的部分（上游注释逐字：节流编辑也要能显示半个代码块）。
#[must_use]
pub fn format_html(markdown: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code_buf: Vec<&str> = Vec::new();
    for line in markdown.split('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_code {
                out.push_str(&render_code_block(&code_lang, &code_buf));
                out.push('\n');
                in_code = false;
                code_buf.clear();
                code_lang.clear();
            } else {
                in_code = true;
                code_lang = trimmed.trim_start_matches("```").to_string();
            }
            continue;
        }
        if in_code {
            code_buf.push(line);
            continue;
        }
        out.push_str(&format_line(line));
        out.push('\n');
    }
    if in_code {
        out.push_str(&render_code_block(&code_lang, &code_buf));
        out.push('\n');
    }
    out.trim_end_matches('\n').to_string()
}

/// 渲染一个代码块（上游 `renderCodeBlock`）：`<pre>` 或带语言类的 `<pre><code>`。
#[must_use]
fn render_code_block(lang: &str, lines: &[&str]) -> String {
    let body = escape_html(&lines.join("\n"));
    if lang.is_empty() {
        return format!("<pre>{body}</pre>");
    }
    format!(
        "<pre><code class=\"language-{}\">{body}</code></pre>",
        escape_html(lang)
    )
}

/// 转换**一行非代码**文本（上游 `formatLine`）：标题 / 无序列表项 / 行内。
#[must_use]
pub fn format_line(line: &str) -> String {
    if let Some(rest) = strip_heading(line) {
        return format!("<b>{}</b>", format_inline(rest));
    }
    if let Some(head) = split_bullet(line) {
        return format!("{}• {}", head.indent, format_inline(head.rest));
    }
    format_inline(line)
}

/// 标题：`^#{1,6}\s+(.*)$` → `Some(标题正文)`。
#[must_use]
fn strip_heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|ch| *ch == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    let spaces = rest.chars().take_while(|ch| ch.is_whitespace()).count();
    if spaces == 0 {
        return None;
    }
    Some(&rest[spaces..])
}

/// 无序列表项的一刀两段（上游 `reBullet` 的两个子匹配）。
struct BulletSplit<'a> {
    /// 捕获组 1：前导空白（逐字保留）。
    indent: &'a str,
    /// 整段匹配之后剩下的正文。
    rest: &'a str,
}

/// 无序列表项：`^(\s*)[-*]\s+`（`\s*` 贪婪 ⇒ 前导空白全吃）。
///
/// 用 `char_indices` 走，避免把多字节空白切在字节边界上（Go 的 `\s` 是 rune 级）。
#[must_use]
fn split_bullet(line: &str) -> Option<BulletSplit<'_>> {
    let mut indent_end = 0;
    for (index, ch) in line.char_indices() {
        if ch.is_whitespace() {
            indent_end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    let marker = line[indent_end..].chars().next()?;
    if marker != '-' && marker != '*' {
        return None;
    }
    let after_marker = indent_end + marker.len_utf8();
    let spaces = line[after_marker..]
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .count();
    if spaces == 0 {
        return None;
    }
    let mut rest_start = after_marker;
    for _ in 0..spaces {
        let ch = line[rest_start..].chars().next()?;
        rest_start += ch.len_utf8();
    }
    Some(BulletSplit {
        indent: &line[..indent_end],
        rest: &line[rest_start..],
    })
}

/// 一行内的样式转换（上游 `formatInline`）：
/// **先后顺序本身就是规范** —— 先摘出行内代码、再摘出链接，然后**整体转义**，
/// 再依次套 `<b>` / `<i>` / `<s>`，最后把两个占位符换回已转义的实体。
#[must_use]
pub fn format_inline(text: &str) -> String {
    let (with_code_tokens, code_spans) = extract_code_spans(text);
    let (with_tokens, links) = extract_links(&with_code_tokens);

    let mut out = escape_html(&with_tokens);
    out = replace_bold(&out);
    out = replace_italic(&out);
    out = replace_strike(&out);

    for link in links {
        let tag = format!(
            "<a href=\"{}\">{}</a>",
            escape_html(&link.url),
            escape_html(&link.label)
        );
        out = out.replacen(LINK_TOKEN, &tag, 1);
    }
    for span in code_spans {
        let tag = format!("<code>{}</code>", escape_html(&span));
        out = out.replacen(CODE_TOKEN, &tag, 1);
    }
    out
}

/// 摘出行内代码跨度（上游 `reInlineCode = `([^`]+)`` + `strings.Trim(m, "`")`）。
#[must_use]
fn extract_code_spans(text: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        match after_open.find('`') {
            // 反引号内侧按定义不含反引号 ⇒ `Trim` 是 no-op，但保留这一步以对齐上游。
            Some(close) if close > 0 => {
                spans.push(after_open[..close].trim_matches('`').to_string());
                out.push_str(CODE_TOKEN);
                rest = &after_open[close + 1..];
            }
            // 空内容（``` ``）或未闭合 ⇒ 这个反引号是字面量，继续往后找。
            _ => {
                out.push('`');
                rest = after_open;
            }
        }
    }
    out.push_str(rest);
    (out, spans)
}

/// 一条链路的两个字段（上游 `reLink` 的捕获组）。
struct LinkSpan {
    label: String,
    url: String,
}

/// 摘出链接（上游 `reLink = \[([^\]]+)\]\(([^)\s]+)\)`）。
#[must_use]
fn extract_links(text: &str) -> (String, Vec<LinkSpan>) {
    let mut out = String::with_capacity(text.len());
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find(']') else {
            out.push('[');
            rest = after_open;
            continue;
        };
        if close == 0 {
            // 空标签（`[]`）⇒ 字面量。
            out.push('[');
            rest = after_open;
            continue;
        }
        let label = &after_open[..close];
        let after_label = &after_open[close + 1..];
        let Some(paren) = after_label.strip_prefix('(') else {
            out.push('[');
            rest = after_open;
            continue;
        };
        let url_end = paren
            .find(|ch: char| ch == ')' || ch.is_whitespace())
            .unwrap_or(paren.len());
        if url_end == 0 || !paren[url_end..].starts_with(')') {
            // URL 为空，或结尾不是 `)` ⇒ 整体是字面量。
            out.push('[');
            rest = after_open;
            continue;
        }
        links.push(LinkSpan {
            label: label.to_string(),
            url: paren[..url_end].to_string(),
        });
        out.push_str(LINK_TOKEN);
        rest = &paren[url_end + 1..];
    }
    out.push_str(rest);
    (out, links)
}

/// 从**字符边界** `from` 之后的**第一个字符**起，找 `needle` 的**最小**出现位置。
///
/// 非贪婪 `.+?` 的等价物：内容至少一个字符 ⇒ 收口必须在 `from` 的下一个字符边界之后。
/// 直接写 `text[from + 1..]` 会在多字节字符上 panic（`中` 占 3 字节），所以这里逐 `char` 走。
fn find_after_char(text: &str, from: usize, needle: &str) -> Option<usize> {
    let mut index = from
        + text
            .get(from..)
            .and_then(|tail| tail.chars().next())
            .map_or(0, char::len_utf8);
    while index <= text.len() {
        if text[index..].starts_with(needle) {
            return Some(index);
        }
        index += text[index..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// 加粗：`\*\*(.+?)\*\*` → `<b>$1</b>`（非贪婪 ⇒ 内容取**最短**可行跨度）。
#[must_use]
fn replace_bold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let Some(open) = text[index..].find("**") else {
            out.push_str(&text[index..]);
            break;
        };
        let open = index + open;
        out.push_str(&text[index..open]);
        if let Some(close) = find_after_char(text, open + 2, "**") {
            out.push_str("<b>");
            out.push_str(&text[open + 2..close]);
            out.push_str("</b>");
            index = close + 2;
        } else {
            // 没有收口 ⇒ 这个 `**` 是字面量，从它之后继续（对齐 regexp 的逐位推进）。
            out.push_str(&text[open..open + 2]);
            index = open + 2;
        }
    }
    out
}

/// 删除线：`~~(.+?)~~` → `<s>$1</s>`。
#[must_use]
fn replace_strike(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let Some(open) = text[index..].find("~~") else {
            out.push_str(&text[index..]);
            break;
        };
        let open = index + open;
        out.push_str(&text[index..open]);
        if let Some(close) = find_after_char(text, open + 2, "~~") {
            out.push_str("<s>");
            out.push_str(&text[open + 2..close]);
            out.push_str("</s>");
            index = close + 2;
        } else {
            out.push_str(&text[open..open + 2]);
            index = open + 2;
        }
    }
    out
}

/// 斜体：`(^|[^*])\*([^*]+?)\*` → `$1<i>$2</i>`。
///
/// 这条模式有个**会被写丢**的细节：**分组 1 吃掉开星号前面的那个字符**（`^` 情况下是零宽）。
/// 所以整段匹配是「前导字符 + `*` + 内容 + `*`」，且前导字符**不能**是 `*`
/// —— 这正是 `**bold**` 已经被换掉之后、剩下的 `*` 才被当斜体的原因。
/// 内容 `[^*]+?` 里不能有 `*` ⇒ 收口就是"内容之后的第一个 `*`"。
#[must_use]
fn replace_italic(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        let ch = text[index..].chars().next().unwrap_or('\0');
        let char_len = ch.len_utf8();
        // 情况 1：`^` + `*`（零宽分组 1）。
        let anchored = index == 0 && ch == '*';
        // 情况 2：`[^*]` + `*`（分组 1 = 那个字符）。
        let prefixed = ch != '*' && bytes.get(index + char_len) == Some(&b'*');
        if anchored || prefixed {
            let star = index + if anchored { 0 } else { char_len };
            let content_start = star + 1;
            if content_start < bytes.len() && bytes.get(content_start) != Some(&b'*') {
                if let Some(close) = find_after_char(text, content_start, "*") {
                    if anchored {
                        out.push_str("<i>");
                    } else {
                        out.push(ch);
                        out.push_str("<i>");
                    }
                    out.push_str(&text[content_start..close]);
                    out.push_str("</i>");
                    index = close + 1;
                    continue;
                }
            }
        }
        out.push(ch);
        index += char_len;
    }
    out
}

#[cfg(test)]
mod tests;

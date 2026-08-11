//! Text-based issue reference extraction.
//!
//! 与 Node `packages/shared/src/issue-references.ts` 严格对齐：
//! - 识别 `[A-Z][A-Z0-9]*-\\d+` 形式（`PAP-123`）
//! - 识别 `http(s)://...` 与 `/issues/...` 链接
//! - 跳过 markdown code block（fenced ``` 与 inline `）内的引用
//! - trim 尾部标点（`,.!?;:)]`）

use regex::Regex;
use std::sync::LazyLock;

/// 匹配 issue 标识符 `PAP-123` 形式。
pub static ISSUE_REFERENCE_IDENTIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][A-Z0-9]*-\d+$").unwrap());

/// 匹配 token：URL / 路径 / 标识符。
static ISSUE_REFERENCE_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s<>()]+|\/[^\s<>()]+|[A-Z][A-Z0-9]*-\d+").unwrap());

/// 一个提取出的引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifierMatch {
    pub index: usize,
    pub length: usize,
    pub identifier: String,
    pub matched_text: String,
}

/// 把字符串归一化为合法标识符，否则返回 None。
pub fn normalize_identifier(value: &str) -> Option<String> {
    let trimmed = value.trim().to_uppercase();
    if ISSUE_REFERENCE_IDENTIFIER_RE.is_match(&trimmed) {
        Some(trimmed)
    } else {
        None
    }
}

/// 从 markdown 中剔除 code block（fenced ``` 与 inline `）。
pub fn strip_markdown_code(markdown: &str) -> String {
    let mut output = String::with_capacity(markdown.len());
    let bytes = markdown.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let at_line_start = i == 0 || bytes[i - 1] == b'\n';

        // fenced code block ``` 或 ~~~
        if at_line_start && (bytes[i..].starts_with(b"```") || bytes[i..].starts_with(b"~~~")) {
            let fence_len = if bytes[i..].starts_with(b"```") {
                bytes[i..].iter().take_while(|&&c| c == b'`').count()
            } else {
                bytes[i..].iter().take_while(|&&c| c == b'~').count()
            };
            let fence = &markdown[i..i + fence_len];
            let block_start = i;
            i += fence_len;
            // skip rest of opening line
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // newline
            }
            // find closing fence
            while i < bytes.len() {
                let line_start = i == 0 || bytes[i - 1] == b'\n';
                if line_start && markdown[i..].starts_with(fence) {
                    i += fence_len;
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                    break;
                }
                i += 1;
            }
            // preserve newlines, replace other chars with space
            for ch in markdown[block_start..i].chars() {
                if ch == '\n' {
                    output.push('\n');
                } else {
                    output.push(' ');
                }
            }
            continue;
        }

        // inline code `
        if bytes[i] == b'`' {
            let tick_count = bytes[i..].iter().take_while(|&&c| c == b'`').count();
            let inline_start = i;
            i += tick_count;
            let close = markdown[i..].find(&"`".repeat(tick_count));
            if let Some(close_offset) = close {
                i += close_offset + tick_count;
                for ch in markdown[inline_start..i].chars() {
                    if ch == '\n' {
                        output.push('\n');
                    } else {
                        output.push(' ');
                    }
                }
                continue;
            } else {
                output.push_str(&markdown[inline_start..inline_start + tick_count]);
                i = inline_start + tick_count;
                continue;
            }
        }

        // pass through (UTF-8 safe via char_indices)
        if let Some((offset, ch)) = markdown[i..].char_indices().next() {
            output.push(ch);
            i += offset + ch.len_utf8();
        } else {
            i += 1;
        }
    }
    output
}

/// trim 尾部标点（`. , ! ? ; : ) ]`），保留括号配对的尾部。
fn trim_trailing_punctuation(token: &str) -> String {
    let mut trimmed: String = token.to_string();
    while !trimmed.is_empty() {
        let last = trimmed.chars().last().unwrap();
        let stripped = matches!(last, '.' | ',' | '!' | '?' | ';' | ':');
        let unbalanced = (last == ')' || last == ']') && {
            if last == ')' {
                trimmed.matches('(').count() < trimmed.matches(')').count()
            } else {
                trimmed.matches('[').count() < trimmed.matches(']').count()
            }
        };
        if !stripped && !unbalanced {
            break;
        }
        trimmed.pop();
    }
    trimmed
}

/// 把 `https://.../issues/pap-123` 或 `/issues/pap-123` 解析为 identifier。
pub fn parse_issue_href(href: &str) -> Option<String> {
    let raw = href.trim();
    if raw.is_empty() {
        return None;
    }
    // 简易 path 提取：找 `/issues/<identifier>`
    let after_slash = raw.find("/issues/")?;
    let rest = &raw[after_slash + "/issues/".len()..];
    // 取下一个 / 或 ? 或 # 之前的部分
    let end = rest
        .find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let id = &rest[..end];
    normalize_identifier(id)
}

/// 从 text 中查找所有匹配 token（含 URL / 路径 / identifier）。
fn find_token_matches(text: &str) -> Vec<IdentifierMatch> {
    let mut out = Vec::new();
    for m in ISSUE_REFERENCE_TOKEN_RE.find_iter(text) {
        let raw_token = m.as_str();
        let cleaned = trim_trailing_punctuation(raw_token);
        if cleaned.is_empty() {
            continue;
        }
        let identifier = normalize_identifier(&cleaned).or_else(|| parse_issue_href(&cleaned));
        let identifier = match identifier {
            Some(id) => id,
            None => continue,
        };
        out.push(IdentifierMatch {
            index: m.start(),
            length: cleaned.len(),
            identifier,
            matched_text: cleaned,
        });
    }
    out
}

/// 抽取去重后的 identifiers（顺序保留）。
pub fn extract_identifiers(markdown: &str) -> Vec<String> {
    let scrubbed = strip_markdown_code(markdown);
    let mut seen = std::collections::HashSet::new();
    let mut ordered = Vec::new();
    for m in find_token_matches(&scrubbed) {
        if seen.insert(m.identifier.clone()) {
            ordered.push(m.identifier);
        }
    }
    ordered
}

/// 抽取所有匹配（含 index / length / matched_text）。
pub fn extract_matches(markdown: &str) -> Vec<IdentifierMatch> {
    let scrubbed = strip_markdown_code(markdown);
    let mut seen = std::collections::HashSet::new();
    let mut ordered = Vec::new();
    for m in find_token_matches(&scrubbed) {
        if seen.insert(m.identifier.clone()) {
            ordered.push(m);
        }
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_identifier_basic() {
        assert_eq!(normalize_identifier("pap-1"), Some("PAP-1".to_string()));
        assert_eq!(normalize_identifier("PAP-123"), Some("PAP-123".to_string()));
        assert_eq!(normalize_identifier("  ABC-9 "), Some("ABC-9".to_string()));
        assert_eq!(normalize_identifier("nope"), None);
        assert_eq!(normalize_identifier("PAP-"), None);
        assert_eq!(normalize_identifier("1PAP-1"), None);
    }

    #[test]
    fn parse_issue_href_basic() {
        assert_eq!(parse_issue_href("/issues/pap-1"), Some("PAP-1".to_string()));
        assert_eq!(
            parse_issue_href("https://example.com/issues/pap-1"),
            Some("PAP-1".to_string())
        );
        assert_eq!(
            parse_issue_href("https://example.com/issues/pap-1?foo=bar"),
            Some("PAP-1".to_string())
        );
        assert_eq!(parse_issue_href("/something/else"), None);
    }

    #[test]
    fn trim_trailing_punctuation_basic() {
        assert_eq!(trim_trailing_punctuation("PAP-1,"), "PAP-1");
        assert_eq!(trim_trailing_punctuation("PAP-1."), "PAP-1");
        assert_eq!(trim_trailing_punctuation("(PAP-1)"), "(PAP-1)");
        assert_eq!(trim_trailing_punctuation("PAP-1)"), "PAP-1");
    }

    #[test]
    fn extract_identifiers_basic() {
        assert_eq!(extract_identifiers("hello PAP-1 world"), vec!["PAP-1"]);
        assert_eq!(
            extract_identifiers("PAP-1 and PAP-2"),
            vec!["PAP-1", "PAP-2"]
        );
        assert_eq!(extract_identifiers("no refs here"), Vec::<String>::new());
    }

    #[test]
    fn extract_identifiers_dedup() {
        assert_eq!(
            extract_identifiers("PAP-1 [link](/issues/pap-1) PAP-1"),
            vec!["PAP-1"]
        );
    }

    #[test]
    fn extract_identifiers_strips_fenced_code() {
        let md = "PAP-1 \n```\nPAP-2\n```\nPAP-3";
        assert_eq!(extract_identifiers(md), vec!["PAP-1", "PAP-3"]);
    }

    #[test]
    fn extract_identifiers_strips_inline_code() {
        let md = "PAP-1 `PAP-2` PAP-3";
        assert_eq!(extract_identifiers(md), vec!["PAP-1", "PAP-3"]);
    }

    #[test]
    fn extract_identifiers_handles_href() {
        assert_eq!(
            extract_identifiers("see [this](/issues/pap-1)"),
            vec!["PAP-1"]
        );
    }

    #[test]
    fn extract_identifiers_preserves_order() {
        assert_eq!(
            extract_identifiers("PAP-3 PAP-1 PAP-2"),
            vec!["PAP-3", "PAP-1", "PAP-2"]
        );
    }
}

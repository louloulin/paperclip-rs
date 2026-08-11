#![forbid(unsafe_code)]

//! Issue reference parsing for cross-referencing issues from text/markdown.
//!
//! R528: Direct port of `paperclip/packages/shared/src/issue-references.ts`.
//!
//! 设计原则:
//! - 所有 pub fn 都是纯函数 (无 IO, 无副作用, 无环境依赖)
//! - regex 编译成 `Lazy<Regex>` 一次, 后续零成本
//! - 接受 `&str`, 返回 `String` 或 plain `Option` / `Vec`
//! - `url` crate 仅用于 `parseIssueReferenceHref`, 不引入额外业务依赖
//!
//! 范围 (本 crate):
//! - [`ISSUE_REFERENCE_IDENTIFIER_RE`] / [`ISSUE_REFERENCE_TOKEN_RE`] regex
//! - [`normalize_issue_identifier`] — `"pap-123"` → `"PAP-123"` 或 None
//! - [`build_issue_reference_href`] — `"PAP-123"` → `"/issues/PAP-123"`
//! - [`parse_issue_reference_href`] — `/issues/PAP-123` → `{ identifier }`
//! - [`find_issue_reference_matches`] — 纯文本里找出所有引用
//! - [`extract_issue_reference_identifiers`] — markdown 去重抽取 identifier 列表
//! - [`extract_issue_reference_matches`] — markdown 去重抽取完整 match
//!
//! **不** 范围 (留给集成层):
//! - `server/src/services/issue-references.ts` 的 DB 持久化 (issueReferenceService)
//! - UI `src/lib/issue-reference.ts` (TS 端保留, 不 port 到 Rust — UI 是冻结契约)
//!
//! Node 上游 URL 解析用 `URL` constructor + try/catch; Rust 用 `url::Url::parse`,
//! 失败时 `parse_issue_reference_href` 返回 `None` (与上游语义一致)。

use once_cell::sync::Lazy;
use regex::Regex;
use url::Url;

/// Regex matching a bare issue identifier like `PAP-123` or `PC1A2-7`.
///
/// Mirrors Node upstream `ISSUE_REFERENCE_IDENTIFIER_RE`.
pub static ISSUE_REFERENCE_IDENTIFIER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Z][A-Z0-9]*-\d+$").expect("valid regex pattern"));

/// Regex matching any of: bare identifier, absolute URL, or relative path.
///
/// Mirrors Node upstream `ISSUE_REFERENCE_TOKEN_RE` (`g` + `i` flags).
/// `i` is critical — without it `pap-123` (lowercase) would not match.
pub static ISSUE_REFERENCE_TOKEN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)https?://[^\s<>()]+|/[^\s<>()]+|[A-Z][A-Z0-9]*-\d+")
        .expect("valid regex pattern")
});

/// A single issue-reference match extracted from text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueReferenceMatch {
    /// Byte offset in the original (cleaned) input.
    pub index: usize,
    /// Byte length of the cleaned token (after trailing punctuation is stripped).
    pub length: usize,
    /// Normalized uppercase identifier (`"PAP-123"`).
    pub identifier: String,
    /// Original matched text (cleaned of trailing punctuation).
    pub matched_text: String,
}

/// Normalize a candidate identifier to its canonical form (`PAP-123`).
///
/// - Trims surrounding whitespace
/// - Converts to uppercase
/// - Validates against [`ISSUE_REFERENCE_IDENTIFIER_RE`]
///
/// Returns `None` if the value is not a valid identifier shape.
#[must_use]
pub fn normalize_issue_identifier(value: &str) -> Option<String> {
    let trimmed = value.trim().to_uppercase();
    if ISSUE_REFERENCE_IDENTIFIER_RE.is_match(&trimmed) {
        Some(trimmed)
    } else {
        None
    }
}

/// Build a canonical issue href `/issues/<NORMALIZED>`.
///
/// Falls back to the trimmed raw identifier when normalization fails
/// (mirrors Node upstream behaviour: `normalized ?? identifier.trim()`).
#[must_use]
pub fn build_issue_reference_href(identifier: &str) -> String {
    let normalized = normalize_issue_identifier(identifier);
    let fallback = identifier.trim();
    match normalized {
        Some(n) => format!("/issues/{n}"),
        None => format!("/issues/{fallback}"),
    }
}

/// Parse an issue href (relative or absolute) and extract its identifier.
///
/// Accepts both `/issues/PAP-123` and `https://paperclip.ing/PAP/issues/pap-789`.
///
/// Returns `None` when the URL cannot be parsed or the path doesn't contain
/// an `issues/<id>` segment.
#[must_use]
pub fn parse_issue_reference_href(href: &str) -> Option<IssueIdentifierRef> {
    let raw = href.trim();
    if raw.is_empty() {
        return None;
    }

    let url = if raw.starts_with('/') {
        Url::parse(&format!("https://paperclip.invalid{raw}")).ok()?
    } else {
        Url::parse(raw).ok()?
    };

    let segments: Vec<&str> = url
        .path()
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    for index in 0..segments.len().saturating_sub(1) {
        let segment = segments[index];
        if !segment.eq_ignore_ascii_case("issues") {
            continue;
        }
        let candidate = segments[index + 1];
        if let Some(identifier) = normalize_issue_identifier(candidate) {
            return Some(IssueIdentifierRef { identifier });
        }
    }

    None
}

/// Bare identifier ref returned by [`parse_issue_reference_href`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueIdentifierRef {
    pub identifier: String,
}

/// Find all issue-reference tokens in arbitrary text.
///
/// - Skips empty input
/// - Cleans each match of trailing `.,!?;:)`
/// - Validates each cleaned token via [`normalize_issue_identifier`] or
///   [`parse_issue_reference_href`] (for URL/path tokens)
/// - Returns matches in source order
#[must_use]
pub fn find_issue_reference_matches(text: &str) -> Vec<IssueReferenceMatch> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut matches: Vec<IssueReferenceMatch> = Vec::new();
    for caps in ISSUE_REFERENCE_TOKEN_RE.find_iter(text) {
        let raw_token = caps.as_str();
        let cleaned_token = trim_trailing_punctuation(raw_token);
        if cleaned_token.is_empty() {
            continue;
        }

        let identifier = normalize_issue_identifier(&cleaned_token)
            .or_else(|| parse_issue_reference_href(&cleaned_token).map(|r| r.identifier));

        if let Some(identifier) = identifier {
            matches.push(IssueReferenceMatch {
                index: caps.start(),
                length: cleaned_token.len(),
                identifier,
                matched_text: cleaned_token.to_string(),
            });
        }
    }
    matches
}

/// Extract unique issue identifiers from markdown, deduplicated by first-seen order.
///
/// Markdown code spans (inline `` ` `` and fenced `` ``` ``) are stripped before
/// scanning, so references inside code blocks are ignored.
#[must_use]
pub fn extract_issue_reference_identifiers(markdown: &str) -> Vec<String> {
    let scrubbed = strip_markdown_code(markdown);
    let mut seen = std::collections::HashSet::new();
    let mut ordered: Vec<String> = Vec::new();

    for m in find_issue_reference_matches(&scrubbed) {
        if seen.insert(m.identifier.clone()) {
            ordered.push(m.identifier);
        }
    }
    ordered
}

/// Extract unique issue reference matches from markdown, deduplicated by identifier.
///
/// Identical in semantics to [`extract_issue_reference_identifiers`] but returns
/// full [`IssueReferenceMatch`] records (with byte offsets and matched text).
#[must_use]
pub fn extract_issue_reference_matches(markdown: &str) -> Vec<IssueReferenceMatch> {
    let scrubbed = strip_markdown_code(markdown);
    let mut seen = std::collections::HashSet::new();
    let mut ordered: Vec<IssueReferenceMatch> = Vec::new();

    for m in find_issue_reference_matches(&scrubbed) {
        if seen.insert(m.identifier.clone()) {
            ordered.push(m);
        }
    }
    ordered
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Remove trailing `.,!?;:)` characters, with parentheses-aware trimming.
///
/// Mirrors Node upstream `trimTrailingPunctuation`.
/// Trim trailing `.,!?;:)` characters from a token, with parentheses-aware trimming.
///
/// Public so other crates (e.g. `pc-external-objects-server`) can reuse.
/// Mirrors Node upstream `trimTrailingPunctuation`.
pub fn trim_trailing_punctuation(token: &str) -> String {
    const PUNCT: &[char] = &['.', ',', '!', '?', ';', ':', ')'];
    let mut trimmed = token.to_string();
    while let Some(last) = trimmed.chars().last() {
        let is_close_paren = last == ')';
        let is_close_bracket = last == ']';

        if !PUNCT.contains(&last) && !is_close_bracket {
            break;
        }

        if is_close_paren {
            let open = trimmed.matches('(').count();
            let close = trimmed.matches(')').count();
            if open >= close {
                break;
            }
        }
        if is_close_bracket {
            let open = trimmed.matches('[').count();
            let close = trimmed.matches(']').count();
            if open >= close {
                break;
            }
        }
        trimmed.pop();
    }
    trimmed
}

/// Replace every non-newline character with a single space, preserving line breaks.
/// Replace every non-newline character with a single space, preserving line breaks.
///
/// Public for reuse; mirrors Node upstream `preserveNewlinesAsWhitespace`.
pub fn preserve_newlines_as_whitespace(value: &str) -> String {
    value
        .chars()
        .map(|c| if c == '\n' { '\n' } else { ' ' })
        .collect()
}

/// Strip inline `` ` `` and fenced `` ``` `` code blocks from markdown, replacing
/// their bytes with whitespace (preserving line structure).
///
/// Mirrors Node upstream `stripMarkdownCode`.
/// Strip inline `` ` `` and fenced `` ``` `` code blocks from markdown, replacing
/// their bytes with whitespace (preserving line structure).
///
/// Public so other crates (e.g. `pc-external-objects-server`) can reuse.
/// Mirrors Node upstream `stripMarkdownCode`.
pub fn strip_markdown_code(markdown: &str) -> String {
    if markdown.is_empty() {
        return String::new();
    }

    let bytes = markdown.as_bytes();
    let mut output: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        let remaining = &markdown[index..];

        // Try fenced code block (``` or ~~~) at line start
        let at_line_start = index == 0 || bytes[index - 1] == b'\n';
        if at_line_start {
            if let Some((fence_char, fence_len)) = detect_fence_opener(remaining) {
                let fence: String = std::iter::repeat(fence_char).take(fence_len).collect();
                let block_start = index;
                index += fence_len;
                // Skip until end of opening fence line
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                if index < bytes.len() {
                    index += 1; // consume newline
                }
                // Scan until matching fence at line start
                while index < bytes.len() {
                    let line_start = index == 0 || bytes[index - 1] == b'\n';
                    if line_start && markdown[index..].starts_with(&fence) {
                        index += fence_len;
                        while index < bytes.len() && bytes[index] != b'\n' {
                            index += 1;
                        }
                        if index < bytes.len() {
                            index += 1;
                        }
                        break;
                    }
                    index += 1;
                }
                let block_end = index;
                let preserved = preserve_newlines_as_whitespace(&markdown[block_start..block_end]);
                output.extend_from_slice(preserved.as_bytes());
                continue;
            }
        }

        // Inline code: run of backticks
        if bytes[index] == b'`' {
            let mut tick_count = 1;
            while index + tick_count < bytes.len() && bytes[index + tick_count] == b'`' {
                tick_count += 1;
            }
            let inline_start = index;
            index += tick_count;
            let close_index = scan_for_backtick_run(&markdown[index..], tick_count);
            let end_pos = match close_index {
                Some(p) => index + p + tick_count,
                None => {
                    // No closing fence — preserve as literal
                    output.extend_from_slice(&bytes[inline_start..inline_start + tick_count]);
                    index = inline_start + tick_count;
                    continue;
                }
            };
            let preserved = preserve_newlines_as_whitespace(&markdown[inline_start..end_pos]);
            output.extend_from_slice(preserved.as_bytes());
            index = end_pos;
            continue;
        }

        output.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(output).unwrap_or_else(|_| markdown.to_string())
}

/// Detect a fenced code block opener (3+ backticks or 3+ tildes) at the
/// start of `s`. Returns the fence's character and its byte length.
///
/// Public for reuse.
#[allow(dead_code)]
/// start of `s`. Returns the fence's character and its byte length.
fn detect_fence_opener(s: &str) -> Option<(char, usize)> {
    let bytes = s.as_bytes();
    let first = *bytes.first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let fence_char = first as char;
    let len = bytes.iter().take_while(|&&b| b == first).count();
    if len >= 3 {
        Some((fence_char, len))
    } else {
        None
    }
}

/// Scan `s` for a run of exactly `n` backticks (n >= 1), return byte offset.
///
/// Public for reuse.
pub fn scan_for_backtick_run(s: &str, n: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + n <= bytes.len() {
        if bytes[i] == b'`' {
            let mut run = 1;
            while i + run < bytes.len() && bytes[i + run] == b'`' {
                run += 1;
            }
            if run == n {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r528_normalize_uppercases_valid_identifiers() {
        assert_eq!(
            normalize_issue_identifier("pap-123"),
            Some("PAP-123".to_string())
        );
        assert_eq!(
            normalize_issue_identifier("pc1a2-7"),
            Some("PC1A2-7".to_string())
        );
    }

    #[test]
    fn r528_normalize_trims_whitespace() {
        assert_eq!(
            normalize_issue_identifier("  pap-1  "),
            Some("PAP-1".to_string())
        );
    }

    #[test]
    fn r528_normalize_rejects_invalid() {
        assert_eq!(normalize_issue_identifier("not-an-issue"), None);
        assert_eq!(normalize_issue_identifier(""), None);
        assert_eq!(normalize_issue_identifier("123"), None);
        assert_eq!(normalize_issue_identifier("pap_123"), None); // underscore not allowed
        assert_eq!(normalize_issue_identifier("1ABC-1"), None); // must start with letter
        assert_eq!(normalize_issue_identifier("PAP"), None); // no dash
        assert_eq!(normalize_issue_identifier("PAP-"), None); // no number
        assert_eq!(normalize_issue_identifier("-123"), None);
    }

    #[test]
    fn r528_build_href_canonical_form() {
        assert_eq!(build_issue_reference_href("pap-123"), "/issues/PAP-123");
        assert_eq!(build_issue_reference_href("PAP-1"), "/issues/PAP-1");
    }

    #[test]
    fn r528_build_href_falls_back_on_invalid() {
        // Invalid identifier still produces a path (trimmed)
        assert_eq!(
            build_issue_reference_href(" not-valid "),
            "/issues/not-valid"
        );
    }

    #[test]
    fn r528_parse_relative_href() {
        assert_eq!(
            parse_issue_reference_href("/issues/PAP-123"),
            Some(IssueIdentifierRef {
                identifier: "PAP-123".to_string()
            })
        );
        assert_eq!(
            parse_issue_reference_href("/PAP/issues/pap-456"),
            Some(IssueIdentifierRef {
                identifier: "PAP-456".to_string()
            })
        );
    }

    #[test]
    fn r528_parse_absolute_href_with_fragment() {
        assert_eq!(
            parse_issue_reference_href("https://paperclip.ing/PAP/issues/pap-789#comment-1"),
            Some(IssueIdentifierRef {
                identifier: "PAP-789".to_string()
            })
        );
    }

    #[test]
    fn r528_parse_rejects_non_issue_path() {
        assert_eq!(
            parse_issue_reference_href("https://paperclip.ing/projects/PAP-789"),
            None
        );
        assert_eq!(parse_issue_reference_href(""), None);
        assert_eq!(parse_issue_reference_href("   "), None);
    }

    #[test]
    fn r528_parse_rejects_malformed_url() {
        assert_eq!(parse_issue_reference_href("not a url"), None);
        assert_eq!(parse_issue_reference_href("ht://bad"), None);
    }

    #[test]
    fn r528_find_matches_plain_text() {
        let text = "See PAP-1, /issues/PC1A2-2, and https://x.test/PAP/issues/pc1a2-3.";
        let matches = find_issue_reference_matches(text);
        assert_eq!(
            matches,
            vec![
                IssueReferenceMatch {
                    index: 4,
                    length: 5,
                    identifier: "PAP-1".to_string(),
                    matched_text: "PAP-1".to_string(),
                },
                IssueReferenceMatch {
                    index: 11,
                    length: 15,
                    identifier: "PC1A2-2".to_string(),
                    matched_text: "/issues/PC1A2-2".to_string(),
                },
                IssueReferenceMatch {
                    index: 32,
                    length: 33,
                    identifier: "PC1A2-3".to_string(),
                    matched_text: "https://x.test/PAP/issues/pc1a2-3".to_string(),
                },
            ]
        );
    }

    #[test]
    fn r528_find_matches_trims_trailing_bracket() {
        let matches = find_issue_reference_matches("See /issues/PAP-123] for context.");
        assert_eq!(
            matches,
            vec![IssueReferenceMatch {
                index: 4,
                length: 15,
                identifier: "PAP-123".to_string(),
                matched_text: "/issues/PAP-123".to_string(),
            }]
        );
    }

    #[test]
    fn r528_find_matches_does_not_capture_outer_parens() {
        // The regex `[A-Z][A-Z0-9]*-\d+` doesn't match `(` (negated char class),
        // so we capture just `PAP-1` not `(PAP-1)`. The surrounding `(` / `)`
        // remain in the text but are not part of the matched_token.
        let matches = find_issue_reference_matches("text (PAP-1) more");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].identifier, "PAP-1");
        assert_eq!(matches[0].matched_text, "PAP-1");
        assert_eq!(matches[0].length, 5);
        assert_eq!(matches[0].index, 6); // position of `P` in "PAP-1"
    }

    #[test]
    fn r528_find_matches_parens_unbalanced_trims() {
        // `PAP-1)` — unbalanced trailing `)` is trimmed
        let matches = find_issue_reference_matches("text PAP-1) more");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "PAP-1");
        assert_eq!(matches[0].length, 5);
    }

    #[test]
    fn r528_find_matches_handles_unbalanced_paren() {
        // `PAP-1)` — unbalanced, trim right
        let matches = find_issue_reference_matches("text PAP-1) more");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "PAP-1");
        assert_eq!(matches[0].length, 5);
    }

    #[test]
    fn r528_find_matches_handles_unbalanced_bracket() {
        let matches = find_issue_reference_matches("text PAP-1] more");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "PAP-1");
    }

    #[test]
    fn r528_find_matches_empty_input() {
        let matches = find_issue_reference_matches("");
        assert!(matches.is_empty());
    }

    #[test]
    fn r528_extract_dedupes_identifiers() {
        assert_eq!(
            extract_issue_reference_identifiers("PAP-1 [again](/issues/pap-1) PAP-2"),
            vec!["PAP-1".to_string(), "PAP-2".to_string()]
        );
    }

    #[test]
    fn r528_extract_skips_inline_code_and_fenced_blocks() {
        let markdown = [
            "Use PAP-1 here.",
            "",
            "`PAP-2` should not count.",
            "",
            "```md",
            "PAP-3",
            "/issues/PAP-4",
            "```",
            "",
            "Final /issues/PAP-5 mention.",
        ]
        .join("\n");

        assert_eq!(
            extract_issue_reference_identifiers(&markdown),
            vec!["PAP-1".to_string(), "PAP-5".to_string()]
        );
    }

    #[test]
    fn r528_extract_matches_dedupes() {
        let markdown = "PAP-1 PAP-1 /issues/PAP-1 https://x.test/PAP/issues/pap-1";
        let matches = extract_issue_reference_matches(markdown);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].identifier, "PAP-1");
    }

    #[test]
    fn r528_strip_inline_code_preserves_lines() {
        // The newline structure should remain (newlines kept, other chars replaced with space)
        let stripped = strip_markdown_code("hello `PAP-1` world\nPAP-2");
        // Backtick run replaced with whitespace of equal length, newlines preserved
        assert_eq!(stripped, "hello         world\nPAP-2");
    }

    #[test]
    fn r528_strip_fenced_with_tilde_fence() {
        let markdown = "~~~md\nPAP-1\n~~~\nPAP-2";
        let ids = extract_issue_reference_identifiers(markdown);
        assert_eq!(ids, vec!["PAP-2".to_string()]);
    }

    #[test]
    fn r528_strip_unmatched_inline_code_keeps_literal() {
        let stripped = strip_markdown_code("`PAP-1 rest of line");
        // No closing backtick — literal preserved
        assert!(stripped.contains("PAP-1"));
    }

    #[test]
    fn r528_strip_empty_input() {
        assert_eq!(strip_markdown_code(""), "");
    }
}

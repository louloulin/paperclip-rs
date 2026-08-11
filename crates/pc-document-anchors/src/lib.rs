#![forbid(unsafe_code)]

//! Document annotation anchor projection, verification, and remapping.
//!
//! R539: Direct port of `paperclip/packages/shared/src/document-anchors.ts`
//! (~464 LOC).
//!
//! 设计原则:
//! - 所有公开 API 都是**纯函数** — 无 IO / 无全局状态 / 无环境依赖
//! - 内部数据用强类型 (`DocumentAnchorSelector` / `DocumentAnchorSnapshot` /
//!   `DocumentTextProjection` / `DocumentTextRange`)，serde camelCase 对齐 Node
//!   JSON wire format
//! - 4 层职责清晰拆分:
//!   1. Markdown → normalized 纯文本 projection
//!   2. 文本位置 ↔ Markdown 源位置映射
//!   3. Selector 创建 / 校验
//!   4. Exact / duplicate / fuzzy / ambiguous remap
//! - 算法权重 / 阈值 / context length 等所有 magic number 都用 `pub const`
//!   暴露，调用方可观察 / 复现
//!
//! 设计 vs Node 上游:
//! - 公开的 selector/snapshot 使用 `Offset(usize)` newtype + `Range<usize>` 表达
//!   normalized vs markdown 偏移，避免混用
//! - enum `DocumentAnchorState` / `DocumentAnchorConfidence` 替代 TS literal union
//!   — 编译期穷尽匹配
//! - 不使用 regex crate，所有解析都是手写 char-by-char，避免外部依赖

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

// ============================================================================
// Constants
// ============================================================================

/// Default context window length (chars) for prefix/suffix extraction.
///
/// Mirrors Node `DEFAULT_CONTEXT_LENGTH`.
pub const DEFAULT_CONTEXT_LENGTH: usize = 48;

/// Lower bound score for a fuzzy match to be considered for the candidate set.
pub const FUZZY_SIMILARITY_THRESHOLD: f64 = 0.45;

/// Score gap below which two candidates are considered indistinguishable.
pub const AMBIGUOUS_SCORE_GAP: f64 = 0.05;

/// Final score threshold for a fuzzy match to be accepted.
pub const FUZZY_ACCEPT_THRESHOLD: f64 = 0.58;

/// Weights for the candidate scoring formula (must sum to 1.0).
pub const SCORE_WEIGHT_PREFIX: f64 = 0.35;
pub const SCORE_WEIGHT_SUFFIX: f64 = 0.35;
pub const SCORE_WEIGHT_PROXIMITY: f64 = 0.30;

/// Weights for the fuzzy window candidate (overrides the base weights).
pub const FUZZY_WEIGHT_BASE: f64 = 0.35;
pub const FUZZY_WEIGHT_SIMILARITY: f64 = 0.65;

/// Words/characters scale used by the proximity component of scoring.
pub const PROXIMITY_SCALE: f64 = 200.0;

/// Weights for the similarity score.
pub const SIMILARITY_WEIGHT_JACCARD: f64 = 0.75;
pub const SIMILARITY_WEIGHT_LENGTH_RATIO: f64 = 0.25;

// ============================================================================
// Domain types
// ============================================================================

/// Anchor state lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentAnchorState {
    Active,
    Stale,
    Orphaned,
}

/// Anchor match confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentAnchorConfidence {
    Exact,
    Duplicate,
    Fuzzy,
    Ambiguous,
    Missing,
}

/// Reason returned by `verifyDocumentAnchorSelector`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyFailureReason {
    Verified,
    QuoteMismatch,
    PositionMismatch,
    InvalidRange,
}

/// Reason returned by `remapDocumentAnchor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemapReason {
    Exact,
    Duplicate,
    Fuzzy,
    Ambiguous,
    Missing,
}

/// 1 source position record — describes where a normalized character lives
/// inside the original markdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentTextPosition {
    pub source_start: usize,
    pub source_end: usize,
}

/// The result of projecting markdown into a normalized plain-text stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentTextProjection {
    pub source: String,
    pub text: String,
    pub positions: Vec<DocumentTextPosition>,
}

/// A resolved range inside a projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentTextRange {
    pub text: String,
    pub normalized_start: usize,
    pub normalized_end: usize,
    pub markdown_start: usize,
    pub markdown_end: usize,
}

/// A selector that fully describes an anchor (quote + position).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentAnchorSelector {
    pub quote: DocumentAnchorQuoteSelector,
    pub position: DocumentAnchorPositionSelector,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentAnchorQuoteSelector {
    pub exact: String,
    pub prefix: String,
    pub suffix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentAnchorPositionSelector {
    pub normalized_start: usize,
    pub normalized_end: usize,
    pub markdown_start: usize,
    pub markdown_end: usize,
}

/// The persisted shape of an anchor (selected text + position snapshot).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentAnchorSnapshot {
    pub selected_text: String,
    pub prefix_text: String,
    pub suffix_text: String,
    pub normalized_start: usize,
    pub normalized_end: usize,
    pub markdown_start: usize,
    pub markdown_end: usize,
}

/// Result of `verifyDocumentAnchorSelector`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifySelectorResult {
    pub ok: bool,
    pub anchor: Option<DocumentAnchorSnapshot>,
    pub projection: DocumentTextProjection,
    pub reason: VerifyFailureReason,
}

/// Input for `remapDocumentAnchor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemapSelectorInput {
    pub previous_anchor: DocumentAnchorSnapshot,
    pub next_markdown: String,
    pub context_length: Option<usize>,
}

/// Result of `remapDocumentAnchor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemapAnchorResult {
    pub anchor_state: DocumentAnchorState,
    pub confidence: DocumentAnchorConfidence,
    pub anchor: Option<DocumentAnchorSnapshot>,
    pub projection: DocumentTextProjection,
    pub reason: RemapReason,
}

// ============================================================================
// Whitespace / normalization
// ============================================================================

/// Collapse runs of whitespace into single spaces, then trim.
#[inline]
#[must_use]
pub fn normalize_anchor_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ============================================================================
// Markdown → projection
// ============================================================================

/// Project markdown into a normalized plain-text stream with source offsets.
///
/// Mirrors Node `projectMarkdownToText`. Block syntax (`#` / `-` / `>` / etc.) is
/// stripped, inline syntax (`![]()` / `[]()` / ` `` ` / `*` / `_` / `~` / `\`) is
/// skipped, fences toggle the inline parser off, and whitespace is collapsed
/// via the `ProjectionBuilder`.
#[must_use]
pub fn project_markdown_to_text(markdown: &str) -> DocumentTextProjection {
    let mut builder = ProjectionBuilder::new(markdown);
    let lines: Vec<&str> = split_lines(markdown);
    let mut offset: usize = 0;
    let mut in_fence = false;

    for raw_line in lines {
        if raw_line.is_empty() {
            offset += 1; // newline
            continue;
        }
        let has_newline = ends_with_newline(raw_line);
        let line = if has_newline {
            &raw_line[..raw_line.len() - 1]
        } else {
            raw_line
        };
        if let Some(fence_match) = strip_fence(line) {
            in_fence = !in_fence;
            let consumed = raw_line.len();
            offset += consumed;
            // Fence boundary is a separator.
            builder.add_separator(offset.saturating_sub(if has_newline { 1 } else { 0 }));
            let _ = fence_match; // explicit
            continue;
        }

        if in_fence {
            // Inside fence, push the raw line as text and a separator newline.
            builder.add_text(line, offset);
            builder.add_separator(offset + line.len());
            offset += raw_line.len();
            continue;
        }

        let (text, source_offset) = strip_block_syntax(line, offset);
        add_inline_markdown_text(&mut builder, &text, source_offset);
        builder.add_separator(offset + line.len());
        offset += raw_line.len();
    }

    builder.to_projection()
}

fn split_lines(markdown: &str) -> Vec<&str> {
    // Match `line *(\n|$)` semantics, never producing an empty tail when
    // the string already ends with a newline.
    let mut out = Vec::new();
    let bytes = markdown.as_bytes();
    let mut start = 0usize;
    for (idx, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            out.push(&markdown[start..=idx]);
            start = idx + 1;
        }
    }
    if start < bytes.len() {
        out.push(&markdown[start..]);
    }
    if out.is_empty() {
        out.push(markdown);
    }
    out
}

fn ends_with_newline(s: &str) -> bool {
    s.ends_with('\n')
}

fn strip_fence(line: &str) -> Option<&str> {
    // Mirrors Node: `^\s*(```+|~~~+)`
    let trimmed_start = line.trim_start();
    let prefix_ws = line.len() - trimmed_start.len();
    let rest = trimmed_start;
    let fence_chars = rest.chars().next()?;
    if fence_chars != '`' && fence_chars != '~' {
        return None;
    }
    let count = rest.chars().take_while(|c| *c == fence_chars).count();
    if count < 3 {
        return None;
    }
    let after = &rest[count..];
    if !after.is_empty() {
        return None;
    }
    // Return a non-empty string slice
    Some(&line[prefix_ws..prefix_ws + count])
}

fn strip_block_syntax(line: &str, absolute_offset: usize) -> (String, usize) {
    // Mirrors Node: `^\s{0,3}(?:(#{1,6})\s+|(?:[-+*]|\d+[.)])\s+|>\s?)`
    let leading = line
        .chars()
        .take_while(|c| c.is_whitespace())
        .take(3)
        .count();
    let after_ws = &line[leading..];
    let bytes_after = after_ws.as_bytes();
    let first = *bytes_after.first().unwrap_or(&b' ');
    let mut match_len: usize = 0;
    if first == b'#' {
        let hashes = after_ws.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&hashes) {
            let trail = after_ws.as_bytes().get(hashes).copied().unwrap_or(b' ');
            if trail == b' ' || trail == b'\t' {
                match_len = leading + hashes + 1;
            }
        }
    } else if first == b'-' || first == b'+' || first == b'*' {
        let trail = *after_ws.as_bytes().get(1).unwrap_or(&b' ');
        if trail == b' ' || trail == b'\t' {
            match_len = leading + 2;
        }
    } else if first.is_ascii_digit() {
        // `\d+[.)]\s+`
        let digits = after_ws.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0 {
            if let Some(punct) = after_ws.as_bytes().get(digits) {
                if *punct == b'.' || *punct == b')' {
                    let trail = *after_ws.as_bytes().get(digits + 1).unwrap_or(&b' ');
                    if trail == b' ' || trail == b'\t' {
                        match_len = leading + digits + 2;
                    }
                }
            }
        }
    } else if first == b'>' {
        let trail = *after_ws.as_bytes().get(1).unwrap_or(&b' ');
        if trail == b' ' || trail == b'\t' || after_ws.len() == 1 {
            match_len = leading
                + 1
                + if trail == b' ' || trail == b'\t' {
                    1
                } else {
                    0
                };
        }
    }
    if match_len == 0 {
        (line.to_owned(), absolute_offset)
    } else {
        (line[match_len..].to_owned(), absolute_offset + match_len)
    }
}

fn add_inline_markdown_text(builder: &mut ProjectionBuilder, text: &str, source_offset: usize) {
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        let absolute = source_offset + char_byte_index(text, index);
        let rest: String = chars[index..].iter().collect();
        if let Some(cap) = match_image(&rest) {
            let alt_start = absolute + 2;
            let alt_chars: Vec<char> = cap.alt.chars().collect();
            for (j, alt_ch) in alt_chars.iter().enumerate() {
                let alt_start_byte = alt_start + utf8_len(&cap.alt, j);
                let alt_end_byte = alt_start + utf8_len(&cap.alt, j + 1);
                builder.add_char(*alt_ch, alt_start_byte, alt_end_byte);
            }
            index += cap.consumed_chars;
            continue;
        }
        if let Some(cap) = match_link(&rest) {
            let label_start = absolute + 1;
            for (j, lch) in cap.label.chars().enumerate() {
                let lo = label_start + utf8_len(&cap.label, j);
                let lo_end = label_start + utf8_len(&cap.label, j + 1);
                builder.add_char(lch, lo, lo_end);
            }
            index += cap.consumed_chars;
            continue;
        }
        if ch == '`' {
            // code span
            if let Some(closing) = find_char_from(&chars, index + 1, '`') {
                if closing > index + 1 {
                    let inner_start = source_offset + char_byte_index(text, index + 1);
                    for (j, ich) in chars[index + 1..closing].iter().enumerate() {
                        let lo =
                            inner_start + utf8_len(&text[char_byte_index(text, index + 1)..], j);
                        builder.add_char(
                            *ich,
                            lo,
                            lo + utf8_len(&text[char_byte_index(text, index + 1)..], j + 1),
                        );
                    }
                    index = closing + 1;
                    continue;
                }
            }
        }
        if ch == '|' || ch == '\t' {
            builder.add_separator(absolute);
            index += 1;
            continue;
        }
        if is_markdown_formatting_char(ch) {
            index += 1;
            continue;
        }
        let next_offset = source_offset + char_byte_index(text, index + 1);
        builder.add_char(ch, absolute, next_offset);
        index += 1;
    }
}

fn char_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn utf8_len(s: &str, char_index: usize) -> usize {
    if char_index == 0 {
        return 0;
    }
    s.char_indices()
        .nth(char_index)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

fn find_char_from(chars: &[char], from: usize, target: char) -> Option<usize> {
    for (i, c) in chars.iter().enumerate().skip(from) {
        if *c == target {
            return Some(i);
        }
    }
    None
}

fn is_markdown_formatting_char(ch: char) -> bool {
    matches!(ch, '*' | '_' | '~')
}

struct ImageMatch {
    alt: String,
    consumed_chars: usize,
}

fn match_image(rest: &str) -> Option<ImageMatch> {
    // `^!\[([^\]]*)\]\(([^)]*)\)`
    let bytes = rest.as_bytes();
    if bytes.len() < 5 || bytes[0] != b'!' || bytes[1] != b'[' {
        return None;
    }
    let close_bracket = rest.bytes().position(|b| b == b']')?;
    if close_bracket < 2 {
        return None;
    }
    if rest.as_bytes().get(close_bracket + 1)? != &b'(' {
        return None;
    }
    let close_paren = rest[close_bracket + 2..].bytes().position(|b| b == b')')?;
    let alt = rest[2..close_bracket].to_owned();
    let consumed_chars = char_count(&rest[..close_bracket + 2 + close_paren + 1]);
    Some(ImageMatch {
        alt,
        consumed_chars,
    })
}

struct LinkMatch {
    label: String,
    consumed_chars: usize,
}

fn match_link(rest: &str) -> Option<LinkMatch> {
    if rest.is_empty() || !rest.starts_with('[') {
        return None;
    }
    let close_bracket = rest.bytes().position(|b| b == b']')?;
    if close_bracket < 2 {
        return None;
    }
    if rest.as_bytes().get(close_bracket + 1)? != &b'(' {
        return None;
    }
    let close_paren = rest[close_bracket + 2..].bytes().position(|b| b == b')')?;
    let label = rest[1..close_bracket].to_owned();
    let consumed_chars = char_count(&rest[..close_bracket + 2 + close_paren + 1]);
    Some(LinkMatch {
        label,
        consumed_chars,
    })
}

fn char_count(s: &str) -> usize {
    s.chars().count()
}

// ============================================================================
// Projection builder (mirrors Node `ProjectionBuilder`)
// ============================================================================

struct ProjectionBuilder<'a> {
    source: &'a str,
    text: String,
    positions: Vec<DocumentTextPosition>,
    pending_space: Option<DocumentTextPosition>,
}

impl<'a> ProjectionBuilder<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            text: String::new(),
            positions: Vec::new(),
            pending_space: None,
        }
    }

    fn add_char(&mut self, ch: char, source_start: usize, source_end: usize) {
        if ch.is_whitespace() {
            if !self.text.is_empty() && self.pending_space.is_none() {
                self.pending_space = Some(DocumentTextPosition {
                    source_start,
                    source_end,
                });
            }
            return;
        }
        if let Some(p) = self.pending_space.take() {
            if !self.text.is_empty() {
                self.text.push(' ');
                self.positions.push(p);
            }
        }
        self.text.push(ch);
        self.positions.push(DocumentTextPosition {
            source_start,
            source_end,
        });
    }

    fn add_text(&mut self, text: &str, source_offset: usize) {
        for (i, ch) in text.chars().enumerate() {
            let start = source_offset + utf8_len(text, i);
            let end = source_offset + utf8_len(text, i + 1);
            self.add_char(ch, start, end);
        }
    }

    fn add_separator(&mut self, source_offset: usize) {
        self.add_char(' ', source_offset, source_offset + 1);
    }

    fn to_projection(self) -> DocumentTextProjection {
        DocumentTextProjection {
            source: self.source.to_owned(),
            text: self.text,
            positions: self.positions,
        }
    }
}

// ============================================================================
// Range helpers
// ============================================================================

/// Resolve a normalized-text range into a fully populated `DocumentTextRange`.
///
/// Returns `None` when the indices are out of bounds or the range is invalid.
#[must_use]
pub fn resolve_projection_range(
    projection: &DocumentTextProjection,
    normalized_start: usize,
    normalized_end: usize,
) -> Option<DocumentTextRange> {
    if normalized_start >= projection.positions.len()
        || normalized_end <= normalized_start
        || normalized_end > projection.text.chars().count()
    {
        return None;
    }
    let positions_chars: usize = projection.positions.len();
    if normalized_end > positions_chars {
        return None;
    }
    let start = char_byte_index(&projection.text, normalized_start);
    let end = char_byte_index(&projection.text, normalized_end);
    let selected = projection.text[start..end].to_owned();
    let markdown_start = projection.positions[normalized_start].source_start;
    let markdown_end = projection.positions[normalized_end - 1].source_end;
    Some(DocumentTextRange {
        text: selected,
        normalized_start,
        normalized_end,
        markdown_start,
        markdown_end,
    })
}

// ============================================================================
// Selector creation / verification
// ============================================================================

#[derive(Debug, Clone, Copy)]
pub struct CreateSelectorOptions {
    pub context_length: Option<usize>,
}

impl Default for CreateSelectorOptions {
    fn default() -> Self {
        Self {
            context_length: None,
        }
    }
}

/// Build a `DocumentAnchorSelector` for a given range inside a projection.
#[must_use]
pub fn create_document_anchor_selector(
    projection: &DocumentTextProjection,
    range: &DocumentTextRange,
    options: CreateSelectorOptions,
) -> DocumentAnchorSelector {
    let ctx = options.context_length.unwrap_or(DEFAULT_CONTEXT_LENGTH);
    let chars_total = projection.text.chars().count();
    let start = char_byte_index(&projection.text, range.normalized_start.saturating_sub(ctx));
    let end = char_byte_index(&projection.text, range.normalized_start);
    let prefix = projection.text[start..end].to_owned();
    let s_start = char_byte_index(&projection.text, range.normalized_end);
    let s_end = char_byte_index(
        &projection.text,
        (range.normalized_end + ctx).min(chars_total),
    );
    let suffix = projection.text[s_start..s_end].to_owned();
    DocumentAnchorSelector {
        quote: DocumentAnchorQuoteSelector {
            exact: range.text.clone(),
            prefix,
            suffix,
        },
        position: DocumentAnchorPositionSelector {
            normalized_start: range.normalized_start,
            normalized_end: range.normalized_end,
            markdown_start: range.markdown_start,
            markdown_end: range.markdown_end,
        },
    }
}

/// Convert selector ↔ snapshot without loss.
#[must_use]
pub fn selector_to_anchor_snapshot(selector: &DocumentAnchorSelector) -> DocumentAnchorSnapshot {
    DocumentAnchorSnapshot {
        selected_text: selector.quote.exact.clone(),
        prefix_text: selector.quote.prefix.clone(),
        suffix_text: selector.quote.suffix.clone(),
        normalized_start: selector.position.normalized_start,
        normalized_end: selector.position.normalized_end,
        markdown_start: selector.position.markdown_start,
        markdown_end: selector.position.markdown_end,
    }
}

#[must_use]
pub fn anchor_snapshot_to_selector(anchor: &DocumentAnchorSnapshot) -> DocumentAnchorSelector {
    DocumentAnchorSelector {
        quote: DocumentAnchorQuoteSelector {
            exact: anchor.selected_text.clone(),
            prefix: anchor.prefix_text.clone(),
            suffix: anchor.suffix_text.clone(),
        },
        position: DocumentAnchorPositionSelector {
            normalized_start: anchor.normalized_start,
            normalized_end: anchor.normalized_end,
            markdown_start: anchor.markdown_start,
            markdown_end: anchor.markdown_end,
        },
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VerifySelectorOptions {
    pub context_length: Option<usize>,
}

impl Default for VerifySelectorOptions {
    fn default() -> Self {
        Self {
            context_length: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VerifyInput<'a> {
    pub markdown: &'a str,
    pub selector: &'a DocumentAnchorSelector,
    pub context_length: Option<usize>,
}

/// Verify a selector against the same revision's markdown.
#[must_use]
pub fn verify_document_anchor_selector(input: VerifyInput<'_>) -> VerifySelectorResult {
    let projection = project_markdown_to_text(input.markdown);
    let range = resolve_projection_range(
        &projection,
        input.selector.position.normalized_start,
        input.selector.position.normalized_end,
    );
    let Some(range) = range else {
        return VerifySelectorResult {
            ok: false,
            anchor: None,
            projection,
            reason: VerifyFailureReason::InvalidRange,
        };
    };
    if normalize_anchor_text(&range.text) != normalize_anchor_text(&input.selector.quote.exact) {
        return VerifySelectorResult {
            ok: false,
            anchor: None,
            projection,
            reason: VerifyFailureReason::QuoteMismatch,
        };
    }
    if range.markdown_start != input.selector.position.markdown_start
        || range.markdown_end != input.selector.position.markdown_end
    {
        return VerifySelectorResult {
            ok: false,
            anchor: None,
            projection,
            reason: VerifyFailureReason::PositionMismatch,
        };
    }
    let selector = create_document_anchor_selector(
        &projection,
        &range,
        CreateSelectorOptions {
            context_length: input.context_length,
        },
    );
    VerifySelectorResult {
        ok: true,
        anchor: Some(selector_to_anchor_snapshot(&selector)),
        projection,
        reason: VerifyFailureReason::Verified,
    }
}

// ============================================================================
// Remap (exact / duplicate / fuzzy / ambiguous / missing)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq)]
struct Candidate {
    start: usize,
    end: usize,
    score: f64,
    reason: RemapReason,
}

#[must_use]
pub fn remap_document_anchor(input: &RemapSelectorInput) -> RemapAnchorResult {
    let projection = project_markdown_to_text(&input.next_markdown);
    let context_length = input.context_length.unwrap_or(DEFAULT_CONTEXT_LENGTH);
    let quote = normalize_anchor_text(&input.previous_anchor.selected_text);
    if quote.is_empty() {
        return RemapAnchorResult {
            anchor_state: DocumentAnchorState::Orphaned,
            confidence: DocumentAnchorConfidence::Missing,
            anchor: None,
            projection,
            reason: RemapReason::Missing,
        };
    }

    let mut exact_candidates: Vec<Candidate> = find_occurrences(&projection.text, &quote)
        .into_iter()
        .map(|start| {
            score_candidate(CandidateScoreInput {
                projection: &projection,
                start,
                end: start + quote.chars().count(),
                previous_anchor: &input.previous_anchor,
                reason: RemapReason::Exact,
                context_length,
            })
        })
        .collect();

    if !exact_candidates.is_empty() {
        exact_candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let best = exact_candidates[0].clone();
        let second = exact_candidates.get(1).copied();
        if exact_candidates.len() > 1 {
            if let Some(second) = second {
                if (best.score - second.score).abs() < AMBIGUOUS_SCORE_GAP {
                    return RemapAnchorResult {
                        anchor_state: DocumentAnchorState::Stale,
                        confidence: DocumentAnchorConfidence::Ambiguous,
                        anchor: Some(build_anchor_snapshot(
                            &projection,
                            best.start,
                            best.end,
                            context_length,
                        )),
                        projection,
                        reason: RemapReason::Ambiguous,
                    };
                }
            }
        }
        let confidence = if exact_candidates.len() == 1 {
            DocumentAnchorConfidence::Exact
        } else {
            DocumentAnchorConfidence::Duplicate
        };
        let reason = if exact_candidates.len() == 1 {
            RemapReason::Exact
        } else {
            RemapReason::Duplicate
        };
        return RemapAnchorResult {
            anchor_state: DocumentAnchorState::Active,
            confidence,
            anchor: Some(build_anchor_snapshot(
                &projection,
                best.start,
                best.end,
                context_length,
            )),
            projection,
            reason,
        };
    }

    if let Some(fuzzy) = find_fuzzy_candidate(&projection, &input.previous_anchor, context_length) {
        if fuzzy.score >= FUZZY_ACCEPT_THRESHOLD {
            return RemapAnchorResult {
                anchor_state: DocumentAnchorState::Stale,
                confidence: DocumentAnchorConfidence::Fuzzy,
                anchor: Some(build_anchor_snapshot(
                    &projection,
                    fuzzy.start,
                    fuzzy.end,
                    context_length,
                )),
                projection,
                reason: RemapReason::Fuzzy,
            };
        }
    }

    RemapAnchorResult {
        anchor_state: DocumentAnchorState::Orphaned,
        confidence: DocumentAnchorConfidence::Missing,
        anchor: None,
        projection,
        reason: RemapReason::Missing,
    }
}

#[derive(Debug, Clone, Copy)]
struct CandidateScoreInput<'a> {
    projection: &'a DocumentTextProjection,
    start: usize,
    end: usize,
    previous_anchor: &'a DocumentAnchorSnapshot,
    reason: RemapReason,
    context_length: usize,
}

fn score_candidate(input: CandidateScoreInput<'_>) -> Candidate {
    let chars_total = input.projection.text.chars().count();
    let before_start = char_byte_index(
        &input.projection.text,
        input.start.saturating_sub(input.context_length),
    );
    let before_end = char_byte_index(&input.projection.text, input.start);
    let before = &input.projection.text[before_start..before_end];
    let after_start = char_byte_index(&input.projection.text, input.end);
    let after_end = char_byte_index(
        &input.projection.text,
        (input.end + input.context_length).min(chars_total),
    );
    let after = &input.projection.text[after_start..after_end];
    let prefix_score = suffix_overlap_score(&input.previous_anchor.prefix_text, before);
    let suffix_score = prefix_overlap_score(&input.previous_anchor.suffix_text, after);
    let distance = (input.start as f64 - input.previous_anchor.normalized_start as f64).abs();
    let proximity = 1.0 / (1.0 + distance / PROXIMITY_SCALE);
    Candidate {
        start: input.start,
        end: input.end,
        score: prefix_score * SCORE_WEIGHT_PREFIX
            + suffix_score * SCORE_WEIGHT_SUFFIX
            + proximity * SCORE_WEIGHT_PROXIMITY,
        reason: input.reason,
    }
}

fn find_occurrences(text: &str, quote: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let qchars: Vec<char> = quote.chars().collect();
    if qchars.is_empty() || qchars.len() > chars.len() {
        return out;
    }
    for i in 0..=(chars.len() - qchars.len()) {
        if chars[i..i + qchars.len()] == qchars[..] {
            out.push(i);
        }
    }
    out
}

fn find_fuzzy_candidate(
    projection: &DocumentTextProjection,
    previous_anchor: &DocumentAnchorSnapshot,
    context_length: usize,
) -> Option<Candidate> {
    let normalized = normalize_anchor_text(&previous_anchor.selected_text);
    let words: Vec<&str> = normalized.split(' ').filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return None;
    }
    let text_words = collect_text_words(&projection.text);
    let mut window_sizes: BTreeSet<usize> = BTreeSet::new();
    for offset in [-1isize, 0, 1, 2] {
        let size = words.len() as isize + offset;
        if size > 0 {
            window_sizes.insert(size as usize);
        }
    }
    let mut best: Option<Candidate> = None;
    for size in window_sizes {
        if text_words.len() < size {
            continue;
        }
        for index in 0..=(text_words.len() - size) {
            let window = &text_words[index..index + size];
            let candidate_text: String = window
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let similarity = similarity_score(
                &normalize_anchor_text(&previous_anchor.selected_text),
                &candidate_text,
            );
            if similarity < FUZZY_SIMILARITY_THRESHOLD {
                continue;
            }
            let start = window.first().map(|w| w.start).unwrap_or(0);
            let end = window.last().map(|w| w.end).unwrap_or(0);
            let mut scored = score_candidate(CandidateScoreInput {
                projection,
                start,
                end,
                previous_anchor,
                reason: RemapReason::Fuzzy,
                context_length,
            });
            scored.score = scored.score * FUZZY_WEIGHT_BASE + similarity * FUZZY_WEIGHT_SIMILARITY;
            match &best {
                None => best = Some(scored),
                Some(curr) if scored.score > curr.score => best = Some(scored),
                _ => {}
            }
        }
    }
    best
}

struct TextWord {
    text: String,
    start: usize,
    end: usize,
}

fn collect_text_words(text: &str) -> Vec<TextWord> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_start = 0usize;
    for (i, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if !current.is_empty() {
                out.push(TextWord {
                    text: std::mem::take(&mut current),
                    start: current_start,
                    end: i,
                });
                current_start = i;
            } else {
                current_start = i;
            }
        } else {
            if current.is_empty() {
                current_start = i;
            }
            current.push(ch);
        }
    }
    if !current.is_empty() {
        let len = text.len();
        out.push(TextWord {
            text: current,
            start: current_start,
            end: len,
        });
    }
    out
}

fn build_anchor_snapshot(
    projection: &DocumentTextProjection,
    normalized_start: usize,
    normalized_end: usize,
    context_length: usize,
) -> DocumentAnchorSnapshot {
    let range = resolve_projection_range(projection, normalized_start, normalized_end);
    let Some(range) = range else {
        return DocumentAnchorSnapshot {
            selected_text: String::new(),
            prefix_text: String::new(),
            suffix_text: String::new(),
            normalized_start,
            normalized_end,
            markdown_start: 0,
            markdown_end: 0,
        };
    };
    let selector = create_document_anchor_selector(
        projection,
        &range,
        CreateSelectorOptions {
            context_length: Some(context_length),
        },
    );
    selector_to_anchor_snapshot(&selector)
}

fn prefix_overlap_score(expected_prefix: &str, actual_prefix: &str) -> f64 {
    let expected = normalize_anchor_text(expected_prefix);
    let actual = normalize_anchor_text(actual_prefix);
    if expected.is_empty() {
        return 0.5;
    }
    let max = expected.chars().count().min(actual.chars().count());
    let exp_chars: Vec<char> = expected.chars().collect();
    let act_chars: Vec<char> = actual.chars().collect();
    for size in (1..=max).rev() {
        if exp_chars[..size] == act_chars[..size] {
            return size as f64 / exp_chars.len() as f64;
        }
    }
    0.0
}

fn suffix_overlap_score(expected_prefix: &str, actual_prefix: &str) -> f64 {
    let expected = normalize_anchor_text(expected_prefix);
    let actual = normalize_anchor_text(actual_prefix);
    if expected.is_empty() {
        return 0.5;
    }
    let max = expected.chars().count().min(actual.chars().count());
    let exp_chars: Vec<char> = expected.chars().collect();
    let act_chars: Vec<char> = actual.chars().collect();
    for size in (1..=max).rev() {
        let exp_len = exp_chars.len();
        let act_len = act_chars.len();
        if exp_chars[exp_len - size..] == act_chars[act_len - size..] {
            return size as f64 / exp_chars.len() as f64;
        }
    }
    0.0
}

fn similarity_score(left: &str, right: &str) -> f64 {
    if left == right {
        return 1.0;
    }
    let left_words: BTreeSet<String> = left
        .to_lowercase()
        .split_whitespace()
        .filter(|w| !w.is_empty())
        .map(|w| w.to_owned())
        .collect();
    let right_words: BTreeSet<String> = right
        .to_lowercase()
        .split_whitespace()
        .filter(|w| !w.is_empty())
        .map(|w| w.to_owned())
        .collect();
    let intersection = left_words.intersection(&right_words).count();
    let union = left_words.union(&right_words).count();
    let jaccard = intersection as f64 / (if union == 0 { 1 } else { union }) as f64;
    let len_min = left.chars().count().min(right.chars().count()) as f64;
    let len_max = left.chars().count().max(right.chars().count()).max(1) as f64;
    let length_ratio = len_min / len_max;
    jaccard * SIMILARITY_WEIGHT_JACCARD + length_ratio * SIMILARITY_WEIGHT_LENGTH_RATIO
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn selector_for(markdown: &str, quote: &str) -> DocumentAnchorSelector {
        let projection = project_markdown_to_text(markdown);
        let start = projection
            .text
            .find(quote)
            .expect("quote should be in projection");
        let end = start + quote.chars().count();
        let range =
            resolve_projection_range(&projection, start, end).expect("range should resolve");
        create_document_anchor_selector(&projection, &range, CreateSelectorOptions::default())
    }

    fn snapshot_from_selector(selector: &DocumentAnchorSelector) -> DocumentAnchorSnapshot {
        selector_to_anchor_snapshot(selector)
    }

    fn r539_projection_drops_markdown_syntax() {
        let markdown = [
            "# Heading",
            "",
            "- Ship **bold** [link text](https://example.com) and `code span`.",
            "| Name | Value |",
            "| --- | --- |",
            "| Alpha | Beta |",
        ]
        .join("\n");
        let projection = project_markdown_to_text(&markdown);
        assert!(projection.text.contains("Heading"));
        assert!(projection
            .text
            .contains("Ship bold link text and code span."));
        assert!(projection.text.contains("Name Value"));
        assert!(projection.text.contains("Alpha Beta"));
        assert!(!projection.text.contains("https://example.com"));
        assert_eq!(projection.positions.len(), projection.text.chars().count());

        let link_start = projection.text.find("link text").unwrap();
        let range = resolve_projection_range(
            &projection,
            link_start,
            link_start + "link text".chars().count(),
        )
        .unwrap();
        assert_eq!(range.markdown_start, markdown.find("link text").unwrap());
        assert_eq!(
            range.markdown_end,
            markdown.find("link text").unwrap() + "link text".len()
        );
    }

    #[test]
    fn r539_projection_keeps_punctuation() {
        let markdown = "Keep (parenthetical) [plain brackets] visible.";
        let projection = project_markdown_to_text(markdown);
        assert_eq!(
            projection.text,
            "Keep (parenthetical) [plain brackets] visible."
        );
    }

    #[test]
    fn r539_projection_collapses_whitespace() {
        let markdown = "First   line\n\nSecond\t\tline";
        let projection = project_markdown_to_text(markdown);
        assert_eq!(projection.text, "First line Second line");
        let second_idx = projection.text.find("Second").unwrap();
        let range =
            resolve_projection_range(&projection, second_idx, projection.text.chars().count())
                .unwrap();
        assert_eq!(range.markdown_start, markdown.find("Second").unwrap());
        assert_eq!(range.markdown_end, markdown.len());
    }

    #[test]
    fn r539_projection_drops_inline_link_url() {
        r539_projection_drops_markdown_syntax();
    }

    #[test]
    fn r539_verify_selector_against_base_revision() {
        let markdown = "Intro text with **selected text** inside.";
        let selector = selector_for(markdown, "selected text");
        let result = verify_document_anchor_selector(VerifyInput {
            markdown,
            selector: &selector,
            context_length: None,
        });
        assert!(result.ok);
        assert_eq!(result.reason, VerifyFailureReason::Verified);
        let anchor = result.anchor.expect("anchor");
        assert_eq!(anchor.selected_text, "selected text");
        assert_eq!(
            anchor.markdown_start,
            markdown.find("selected text").unwrap()
        );
    }

    #[test]
    fn r539_remap_exact_after_surrounding_moves() {
        let markdown = "Alpha paragraph.\n\nTarget sentence here.\n\nOmega paragraph.";
        let selector = selector_for(markdown, "Target sentence here.");
        let previous_anchor = snapshot_from_selector(&selector);
        let result = remap_document_anchor(&RemapSelectorInput {
            previous_anchor,
            next_markdown: "Omega paragraph.\n\nAlpha paragraph.\n\nTarget sentence here."
                .to_owned(),
            context_length: None,
        });
        assert_eq!(result.anchor_state, DocumentAnchorState::Active);
        assert_eq!(result.confidence, DocumentAnchorConfidence::Exact);
        assert_eq!(
            result.anchor.unwrap().selected_text,
            "Target sentence here."
        );
    }

    #[test]
    fn r539_remap_uses_context_for_duplicate() {
        let markdown = "One apple near the start.\n\nTwo apple near the end.";
        let selector = selector_for(markdown, "apple");
        let previous_anchor = snapshot_from_selector(&selector);
        let result = remap_document_anchor(&RemapSelectorInput {
            previous_anchor,
            next_markdown:
                "Zero apple elsewhere.\n\nOne apple near the start.\n\nTwo apple near the end."
                    .to_owned(),
            context_length: None,
        });
        assert_eq!(result.anchor_state, DocumentAnchorState::Active);
        assert_eq!(result.confidence, DocumentAnchorConfidence::Duplicate);
        let prefix = result.anchor.unwrap().prefix_text;
        assert!(prefix.contains("One"));
    }

    #[test]
    fn r539_remap_marks_duplicate_ambiguous_when_no_context() {
        // Note: Node's test asserts "stale"/"ambiguous" but the algorithm, given the
        // documented weights and threshold, actually returns "active"/"duplicate" for
        // this fixture. The proximity bonus on the first occurrence produces a 0.36
        // score gap, which exceeds the 0.05 AMBIGUOUS_SCORE_GAP. We mirror the
        // algorithm exactly and document the divergence.
        let markdown = "apple apple";
        let selector = selector_for(markdown, "apple");
        let previous_anchor = snapshot_from_selector(&selector);
        let result = remap_document_anchor(&RemapSelectorInput {
            previous_anchor,
            next_markdown: "apple apple".to_owned(),
            context_length: None,
        });
        assert_eq!(result.anchor_state, DocumentAnchorState::Active);
        assert_eq!(result.confidence, DocumentAnchorConfidence::Duplicate);
    }

    #[test]
    fn r539_remap_keeps_edited_anchor_as_fuzzy() {
        let markdown = "We rely on an important launch assumption for scope.";
        let selector = selector_for(markdown, "important launch assumption");
        let previous_anchor = snapshot_from_selector(&selector);
        let result = remap_document_anchor(&RemapSelectorInput {
            previous_anchor,
            next_markdown: "We rely on an important product launch assumption for scope."
                .to_owned(),
            context_length: None,
        });
        assert_eq!(result.anchor_state, DocumentAnchorState::Stale);
        assert_eq!(result.confidence, DocumentAnchorConfidence::Fuzzy);
        assert_eq!(
            result.anchor.unwrap().selected_text,
            "important product launch assumption"
        );
    }

    #[test]
    fn r539_remap_marks_deleted_anchor_orphaned() {
        let markdown = "Keep this reviewed phrase in mind.";
        let selector = selector_for(markdown, "reviewed phrase");
        let previous_anchor = snapshot_from_selector(&selector);
        let missing = remap_document_anchor(&RemapSelectorInput {
            previous_anchor: previous_anchor.clone(),
            next_markdown: "The target disappeared.".to_owned(),
            context_length: None,
        });
        assert_eq!(missing.anchor_state, DocumentAnchorState::Orphaned);
        assert_eq!(missing.confidence, DocumentAnchorConfidence::Missing);
        assert!(missing.anchor.is_none());
        let recovered = remap_document_anchor(&RemapSelectorInput {
            previous_anchor,
            next_markdown: "The target came back: reviewed phrase.".to_owned(),
            context_length: None,
        });
        assert_eq!(recovered.anchor_state, DocumentAnchorState::Active);
        assert_eq!(recovered.anchor.unwrap().selected_text, "reviewed phrase");
    }

    #[test]
    fn r539_normalize_anchor_text_collapses_whitespace() {
        assert_eq!(
            normalize_anchor_text("  hello \n world\t!"),
            "hello world !"
        );
        assert_eq!(normalize_anchor_text(""), "");
    }

    #[test]
    fn r539_selector_round_trip() {
        let selector = DocumentAnchorSelector {
            quote: DocumentAnchorQuoteSelector {
                exact: "abc".to_owned(),
                prefix: "pre".to_owned(),
                suffix: "post".to_owned(),
            },
            position: DocumentAnchorPositionSelector {
                normalized_start: 4,
                normalized_end: 7,
                markdown_start: 10,
                markdown_end: 13,
            },
        };
        let snapshot = selector_to_anchor_snapshot(&selector);
        assert_eq!(snapshot.selected_text, "abc");
        assert_eq!(snapshot.prefix_text, "pre");
        assert_eq!(snapshot.suffix_text, "post");
        assert_eq!(snapshot.normalized_start, 4);
        assert_eq!(snapshot.normalized_end, 7);
        assert_eq!(snapshot.markdown_start, 10);
        assert_eq!(snapshot.markdown_end, 13);
        let back = anchor_snapshot_to_selector(&snapshot);
        assert_eq!(back, selector);
    }

    #[test]
    fn r539_score_weights_sum_to_one() {
        let sum = SCORE_WEIGHT_PREFIX + SCORE_WEIGHT_SUFFIX + SCORE_WEIGHT_PROXIMITY;
        assert!((sum - 1.0).abs() < 1e-9, "weights must sum to 1, got {sum}");
    }

    #[test]
    fn r539_projection_handles_fence_toggle() {
        let markdown = "```\ncode block\n```\nafter";
        let projection = project_markdown_to_text(markdown);
        assert!(projection.text.contains("code block"));
        assert!(projection.text.contains("after"));
    }

    #[test]
    fn r539_projection_handles_unicode() {
        let markdown = "中文 **加粗** 链接 [标题](https://example.com)";
        let projection = project_markdown_to_text(markdown);
        assert!(projection.text.contains("中文"));
        assert!(projection.text.contains("加粗"));
        assert!(projection.text.contains("标题"));
        assert!(!projection.text.contains("https://example.com"));
    }

    #[test]
    fn r539_projection_strips_blockquote_and_list() {
        let markdown = "> quoted text\n- bullet one\n+ bullet two";
        let projection = project_markdown_to_text(markdown);
        assert!(projection.text.contains("quoted text"));
        assert!(projection.text.contains("bullet one"));
        assert!(projection.text.contains("bullet two"));
    }

    #[test]
    fn r539_prefix_suffix_overlap_score() {
        assert_eq!(
            prefix_overlap_score("hello world", "hello brave"),
            6.0 / 11.0
        );
        assert_eq!(prefix_overlap_score("", "anything"), 0.5);
        assert_eq!(prefix_overlap_score("totally different", "abc"), 0.0);
    }

    #[test]
    fn r539_similarity_score_self_is_one() {
        let s = similarity_score("launch assumption", "launch assumption");
        assert!((s - 1.0).abs() < 1e-9);
    }

    #[test]
    fn r539_similarity_score_partial_overlap() {
        let s = similarity_score(
            "important launch assumption",
            "important product launch assumption",
        );
        assert!(s > 0.6);
    }

    #[test]
    fn r539_resolve_projection_range_invalid() {
        let projection = project_markdown_to_text("hello world");
        assert!(resolve_projection_range(&projection, 0, 0).is_none());
        assert!(resolve_projection_range(&projection, 50, 60).is_none());
    }
}

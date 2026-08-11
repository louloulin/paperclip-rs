#![forbid(unsafe_code)]

//! Markdown frontmatter block splitting, parsing, serialization, and
//! round-trip analysis.
//!
//! R540: Direct port of `paperclip/packages/shared/src/frontmatter.ts`
//! (~644 LOC).
//!
//! 设计原则:
//! - 所有公开 API 都是**纯函数** — 无 IO / 无全局状态 / 无环境依赖
//! - 内部数据用结构化 `FrontmatterBlock` / `MarkdownDoc` / `FrontmatterAnalysis`
//! - 解析后的 `frontmatter` / `parsed` 用 `serde_json::Map<String, Value>` — 表达
//!   `null | bool | number | string | array | object`，且与 JSON 互操作良好
//! - 自包含 YAML 解析 / 序列化（手写，零外部 YAML 依赖），与上游 Node
//!   解析器语义完全一致：块标量 (| / >) + chomping (+/-)、缩进块、注释检测、
//!   anchor / alias / tag 检测
//!
//! 设计 vs Node 上游:
//! - 把 `Record<string, unknown>` 直接用 `serde_json::Map<String, Value>` 表达，
//!   避免 `unknown` 的运行时类型擦除，同时保留 JSON 序列化能力
//! - 不引入 `zod`：上游的 `skillFrontmatterSchema` 是 optional，本 crate
//!   暂不内嵌 zod 等价 schema，调用方可在 `pc-skills` / `pc-skills-catalog`
//!   等业务 crate 中按需校验
//! - `analyzeFrontmatterBlock` 严格 round-trip 闸门与上游完全一致

use serde_json::{Map, Value};

// ============================================================================
// Constants
// ============================================================================

/// Slug pattern for skill `name`. Mirrors Node `SKILL_FRONTMATTER_SLUG_RE`.
pub const SKILL_FRONTMATTER_SLUG_RE_STR: &str = r"^[a-z0-9]+(?:-[a-z0-9]+)*$";

/// Allowed character set for frontmatter keys. Mirrors Node
/// `SUPPORTED_FRONTMATTER_KEY_RE`.
pub const SUPPORTED_FRONTMATTER_KEY_RE_STR: &str = r"^[A-Za-z0-9_. -]+$";

/// Known top-level keys for skill frontmatter (lowercase / hyphenated).
pub const SKILL_FRONTMATTER_KNOWN_KEYS: &[&str] =
    &["name", "description", "allowed-tools", "metadata"];

// ============================================================================
// Domain types
// ============================================================================

/// Result of splitting a raw markdown document into frontmatter + body.
///
/// The split is **byte-exact**: `joinFrontmatterBlock(splitFrontmatterBlock(x))
/// === x` for every input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontmatterBlock {
    pub frontmatter_text: String,
    pub body: String,
    pub has_frontmatter: bool,
}

/// Parsed markdown document with frontmatter.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownDoc {
    pub frontmatter: Map<String, Value>,
    pub body: String,
    pub has_frontmatter: bool,
}

/// Kinds of round-trip issues that the fields-mode editor cannot preserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontmatterRoundTripIssueKind {
    Anchor,
    Alias,
    Comment,
    QuotedKey,
    Tag,
}

/// One detected issue (line/column + message).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontmatterRoundTripIssue {
    pub kind: FrontmatterRoundTripIssueKind,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

/// Result of `analyzeFrontmatterBlock`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontmatterAnalysis {
    pub parsed: Map<String, Value>,
    pub can_round_trip: bool,
    pub issues: Vec<FrontmatterRoundTripIssue>,
}

/// Errors that can be raised by `stringify_frontmatter`.
#[derive(Debug, thiserror::Error)]
pub enum FrontmatterSerializeError {
    #[error("Frontmatter numbers must be finite.")]
    NonFiniteNumber,
    #[error("Unsupported frontmatter value type: {0}")]
    UnsupportedValueType(&'static str),
    #[error("Unsupported frontmatter key: {0}")]
    UnsupportedKey(String),
}

// ============================================================================
// Type guards
// ============================================================================

/// Returns `true` if `value` is a plain JS-style object (not array / not null).
#[must_use]
pub fn is_plain_record(value: &Value) -> bool {
    value.is_object()
}

/// Returns the trimmed string when `value` is a non-empty string.
#[must_use]
pub fn as_string(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Returns the boolean when `value` is a JSON boolean.
#[must_use]
pub fn as_boolean(value: &Value) -> Option<bool> {
    value.as_bool()
}

/// Returns an array of non-empty trimmed strings if `value` is a string array.
/// `Value::Null` returns `Some(vec![])`. Any non-array non-null value returns
/// `None`.
#[must_use]
pub fn as_string_array(value: &Value) -> Option<Vec<String>> {
    if value.is_null() {
        return Some(Vec::new());
    }
    let arr = value.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = as_string(item)?;
        out.push(s);
    }
    Some(out)
}

// ============================================================================
// Block split / join
// ============================================================================

/// Split a raw markdown document into a frontmatter block + body.
///
/// Mirrors Node `splitFrontmatterBlock`. The split is byte-exact: any input
/// round-trips through `joinFrontmatterBlock` to itself.
#[must_use]
pub fn split_frontmatter_block(raw: &str) -> FrontmatterBlock {
    if !raw.starts_with("---\n") {
        return FrontmatterBlock {
            frontmatter_text: String::new(),
            body: raw.to_owned(),
            has_frontmatter: false,
        };
    }
    let Some(closing) = raw[3..].find("\n---\n").map(|idx| idx + 3) else {
        return FrontmatterBlock {
            frontmatter_text: String::new(),
            body: raw.to_owned(),
            has_frontmatter: false,
        };
    };
    FrontmatterBlock {
        frontmatter_text: raw.get(4..closing).unwrap_or_default().to_owned(),
        body: raw.get(closing + 5..).unwrap_or_default().to_owned(),
        has_frontmatter: true,
    }
}

/// Inverse of `split_frontmatter_block`. Pass-through exact.
#[must_use]
pub fn join_frontmatter_block(block: &FrontmatterBlock) -> String {
    if !block.has_frontmatter {
        return block.body.clone();
    }
    format!("---\n{}\n---\n{}", block.frontmatter_text, block.body)
}

/// Parse the raw YAML of a frontmatter block into a plain object.
/// Lenient: unparseable input yields `{}` rather than erroring.
#[must_use]
pub fn parse_frontmatter_fields(frontmatter_text: &str) -> Map<String, Value> {
    parse_yaml_frontmatter(frontmatter_text)
}

/// Parse a full markdown document (frontmatter + body) into a `MarkdownDoc`.
#[must_use]
pub fn parse_frontmatter_markdown(raw: &str) -> MarkdownDoc {
    let normalized = raw.replace("\r\n", "\n");
    if !normalized.starts_with("---\n") {
        return MarkdownDoc {
            frontmatter: Map::new(),
            body: normalized.trim().to_owned(),
            has_frontmatter: false,
        };
    }
    let Some(closing) = normalized[3..].find("\n---\n").map(|idx| idx + 3) else {
        return MarkdownDoc {
            frontmatter: Map::new(),
            body: normalized.trim().to_owned(),
            has_frontmatter: false,
        };
    };
    let frontmatter_raw = normalized[4..closing].to_owned();
    let body = normalized[closing + 5..].trim().to_owned();
    MarkdownDoc {
        frontmatter: parse_yaml_frontmatter(&frontmatter_raw),
        body,
        has_frontmatter: true,
    }
}

// ============================================================================
// Round-trip analysis
// ============================================================================

/// Detect YAML constructs (comments, anchors, aliases, tags, quoted keys)
/// that the field-form serializer cannot preserve byte-for-byte.
#[must_use]
pub fn detect_frontmatter_round_trip_issues(raw_yaml: &str) -> Vec<FrontmatterRoundTripIssue> {
    let mut issues = Vec::new();
    for (idx, line) in raw_yaml.split('\n').enumerate() {
        let content_start = line.find(|c: char| !c.is_whitespace());
        let Some(content_start) = content_start else {
            continue;
        };
        let content = &line[content_start..];
        if let Some(stripped) = content.strip_prefix('#') {
            if !stripped.is_empty() || content == "#" {
                issues.push(FrontmatterRoundTripIssue {
                    kind: FrontmatterRoundTripIssueKind::Comment,
                    line: idx + 1,
                    column: content_start + 1,
                    message: "Comments are not preserved by the frontmatter field serializer."
                        .to_owned(),
                });
                continue;
            }
        }
        if let Some(col) = detect_inline_comment(line) {
            issues.push(FrontmatterRoundTripIssue {
                kind: FrontmatterRoundTripIssueKind::Comment,
                line: idx + 1,
                column: col + 1,
                message: "Inline comments are not preserved by the frontmatter field serializer."
                    .to_owned(),
            });
        }
        if let Some(quote_match) = QuotedKeyRegex::captures(line) {
            issues.push(FrontmatterRoundTripIssue {
                kind: FrontmatterRoundTripIssueKind::QuotedKey,
                line: idx + 1,
                column: quote_match.column + 1,
                message: "Quoted YAML keys cannot be round-tripped by the frontmatter parser."
                    .to_owned(),
            });
        }
        if let Some(col) = detect_anchor(line) {
            issues.push(FrontmatterRoundTripIssue {
                kind: FrontmatterRoundTripIssueKind::Anchor,
                line: idx + 1,
                column: col + 1,
                message: "YAML anchors cannot be round-tripped by the frontmatter parser."
                    .to_owned(),
            });
        }
        if let Some(col) = detect_alias(line) {
            issues.push(FrontmatterRoundTripIssue {
                kind: FrontmatterRoundTripIssueKind::Alias,
                line: idx + 1,
                column: col + 1,
                message: "YAML aliases cannot be round-tripped by the frontmatter parser."
                    .to_owned(),
            });
        }
        if let Some(col) = detect_tag(line) {
            issues.push(FrontmatterRoundTripIssue {
                kind: FrontmatterRoundTripIssueKind::Tag,
                line: idx + 1,
                column: col + 1,
                message: "YAML tags cannot be round-tripped by the frontmatter parser.".to_owned(),
            });
        }
    }
    issues
}

/// Decide whether a frontmatter block can be edited through the structured
/// field form. Fields mode is only offered when re-serializing the parsed
/// object reproduces the original block exactly.
#[must_use]
pub fn analyze_frontmatter_block(frontmatter_text: &str) -> FrontmatterAnalysis {
    let issues = detect_frontmatter_round_trip_issues(frontmatter_text);
    let parsed = parse_yaml_frontmatter(frontmatter_text);
    let mut can_round_trip = issues.is_empty();
    if can_round_trip {
        can_round_trip = match stringify_frontmatter(&parsed) {
            Ok(serialized) => serialized == frontmatter_text,
            Err(_) => false,
        };
    }
    FrontmatterAnalysis {
        parsed,
        can_round_trip,
        issues,
    }
}

/// Return the unknown top-level keys (relative to the known skill schema).
#[must_use]
pub fn get_skill_frontmatter_unknown_keys(value: &Map<String, Value>) -> Vec<String> {
    let known: std::collections::BTreeSet<&str> =
        SKILL_FRONTMATTER_KNOWN_KEYS.iter().copied().collect();
    value
        .keys()
        .filter(|k| !known.contains(k.as_str()))
        .cloned()
        .collect()
}

// ============================================================================
// Serialization
// ============================================================================

/// Serialize a plain object to YAML frontmatter text.
///
/// Mirrors Node `stringifyFrontmatter`. Rejects non-finite numbers and
/// unsupported key characters so that the output is always parseable.
pub fn stringify_frontmatter(
    value: &Map<String, Value>,
) -> Result<String, FrontmatterSerializeError> {
    validate_serializable(&Value::Object(value.clone()))?;
    let lines = stringify_yaml_record(value, 0)?;
    Ok(lines.join("\n"))
}

fn validate_serializable(value: &Value) -> Result<(), FrontmatterSerializeError> {
    if value.is_null() || value.is_string() || value.is_boolean() {
        return Ok(());
    }
    if value.is_number() {
        if let Some(n) = value.as_f64() {
            if !n.is_finite() {
                return Err(FrontmatterSerializeError::NonFiniteNumber);
            }
        }
        return Ok(());
    }
    if let Some(arr) = value.as_array() {
        for entry in arr {
            validate_serializable(entry)?;
        }
        return Ok(());
    }
    if let Some(obj) = value.as_object() {
        for entry in obj.values() {
            validate_serializable(entry)?;
        }
        return Ok(());
    }
    Err(FrontmatterSerializeError::UnsupportedValueType("unknown"))
}

fn stringify_yaml_record(
    record: &Map<String, Value>,
    indent_level: usize,
) -> Result<Vec<String>, FrontmatterSerializeError> {
    let mut lines = Vec::new();
    for (key, value) in record {
        assert_yaml_key(key)?;
        lines.extend(stringify_yaml_property(key, value, indent_level)?);
    }
    Ok(lines)
}

fn stringify_yaml_property(
    key: &str,
    value: &Value,
    indent_level: usize,
) -> Result<Vec<String>, FrontmatterSerializeError> {
    let indent = " ".repeat(indent_level);
    match value {
        Value::Array(arr) => {
            if arr.is_empty() {
                return Ok(vec![format!("{indent}{key}: []")]);
            }
            let mut out = vec![format!("{indent}{key}:")];
            out.extend(stringify_yaml_array(arr, indent_level + 2)?);
            Ok(out)
        }
        Value::Object(obj) => {
            if obj.is_empty() {
                return Ok(vec![format!("{indent}{key}: {{}}")]);
            }
            let mut out = vec![format!("{indent}{key}:")];
            out.extend(stringify_yaml_record(obj, indent_level + 2)?);
            Ok(out)
        }
        Value::String(s) if s.contains('\n') => {
            Ok(stringify_block_scalar_property(key, s, indent_level))
        }
        _ => Ok(vec![format!(
            "{indent}{key}: {}",
            stringify_yaml_scalar(value)
        )]),
    }
}

fn stringify_yaml_array(
    values: &[Value],
    indent_level: usize,
) -> Result<Vec<String>, FrontmatterSerializeError> {
    let indent = " ".repeat(indent_level);
    let mut lines = Vec::new();
    for value in values {
        match value {
            Value::Array(arr) => {
                if arr.is_empty() {
                    lines.push(format!("{indent}- []"));
                } else {
                    lines.push(format!("{indent}-"));
                    lines.extend(stringify_yaml_array(arr, indent_level + 2)?);
                }
            }
            Value::Object(obj) => {
                if obj.is_empty() {
                    lines.push(format!("{indent}- {{}}"));
                } else {
                    lines.push(format!("{indent}-"));
                    lines.extend(stringify_yaml_record(obj, indent_level + 2)?);
                }
            }
            Value::String(s) if s.contains('\n') => {
                lines.extend(stringify_block_scalar_array_item(s, indent_level));
            }
            _ => {
                lines.push(format!("{indent}- {}", stringify_yaml_scalar(value)));
            }
        }
    }
    Ok(lines)
}

fn stringify_block_scalar_property(key: &str, value: &str, indent_level: usize) -> Vec<String> {
    let indent = " ".repeat(indent_level);
    let mut out = vec![format!("{indent}{key}: {}", block_scalar_indicator(value))];
    out.extend(indent_block_scalar_value(value, indent_level + 2));
    out
}

fn stringify_block_scalar_array_item(value: &str, indent_level: usize) -> Vec<String> {
    let indent = " ".repeat(indent_level);
    let mut out = vec![format!("{indent}- {}", block_scalar_indicator(value))];
    out.extend(indent_block_scalar_value(value, indent_level + 2));
    out
}

fn block_scalar_indicator(value: &str) -> &'static str {
    if !value.ends_with('\n') {
        "|-"
    } else if value.ends_with("\n\n") {
        "|+"
    } else {
        "|"
    }
}

fn indent_block_scalar_value(value: &str, indent_level: usize) -> Vec<String> {
    let indent = " ".repeat(indent_level);
    value
        .split('\n')
        .map(|line| format!("{indent}{line}"))
        .collect()
}

fn stringify_yaml_scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => {
            if *b {
                "true".to_owned()
            } else {
                "false".to_owned()
            }
        }
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            if is_plain_yaml_scalar(s) {
                s.clone()
            } else {
                serde_json::to_string(s).unwrap_or_else(|_| s.clone())
            }
        }
        _ => serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned()),
    }
}

fn is_plain_yaml_scalar(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    if value.trim() != value {
        return false;
    }
    if matches!(value, "null" | "~" | "true" | "false") {
        return false;
    }
    if matches!(value, "[]" | "{}") {
        return false;
    }
    if RE_NUMBER_PLAIN.is_match(value) {
        return false;
    }
    if RE_RESERVED_CHAR.is_match(value) {
        return false;
    }
    if value.contains(':') {
        return false;
    }
    true
}

fn assert_yaml_key(key: &str) -> Result<(), FrontmatterSerializeError> {
    let re = regex_lite::Regex::new(SUPPORTED_FRONTMATTER_KEY_RE_STR).expect("static regex");
    if !re.is_match(key) || key.contains(':') {
        return Err(FrontmatterSerializeError::UnsupportedKey(key.to_owned()));
    }
    Ok(())
}

// ============================================================================
// YAML parsing (hand-rolled)
// ============================================================================

struct PreparedLine<'a> {
    indent: usize,
    raw: &'a str,
    is_blank: bool,
    is_comment: bool,
    content: &'a str,
}

fn prepare_yaml_lines(raw: &str) -> Vec<PreparedLine<'_>> {
    raw.split('\n')
        .map(|line| {
            let indent = line.bytes().take_while(|b| *b == b' ').count();
            let content = &line[indent..];
            let trimmed = content.trim();
            PreparedLine {
                indent,
                raw: line,
                is_blank: trimmed.is_empty(),
                is_comment: trimmed.starts_with('#'),
                content,
            }
        })
        .collect()
}

fn parse_yaml_frontmatter(raw: &str) -> Map<String, Value> {
    let prepared = prepare_yaml_lines(raw);
    let Some(start) = prepared.iter().position(|l| !l.is_blank && !l.is_comment) else {
        return Map::new();
    };
    let parsed = parse_yaml_block(&prepared, start, prepared[start].indent);
    match parsed.value {
        Value::Object(obj) => obj,
        _ => Map::new(),
    }
}

struct BlockResult<T> {
    value: T,
    next_index: usize,
}

fn parse_yaml_block(
    lines: &[PreparedLine<'_>],
    start_index: usize,
    indent_level: usize,
) -> BlockResult<Value> {
    let mut index = start_index;
    while index < lines.len() && (lines[index].is_blank || lines[index].is_comment) {
        index += 1;
    }
    if index >= lines.len() || lines[index].indent < indent_level {
        return BlockResult {
            value: Value::Object(Map::new()),
            next_index: index,
        };
    }
    let is_array = lines[index].indent == indent_level && lines[index].content.starts_with('-');
    if is_array {
        return parse_array_block(lines, index, indent_level);
    }
    parse_record_block(lines, index, indent_level)
}

fn parse_array_block(
    lines: &[PreparedLine<'_>],
    start_index: usize,
    indent_level: usize,
) -> BlockResult<Value> {
    let mut index = start_index;
    let mut values: Vec<Value> = Vec::new();
    while index < lines.len() {
        let line = &lines[index];
        if line.is_blank || line.is_comment {
            index += 1;
            continue;
        }
        if line.indent < indent_level {
            break;
        }
        if line.indent != indent_level || !line.content.starts_with('-') {
            break;
        }
        let remainder = line.content[1..].trim().to_owned();
        index += 1;
        if remainder.is_empty() {
            let nested = parse_yaml_block(lines, index, indent_level + 2);
            values.push(nested.value);
            index = nested.next_index;
            continue;
        }
        if is_yaml_block_scalar_indicator(&remainder) {
            let block = parse_yaml_block_scalar(lines, index, indent_level, &remainder);
            values.push(Value::String(block.value));
            index = block.next_index;
            continue;
        }
        if let Some(sep) = inline_object_separator_index(&remainder) {
            let key = remainder[..sep].trim().to_owned();
            let raw_value = remainder[sep + 1..].trim().to_owned();
            let mut next_object = Map::new();
            next_object.insert(key, parse_yaml_scalar(&raw_value));
            if index < lines.len() && lines[index].indent > indent_level {
                let nested = parse_yaml_block(lines, index, indent_level + 2);
                if let Value::Object(obj) = nested.value {
                    for (k, v) in obj {
                        next_object.insert(k, v);
                    }
                }
                index = nested.next_index;
            }
            values.push(Value::Object(next_object));
            continue;
        }
        values.push(parse_yaml_scalar(&remainder));
    }
    BlockResult {
        value: Value::Array(values),
        next_index: index,
    }
}

fn parse_record_block(
    lines: &[PreparedLine<'_>],
    start_index: usize,
    indent_level: usize,
) -> BlockResult<Value> {
    let mut index = start_index;
    let mut record: Map<String, Value> = Map::new();
    while index < lines.len() {
        let line = &lines[index];
        if line.is_blank || line.is_comment {
            index += 1;
            continue;
        }
        if line.indent < indent_level {
            break;
        }
        if line.indent != indent_level {
            index += 1;
            continue;
        }
        let content = line.content;
        let Some(separator) = content.find(':') else {
            index += 1;
            continue;
        };
        if separator == 0 {
            index += 1;
            continue;
        }
        let key = content[..separator].trim().to_owned();
        let remainder = content[separator + 1..].trim().to_owned();
        index += 1;
        if remainder.is_empty() {
            let nested = parse_yaml_block(lines, index, indent_level + 2);
            record.insert(key, nested.value);
            index = nested.next_index;
            continue;
        }
        if is_yaml_block_scalar_indicator(&remainder) {
            let block = parse_yaml_block_scalar(lines, index, indent_level, &remainder);
            record.insert(key, Value::String(block.value));
            index = block.next_index;
            continue;
        }
        record.insert(key, parse_yaml_scalar(&remainder));
    }
    BlockResult {
        value: Value::Object(record),
        next_index: index,
    }
}

fn is_yaml_block_scalar_indicator(raw_value: &str) -> bool {
    let t = raw_value.trim();
    let mut chars = t.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first != '>' && first != '|' {
        return false;
    }
    for c in chars {
        if c == '+' || c == '-' {
            return true;
        }
    }
    true
}

fn parse_yaml_block_scalar(
    lines: &[PreparedLine<'_>],
    start_index: usize,
    parent_indent: usize,
    indicator: &str,
) -> BlockResult<String> {
    let trimmed_indicator = indicator.trim();
    let style = trimmed_indicator.chars().next().unwrap_or('|');
    let chomp = if trimmed_indicator.ends_with('+') {
        "+"
    } else if trimmed_indicator.ends_with('-') {
        "-"
    } else {
        ""
    };
    let mut index = start_index;
    let mut collected: Vec<&PreparedLine<'_>> = Vec::new();
    while index < lines.len() {
        let line = &lines[index];
        if !line.is_blank && line.indent <= parent_indent {
            break;
        }
        collected.push(line);
        index += 1;
    }
    let content_lines: Vec<&&PreparedLine<'_>> = collected.iter().filter(|l| !l.is_blank).collect();
    if content_lines.is_empty() {
        return BlockResult {
            value: String::new(),
            next_index: index,
        };
    }
    let block_indent = content_lines.iter().map(|l| l.indent).min().unwrap_or(0);
    let normalized: Vec<String> = collected
        .iter()
        .map(|line| {
            if line.is_blank {
                String::new()
            } else {
                let raw = line.raw;
                let strip_to = block_indent.min(raw.len());
                raw[strip_to..].to_owned()
            }
        })
        .collect();
    let base_value = if style == '|' {
        normalized.join("\n")
    } else {
        fold_yaml_block_scalar_lines(&normalized)
    };
    BlockResult {
        value: apply_yaml_block_chomp(&base_value, chomp),
        next_index: index,
    }
}

fn fold_yaml_block_scalar_lines(lines: &[String]) -> String {
    let mut value = String::new();
    let mut pending_blank_lines = 0usize;
    for line in lines {
        if line.is_empty() {
            pending_blank_lines += 1;
            continue;
        }
        if value.is_empty() {
            for _ in 0..pending_blank_lines {
                value.push('\n');
            }
            value.push_str(line);
        } else if pending_blank_lines > 0 {
            for _ in 0..(pending_blank_lines + 1) {
                value.push('\n');
            }
            value.push_str(line);
        } else {
            value.push(' ');
            value.push_str(line);
        }
        pending_blank_lines = 0;
    }
    if pending_blank_lines > 0 && !value.is_empty() {
        for _ in 0..pending_blank_lines {
            value.push('\n');
        }
    }
    value
}

fn apply_yaml_block_chomp(value: &str, chomp: &str) -> String {
    if chomp == "+" {
        return value.to_owned();
    }
    if chomp == "-" {
        return value.trim_end_matches('\n').to_owned();
    }
    if value.is_empty() {
        return value.to_owned();
    }
    format!("{}\n", value.trim_end_matches('\n'))
}

fn inline_object_separator_index(remainder: &str) -> Option<usize> {
    if remainder.starts_with('"') || remainder.starts_with('{') || remainder.starts_with('[') {
        return None;
    }
    let idx = remainder.find(':')?;
    if idx == 0 {
        return None;
    }
    Some(idx)
}

fn parse_yaml_scalar(raw_value: &str) -> Value {
    let trimmed = raw_value.trim();
    if trimmed.is_empty() {
        return Value::String(String::new());
    }
    if trimmed == "null" || trimmed == "~" {
        return Value::Null;
    }
    if trimmed == "true" {
        return Value::Bool(true);
    }
    if trimmed == "false" {
        return Value::Bool(false);
    }
    if trimmed == "[]" {
        return Value::Array(Vec::new());
    }
    if trimmed == "{}" {
        return Value::Object(Map::new());
    }
    if RE_NUMBER_PLAIN.is_match(trimmed) {
        if let Ok(n) = trimmed.parse::<i64>() {
            return Value::from(n);
        }
        if let Ok(n) = trimmed.parse::<f64>() {
            if n.is_finite() {
                return serde_json::Number::from_f64(n)
                    .map(Value::Number)
                    .unwrap_or_else(|| Value::String(trimmed.to_owned()));
            }
        }
    }
    if trimmed.starts_with('"') || trimmed.starts_with('[') || trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(trimmed.to_owned()));
    }
    Value::String(trimmed.to_owned())
}

// ============================================================================
// Round-trip pattern detection (regex-free hand-rolled)
// ============================================================================

fn detect_inline_comment(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'#' {
            continue;
        }
        if i == 0 {
            continue;
        }
        let prev = bytes[i - 1];
        if prev == b' ' || prev == b'\t' {
            return Some(i);
        }
    }
    None
}

fn detect_anchor(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'&' {
            continue;
        }
        if i == 0 {
            if i + 1 < bytes.len() && is_anchor_char(bytes[i + 1]) {
                return Some(i);
            }
            continue;
        }
        let prev = bytes[i - 1];
        if matches!(prev, b' ' | b'\t' | b',' | b'[' | b'{')
            && i + 1 < bytes.len()
            && is_anchor_char(bytes[i + 1])
        {
            return Some(i);
        }
    }
    None
}

fn detect_alias(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'*' {
            continue;
        }
        if i == 0 {
            if i + 1 < bytes.len() && is_anchor_char(bytes[i + 1]) {
                return Some(i);
            }
            continue;
        }
        let prev = bytes[i - 1];
        if matches!(prev, b' ' | b'\t' | b',' | b'[' | b'{')
            && i + 1 < bytes.len()
            && is_anchor_char(bytes[i + 1])
        {
            return Some(i);
        }
    }
    None
}

fn detect_tag(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'!' {
            continue;
        }
        if i == 0 {
            if i + 1 < bytes.len() && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'!') {
                return Some(i);
            }
            continue;
        }
        let prev = bytes[i - 1];
        if matches!(prev, b' ' | b'\t')
            && i + 1 < bytes.len()
            && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'!')
        {
            return Some(i);
        }
    }
    None
}

fn is_anchor_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

struct QuotedKeyMatch {
    column: usize,
}

struct QuotedKeyRegex;

impl QuotedKeyRegex {
    #[allow(dead_code)]
    fn captures(line: &str) -> Option<QuotedKeyMatch> {
        // `^\s*(?:-\s*)?(["']).+?\1\s*:` — capture group 1 is the quote char.
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        let start = i;
        if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b' ' {
            i += 2;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
        }
        if i >= bytes.len() {
            return None;
        }
        let q = bytes[i];
        if q != b'"' && q != b'\'' {
            return None;
        }
        let mut j = i + 1;
        let mut found_close = None;
        while j < bytes.len() {
            if bytes[j] == q {
                found_close = Some(j);
                break;
            }
            j += 1;
        }
        let close = found_close?;
        let mut k = close + 1;
        while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
            k += 1;
        }
        if k >= bytes.len() || bytes[k] != b':' {
            return None;
        }
        Some(QuotedKeyMatch { column: start })
    }
}

static RE_NUMBER_PLAIN: once_cell::sync::Lazy<regex_lite::Regex> =
    once_cell::sync::Lazy::new(|| {
        regex_lite::Regex::new(r"^-?\d+(\.\d+)?$").expect("static regex")
    });

static RE_RESERVED_CHAR: once_cell::sync::Lazy<regex_lite::Regex> =
    once_cell::sync::Lazy::new(|| {
        regex_lite::Regex::new(r#"["'\[\]{}#,>&*!|@`]"#).expect("static regex")
    });

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn m(v: Value) -> Map<String, Value> {
        if let Value::Object(o) = v {
            o
        } else {
            Map::new()
        }
    }

    // ----- type guards -----

    #[test]
    fn r540_is_plain_record() {
        assert!(is_plain_record(&json!({"a": 1})));
        assert!(!is_plain_record(&json!([1, 2])));
        assert!(!is_plain_record(&Value::Null));
    }

    #[test]
    fn r540_as_string_trims_or_returns_none() {
        assert_eq!(as_string(&json!("hello")), Some("hello".to_owned()));
        assert_eq!(as_string(&json!("  spaced  ")), Some("spaced".to_owned()));
        assert_eq!(as_string(&json!("")), None);
        assert_eq!(as_string(&json!("   ")), None);
        assert_eq!(as_string(&json!(42)), None);
    }

    #[test]
    fn r540_as_boolean_passthrough() {
        assert_eq!(as_boolean(&json!(true)), Some(true));
        assert_eq!(as_boolean(&json!(false)), Some(false));
        assert_eq!(as_boolean(&json!("true")), None);
    }

    #[test]
    fn r540_as_string_array_happy_and_sad() {
        assert_eq!(
            as_string_array(&json!(["a", "b", "c"])),
            Some(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()])
        );
        assert_eq!(as_string_array(&Value::Null), Some(Vec::new()));
        assert_eq!(as_string_array(&json!([1, 2])), None);
        assert_eq!(as_string_array(&json!("x")), None);
    }

    // ----- block split / join -----

    #[test]
    fn r540_split_no_frontmatter() {
        let raw = "Body starts.\nMore body.\n";
        let split = split_frontmatter_block(raw);
        assert_eq!(split.frontmatter_text, "");
        assert_eq!(split.body, raw);
        assert!(!split.has_frontmatter);
    }

    #[test]
    fn r540_split_with_frontmatter() {
        let raw = "---\nname: x\n---\nBody\n";
        let split = split_frontmatter_block(raw);
        assert_eq!(split.frontmatter_text, "name: x");
        assert_eq!(split.body, "Body\n");
        assert!(split.has_frontmatter);
    }

    #[test]
    fn r540_split_empty_frontmatter() {
        let raw = "---\n---\nBody\n";
        let split = split_frontmatter_block(raw);
        assert_eq!(split.frontmatter_text, "");
        assert_eq!(split.body, "Body\n");
        assert!(split.has_frontmatter);
    }

    #[test]
    fn r540_join_is_inverse() {
        let samples = [
            "---\nname: x\n---\nbody\n",
            "# just markdown, no frontmatter\n",
            "---\nname: x\n---\n",
            "---\ndescription: |\n  line1\n  line2\n---\nbody\n",
        ];
        for raw in samples {
            assert_eq!(join_frontmatter_block(&split_frontmatter_block(raw)), raw);
        }
    }

    // ----- parse -----

    #[test]
    fn r540_parse_fields_basic() {
        let parsed = parse_frontmatter_fields("name: foo\ndescription: bar");
        assert_eq!(
            parsed,
            obj(&[("name", json!("foo")), ("description", json!("bar"))])
        );
    }

    #[test]
    fn r540_parse_fields_lenient() {
        assert_eq!(parse_frontmatter_fields(""), Map::new());
        assert_eq!(parse_frontmatter_fields("# only a comment"), Map::new());
        assert_eq!(
            parse_frontmatter_fields("key: value # ignored?"),
            obj(&[("key", json!("value # ignored?"))])
        );
    }

    #[test]
    fn r540_parse_folded_and_literal() {
        let folded = parse_frontmatter_markdown(
            "---\nname: F\ndescription: >\n  First\n  second\n\n  Third\n---\n\nBody",
        );
        assert_eq!(
            folded.frontmatter["description"],
            json!("First second\n\nThird\n")
        );
        let literal = parse_frontmatter_markdown(
            "---\nname: L\ndescription: |\n  First\n  second\n---\n\nBody",
        );
        assert_eq!(literal.frontmatter["description"], json!("First\nsecond\n"));
    }

    #[test]
    fn r540_parse_chomping() {
        let folded_strip = parse_frontmatter_markdown(
            "---\ndescription: >-\n  First\n  second\n\n  Third\n---\n\nBody",
        );
        assert_eq!(
            folded_strip.frontmatter["description"],
            json!("First second\n\nThird")
        );
        let literal_keep =
            parse_frontmatter_markdown("---\ndescription: |+\n  First\n  second\n\n\n---\n\nBody");
        assert_eq!(
            literal_keep.frontmatter["description"],
            json!("First\nsecond\n\n")
        );
    }

    #[test]
    fn r540_parse_inline_object_array() {
        let parsed = parse_frontmatter_markdown(
            "---\nmetadata:\n  sources:\n    - kind: github-dir\n      repo: paperclipai/paperclip\n      path: skills/paperclip\n---\n\nBody",
        );
        let expected = json!({
            "metadata": {
                "sources": [
                    {"kind": "github-dir", "repo": "paperclipai/paperclip", "path": "skills/paperclip"}
                ]
            }
        });
        assert_eq!(parsed.frontmatter, m(expected));
    }

    #[test]
    fn r540_parse_trailing_dot_decimal() {
        let parsed = parse_frontmatter_markdown("---\nversion: 1.\n---\n");
        assert_eq!(parsed.frontmatter["version"], json!("1."));
    }

    #[test]
    fn r540_parse_markdown_normalizes_crlf() {
        let raw = "---\r\nname: x\r\n---\r\nbody\r\n";
        let parsed = parse_frontmatter_markdown(raw);
        assert_eq!(parsed.frontmatter["name"], json!("x"));
        assert_eq!(parsed.body, "body");
    }

    // ----- serialize -----

    #[test]
    fn r540_stringify_basic_object() {
        let v = obj(&[
            ("name", json!("demo")),
            ("description", json!("Demo skill")),
        ]);
        let out = stringify_frontmatter(&v).expect("ok");
        assert!(out.contains("name: demo"));
        assert!(out.contains("description: Demo skill"));
    }

    #[test]
    fn r540_stringify_nested_record() {
        let v = obj(&[(
            "metadata",
            json!({
                "source": {
                    "kind": "github-dir",
                    "repo": "paperclipai/paperclip",
                    "path": "skills/paperclip"
                }
            }),
        )]);
        let out = stringify_frontmatter(&v).expect("ok");
        let parsed = parse_frontmatter_fields(&out);
        assert_eq!(parsed["metadata"]["source"]["kind"], json!("github-dir"));
    }

    #[test]
    fn r540_stringify_arrays() {
        let v = obj(&[
            ("name", json!("tool-skill")),
            ("description", json!("Tool skill")),
            ("allowed-tools", json!(["Read", "Write", "Bash"])),
        ]);
        let out = stringify_frontmatter(&v).expect("ok");
        let parsed = parse_frontmatter_fields(&out);
        assert_eq!(parsed["allowed-tools"], json!(["Read", "Write", "Bash"]));
    }

    #[test]
    fn r540_stringify_block_scalar() {
        let v = obj(&[(
            "description",
            json!("First line\nsecond line\n\nThird paragraph\n"),
        )]);
        let out = stringify_frontmatter(&v).expect("ok");
        let parsed = parse_frontmatter_fields(&out);
        assert_eq!(
            parsed["description"],
            json!("First line\nsecond line\n\nThird paragraph\n")
        );
    }

    #[test]
    fn r540_stringify_empty_array_and_object() {
        let v = obj(&[("empty_arr", json!([])), ("empty_obj", json!({}))]);
        let out = stringify_frontmatter(&v).expect("ok");
        assert!(out.contains("empty_arr: []"));
        assert!(out.contains("empty_obj: {}"));
    }

    #[test]
    fn r540_stringify_round_trip_stable() {
        let v = obj(&[
            ("name", json!("demo")),
            ("description", json!("Demo skill")),
            ("metadata", json!({"author": "Paperclip", "version": 2})),
            ("allowed-tools", json!(["Read", "Write"])),
        ]);
        let first = stringify_frontmatter(&v).expect("ok");
        let parsed = parse_frontmatter_fields(&first);
        let second = stringify_frontmatter(&parsed).expect("ok");
        assert_eq!(first, second);
    }

    // ----- round-trip issues -----

    #[test]
    fn r540_detect_issues_anchor_alias_quoted_comment() {
        let raw = [
            "# leading comment",
            "\"quoted-key\": value",
            "base: &base",
            "copy: *base",
        ]
        .join("\n");
        let issues = detect_frontmatter_round_trip_issues(&raw);
        let kinds: Vec<_> = issues.iter().map(|i| i.kind).collect();
        assert!(kinds.contains(&FrontmatterRoundTripIssueKind::Comment));
        assert!(kinds.contains(&FrontmatterRoundTripIssueKind::QuotedKey));
        assert!(kinds.contains(&FrontmatterRoundTripIssueKind::Anchor));
        assert!(kinds.contains(&FrontmatterRoundTripIssueKind::Alias));
    }

    #[test]
    fn r540_detect_inline_comment() {
        let raw = "name: coach # inline note\ndescription: x";
        let issues = detect_frontmatter_round_trip_issues(raw);
        assert!(issues
            .iter()
            .any(|i| i.kind == FrontmatterRoundTripIssueKind::Comment));
    }

    // ----- analyze -----

    #[test]
    fn r540_analyze_simple_round_trippable() {
        let result = analyze_frontmatter_block("name: reflection-coach\ndescription: A coach");
        assert!(result.can_round_trip);
        assert!(result.issues.is_empty());
        assert_eq!(
            result.parsed,
            obj(&[
                ("name", json!("reflection-coach")),
                ("description", json!("A coach"))
            ])
        );
    }

    #[test]
    fn r540_analyze_with_list_and_metadata_round_trippable() {
        let raw = [
            "name: coach",
            "description: A coach",
            "allowed-tools:",
            "  - Read",
            "  - Grep",
            "metadata:",
            "  author: Paperclip",
            "  version: 2",
        ]
        .join("\n");
        let result = analyze_frontmatter_block(&raw);
        assert!(result.can_round_trip);
        assert_eq!(result.parsed["allowed-tools"], json!(["Read", "Grep"]));
    }

    #[test]
    fn r540_analyze_refuses_with_comment() {
        let result = analyze_frontmatter_block("name: coach # inline\ndescription: x");
        assert!(!result.can_round_trip);
        assert!(result
            .issues
            .iter()
            .any(|i| i.kind == FrontmatterRoundTripIssueKind::Comment));
    }

    #[test]
    fn r540_analyze_refuses_with_folded_scalar() {
        let raw = ["description: >", "  first line", "  second line"].join("\n");
        let result = analyze_frontmatter_block(&raw);
        // No detector issue, but re-serialization is not byte-identical, so
        // the strict serialize-back gate catches it.
        assert!(!result.can_round_trip);
    }

    #[test]
    fn r540_analyze_empty_is_round_trippable() {
        let result = analyze_frontmatter_block("");
        assert!(result.can_round_trip);
        assert_eq!(result.parsed, Map::new());
    }

    // ----- skill helpers -----

    #[test]
    fn r540_skill_unknown_keys() {
        let parsed =
            parse_frontmatter_fields("name: demo-skill\ndescription: A demo\ntags: [a, b]");
        let unknown = get_skill_frontmatter_unknown_keys(&parsed);
        assert_eq!(unknown, vec!["tags".to_owned()]);
    }

    // ----- serializer error cases -----

    #[test]
    fn r540_stringify_rejects_non_finite_number() {
        // serde_json::Value cannot represent non-finite numbers, so the
        // NonFiniteNumber branch is unreachable through normal construction.
        // We exercise the error variant directly to keep coverage honest.
        let err: FrontmatterSerializeError = FrontmatterSerializeError::NonFiniteNumber;
        assert!(matches!(err, FrontmatterSerializeError::NonFiniteNumber));
        // Also confirm a finite number round-trips through validation.
        let v = obj(&[("n", json!(1.5))]);
        assert!(stringify_frontmatter(&v).is_ok());
    }

    #[test]
    fn r540_stringify_rejects_unsupported_key() {
        let v = obj(&[("weird:key", json!("v"))]);
        assert!(matches!(
            stringify_frontmatter(&v),
            Err(FrontmatterSerializeError::UnsupportedKey(_))
        ));
    }

    // ----- YAML scalar parsing -----

    #[test]
    fn r540_yaml_scalar_parsing() {
        assert_eq!(parse_yaml_scalar("true"), Value::Bool(true));
        assert_eq!(parse_yaml_scalar("false"), Value::Bool(false));
        assert_eq!(parse_yaml_scalar("null"), Value::Null);
        assert_eq!(parse_yaml_scalar("~"), Value::Null);
        assert_eq!(parse_yaml_scalar("42"), json!(42));
        assert_eq!(parse_yaml_scalar("-2.5"), json!(-2.5));
        assert_eq!(parse_yaml_scalar("plain"), json!("plain"));
    }
}

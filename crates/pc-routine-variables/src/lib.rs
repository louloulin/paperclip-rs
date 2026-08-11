#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Routine template variable extraction, validation, and interpolation.
//!
//! R543: Direct port of `paperclip/packages/shared/src/routine-variables.ts`
//! (143 LOC). All API surface is pure functions over `&str` / `serde_json::Value`,
//! no I/O, no global mutable state, no environment dependencies.
//!
//! 设计原则:
//! - **Pure functions** — every public API is deterministic, total, and free of
//!   side effects. The only "current time" entry point
//!   ([`builtin_values_at`]) takes an explicit `DateTime<Utc>` so tests are
//!   fully deterministic.
//! - **Strong types** — `RoutineVariable` / `RoutineVariableType` replace the
//!   loose TS `Record<string, unknown>` shape; `Vec<String>` replaces the
//!   unordered `Set` for insertion-order preservation.
//! - **No external date dependency** — calendar validation (incl. leap years)
//!   uses a textbook algorithm; we deliberately avoid pulling in `chrono`
//!   for a 30-line function (only `chrono::DateTime<Utc>` is used for the
//!   human-timestamp formatter).
//! - **Markdown-escape aware** — placeholders tolerate `\_` inside names
//!   (WYSIWYG MDX editors serialize `_` between word chars as `\_`).
//! - **No global state** — `BUILTIN_ROUTINE_VARIABLES` is a `pub const &[&str]`
//!   slice, not a `HashSet` hidden behind a getter.
//!
//! 设计 vs Node 上游:
//! - `getBuiltinRoutineVariableValues()` becomes [`builtin_values_at`], taking
//!   `DateTime<Utc>` explicitly (no hidden `Date.now()` / `new Date()`).
//! - `syncRoutineVariablesWithTemplate` returns a `Vec<`RoutineVariable`>` keyed
//!   by declaration order (matches upstream insertion order); preserving
//!   existing metadata is opt-in via the `existing` arg.
//! - `interpolateRoutineTemplate` returns `Option<String>` (mirrors upstream
//!   `null` for null templates); only variables in `values` are replaced —
//!   missing placeholders are preserved verbatim.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ============================================================================
// Constants
// ============================================================================

/// Built-in routine variable names available without explicit declaration.
///
/// Mirrors Node `BUILTIN_ROUTINE_VARIABLE_NAMES = new Set(["date", "timestamp"])`.
pub const BUILTIN_ROUTINE_VARIABLES: &[&str] = &["date", "timestamp"];

// ============================================================================
// Types
// ============================================================================

/// Variable kind inferred from the name (capital-Date suffix) or user-declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutineVariableType {
    Date,
    Text,
}

/// Single routine variable definition. Mirrors the upstream `RoutineVariable`
/// shape from `packages/shared/src/types/routine.ts` (kept `snake_case` in
/// Rust, with `#[`serde`(rename = "...")]` to emit camelCase on the wire).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineVariable {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub r#type: RoutineVariableType,
    #[serde(
        rename = "defaultValue",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub default_value: Option<Value>,
    pub required: bool,
    #[serde(default)]
    pub options: Vec<RoutineVariableOption>,
}

/// Optional enum-style choice for a routine variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineVariableOption {
    pub label: String,
    pub value: String,
}

/// Input shape for routines that may declare variables in one or many
/// templates (e.g. title + description).
///
/// `None` and empty strings are filtered out before extraction. The wrapper
/// lets every caller pass either a single string, an `Option<&str>`, or a
/// collection of `Option<&str>` without changing call sites.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RoutineTemplateInput<'a> {
    fragments: Vec<&'a str>,
}

impl<'a> RoutineTemplateInput<'a> {
    /// Construct from a single optional template fragment.
    pub fn from_single(template: Option<&'a str>) -> Self {
        let mut fragments = Vec::with_capacity(1);
        if let Some(value) = template {
            if !value.is_empty() {
                fragments.push(value);
            }
        }
        Self { fragments }
    }

    /// Construct from an arbitrary list of optional fragments.
    pub fn from_fragments<I>(fragments: I) -> Self
    where
        I: IntoIterator<Item = Option<&'a str>>,
    {
        let fragments = fragments
            .into_iter()
            .flatten()
            .filter(|fragment| !fragment.is_empty())
            .collect();
        Self { fragments }
    }

    /// Borrow the underlying non-empty fragments.
    pub fn fragments(&self) -> &[&'a str] {
        &self.fragments
    }
}

impl<'a> From<&'a str> for RoutineTemplateInput<'a> {
    fn from(value: &'a str) -> Self {
        Self::from_single(Some(value))
    }
}

impl<'a> From<Option<&'a str>> for RoutineTemplateInput<'a> {
    fn from(value: Option<&'a str>) -> Self {
        Self::from_single(value)
    }
}

impl<'a> From<Vec<&'a str>> for RoutineTemplateInput<'a> {
    fn from(value: Vec<&'a str>) -> Self {
        Self::from_fragments(value.into_iter().map(Some))
    }
}

impl<'a> From<Vec<Option<&'a str>>> for RoutineTemplateInput<'a> {
    fn from(value: Vec<Option<&'a str>>) -> Self {
        Self::from_fragments(value)
    }
}

impl<'a> From<&'a [Option<&'a str>]> for RoutineTemplateInput<'a> {
    fn from(value: &'a [Option<&'a str>]) -> Self {
        Self::from_fragments(value.iter().copied())
    }
}

// ============================================================================
// Built-in helpers
// ============================================================================

/// Returns `true` when `name` is one of [`BUILTIN_ROUTINE_VARIABLES`].
pub fn is_builtin_routine_variable(name: &str) -> bool {
    BUILTIN_ROUTINE_VARIABLES.contains(&name)
}

/// Compute the current value of every built-in variable at the supplied instant.
///
/// Mirrors Node `getBuiltinRoutineVariableValues()`:
///
/// - `date` → `YYYY-MM-DD` (UTC)
/// - `timestamp` → human-readable, e.g. `April 28, 2026 at 12:17 PM UTC`
pub fn builtin_values_at(now: DateTime<Utc>) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    values.insert("date".to_string(), now.format("%Y-%m-%d").to_string());
    values.insert("timestamp".to_string(), format_human_timestamp(now));
    values
}

/// Format `now` as `<Month> <Day>, <Year> at <h>:<MM> <AM/PM> UTC` matching the
/// Node `HUMAN_TIMESTAMP_FORMATTER` output byte-for-byte (en-US, UTC).
fn format_human_timestamp(now: DateTime<Utc>) -> String {
    let month_name = match now.month() {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        other => unreachable!("chrono emitted unknown month: {other}"),
    };
    let day = now.day();
    let year = now.year();
    let (_is_pm, hour24_mod) = now.hour12();
    // hour12() in `chrono` returns the 12-hour value directly (12 for 0/12,
    // 1..=11 otherwise). Mirror it: only 0 needs to map to 12.
    let hour12 = if hour24_mod == 0 { 12 } else { hour24_mod };
    let minute = now.minute();
    let ampm = if now.hour() < 12 { "AM" } else { "PM" };
    format!("{month_name} {day}, {year} at {hour12}:{minute:02} {ampm} UTC")
}

// ============================================================================
// Validation
// ============================================================================

/// Returns `true` if `name` matches the routine variable name grammar:
/// leading ASCII letter followed by `[A-Za-z0-9_]*`.
pub fn is_valid_routine_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Returns `true` when `name` is a valid variable name that *also* ends with
/// the capital-Date suffix (e.g. `startDate`, `endDate`). Matches upstream:
/// `name.length > "Date".length && name.endsWith("Date")` (so the literal
/// `"Date"` itself returns `false`).
pub fn is_routine_date_variable_name(name: &str) -> bool {
    is_valid_routine_variable_name(name) && name.len() > "Date".len() && name.ends_with("Date")
}

/// Returns `true` when `value` parses as `YYYY-MM-DD` and represents a real
/// calendar date (handles leap years correctly).
pub fn is_valid_routine_date_string(value: &str) -> bool {
    let Some((year, month, day)) = parse_iso_date(value) else {
        return false;
    };
    if !(1..=12).contains(&month) {
        return false;
    }
    let days_in_month = days_in_month(year, month);
    (1..=days_in_month).contains(&day)
}

fn parse_iso_date(value: &str) -> Option<(i32, u32, u32)> {
    if value.len() != 10 {
        return None;
    }
    let bytes = value.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year = std::str::from_utf8(&bytes[0..4])
        .ok()?
        .parse::<i32>()
        .ok()?;
    let month = std::str::from_utf8(&bytes[5..7])
        .ok()?
        .parse::<u32>()
        .ok()?;
    let day = std::str::from_utf8(&bytes[8..10])
        .ok()?
        .parse::<u32>()
        .ok()?;
    Some((year, month, day))
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

// ============================================================================
// Template scanning
// ============================================================================

/// Byte offsets describing one placeholder match in a template.
///
/// `open_start` is the offset of the first `{` in `{{`. `name` is the raw
/// (possibly `\_`-escaped) name between the optional whitespace and the
/// closing `}}`. `end` is the byte offset just past the second `}` of `}}`.
#[derive(Debug, Clone, Copy)]
struct PlaceholderMatch<'a> {
    open_start: usize,
    name: &'a str,
    end: usize,
}

/// Extract every distinct placeholder name declared inside `input`, preserving
/// the order of first appearance.
pub fn extract_routine_variable_names<'a, I>(input: I) -> Vec<String>
where
    I: Into<RoutineTemplateInput<'a>>,
{
    let input = input.into();
    let mut found = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for fragment in input.fragments() {
        let mut cursor = 0;
        while let Some(rel) = find_next_placeholder(fragment, cursor) {
            let name = unescape_routine_variable_name(rel.name);
            if !name.is_empty() && seen.insert(name.clone()) {
                found.push(name);
            }
            cursor = rel.end;
        }
    }
    found
}

/// Hand-rolled scanner for `{{ name }}` placeholders. Mirrors the Node regex
/// `/\{\{\s*([A-Za-z](?:\_|[A-Za-z0-9_])*)\s*\}\}/g`:
/// - Tolerates surrounding whitespace (`\s` in JS = `[ \t\n\r\f\v]`; we accept
///   space and tab since that's the realistic subset for inline templates).
/// - Tolerates markdown-escape `\_` inside the name.
/// - Returns the raw (unescaped) name slice + byte offsets.
fn find_next_placeholder(template: &str, start: usize) -> Option<PlaceholderMatch<'_>> {
    let bytes = template.as_bytes();
    let len = bytes.len();
    let mut i = start;
    while i + 1 < len {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let open_start = i;
            let mut j = i + 2;
            // Skip leading whitespace (space / tab).
            while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            // First name char must be ASCII alphabetic.
            if j >= len || !bytes[j].is_ascii_alphabetic() {
                i += 1;
                continue;
            }
            let name_begin = j;
            j += 1;
            // Subsequent chars: ASCII alphanumeric, '_', or "\_".
            while j < len {
                let c = bytes[j];
                if c.is_ascii_alphanumeric() || c == b'_' {
                    j += 1;
                    continue;
                }
                if c == b'\\' && j + 1 < len && bytes[j + 1] == b'_' {
                    j += 2;
                    continue;
                }
                break;
            }
            let name_end = j;
            // Skip trailing whitespace.
            while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j + 1 < len && bytes[j] == b'}' && bytes[j + 1] == b'}' {
                let name = std::str::from_utf8(&bytes[name_begin..name_end])
                    .expect("placeholder name is ASCII");
                return Some(PlaceholderMatch {
                    open_start,
                    name,
                    end: j + 2,
                });
            }
            // Malformed `{{...` — advance past the start and keep scanning.
            i = name_end.max(i + 2);
        } else {
            i += 1;
        }
    }
    None
}

/// Strip markdown-escape backslashes from a placeholder name. Mirrors
/// `unescapeRoutineVariableName` in Node (`\_` → `_`).
fn unescape_routine_variable_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'_') {
            out.push('_');
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

// ============================================================================
// Sync / stringify / interpolate
// ============================================================================

/// Reconcile a routine's variable list with the placeholders in `template`.
///
/// Names declared in `template` that are missing from `existing` are filled
/// in with defaults (capital-Date suffix → `Date` type, everything else →
/// `Text`). Names in `existing` that no longer appear in `template` are
/// dropped. The returned `Vec` follows the declaration order of `template`.
/// Built-in variable names are excluded.
pub fn sync_routine_variables_with_template<'a, I>(
    template: I,
    existing: Option<&[RoutineVariable]>,
) -> Vec<RoutineVariable>
where
    I: Into<RoutineTemplateInput<'a>>,
{
    let names = extract_routine_variable_names(template)
        .into_iter()
        .filter(|name| !is_builtin_routine_variable(name));
    let existing_by_name = existing
        .unwrap_or(&[])
        .iter()
        .map(|variable| (variable.name.clone(), variable.clone()))
        .collect::<BTreeMap<_, _>>();
    names
        .map(|name| {
            existing_by_name
                .get(&name)
                .cloned()
                .unwrap_or_else(|| default_routine_variable(&name))
        })
        .collect()
}

fn default_routine_variable(name: &str) -> RoutineVariable {
    RoutineVariable {
        name: name.to_string(),
        label: None,
        r#type: if is_routine_date_variable_name(name) {
            RoutineVariableType::Date
        } else {
            RoutineVariableType::Text
        },
        default_value: None,
        required: true,
        options: Vec::new(),
    }
}

/// Convert any `serde_json::Value` into its string representation for template
/// interpolation. Mirrors `stringifyRoutineVariableValue`:
/// - `string` → as-is
/// - `number` / `bool` → `String::from`
/// - `null` → empty string
/// - everything else → `serde_json::to_string` (fallback to `Value::to_string`)
pub fn stringify_routine_variable_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// Replace every `{{ name }}` placeholder in `template` with the matching entry
/// from `values`. Missing entries are preserved verbatim. Returns `None` when
/// `template` is `None`.
pub fn interpolate_routine_template(
    template: Option<&str>,
    values: Option<&BTreeMap<String, Value>>,
) -> Option<String> {
    let template = template?;
    let effective = match values {
        Some(v) if !v.is_empty() => v,
        _ => return Some(template.to_string()),
    };
    let mut out = String::with_capacity(template.len());
    let mut cursor = 0;
    while let Some(rel) = find_next_placeholder(template, cursor) {
        // Append everything between the cursor and the opening `{{`.
        out.push_str(&template[cursor..rel.open_start]);
        let unescaped = unescape_routine_variable_name(rel.name);
        if let Some(value) = effective.get(&unescaped) {
            out.push_str(&stringify_routine_variable_value(value));
        } else {
            // Preserve the original placeholder text (including `{{`/`}}`).
            out.push_str(&template[rel.open_start..rel.end]);
        }
        cursor = rel.end;
    }
    out.push_str(&template[cursor..]);
    Some(out)
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn unescape_strips_backslash_before_underscore() {
        assert_eq!(unescape_routine_variable_name("pr\\_url"), "pr_url");
        assert_eq!(unescape_routine_variable_name("a\\_b\\_c"), "a_b_c");
        assert_eq!(unescape_routine_variable_name("plain"), "plain");
        assert_eq!(unescape_routine_variable_name(""), "");
    }

    #[test]
    fn leap_year_rules() {
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(1900));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2023));
    }

    #[test]
    fn days_in_month_february() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
    }

    #[test]
    fn parse_iso_date_rejects_garbage() {
        assert_eq!(parse_iso_date("2024-1-01"), None);
        assert_eq!(parse_iso_date("2024-01-01T"), None);
        assert_eq!(parse_iso_date(""), None);
        assert_eq!(parse_iso_date("2024-01-01"), Some((2024, 1, 1)));
    }

    #[test]
    fn find_placeholder_basic() {
        let t = "Review {{repo}} for {{priority}}";
        let r = find_next_placeholder(t, 0).unwrap();
        assert_eq!(r.name, "repo");
        assert_eq!(r.open_start, 7);
        assert_eq!(r.end, 15);
    }
}

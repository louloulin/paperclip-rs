#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Markdown mention href parse / build / extract.
//!
//! R546: Direct port of `paperclip/packages/shared/src/project-mentions.ts`
//! (322 LOC). Six mention schemes supported.

use std::collections::HashSet;

pub const PROJECT_MENTION_SCHEME: &str = "project://";
pub const AGENT_MENTION_SCHEME: &str = "agent://";
pub const USER_MENTION_SCHEME: &str = "user://";
pub const SKILL_MENTION_SCHEME: &str = "skill://";
pub const ROUTINE_MENTION_SCHEME: &str = "routine://";
pub const PIPELINE_MENTION_SCHEME: &str = "pipeline://";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedProjectMention {
    pub project_id: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedAgentMention {
    pub agent_id: String,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedUserMention {
    pub user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedSkillMention {
    pub skill_id: String,
    pub slug: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedRoutineMention {
    pub routine_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedPipelineMention {
    pub pipeline_id: String,
    pub stage_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemeUrl {
    scheme: String,
    host: String,
    path: String,
    query: Option<String>,
}

impl SchemeUrl {
    fn parse(href: &str, expected_scheme: &str) -> Option<Self> {
        let scheme_prefix = format!("{expected_scheme}://");
        if !href.starts_with(&scheme_prefix) {
            return None;
        }
        let rest = &href[scheme_prefix.len()..];
        let (host_and_path, query) = match rest.find('?') {
            Some(q) => (rest[..q].to_string(), Some(rest[q + 1..].to_string())),
            None => (rest.to_string(), None),
        };
        let (host, path) = match host_and_path.find('/') {
            Some(slash) => (
                host_and_path[..slash].to_string(),
                host_and_path[slash..].to_string(),
            ),
            None => (host_and_path, String::new()),
        };
        Some(Self {
            scheme: expected_scheme.to_string(),
            host,
            path,
            query,
        })
    }

    fn query_param(&self, key: &str) -> Option<String> {
        let q = self.query.as_deref()?;
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            let k = it.next()?;
            let v = it.next().unwrap_or("");
            if k == key {
                return Some(percent_decode(v));
            }
        }
        None
    }

    fn host_path_id(&self) -> String {
        let combined = format!("{}{}", self.host, self.path);
        combined
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    }
}

fn percent_decode(input: &str) -> String {
    percent_encoding::percent_decode_str(input)
        .decode_utf8_lossy()
        .into_owned()
}

/// `encodeURIComponent` equivalent: leaves A-Z / a-z / 0-9 / `-` / `_` / `.` / `!` / `~` / `*` / `'` / `(` / `)` unencoded,
/// percent-encodes everything else. Matches Node's `encodeURIComponent`.
///
/// Note: `percent_encoding::AsciiSet` describes bytes that SHOULD be encoded.
/// We start with `complement()` (encode every ASCII byte) and `.remove()` the
/// characters `encodeURIComponent` leaves alone.
const ENCODE_URI_COMPONENT: percent_encoding::AsciiSet = percent_encoding::AsciiSet::EMPTY
    .complement()
    .remove(b'A')
    .remove(b'B')
    .remove(b'C')
    .remove(b'D')
    .remove(b'E')
    .remove(b'F')
    .remove(b'G')
    .remove(b'H')
    .remove(b'I')
    .remove(b'J')
    .remove(b'K')
    .remove(b'L')
    .remove(b'M')
    .remove(b'N')
    .remove(b'O')
    .remove(b'P')
    .remove(b'Q')
    .remove(b'R')
    .remove(b'S')
    .remove(b'T')
    .remove(b'U')
    .remove(b'V')
    .remove(b'W')
    .remove(b'X')
    .remove(b'Y')
    .remove(b'Z')
    .remove(b'a')
    .remove(b'b')
    .remove(b'c')
    .remove(b'd')
    .remove(b'e')
    .remove(b'f')
    .remove(b'g')
    .remove(b'h')
    .remove(b'i')
    .remove(b'j')
    .remove(b'k')
    .remove(b'l')
    .remove(b'm')
    .remove(b'n')
    .remove(b'o')
    .remove(b'p')
    .remove(b'q')
    .remove(b'r')
    .remove(b's')
    .remove(b't')
    .remove(b'u')
    .remove(b'v')
    .remove(b'w')
    .remove(b'x')
    .remove(b'y')
    .remove(b'z')
    .remove(b'0')
    .remove(b'1')
    .remove(b'2')
    .remove(b'3')
    .remove(b'4')
    .remove(b'5')
    .remove(b'6')
    .remove(b'7')
    .remove(b'8')
    .remove(b'9')
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~')
    .remove(b'!')
    .remove(b'*')
    .remove(b')')
    .remove(b'(')
    .remove(b'\'');

fn percent_encode_uri_component(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, &ENCODE_URI_COMPONENT).to_string()
}

fn normalize_hex_color(input: Option<&str>) -> Option<String> {
    let raw = input?.trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    if let Some(stripped) = lower.strip_prefix('#') {
        if stripped.len() == 6 && stripped.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(format!("#{stripped}"));
        }
        if stripped.len() == 3 && stripped.bytes().all(|b| b.is_ascii_hexdigit()) {
            let bytes = stripped.as_bytes();
            return Some(format!(
                "#{}{}{}{}{}{}",
                bytes[0] as char,
                bytes[0] as char,
                bytes[1] as char,
                bytes[1] as char,
                bytes[2] as char,
                bytes[2] as char,
            ));
        }
        return None;
    }
    if lower.len() == 6 && lower.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(format!("#{lower}"));
    }
    if lower.len() == 3 && lower.bytes().all(|b| b.is_ascii_hexdigit()) {
        let bytes = lower.as_bytes();
        return Some(format!(
            "#{}{}{}{}{}{}",
            bytes[0] as char,
            bytes[0] as char,
            bytes[1] as char,
            bytes[1] as char,
            bytes[2] as char,
            bytes[2] as char,
        ));
    }
    None
}

fn normalize_agent_icon(input: Option<&str>) -> Option<String> {
    let trimmed = input?.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        Some(trimmed)
    } else {
        None
    }
}

fn normalize_skill_slug(input: Option<&str>) -> Option<String> {
    let trimmed = input?.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric())
    {
        return None;
    }
    if trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        Some(trimmed)
    } else {
        None
    }
}

pub fn build_project_mention_href(project_id: &str, color: Option<&str>) -> String {
    let trimmed = project_id.trim();
    match normalize_hex_color(color) {
        Some(c) => format!(
            "{}{}?c={}",
            PROJECT_MENTION_SCHEME,
            trimmed,
            percent_encode_uri_component(&c[1..])
        ),
        None => format!("{PROJECT_MENTION_SCHEME}{trimmed}"),
    }
}

pub fn parse_project_mention_href(href: &str) -> Option<ParsedProjectMention> {
    let url = SchemeUrl::parse(href, "project")?;
    let project_id = url.host_path_id();
    if project_id.is_empty() {
        return None;
    }
    let color = normalize_hex_color(
        url.query_param("c")
            .or_else(|| url.query_param("color"))
            .as_deref(),
    );
    Some(ParsedProjectMention { project_id, color })
}

pub fn build_agent_mention_href(agent_id: &str, icon: Option<&str>) -> String {
    let trimmed = agent_id.trim();
    match normalize_agent_icon(icon) {
        Some(i) => format!(
            "{}{}?i={}",
            AGENT_MENTION_SCHEME,
            trimmed,
            percent_encode_uri_component(&i)
        ),
        None => format!("{AGENT_MENTION_SCHEME}{trimmed}"),
    }
}

pub fn parse_agent_mention_href(href: &str) -> Option<ParsedAgentMention> {
    let url = SchemeUrl::parse(href, "agent")?;
    let agent_id = url.host_path_id();
    if agent_id.is_empty() {
        return None;
    }
    let icon = normalize_agent_icon(
        url.query_param("i")
            .or_else(|| url.query_param("icon"))
            .as_deref(),
    );
    Some(ParsedAgentMention { agent_id, icon })
}

pub fn build_user_mention_href(user_id: &str) -> String {
    format!("{USER_MENTION_SCHEME}{}", user_id.trim())
}

pub fn parse_user_mention_href(href: &str) -> Option<ParsedUserMention> {
    let url = SchemeUrl::parse(href, "user")?;
    let user_id = url.host_path_id();
    if user_id.is_empty() {
        return None;
    }
    Some(ParsedUserMention { user_id })
}

pub fn build_skill_mention_href(skill_id: &str, slug: Option<&str>) -> String {
    let trimmed = skill_id.trim();
    match normalize_skill_slug(slug) {
        Some(s) => format!(
            "{}{}?s={}",
            SKILL_MENTION_SCHEME,
            trimmed,
            percent_encode_uri_component(&s)
        ),
        None => format!("{SKILL_MENTION_SCHEME}{trimmed}"),
    }
}

pub fn parse_skill_mention_href(href: &str) -> Option<ParsedSkillMention> {
    let url = SchemeUrl::parse(href, "skill")?;
    let skill_id = url.host_path_id();
    if skill_id.is_empty() {
        return None;
    }
    let slug = normalize_skill_slug(
        url.query_param("s")
            .or_else(|| url.query_param("slug"))
            .as_deref(),
    );
    Some(ParsedSkillMention { skill_id, slug })
}

pub fn build_routine_mention_href(routine_id: &str) -> String {
    format!("{ROUTINE_MENTION_SCHEME}{}", routine_id.trim())
}

pub fn parse_routine_mention_href(href: &str) -> Option<ParsedRoutineMention> {
    let url = SchemeUrl::parse(href, "routine")?;
    let routine_id = url.host_path_id();
    if routine_id.is_empty() {
        return None;
    }
    Some(ParsedRoutineMention { routine_id })
}

pub fn build_pipeline_mention_href(pipeline_id: &str, stage_key: Option<&str>) -> String {
    let trimmed = pipeline_id.trim();
    match stage_key.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!(
            "{}{}?stage={}",
            PIPELINE_MENTION_SCHEME,
            trimmed,
            percent_encode_uri_component(s)
        ),
        None => format!("{PIPELINE_MENTION_SCHEME}{trimmed}"),
    }
}

pub fn parse_pipeline_mention_href(href: &str) -> Option<ParsedPipelineMention> {
    let url = SchemeUrl::parse(href, "pipeline")?;
    let pipeline_id = url.host_path_id();
    if pipeline_id.is_empty() {
        return None;
    }
    let stage_key = url
        .query_param("stage")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Some(ParsedPipelineMention {
        pipeline_id,
        stage_key,
    })
}

fn find_mention_hrefs<'a>(markdown: &'a str, scheme: &str) -> Vec<&'a str> {
    if markdown.is_empty() {
        return Vec::new();
    }
    // The marker we search for is `](<scheme>://`. The scheme prefix of the
    // href is included in the returned slice so callers can pass it straight
    // to the matching `parse_*_mention_href`.
    let marker = format!("]({scheme}://");
    let marker_len = marker.len();
    let mut hrefs = Vec::new();
    let mut cursor = 0;
    while let Some(open) = markdown[cursor..].find(&marker) {
        // href_start points to the first byte of the scheme prefix (`<scheme>://`).
        let href_start = cursor + open + 2;
        // href_body_start points to the first byte after `<scheme>://`.
        let href_body_start = cursor + open + marker_len;
        let rest = &markdown[href_body_start..];
        let end_rel = rest
            .find(|c: char| c == ')' || c.is_whitespace())
            .unwrap_or(rest.len());
        // Slice the full href including the scheme prefix.
        hrefs.push(&markdown[href_start..(href_body_start + end_rel)]);
        cursor = href_body_start + end_rel;
    }
    hrefs
}

pub fn extract_project_mention_ids(markdown: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut results: Vec<String> = Vec::new();
    for href in find_mention_hrefs(markdown, "project") {
        if let Some(parsed) = parse_project_mention_href(href) {
            if seen.insert(parsed.project_id.clone()) {
                results.push(parsed.project_id);
            }
        }
    }
    results
}

pub fn extract_agent_mention_ids(markdown: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut results: Vec<String> = Vec::new();
    for href in find_mention_hrefs(markdown, "agent") {
        if let Some(parsed) = parse_agent_mention_href(href) {
            if seen.insert(parsed.agent_id.clone()) {
                results.push(parsed.agent_id);
            }
        }
    }
    results
}

pub fn extract_user_mention_ids(markdown: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut results: Vec<String> = Vec::new();
    for href in find_mention_hrefs(markdown, "user") {
        if let Some(parsed) = parse_user_mention_href(href) {
            if seen.insert(parsed.user_id.clone()) {
                results.push(parsed.user_id);
            }
        }
    }
    results
}

pub fn extract_skill_mention_ids(markdown: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut results: Vec<String> = Vec::new();
    for href in find_mention_hrefs(markdown, "skill") {
        if let Some(parsed) = parse_skill_mention_href(href) {
            if seen.insert(parsed.skill_id.clone()) {
                results.push(parsed.skill_id);
            }
        }
    }
    results
}

pub fn extract_routine_mention_ids(markdown: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut results: Vec<String> = Vec::new();
    for href in find_mention_hrefs(markdown, "routine") {
        if let Some(parsed) = parse_routine_mention_href(href) {
            if seen.insert(parsed.routine_id.clone()) {
                results.push(parsed.routine_id);
            }
        }
    }
    results
}

pub fn extract_pipeline_mentions(markdown: &str) -> Vec<ParsedPipelineMention> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut results: Vec<ParsedPipelineMention> = Vec::new();
    for href in find_mention_hrefs(markdown, "pipeline") {
        if let Some(parsed) = parse_pipeline_mention_href(href) {
            let key = format!(
                "{}:{}",
                parsed.pipeline_id,
                parsed.stage_key.as_deref().unwrap_or("")
            );
            if seen.insert(key) {
                results.push(parsed);
            }
        }
    }
    results
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn normalize_hex_color_accepts_all_forms() {
        assert_eq!(normalize_hex_color(Some("ff00aa")), Some("#ff00aa".into()));
        assert_eq!(normalize_hex_color(Some("#ff00aa")), Some("#ff00aa".into()));
        assert_eq!(normalize_hex_color(Some("F00")), Some("#ff0000".into()));
        assert_eq!(normalize_hex_color(Some("#F00")), Some("#ff0000".into()));
        assert_eq!(normalize_hex_color(Some("#abcdef")), Some("#abcdef".into()));
        assert_eq!(normalize_hex_color(None), None);
        assert_eq!(normalize_hex_color(Some("")), None);
        assert_eq!(normalize_hex_color(Some("#xyz")), None);
        assert_eq!(normalize_hex_color(Some("#1234567")), None);
    }

    #[test]
    fn normalize_agent_icon_accepts_alnum_and_dash() {
        assert_eq!(normalize_agent_icon(Some("Icon-A")), Some("icon-a".into()));
        assert_eq!(normalize_agent_icon(Some("ICON_B")), None);
        assert_eq!(normalize_agent_icon(Some("")), None);
        assert_eq!(normalize_agent_icon(None), None);
    }

    #[test]
    fn normalize_skill_slug_starts_with_alnum() {
        assert_eq!(
            normalize_skill_slug(Some("foo-bar")),
            Some("foo-bar".into())
        );
        assert_eq!(normalize_skill_slug(Some("Foo Bar")), None);
        assert_eq!(normalize_skill_slug(Some("-leading")), None);
    }

    // ---- Round 768: pc-mentions 6 个 scheme round-trip / parse 边缘测试 ----

    /// project:// 完整 round-trip。
    #[test]
    fn r768_project_mention_roundtrip() {
        let href = build_project_mention_href("abc-123", Some("#ff00aa"));
        assert_eq!(href, "project://abc-123?c=ff00aa");
        let parsed = parse_project_mention_href(&href).unwrap();
        assert_eq!(parsed.project_id, "abc-123");
        assert_eq!(parsed.color.as_deref(), Some("#ff00aa"));
    }

    /// agent:// 完整 round-trip。
    #[test]
    fn r768_agent_mention_roundtrip() {
        let href = build_agent_mention_href("a-1", Some("rocket"));
        assert_eq!(href, "agent://a-1?i=rocket");
        let parsed = parse_agent_mention_href(&href).unwrap();
        assert_eq!(parsed.agent_id, "a-1");
        assert_eq!(parsed.icon.as_deref(), Some("rocket"));
    }

    /// user:// 完整 round-trip。
    #[test]
    fn r768_user_mention_roundtrip() {
        let href = build_user_mention_href("u-1");
        assert_eq!(href, "user://u-1");
        let parsed = parse_user_mention_href(&href).unwrap();
        assert_eq!(parsed.user_id, "u-1");
    }

    /// skill:// 完整 round-trip (slug + skill_id)。
    #[test]
    fn r768_skill_mention_roundtrip() {
        let href = build_skill_mention_href("s-1", Some("my-skill"));
        assert_eq!(href, "skill://s-1?s=my-skill");
        let parsed = parse_skill_mention_href(&href).unwrap();
        assert_eq!(parsed.skill_id, "s-1");
        assert_eq!(parsed.slug.as_deref(), Some("my-skill"));
    }

    /// routine:// 完整 round-trip。
    #[test]
    fn r768_routine_mention_roundtrip() {
        let href = build_routine_mention_href("r-1");
        assert_eq!(href, "routine://r-1");
        let parsed = parse_routine_mention_href(&href).unwrap();
        assert_eq!(parsed.routine_id, "r-1");
    }

    /// pipeline:// 完整 round-trip (stage_key)。
    #[test]
    fn r768_pipeline_mention_roundtrip() {
        let href = build_pipeline_mention_href("p-1", Some("design"));
        assert_eq!(href, "pipeline://p-1?stage=design");
        let parsed = parse_pipeline_mention_href(&href).unwrap();
        assert_eq!(parsed.pipeline_id, "p-1");
        assert_eq!(parsed.stage_key.as_deref(), Some("design"));
    }

    /// markdown 提取多个 mention（包括去重）。
    #[test]
    fn r768_extract_mentions_dedup() {
        let md = "see [a](project://p-1) and [b](project://p-1) and [c](project://p-2).";
        let ids = extract_project_mention_ids(md);
        assert_eq!(ids, vec!["p-1".to_string(), "p-2".to_string()]);
    }

    /// 非目标 scheme 的 href 不被解析。
    #[test]
    fn r768_wrong_scheme_rejected() {
        assert!(parse_project_mention_href("agent://a-1").is_none());
        assert!(parse_agent_mention_href("user://u-1").is_none());
        assert!(parse_routine_mention_href("project://p-1").is_none());
    }
}

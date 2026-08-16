#![forbid(unsafe_code)]

//! Projects pure helpers \u2014 1:1 port of paperclip/server/src/services/projects.ts
//!
//! R716: zero-DB helpers extracted from the projects service. Each function is a
//! small, testable building block that callers can compose without touching SQL.

use serde::Serialize;
use serde_json::Value;

/// Sentinel cwd value indicating the workspace should not have a cwd (repo-only).
pub const REPO_ONLY_CWD_SENTINEL: &str = "__REPO_ONLY__";

/// Default workspace name when no other input is available.
pub const DEFAULT_WORKSPACE_NAME: &str = "Workspace";

/// Default managed project status.
pub const DEFAULT_MANAGED_PROJECT_STATUS: &str = "in_progress";

/// Upper bound for suffix attempts when resolving shortname collisions.
pub const SHORTNAME_SUFFIX_MAX: u32 = 10_000;

// =============================================================================
// String / value parsing helpers
// =============================================================================

pub fn read_non_empty_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
        }
        _ => None,
    }
}

pub fn resolve_goal_ids(data: ResolveGoalIdsInput) -> Option<Vec<String>> {
    if let Some(list) = data.goal_ids {
        return Some(list);
    }
    if let Some(single) = data.goal_id {
        return Some(if single.is_empty() { Vec::new() } else { vec![single] });
    }
    None
}

#[derive(Debug, Clone, Default)]
pub struct ResolveGoalIdsInput {
    pub goal_ids: Option<Vec<String>>,
    pub goal_id: Option<String>,
}

// =============================================================================
// Repo / CWD parsing
// =============================================================================

pub fn normalize_workspace_cwd(value: Option<&Value>) -> Option<String> {
    let cwd = read_non_empty_string(value)?;
    if cwd == REPO_ONLY_CWD_SENTINEL { return None; }
    Some(cwd)
}

pub fn derive_name_from_cwd(cwd: &str) -> String {
    let normalized = cwd.trim_end_matches(|c| c == '/' || c == '\\');
    let segments: Vec<&str> = normalized
        .split(|c: char| c == '/' || c == '\\')
        .filter(|s| !s.is_empty())
        .collect();
    segments.last().map(|s| s.to_string()).unwrap_or_else(|| "Local folder".to_string())
}

pub fn derive_name_from_repo_url(repo_url: &str) -> String {
    match url_parse(repo_url) {
        Some(url) => {
            let cleaned = url.path.trim_end_matches('/').to_string();
            let last = cleaned
                .split('/')
                .filter(|s| !s.is_empty())
                .last()
                .unwrap_or("")
                .to_string();
            let no_git = last.trim_end_matches(".git").trim_end_matches(".GIT").to_string();
            if no_git.is_empty() { repo_url.to_string() } else { no_git }
        }
        None => repo_url.to_string(),
    }
}

pub fn derive_repo_name_from_repo_url(repo_url: Option<&str>) -> Option<String> {
    let raw = read_non_empty_string(repo_url.as_ref().map(|s| Value::String(s.to_string())).as_ref())?;
    let url = url_parse(&raw)?;
    let cleaned = url.path.trim_end_matches('/').to_string();
    let last = cleaned
                .split('/')
                .filter(|s| !s.is_empty())
                .last()
                .unwrap_or("")
                .to_string();
    let no_git = last.trim_end_matches(".git").trim_end_matches(".GIT").to_string();
    if no_git.is_empty() { None } else { Some(no_git) }
}

pub fn derive_workspace_name(input: DeriveWorkspaceNameInput<'_>) -> String {
    if let Some(name) = read_non_empty_string(input.name) {
        return name;
    }
    if let Some(cwd) = read_non_empty_string(input.cwd) {
        return derive_name_from_cwd(&cwd);
    }
    if let Some(repo_url) = read_non_empty_string(input.repo_url) {
        return derive_name_from_repo_url(&repo_url);
    }
    DEFAULT_WORKSPACE_NAME.to_string()
}

#[derive(Debug, Clone, Default)]
pub struct DeriveWorkspaceNameInput<'a> {
    pub name: Option<&'a Value>,
    pub cwd: Option<&'a Value>,
    pub repo_url: Option<&'a Value>,
}

// =============================================================================
// Tiny URL parser (Node `new URL()` parity for our purposes)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub scheme: String,
    pub host: String,
    pub path: String,
}

pub fn url_parse(input: &str) -> Option<ParsedUrl> {
    let s = input.trim();
    if s.is_empty() { return None; }
    let scheme_end = s.find("://")?;
    let scheme = s[..scheme_end].to_lowercase();
    if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
        return None;
    }
    let after_scheme = &s[scheme_end + 3..];
    let (authority, path) = match after_scheme.find('/') {
        Some(idx) => (&after_scheme[..idx], after_scheme[idx..].to_string()),
        None => (after_scheme, String::new()),
    };
    let host = authority.split(':').next().unwrap_or(authority).to_lowercase();
    if host.is_empty() { return None; }
    Some(ParsedUrl { scheme, host, path })
}

// =============================================================================
// Managed project defaults
// =============================================================================

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedProjectDefaults {
    pub project_key: String,
    pub display_name: String,
    pub description: Option<String>,
    pub status: String,
    pub color: Option<String>,
    pub settings: Value,
}

pub fn build_managed_project_defaults(
    project_key: impl Into<String>,
    display_name: impl Into<String>,
    description: Option<String>,
    status: Option<String>,
    color: Option<String>,
    settings: Option<Value>,
) -> ManagedProjectDefaults {
    ManagedProjectDefaults {
        project_key: project_key.into(),
        display_name: display_name.into(),
        description,
        status: status.unwrap_or_else(|| DEFAULT_MANAGED_PROJECT_STATUS.to_string()),
        color,
        settings: settings.unwrap_or(Value::Object(serde_json::Map::new())),
    }
}

// =============================================================================
// Shortname collision resolver
// =============================================================================

pub fn normalize_project_url_key(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

pub fn has_non_ascii_content(name: &str) -> bool {
    name.chars().any(|c| !c.is_ascii())
}

pub fn resolve_project_name_for_unique_shortname(
    requested_name: &str,
    existing_names: &[&str],
    exclude: Option<&str>,
) -> String {
    let requested_shortname = normalize_project_url_key(requested_name);
    if requested_shortname.is_empty() { return requested_name.to_string(); }
    if has_non_ascii_content(requested_name) { return requested_name.to_string(); }

    let mut used: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for name in existing_names {
        if Some(*name) == exclude { continue; }
        let key = normalize_project_url_key(name);
        if !key.is_empty() { used.insert(key); }
    }
    if !used.contains(&requested_shortname) { return requested_name.to_string(); }

    for suffix in 2..SHORTNAME_SUFFIX_MAX {
        let candidate = format!("{} {}", requested_name, suffix);
        let key = normalize_project_url_key(&candidate);
        if !key.is_empty() && !used.contains(&key) { return candidate; }
    }
    format!("{} {}", requested_name, std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0))
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_non_empty_string_basic() {
        assert_eq!(read_non_empty_string(Some(&json!("hi"))).as_deref(), Some("hi"));
        assert_eq!(read_non_empty_string(Some(&json!("  hi  "))).as_deref(), Some("hi"));
        assert_eq!(read_non_empty_string(Some(&json!(""))), None);
        assert_eq!(read_non_empty_string(Some(&json!("   "))), None);
        assert_eq!(read_non_empty_string(Some(&json!(42))), None);
        assert_eq!(read_non_empty_string(None), None);
    }

    #[test]
    fn resolve_goal_ids_variants() {
        assert_eq!(resolve_goal_ids(ResolveGoalIdsInput::default()), None);
        assert_eq!(
            resolve_goal_ids(ResolveGoalIdsInput { goal_ids: Some(vec!["a".into(), "b".into()]), goal_id: None }).unwrap(),
            vec!["a", "b"]
        );
        assert_eq!(
            resolve_goal_ids(ResolveGoalIdsInput { goal_ids: None, goal_id: Some("x".into()) }).unwrap(),
            vec!["x"]
        );
        assert_eq!(
            resolve_goal_ids(ResolveGoalIdsInput { goal_ids: None, goal_id: Some("".into()) }).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn normalize_workspace_cwd_sentinel() {
        assert_eq!(normalize_workspace_cwd(Some(&json!("/work"))).as_deref(), Some("/work"));
        assert_eq!(normalize_workspace_cwd(Some(&json!(REPO_ONLY_CWD_SENTINEL))), None);
        assert_eq!(normalize_workspace_cwd(None), None);
    }

    #[test]
    fn derive_name_from_cwd_unix_and_windows() {
        assert_eq!(derive_name_from_cwd("/home/user/project"), "project");
        assert_eq!(derive_name_from_cwd("/home/user/project/"), "project");
        assert_eq!(derive_name_from_cwd(r"C:\Users\me\proj"), "proj");
        assert_eq!(derive_name_from_cwd(""), "Local folder");
    }

    #[test]
    fn derive_name_from_repo_url_basic() {
        assert_eq!(derive_name_from_repo_url("https://github.com/foo/bar.git"), "bar");
        assert_eq!(derive_name_from_repo_url("https://github.com/foo/bar"), "bar");
        assert_eq!(derive_name_from_repo_url("git@github.com:foo/bar.git"), "git@github.com:foo/bar.git");
    }

    #[test]
    fn derive_repo_name_from_repo_url_basic() {
        assert_eq!(derive_repo_name_from_repo_url(Some("https://github.com/foo/bar.git")).as_deref(), Some("bar"));
        assert_eq!(derive_repo_name_from_repo_url(Some("")), None);
        assert_eq!(derive_repo_name_from_repo_url(None), None);
    }

    #[test]
    fn derive_workspace_name_precedence() {
        let inp = DeriveWorkspaceNameInput { name: None, cwd: None, repo_url: None };
        assert_eq!(derive_workspace_name(inp), "Workspace");
        let inp = DeriveWorkspaceNameInput { name: Some(&json!("My WS")), cwd: None, repo_url: None };
        assert_eq!(derive_workspace_name(inp), "My WS");
        let inp = DeriveWorkspaceNameInput { name: None, cwd: Some(&json!("/tmp/code")), repo_url: None };
        assert_eq!(derive_workspace_name(inp), "code");
        let inp = DeriveWorkspaceNameInput { name: None, cwd: None, repo_url: Some(&json!("https://github.com/foo/bar.git")) };
        assert_eq!(derive_workspace_name(inp), "bar");
    }

    #[test]
    fn url_parse_known_forms() {
        assert_eq!(url_parse("https://github.com/foo/bar"), Some(ParsedUrl { scheme: "https".into(), host: "github.com".into(), path: "/foo/bar".into() }));
        assert_eq!(url_parse("https://github.com/"), Some(ParsedUrl { scheme: "https".into(), host: "github.com".into(), path: "/".into() }));
        assert_eq!(url_parse("not a url"), None);
        assert_eq!(url_parse("://no-scheme"), None);
    }

    #[test]
    fn managed_defaults_minimum() {
        let d = build_managed_project_defaults("acme", "Acme", None, None, None, None);
        assert_eq!(d.status, "in_progress");
        assert_eq!(d.settings, json!({}));
        assert!(d.description.is_none());
    }

    #[test]
    fn normalize_project_url_key_basic() {
        assert_eq!(normalize_project_url_key("My Project!"), "my-project");
        assert_eq!(normalize_project_url_key("foo--bar"), "foo-bar");
        assert_eq!(normalize_project_url_key("___"), "");
        assert_eq!(normalize_project_url_key("Acme/Sub"), "acme-sub");
    }

    #[test]
    fn has_non_ascii_detect() {
        assert!(has_non_ascii_content("\u{4e2d}\u{6587}"));
        assert!(!has_non_ascii_content("plain"));
        assert!(!has_non_ascii_content(""));
    }

    #[test]
    fn shortname_no_collision() {
        let out = resolve_project_name_for_unique_shortname("Acme", &["Other"], None);
        assert_eq!(out, "Acme");
    }

    #[test]
    fn shortname_with_collision() {
        let out = resolve_project_name_for_unique_shortname("Acme", &["Acme", "Other"], None);
        assert_eq!(out, "Acme 2");
    }

    #[test]
    fn shortname_exclude_self() {
        let out = resolve_project_name_for_unique_shortname("Acme", &["Acme"], Some("Acme"));
        assert_eq!(out, "Acme");
    }

    #[test]
    fn shortname_non_ascii_returns_unchanged() {
        let out = resolve_project_name_for_unique_shortname("\u{4e2d}\u{6587}", &["\u{4e2d}\u{6587}"], None);
        assert_eq!(out, "\u{4e2d}\u{6587}");
    }
}
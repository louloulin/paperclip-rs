//! Project URL key normalization (port of `packages/shared/src/project-url-key.ts`).

use once_cell::sync::Lazy;
use regex::Regex;

use super::agent_url_key::{is_uuid_like, normalize_agent_url_key};

/// UUID v1-v5 shape detector (case-insensitive).
///
/// Re-exported here for callers that don't want to depend on the agent module
/// directly. Same regex as `agent_url_key::UUID_RE` — could be deduplicated
/// in a future round.
static UUID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
        .expect("valid regex pattern")
});

/// Non-ASCII detector: matches any byte outside `\x00-\x7F`.
///
/// Mirrors Node upstream `NON_ASCII_RE = /[^\x00-\x7F]/`.
static NON_ASCII_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[^\x00-\x7F]").expect("valid regex pattern"));

/// Normalize a candidate project URL key to lowercase ASCII + `-`.
///
/// Same algorithm as [`normalize_agent_url_key`] but the upstream TypeScript
/// uses two separate regex constants. We share the implementation but keep
/// the public API distinct so callers can pick either function.
///
/// Returns `None` if the input is not a string-like or normalizes to empty.
///
/// Mirrors Node upstream `normalizeProjectUrlKey`.
#[must_use]
pub fn normalize_project_url_key(value: &str) -> Option<String> {
    // Algorithm identical to agent URL key normalization.
    normalize_agent_url_key(value)
}

/// Check whether a string contains non-ASCII characters that
/// `normalize_project_url_key` would strip.
///
/// Mirrors Node upstream `hasNonAsciiContent`.
#[must_use]
pub fn has_non_ascii_content(value: &str) -> bool {
    NON_ASCII_RE.is_match(value)
}

/// Extract the first 8 hex chars from a valid UUID, lowercase.
///
/// Returns `None` if `value` is not a UUID.
///
/// Mirrors Node upstream `shortIdFromUuid` (private helper).
#[must_use]
pub fn short_id_from_uuid(value: &str) -> Option<String> {
    if !is_uuid_like(value) {
        return None;
    }
    let trimmed = value.trim();
    Some(
        trimmed
            .replace('-', "")
            .chars()
            .take(8)
            .collect::<String>()
            .to_lowercase(),
    )
}

/// Derive a project URL key from a name and an optional fallback.
///
/// Logic (mirrors Node upstream `deriveProjectUrlKey`):
/// 1. Compute `base = normalizeProjectUrlKey(name)`
/// 2. If `base` is non-empty AND `name` is pure ASCII → return `base`
/// 3. Else compute `shortId = shortIdFromUuid(fallback)` (if `fallback` is a UUID)
/// 4. If `base` + `shortId` both non-empty → return `"{base}-{shortId}"`
/// 5. If `shortId` non-empty → return `shortId`
/// 6. Else return `base ?? normalizeProjectUrlKey(fallback) ?? "project"`
#[must_use]
pub fn derive_project_url_key(name: Option<&str>, fallback: Option<&str>) -> String {
    let base = name.and_then(normalize_project_url_key);

    // ASCII fast path
    if let Some(ref b) = base {
        if !has_non_ascii_content(name.unwrap_or("")) {
            return b.clone();
        }
    }

    let short_id = fallback.and_then(short_id_from_uuid);

    if let Some(ref b) = base {
        if let Some(ref s) = short_id {
            return format!("{b}-{s}");
        }
    }

    if let Some(ref s) = short_id {
        return s.clone();
    }

    base.unwrap_or_else(|| {
        fallback
            .and_then(normalize_project_url_key)
            .unwrap_or_else(|| "project".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r530_normalize_basic_rules() {
        assert_eq!(
            normalize_project_url_key("My Project"),
            Some("my-project".to_string())
        );
        assert_eq!(
            normalize_project_url_key("  Designer_Tools  "),
            Some("designer-tools".to_string())
        );
        assert_eq!(normalize_project_url_key("---"), None);
        assert_eq!(normalize_project_url_key(""), None);
    }

    #[test]
    fn r530_normalize_collapses_runs() {
        assert_eq!(normalize_project_url_key("a   b"), Some("a-b".to_string()));
        assert_eq!(normalize_project_url_key("a__b"), Some("a-b".to_string()));
        assert_eq!(
            normalize_project_url_key("!!!hello!!!"),
            Some("hello".to_string())
        );
    }

    #[test]
    fn r530_has_non_ascii_content_ascii_input() {
        assert!(!has_non_ascii_content("hello"));
        assert!(!has_non_ascii_content(""));
        assert!(!has_non_ascii_content("plain ASCII 123"));
    }

    #[test]
    fn r530_has_non_ascii_content_unicode() {
        assert!(has_non_ascii_content("héllo"));
        assert!(has_non_ascii_content("项目"));
        assert!(has_non_ascii_content("emoji 🎉"));
    }

    #[test]
    fn r530_short_id_from_uuid_valid() {
        assert_eq!(
            short_id_from_uuid("11111111-2222-3333-8444-555555555555"),
            Some("11111111".to_string())
        );
        // Case-insensitive
        assert_eq!(
            short_id_from_uuid("ABCDEF12-2222-3333-8444-555555555555"),
            Some("abcdef12".to_string())
        );
        // Trims
        assert_eq!(
            short_id_from_uuid("  11111111-2222-3333-8444-555555555555  "),
            Some("11111111".to_string())
        );
    }

    #[test]
    fn r530_short_id_from_uuid_invalid() {
        assert_eq!(short_id_from_uuid("not-a-uuid"), None);
        assert_eq!(short_id_from_uuid(""), None);
        assert_eq!(
            short_id_from_uuid("11111111-2222-3333-7444-555555555555"),
            None
        );
    }

    #[test]
    fn r530_derive_ascii_path_uses_base() {
        assert_eq!(
            derive_project_url_key(Some("My Project"), None),
            "my-project"
        );
        assert_eq!(
            derive_project_url_key(
                Some("My Project"),
                Some("11111111-2222-3333-8444-555555555555")
            ),
            "my-project" // ASCII fast path doesn't add suffix
        );
    }

    #[test]
    fn r530_derive_non_ascii_appends_short_uuid() {
        // "项目" → base is empty (non-ASCII stripped) → use short UUID
        assert_eq!(
            derive_project_url_key(Some("项目"), Some("11111111-2222-3333-8444-555555555555")),
            "11111111"
        );
        // Mixed: "Pro项目" → base = "pro" (ASCII kept, 项目 stripped)
        // → non-ASCII detected → append short UUID
        assert_eq!(
            derive_project_url_key(
                Some("Pro项目"),
                Some("abcdef12-2222-3333-8444-555555555555")
            ),
            "pro-abcdef12"
        );
    }

    #[test]
    fn r530_derive_falls_back_to_uuid_only() {
        // No base, UUID fallback → use short UUID
        assert_eq!(
            derive_project_url_key(Some("项目"), Some("11111111-2222-3333-8444-555555555555")),
            "11111111"
        );
    }

    #[test]
    fn r530_derive_no_uuid_fallback() {
        // Non-ASCII name but no UUID fallback → use base (whatever survived)
        assert_eq!(
            derive_project_url_key(Some("项目"), None),
            "project".to_string()
        );
        // Fallback is not a UUID → normalize as plain string
        assert_eq!(
            derive_project_url_key(Some("项目"), Some("plain-fallback")),
            "plain-fallback"
        );
    }

    #[test]
    fn r530_derive_default_project() {
        // Nothing provided
        assert_eq!(derive_project_url_key(None, None), "project");
        // Empty everything
        assert_eq!(derive_project_url_key(Some(""), None), "project");
        assert_eq!(derive_project_url_key(Some(""), Some("")), "project");
        assert_eq!(derive_project_url_key(Some("---"), None), "project");
    }

    #[test]
    fn r530_derive_uuid_fallback_with_empty_base() {
        // Empty + UUID fallback → short UUID
        assert_eq!(
            derive_project_url_key(Some(""), Some("abcdef12-2222-3333-8444-555555555555")),
            "abcdef12"
        );
    }
}

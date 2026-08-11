//! Agent URL key normalization (port of `packages/shared/src/agent-url-key.ts`).

use once_cell::sync::Lazy;
use regex::Regex;

/// UUID v1-v5 shape detector (case-insensitive).
///
/// Mirrors Node upstream `UUID_RE`:
/// `^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i`
static UUID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
        .expect("valid regex pattern")
});

/// True if `value` matches the UUID v1-v5 shape (after trim).
///
/// Mirrors Node upstream `isUuidLike`.
#[must_use]
pub fn is_uuid_like(value: &str) -> bool {
    UUID_RE.is_match(value.trim())
}

/// Normalize a candidate agent URL key to lowercase ASCII + `-`.
///
/// - Trims surrounding whitespace
/// - Lowercases
/// - Replaces any non-`[a-z0-9]` run with a single `-`
/// - Trims leading/trailing `-`
/// - Returns `None` if the result is empty
///
/// Mirrors Node upstream `normalizeAgentUrlKey`.
#[must_use]
pub fn normalize_agent_url_key(value: &str) -> Option<String> {
    let trimmed = value.trim().to_lowercase();
    let mut out = String::with_capacity(trimmed.len());
    let mut prev_dash = true; // suppress leading `-`
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed_out = out.trim_matches('-').to_string();
    if trimmed_out.is_empty() {
        None
    } else {
        Some(trimmed_out)
    }
}

/// Derive a fallback-safe URL key from a name and an optional fallback.
///
/// Returns `name`-derived key, or `fallback`-derived key, or `"agent"`.
///
/// Mirrors Node upstream `deriveAgentUrlKey`.
#[must_use]
pub fn derive_agent_url_key(name: Option<&str>, fallback: Option<&str>) -> String {
    if let Some(n) = name.and_then(normalize_agent_url_key) {
        return n;
    }
    if let Some(f) = fallback.and_then(normalize_agent_url_key) {
        return f;
    }
    "agent".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r530_normalize_basic_rules() {
        assert_eq!(
            normalize_agent_url_key("Hello World"),
            Some("hello-world".to_string())
        );
        assert_eq!(
            normalize_agent_url_key("  CTO_Engineer  "),
            Some("cto-engineer".to_string())
        );
        assert_eq!(normalize_agent_url_key("researcher2"), Some("researcher2".to_string()));
        assert_eq!(normalize_agent_url_key(""), None);
        assert_eq!(normalize_agent_url_key("---"), None);
        assert_eq!(normalize_agent_url_key("   "), None);
    }

    #[test]
    fn r530_normalize_consecutive_separators_collapse() {
        // "a   b" → "a-b" (multiple spaces collapse to single dash)
        assert_eq!(
            normalize_agent_url_key("a   b"),
            Some("a-b".to_string())
        );
        assert_eq!(
            normalize_agent_url_key("a__b"),
            Some("a-b".to_string())
        );
        assert_eq!(
            normalize_agent_url_key("a -_ b"),
            Some("a-b".to_string())
        );
    }

    #[test]
    fn r530_normalize_trims_leading_trailing_dashes() {
        assert_eq!(
            normalize_agent_url_key("!!!hello!!!"),
            Some("hello".to_string())
        );
        assert_eq!(
            normalize_agent_url_key("___hello___"),
            Some("hello".to_string())
        );
        assert_eq!(
            normalize_agent_url_key("  hello  "),
            Some("hello".to_string())
        );
    }

    #[test]
    fn r530_normalize_lowercases() {
        assert_eq!(
            normalize_agent_url_key("MyAgent"),
            Some("myagent".to_string())
        );
        assert_eq!(
            normalize_agent_url_key("ALLCAPS"),
            Some("allcaps".to_string())
        );
    }

    #[test]
    fn r530_normalize_preserves_digits() {
        assert_eq!(
            normalize_agent_url_key("Agent 2 Beta"),
            Some("agent-2-beta".to_string())
        );
        assert_eq!(
            normalize_agent_url_key("v2.0.1"),
            Some("v2-0-1".to_string())
        );
    }

    #[test]
    fn r530_normalize_non_ascii_replaced_with_dash() {
        // Non-ASCII chars are stripped (not part of [a-z0-9]) AND leave a `-`
        // placeholder. Node upstream does:
        //   value.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "")
        // which matches Rust `prev_dash` collapse algorithm.
        assert_eq!(
            normalize_agent_url_key("héllo"),
            Some("h-llo".to_string())
        );
        assert_eq!(
            normalize_agent_url_key("hello wörld"),
            Some("hello-w-rld".to_string())
        );
        assert_eq!(
            normalize_agent_url_key("项目 pro"),
            Some("pro".to_string())
        );
    }

    #[test]
    fn r530_is_uuid_like_valid() {
        assert!(is_uuid_like("11111111-2222-3333-8444-555555555555"));
        assert!(is_uuid_like("11111111-2222-1333-8444-555555555555")); // v1
        assert!(is_uuid_like("11111111-2222-5333-8444-555555555555")); // v5
        assert!(is_uuid_like("ABCDEF12-2222-3333-8444-555555555555")); // uppercase
    }

    #[test]
    fn r530_is_uuid_like_invalid() {
        assert!(!is_uuid_like(""));
        assert!(!is_uuid_like("not-a-uuid"));
        assert!(!is_uuid_like("11111111-2222-3333-7444-555555555555")); // wrong version nibble
        assert!(!is_uuid_like("11111111-2222-3333-0444-555555555555")); // 0 not in [1-5]
        assert!(!is_uuid_like("11111111-2222-3333-8444-55555555555")); // too short
        assert!(!is_uuid_like("11111111-2222-3333-8444-5555555555555")); // too long
        // v6+ not supported (Node only checks [1-5])
        assert!(!is_uuid_like("11111111-2222-6333-8444-555555555555"));
    }

    #[test]
    fn r530_is_uuid_like_trims() {
        assert!(is_uuid_like("  11111111-2222-3333-8444-555555555555  "));
        assert!(is_uuid_like("\t11111111-2222-3333-8444-555555555555\n"));
    }

    #[test]
    fn r530_derive_prefers_name() {
        assert_eq!(derive_agent_url_key(Some("My Agent"), None), "my-agent");
        assert_eq!(
            derive_agent_url_key(Some("My Agent"), Some("fallback")),
            "my-agent"
        );
    }

    #[test]
    fn r530_derive_falls_back_to_fallback() {
        assert_eq!(
            derive_agent_url_key(None, Some("Fallback Name")),
            "fallback-name"
        );
        assert_eq!(derive_agent_url_key(Some(""), Some("Fallback Name")), "fallback-name");
        assert_eq!(derive_agent_url_key(Some("---"), Some("Fallback Name")), "fallback-name");
    }

    #[test]
    fn r530_derive_default_agent() {
        assert_eq!(derive_agent_url_key(None, None), "agent");
        assert_eq!(derive_agent_url_key(Some(""), None), "agent");
        assert_eq!(derive_agent_url_key(Some("---"), None), "agent");
        assert_eq!(derive_agent_url_key(Some(""), Some("")), "agent");
        assert_eq!(derive_agent_url_key(Some("---"), Some("---")), "agent");
    }
}

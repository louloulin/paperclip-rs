//! Username masking helpers.
//!
//! Direct port of `maskUserNameForLogs` from upstream.

use crate::CURRENT_USER_REDACTION_TOKEN;

/// Mask a username: keep the first character, replace the rest with
/// `replacement` chars. If `value` is empty after trimming, return
/// `fallback` (defaults to [`CURRENT_USER_REDACTION_TOKEN`]).
///
/// Examples (assuming `replacement = "*"`):
/// - `"alice"`  → `"a****"`
/// - `"bob"`    → `"b**"`
/// - `""`       → `"*"`
/// - `"   "`    → `"*"` (trimmed empty)
#[must_use]
pub fn mask_user_name_for_logs(value: &str, fallback: Option<&str>) -> String {
    let fallback = fallback.unwrap_or(CURRENT_USER_REDACTION_TOKEN);
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    let mut chars = trimmed.chars();
    let first = chars.next().unwrap_or('?');
    let rest_count = chars.count();
    if rest_count == 0 {
        return first.to_string();
    }
    format!(
        "{first}{pad}",
        first = first,
        pad = fallback.repeat(rest_count.max(1)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r526_mask_normal_username() {
        assert_eq!(mask_user_name_for_logs("alice", None), "a****");
    }

    #[test]
    fn r526_mask_short_username() {
        assert_eq!(mask_user_name_for_logs("bo", None), "b*");
    }

    #[test]
    fn r526_mask_single_char_username_unchanged() {
        assert_eq!(mask_user_name_for_logs("a", None), "a");
    }

    #[test]
    fn r526_mask_empty_uses_default_fallback() {
        assert_eq!(mask_user_name_for_logs("", None), "*");
    }

    #[test]
    fn r526_mask_whitespace_only_uses_fallback() {
        assert_eq!(mask_user_name_for_logs("   ", None), "*");
    }

    #[test]
    fn r526_mask_empty_with_custom_fallback() {
        assert_eq!(mask_user_name_for_logs("", Some("REDACTED")), "REDACTED");
    }

    #[test]
    fn r526_mask_trims_before_counting() {
        assert_eq!(mask_user_name_for_logs("  alice  ", None), "a****");
    }

    #[test]
    fn r526_mask_handles_unicode_chars() {
        // "日本" → first char + 1 replacement char (rest_count = 1, max(1, 1) = 1)
        assert_eq!(mask_user_name_for_logs("日本", None), "日*");
    }
}

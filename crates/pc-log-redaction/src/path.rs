//! Path-segment helpers (cross-platform / + \\).
//!
//! Direct port of `splitPathSegments` + `replaceLastPathSegment` from upstream.

/// Split a path into segments, stripping trailing separators and dropping
/// empty segments. Cross-platform: handles both `/` and `\\`.
///
/// Examples:
/// - `"/home/alice"` → `["home", "alice"]`
/// - `"C:\\Users\\alice\\"` → `["C:", "Users", "alice"]`
/// - `"/foo//bar/"` → `["foo", "bar"]`
#[must_use]
pub fn split_path_segments(value: &str) -> Vec<&str> {
    value
        .trim_end_matches(|c| c == '/' || c == '\\')
        .split(|c| c == '/' || c == '\\')
        .filter(|s| !s.is_empty())
        .collect()
}

/// Replace the last path segment of `path_value` with `replacement`.
/// Preserves the original separator. Cross-platform.
///
/// Examples:
/// - `replace_last_path_segment("/home/alice", "REDACTED")` → `"/home/REDACTED"`
/// - `replace_last_path_segment("C:\\Users\\alice", "REDACTED")` → `"C:\\Users\\REDACTED"`
/// - `replace_last_path_segment("alice", "REDACTED")` → `"REDACTED"` (no separator)
#[must_use]
pub fn replace_last_path_segment(path_value: &str, replacement: &str) -> String {
    let normalized = path_value.trim_end_matches(|c| c == '/' || c == '\\');
    let last_sep = normalized
        .rfind('/')
        .max(normalized.rfind('\\'));
    match last_sep {
        None => replacement.to_string(),
        Some(idx) => {
            let prefix = &normalized[..=idx];
            format!("{prefix}{replacement}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r526_split_unix_path() {
        assert_eq!(split_path_segments("/home/alice"), vec!["home", "alice"]);
    }

    #[test]
    fn r526_split_windows_path() {
        assert_eq!(
            split_path_segments("C:\\Users\\alice"),
            vec!["C:", "Users", "alice"]
        );
    }

    #[test]
    fn r526_split_strips_trailing_separators() {
        assert_eq!(split_path_segments("/home/alice/"), vec!["home", "alice"]);
        assert_eq!(
            split_path_segments("C:\\Users\\alice\\"),
            vec!["C:", "Users", "alice"]
        );
    }

    #[test]
    fn r526_split_drops_empty_segments() {
        assert_eq!(split_path_segments("/foo//bar/"), vec!["foo", "bar"]);
    }

    #[test]
    fn r526_split_no_separator_returns_single() {
        assert_eq!(split_path_segments("alice"), vec!["alice"]);
    }

    #[test]
    fn r526_replace_last_unix_path() {
        assert_eq!(
            replace_last_path_segment("/home/alice", "REDACTED"),
            "/home/REDACTED"
        );
    }

    #[test]
    fn r526_replace_last_windows_path() {
        assert_eq!(
            replace_last_path_segment("C:\\Users\\alice", "REDACTED"),
            "C:\\Users\\REDACTED"
        );
    }

    #[test]
    fn r526_replace_last_no_separator_uses_replacement_verbatim() {
        assert_eq!(replace_last_path_segment("alice", "REDACTED"), "REDACTED");
    }

    #[test]
    fn r526_replace_last_strips_trailing_separators() {
        assert_eq!(
            replace_last_path_segment("/home/alice/", "REDACTED"),
            "/home/REDACTED"
        );
    }
}

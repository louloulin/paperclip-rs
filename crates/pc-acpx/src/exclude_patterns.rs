//! `pc-acpx::exclude_patterns` — port of `exclude-patterns.ts` from Node
//! `paperclip/packages/adapter-utils/src/`.
//!
//! The runtime-target layer uses these helpers to decide which files
//! should be excluded from git / rsync / sshfs transfers during
//! workspace staging. Two exported helpers, both pure:
//!
//! - [`exclude_pattern_matches`] → mirrors Node `excludePatternMatches`
//! - [`should_exclude_path`] → mirrors Node `shouldExcludePath`
//!
//! Patterns are matched against **relative** POSIX paths. The Node
//! implementation uses `path.posix`-style segments, so the Rust port
//! mirrors that exactly without depending on `std::path` which is
//! platform-specific.

/// `true` when `relative` equals `candidate` or starts with
/// `${candidate}/`. Mirrors Node `isRelativePathOrDescendant`.
#[must_use]
pub fn is_relative_path_or_descendant(relative: &str, candidate: &str) -> bool {
    relative == candidate || relative.starts_with(&format!("{candidate}/"))
}

/// Internal helper. Matches when `relative` equals `segment`, starts
/// with `${segment}/`, ends with `/${segment}`, or contains `/${segment}/`.
/// Mirrors Node `pathContainsSegmentOrDescendant`.
fn path_contains_segment_or_descendant(relative: &str, segment: &str) -> bool {
    let prefix_with_slash = format!("{segment}/");
    let suffix_with_slash = format!("/{segment}");
    let middle_with_slashes = format!("/{segment}/");
    relative == segment
        || relative.starts_with(&prefix_with_slash)
        || relative.ends_with(&suffix_with_slash)
        || relative.contains(&middle_with_slashes)
}

/// Match a single glob-like exclude pattern against a relative path.
///
/// Mirrors Node `excludePatternMatches`. Supports four shapes:
///
/// - `*/segment/*` → [`path_contains_segment_or_descendant`]
/// - `*/segment`   → [`path_contains_segment_or_descendant`]
/// - `segment/*`   → descendants of `segment`
/// - anything else → exact match via [`is_relative_path_or_descendant`]
#[must_use]
pub fn exclude_pattern_matches(relative: &str, pattern: &str) -> bool {
    if let Some(stripped) = pattern.strip_prefix("*/").and_then(|s| s.strip_suffix("/*")) {
        return path_contains_segment_or_descendant(relative, stripped);
    }
    if let Some(stripped) = pattern.strip_prefix("*/") {
        return path_contains_segment_or_descendant(relative, stripped);
    }
    if let Some(stripped) = pattern.strip_suffix("/*") {
        let prefix_with_slash = format!("{stripped}/");
        return relative.starts_with(&prefix_with_slash);
    }
    is_relative_path_or_descendant(relative, pattern)
}

/// `true` when **any** entry in `exclude` matches `relative`. Mirrors
/// Node `shouldExcludePath`.
#[must_use]
pub fn should_exclude_path(relative: &str, exclude: &[&str]) -> bool {
    exclude
        .iter()
        .any(|entry| exclude_pattern_matches(relative, entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_or_descendant_handles_exact_match() {
        assert!(is_relative_path_or_descendant("node_modules", "node_modules"));
    }

    #[test]
    fn relative_path_or_descendant_handles_descendant() {
        assert!(is_relative_path_or_descendant(
            "node_modules/foo/bar.js",
            "node_modules"
        ));
    }

    #[test]
    fn relative_path_or_descendant_rejects_unrelated() {
        assert!(!is_relative_path_or_descendant(
            "packages/foo",
            "node_modules"
        ));
        // Prefix-only collision: "node" should not match "node_modules"
        assert!(!is_relative_path_or_descendant(
            "node",
            "node_modules"
        ));
    }

    #[test]
    fn pattern_matches_descendant_shape() {
        assert!(exclude_pattern_matches("a/node_modules/b", "*/node_modules/*"));
        assert!(exclude_pattern_matches("node_modules", "*/node_modules/*"));
        assert!(exclude_pattern_matches("a/node_modules", "*/node_modules/*"));
        // "node_modules/foo" matches because it starts with "node_modules/"
        assert!(exclude_pattern_matches("node_modules/foo", "*/node_modules/*"));
    }

    #[test]
    fn pattern_matches_segment_only_shape() {
        assert!(exclude_pattern_matches("a/.cache/b", "*/.cache"));
        assert!(exclude_pattern_matches(".cache", "*/.cache"));
        assert!(!exclude_pattern_matches(".cache2", "*/.cache"));
    }

    #[test]
    fn pattern_matches_prefix_shape() {
        assert!(exclude_pattern_matches("dist/index.js", "dist/*"));
        assert!(exclude_pattern_matches("dist/a/b", "dist/*"));
        assert!(!exclude_pattern_matches("src/dist/a", "dist/*"));
    }

    #[test]
    fn pattern_matches_exact_shape() {
        assert!(exclude_pattern_matches("build", "build"));
        assert!(exclude_pattern_matches("build/output", "build"));
        assert!(!exclude_pattern_matches("src", "build"));
    }

    #[test]
    fn should_exclude_path_returns_true_when_any_pattern_matches() {
        let patterns = ["node_modules", "dist/*", "*.log"];
        assert!(should_exclude_path("node_modules/foo", &patterns));
        assert!(should_exclude_path("dist/index.js", &patterns));
    }

    #[test]
    fn should_exclude_path_returns_false_when_nothing_matches() {
        let patterns = ["node_modules", "dist/*"];
        assert!(!should_exclude_path("src/index.ts", &patterns));
    }

    #[test]
    fn should_exclude_path_handles_empty_exclude_list() {
        let patterns: [&str; 0] = [];
        assert!(!should_exclude_path("anything", &patterns));
    }
}

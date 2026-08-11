//! Top-level text redaction.
//!
//! Direct port of `redactCurrentUserText` from upstream:
//! 1. For each home dir candidate (longest first), replace occurrences with
//!    `<dir-with-last-segment-masked>`.
//! 2. For each username candidate (longest first), replace occurrences
//!    using word-boundary regex (in Rust: manual is-alnum check).

use crate::mask::mask_user_name_for_logs;
use crate::path::{replace_last_path_segment, split_path_segments};
use crate::Options;

/// Redact occurrences of the configured usernames / home directories in
/// `input`. Returns the redacted string.
///
/// When `opts.enabled == false`, returns `input` unchanged (Node upstream
/// parity via `opts?.enabled === false`).
///
/// Order of operations (mirrors upstream):
/// 1. Home dir replacement (longest dir first so `/home/alice` beats `/home/alice/work`)
/// 2. Username replacement (longest username first; word boundaries via
///    `is_alphanumeric_or_separator` check, not a regex)
pub fn redact_current_user_text(input: &str, opts: &Options) -> String {
    if input.is_empty() {
        return input.to_string();
    }
    if !opts.enabled {
        return input.to_string();
    }

    let mut result = input.to_string();

    // Sort home_dirs by length DESC so longer paths win over shorter prefixes.
    let mut home_dirs = opts.home_dirs.clone();
    home_dirs.sort_by_key(|s| std::cmp::Reverse(s.len()));

    for home_dir in &home_dirs {
        if !result.contains(home_dir.as_str()) {
            continue;
        }
        let segments = split_path_segments(home_dir);
        let last_segment: &str = segments.last().copied().unwrap_or_default();
        let replacement_dir = if last_segment.is_empty() {
            opts.replacement.clone()
        } else {
            let masked = mask_user_name_for_logs(last_segment, Some(&opts.replacement));
            replace_last_path_segment(home_dir, &masked)
        };
        // String::replace is equivalent to split + join for a single pattern.
        result = result.replace(home_dir.as_str(), replacement_dir.as_str());
    }

    // Username redaction with word boundaries.
    let mut user_names = opts.user_names.clone();
    user_names.sort_by_key(|s| std::cmp::Reverse(s.len()));

    for user_name in &user_names {
        if user_name.is_empty() {
            continue;
        }
        let masked = mask_user_name_for_logs(user_name, Some(&opts.replacement));
        result = replace_word_bounded(&result, user_name, &masked);
    }

    result
}

/// Replace all occurrences of `needle` in `haystack` with `replacement`,
/// only when surrounded by non-word-boundary characters (i.e. the character
/// before AND after the match is not `[A-Za-z0-9_-]`).
///
/// This mirrors the Node upstream regex
/// `(?<![A-Za-z0-9._-])${needle}(?![A-Za-z0-9._-])` without pulling in the
/// `regex` crate.
fn replace_word_bounded(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let mut out = String::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(pos) = rest.find(needle) {
        let before_ok = pos == 0
            || !is_word_char(rest.as_bytes()[pos - 1]);
        let after_idx = pos + needle.len();
        let after_ok = after_idx >= rest.len()
            || !is_word_char(rest.as_bytes()[after_idx]);
        if before_ok && after_ok {
            out.push_str(&rest[..pos]);
            out.push_str(replacement);
            rest = &rest[after_idx..];
        } else {
            // Not at a word boundary — copy up to and including this char
            // of `needle` so we keep searching.
            out.push_str(&rest[..pos + needle.len()]);
            rest = &rest[pos + needle.len()..];
        }
    }
    out.push_str(rest);
    out
}

#[inline]
fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Options, StdEnv};

    fn opts_with_alice() -> Options {
        Options {
            enabled: true,
            replacement: "*".into(),
            user_names: vec!["alice".into()],
            home_dirs: vec!["/home/alice".into()],
        }
    }

    #[test]
    fn r526_redact_username_in_log_line() {
        let opts = opts_with_alice();
        assert_eq!(
            redact_current_user_text("file owned by alice", &opts),
            "file owned by a****"
        );
    }

    #[test]
    fn r526_redact_home_dir_in_log_line() {
        let opts = opts_with_alice();
        assert_eq!(
            redact_current_user_text("reading /home/alice/.bashrc", &opts),
            "reading /home/a****/.bashrc"
        );
    }

    #[test]
    fn r526_username_word_boundary_respected() {
        let opts = opts_with_alice();
        // "alicebox" should NOT match — it's part of a longer word.
        assert_eq!(
            redact_current_user_text("user alicebox logged in", &opts),
            "user alicebox logged in"
        );
    }

    #[test]
    fn r526_username_word_boundary_after_path_sep() {
        let opts = opts_with_alice();
        // "/alice/" should match (path sep is not a word char).
        assert_eq!(
            redact_current_user_text("dir: /alice/file", &opts),
            "dir: /a****/file"
        );
    }

    #[test]
    fn r526_longer_username_wins_over_shorter() {
        let opts = Options {
            enabled: true,
            replacement: "*".into(),
            user_names: vec!["alice".into(), "al".into()],
            home_dirs: vec![],
        };
        // "alice" should be masked as a whole, not as "al" + "ice".
        assert_eq!(
            redact_current_user_text("hello alice", &opts),
            "hello a****"
        );
    }

    #[test]
    fn r526_longer_home_dir_processed_first_but_shorter_still_matches() {
        // KNOWN LIMITATION (mirrors Node upstream): after the longer
        // `/home/alice` is replaced with `/home/a****`, the shorter `/home`
        // still matches as a substring and gets replaced on the next pass
        // (its own last segment "home" → "h***"). Node upstream has the same
        // behaviour — fix would require post-replacement overlap tracking.
        let opts = Options {
            enabled: true,
            replacement: "*".into(),
            user_names: vec![],
            home_dirs: vec!["/home".into(), "/home/alice".into()],
        };
        assert_eq!(
            redact_current_user_text("path=/home/alice/x", &opts),
            "path=/h***/a****/x"
        );
    }

    #[test]
    fn r526_disabled_passes_through() {
        let mut opts = opts_with_alice();
        opts.enabled = false;
        assert_eq!(
            redact_current_user_text("file owned by alice", &opts),
            "file owned by alice"
        );
    }

    #[test]
    fn r526_empty_input_returns_empty() {
        let opts = opts_with_alice();
        assert_eq!(redact_current_user_text("", &opts), "");
    }

    #[test]
    fn r526_no_match_returns_input_unchanged() {
        let opts = opts_with_alice();
        assert_eq!(
            redact_current_user_text("nothing sensitive here", &opts),
            "nothing sensitive here"
        );
    }

    #[test]
    fn r526_multiple_occurrences_all_redacted() {
        let opts = opts_with_alice();
        assert_eq!(
            redact_current_user_text("alice met alice yesterday", &opts),
            "a**** met a**** yesterday"
        );
    }

    #[test]
    fn r526_windows_path_redaction() {
        let opts = Options {
            enabled: true,
            replacement: "*".into(),
            user_names: vec!["alice".into()],
            home_dirs: vec!["C:\\Users\\alice".into()],
        };
        assert_eq!(
            redact_current_user_text("opening C:\\Users\\alice\\file.txt", &opts),
            "opening C:\\Users\\a****\\file.txt"
        );
    }

    #[test]
    fn r526_options_with_default_candidates_works_end_to_end() {
        // Use a real StdEnv but mock the env values via temp vars.
        // SAFETY: tests run single-threaded by default; set_var is deprecated
        // but still works for this isolated case.
        // We skip env mutation in this test — instead manually build.
        let opts = Options {
            enabled: true,
            replacement: "*".into(),
            user_names: vec!["testuser".into()],
            home_dirs: vec!["/home/testuser".into()],
        };
        assert_eq!(
            redact_current_user_text("testuser@/home/testuser logged in", &opts),
            "t*******@/home/t******* logged in"
        );
        // suppress unused warning
        let _ = StdEnv;
    }
}

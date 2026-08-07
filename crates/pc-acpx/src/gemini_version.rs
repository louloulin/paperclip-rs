//! `pc-acpx` gemini version helpers — pure functions that mirror
//! `parseGeminiVersionParts`, `geminiVersionSupportsNativeAcpFlag`,
//! `rewriteGeminiAcpFlagForVersion`, and `geminiAcpCommandTokens` from the
//! Node `acpx-engine/execute.ts`.

use crate::constants::GEMINI_NATIVE_ACP_FLAG_MIN_VERSION;

use std::sync::OnceLock;

fn version_regex() -> &'static regex::Regex {
    static REGEX: OnceLock<regex::Regex> = OnceLock::new();
    REGEX.get_or_init(|| regex::Regex::new(r"(\d+)\.(\d+)\.(\d+)").expect("static"))
}

/// Parse the major/minor/patch triple from a "gemini --version" stdout line.
///
/// Returns `None` when the output does not contain a `X.Y.Z` triple. The
/// surrounding text (CLI banner, ANSI escapes, version prefix) is tolerated —
/// only the *first* numeric triple is honored.
pub fn parse_gemini_version_parts(output: Option<&str>) -> Option<[u32; 3]> {
    let output = output?;
    let captures = version_regex().captures(output)?;
    let m1 = captures.get(1)?.as_str().parse::<u32>().ok()?;
    let m2 = captures.get(2)?.as_str().parse::<u32>().ok()?;
    let m3 = captures.get(3)?.as_str().parse::<u32>().ok()?;
    Some([m1, m2, m3])
}

/// Returns `true` when `parts` ≥ `GEMINI_NATIVE_ACP_FLAG_MIN_VERSION` OR when
/// the version is unknown (`None`). Older versions downgraded to
/// `--experimental-acp`.
pub fn gemini_version_supports_native_acp_flag(parts: Option<[u32; 3]>) -> bool {
    let parts = match parts {
        Some(parts) => parts,
        None => return true,
    };
    let min = GEMINI_NATIVE_ACP_FLAG_MIN_VERSION;
    for index in 0..min.len() {
        let diff = i64::from(parts.get(index).copied().unwrap_or(0)) - i64::from(min[index]);
        if diff != 0 {
            return diff > 0;
        }
    }
    true
}

/// If `version_parts` is known to be too old, swap `--acp` for
/// `--experimental-acp`. The rest of the command line is preserved verbatim.
pub fn rewrite_gemini_acp_flag_for_version(
    command_shell: &str,
    version_parts: Option<[u32; 3]>,
) -> String {
    if gemini_version_supports_native_acp_flag(version_parts) {
        return command_shell.to_string();
    }
    command_shell
        .trim()
        .split_whitespace()
        .map(|token| {
            if token == "--acp" {
                "--experimental-acp"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Tokenize a `gemini --acp` shell command. Returns `None` when the command
/// is not a recognized gemini invocation (the bin is quoted, the basename is
/// not `gemini`, or `--acp` is missing).
pub fn gemini_acp_command_tokens(command_shell: &str) -> Option<Vec<&str>> {
    let tokens: Vec<&str> = command_shell.trim().split_whitespace().collect();
    let bin = *tokens.first()?;
    if bin.starts_with('\'') || bin.starts_with('"') {
        return None;
    }
    let basename = bin.rsplit(['/', '\\']).next().unwrap_or(bin);
    if basename != "gemini" {
        return None;
    }
    if !tokens.contains(&"--acp") {
        return None;
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_handles_clean_and_padded_outputs() {
        assert_eq!(parse_gemini_version_parts(Some("0.30.0")), Some([0, 30, 0]));
        assert_eq!(
            parse_gemini_version_parts(Some("gemini-cli v1.2.3\n")),
            Some([1, 2, 3])
        );
        assert_eq!(parse_gemini_version_parts(Some("no version here")), None);
        assert_eq!(parse_gemini_version_parts(None), None);
    }

    #[test]
    fn native_acp_flag_supported_for_current_or_unknown_versions() {
        assert!(gemini_version_supports_native_acp_flag(Some([0, 33, 0])));
        assert!(gemini_version_supports_native_acp_flag(Some([0, 34, 1])));
        assert!(gemini_version_supports_native_acp_flag(Some([1, 0, 0])));
        assert!(gemini_version_supports_native_acp_flag(None));
        assert_eq!(
            rewrite_gemini_acp_flag_for_version("gemini --acp", Some([0, 33, 0])),
            "gemini --acp"
        );
    }

    #[test]
    fn native_acp_flag_downgraded_for_legacy_versions() {
        assert!(!gemini_version_supports_native_acp_flag(Some([0, 30, 0])));
        assert!(!gemini_version_supports_native_acp_flag(Some([0, 32, 9])));
        assert_eq!(
            rewrite_gemini_acp_flag_for_version("gemini --acp", Some([0, 30, 0])),
            "gemini --experimental-acp"
        );
        assert_eq!(
            rewrite_gemini_acp_flag_for_version("/opt/bin/gemini --acp", Some([0, 30, 0])),
            "/opt/bin/gemini --experimental-acp"
        );
    }

    #[test]
    fn tokenize_recognizes_gemini_invocation() {
        assert_eq!(
            gemini_acp_command_tokens("gemini --acp"),
            Some(vec!["gemini", "--acp"])
        );
        assert_eq!(
            gemini_acp_command_tokens("/opt/bin/gemini --acp --foo bar"),
            Some(vec!["/opt/bin/gemini", "--acp", "--foo", "bar"])
        );
    }

    #[test]
    fn tokenize_rejects_non_gemini_or_quoted_bin() {
        assert_eq!(gemini_acp_command_tokens("'gemini' --acp"), None);
        assert_eq!(gemini_acp_command_tokens("claude --acp"), None);
        assert_eq!(gemini_acp_command_tokens("gemini"), None);
    }
}

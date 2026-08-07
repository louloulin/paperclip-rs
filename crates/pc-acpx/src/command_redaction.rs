//! `pc-acpx::command_redaction` — port of `command-redaction.ts` from
//! Node `paperclip/packages/adapter-utils/src/`.
//!
//! This module hosts a single pure helper, [`redact_command_text`],
//! that scans command / env-var / token text and replaces any
//! embedded secret patterns with a redacted placeholder. The original
//! Node helper is used both in the runtime-target layer (to scrub
//! sandbox-side error logs) and in the adapter log-redaction helper.
//!
//! The Rust port keeps the same precedence order and pattern set:
//!
//! 1. Authorization header bearer (`Authorization: Bearer …`)
//! 2. CLI secret options (`--api-key <value>` / `--token=…`)
//! 3. Env var assignment (`KEY=…` / `KEY="…"`)
//! 4. OpenAI `sk-…` keys
//! 5. GitHub `gh[psor]_…` tokens
//! 6. JWT triples
//!
//! When the input text does not even contain a "looks-like-secret"
//! hint (see [`maybe_contains_secret_text`]), the function returns it
//! untouched — mirroring the Node fast path.

/// Default redaction placeholder used by [`redact_command_text`].
pub const REDACTED_COMMAND_TEXT_VALUE: &str = "***REDACTED***";

/// Hints that trigger the redaction scan. Mirrors Node
/// `COMMAND_SECRET_HINTS`. The presence of any hint in the lowercased
/// text flips the helper into the active replacement branch.
pub const COMMAND_SECRET_HINTS: &[&str] = &[
    "api", "key", "token", "auth", "bearer", "secret", "pass", "credential", "jwt", "private",
    "cookie", "connectionstring", "sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_",
];

/// True when the lowercased text contains any of
/// [`COMMAND_SECRET_HINTS`] **or** a `.`. Mirrors Node
/// `maybeContainsSecretText`.
#[must_use]
pub fn maybe_contains_secret_text(command: &str) -> bool {
    let lower = command.to_lowercase();
    COMMAND_SECRET_HINTS.iter().any(|hint| lower.contains(hint)) || command.contains('.')
}

/// Redact embedded secret patterns in `command`. Returns the input
/// unchanged when [`maybe_contains_secret_text`] returns false; this
/// keeps the hot path branch-free for ordinary log lines.
///
/// The replacement order matches the Node implementation:
///
/// 1. `Authorization: Bearer <value>` → `<value>` becomes
///    [`REDACTED_COMMAND_TEXT_VALUE`].
/// 2. CLI-style secret options (`--api-key <v>` / `--token=<v>`).
/// 3. Env-var assignment (`KEY=…`, `KEY="…"`, `KEY='…'`).
/// 4. OpenAI keys (`sk-` followed by 12+ chars).
/// 5. GitHub tokens (`gh[psour]_` followed by 20+ chars).
/// 6. JWTs (three dot-separated segments, each 8+ chars, optional
///    signature segment).
pub fn redact_command_text(command: &str, redacted_value: Option<&str>) -> String {
    let placeholder = redacted_value.unwrap_or(REDACTED_COMMAND_TEXT_VALUE);
    if !maybe_contains_secret_text(command) {
        return command.to_string();
    }
    redact_command_text_inner(command, placeholder)
}

fn redact_command_text_inner(command: &str, placeholder: &str) -> String {
    // Pattern body for "secret name" — used in both CLI option and env-var paths.
    // We split into quoted (`"…"`, `'…'`) and unquoted branches to avoid
    // backreferences (the Rust `regex` crate does not support them).
    let secret_name = "[A-Za-z0-9_-]*(?:api[-_]?key|(?:access[-_]?|auth[-_]?)?token|token|authorization|bearer|secret|passwd|password|credential|jwt|private[-_]?key|cookie|connectionstring)[A-Za-z0-9_-]*";

    // 1. Authorization: Bearer <value>
    let auth_re = regex::Regex::new(
        "(?i)\\bAuthorization\\s*:\\s*Bearer\\s+[^\\s\"\\']+"
    ).unwrap();

    // CLI options with double quotes: --api-key "value"
    let cli_dq_re = regex::Regex::new(
        format!(
            "(?i)(\\B-{{1,2}}{secret_name}(?:\\s+|=)\"([^\"]*)\")"
        )
        .as_str(),
    ).unwrap();
    // CLI options with single quotes: --api-key 'value'
    let cli_sq_re = regex::Regex::new(
        format!(
            "(?i)(\\B-{{1,2}}{secret_name}(?:\\s+|=)'([^']*)')"
        )
        .as_str(),
    ).unwrap();
    // CLI options unquoted: --api-key value
    let cli_uq_re = regex::Regex::new(
        format!(
            "(?i)(\\B-{{1,2}}{secret_name}(?:\\s+|=))[^\\s\"\\']+"
        )
        .as_str(),
    ).unwrap();

    // Env-var with double quotes: KEY="value"
    let env_dq_re = regex::Regex::new(
        format!(
            "(?i)(\\b{secret_name}\\s*=\\s*)\"([^\"]*)\""
        )
        .as_str(),
    ).unwrap();
    // Env-var with single quotes: KEY='value'
    let env_sq_re = regex::Regex::new(
        format!(
            "(?i)(\\b{secret_name}\\s*=\\s*)'([^']*)'"
        )
        .as_str(),
    ).unwrap();
    // Env-var unquoted: KEY=value
    let env_uq_re = regex::Regex::new(
        format!(
            "(?i)(\\b{secret_name}\\s*=\\s*)[^\\s\"\\']+"
        )
        .as_str(),
    ).unwrap();

    // Token formats
    let openai_re = regex::Regex::new("\\bsk-[A-Za-z0-9_-]{12,}\\b").unwrap();
    let github_re = regex::Regex::new("\\bgh[pousr]_[A-Za-z0-9_]{20,}\\b").unwrap();
    let jwt_re = regex::Regex::new(
        "\\b[A-Za-z0-9_-]{8,}\\.[A-Za-z0-9_-]{8,}\\.[A-Za-z0-9_-]{8,}(?:\\.[A-Za-z0-9_-]{8,})?\\b",
    ).unwrap();

    // Authorization: Bearer
    let s1 = auth_re.replace_all(command, |caps: &regex::Captures<'_>| {
        let matched = caps.get(0).map_or("", |m| m.as_str());
        let bearer_idx = matched
            .to_lowercase()
            .find("bearer")
            .map(|i| i + "bearer".len())
            .unwrap_or(matched.len());
        let prefix = &matched[..bearer_idx];
        format!("{prefix} {placeholder}")
    });

    // CLI replacements
    let s2 = cli_dq_re
        .replace_all(&s1, |caps: &regex::Captures<'_>| {
            let prefix = caps.get(1).map_or("", |m| m.as_str());
            format!("{prefix}\"{placeholder}\"")
        })
        .into_owned();
    let s3 = cli_sq_re
        .replace_all(&s2, |caps: &regex::Captures<'_>| {
            let prefix = caps.get(1).map_or("", |m| m.as_str());
            format!("{prefix}'{placeholder}'")
        })
        .into_owned();
    let s4 = cli_uq_re
        .replace_all(&s3, |caps: &regex::Captures<'_>| {
            let prefix = caps.get(1).map_or("", |m| m.as_str());
            format!("{prefix}{placeholder}")
        })
        .into_owned();

    // Env-var replacements
    let s5 = env_dq_re
        .replace_all(&s4, |caps: &regex::Captures<'_>| {
            let prefix = caps.get(1).map_or("", |m| m.as_str());
            format!("{prefix}\"{placeholder}\"")
        })
        .into_owned();
    let s6 = env_sq_re
        .replace_all(&s5, |caps: &regex::Captures<'_>| {
            let prefix = caps.get(1).map_or("", |m| m.as_str());
            format!("{prefix}'{placeholder}'")
        })
        .into_owned();
    let s7 = env_uq_re
        .replace_all(&s6, |caps: &regex::Captures<'_>| {
            let prefix = caps.get(1).map_or("", |m| m.as_str());
            format!("{prefix}{placeholder}")
        })
        .into_owned();

    // Token replacements
    let s8 = openai_re.replace_all(&s7, placeholder);
    let s9 = github_re.replace_all(&s8, placeholder);
    jwt_re.replace_all(&s9, placeholder).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_passthrough_when_no_secret_hints() {
        let text = "echo hello world";
        assert_eq!(redact_command_text(text, None), text);
    }

    #[test]
    fn redaction_passthrough_when_no_hints_no_dot() {
        let text = "echo hi";
        assert_eq!(redact_command_text(text, None), text);
    }

    #[test]
    fn redacts_authorization_bearer() {
        let text = "curl -H 'Authorization: Bearer abcdef12345' https://api";
        let redacted = redact_command_text(text, None);
        assert!(redacted.contains("Bearer ***REDACTED***"));
        assert!(!redacted.contains("abcdef12345"));
    }

    #[test]
    fn redacts_openai_style_key() {
        let text = "OPENAI_API_KEY=sk-abcdefghijklmnop1234";
        let redacted = redact_command_text(text, None);
        assert!(!redacted.contains("sk-abcdefghijklmnop1234"));
        assert!(redacted.contains("***REDACTED***"));
    }

    #[test]
    fn redacts_github_token() {
        let text = "GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz1234";
        let redacted = redact_command_text(text, None);
        assert!(!redacted.contains("ghp_abcdef"));
        assert!(redacted.contains("***REDACTED***"));
    }

    #[test]
    fn redacts_cli_secret_option() {
        let text = "codex --api-key abcdef12345";
        let redacted = redact_command_text(text, None);
        assert!(!redacted.contains("abcdef12345"));
        assert!(redacted.contains("***REDACTED***"));
    }

    #[test]
    fn redacts_env_assignment() {
        let text = "DATABASE_PASSWORD=hunter2";
        let redacted = redact_command_text(text, None);
        assert!(!redacted.contains("hunter2"));
        assert!(redacted.contains("***REDACTED***"));
    }

    #[test]
    fn redacts_quoted_env_assignment() {
        let text = "API_TOKEN=\"sk-supersecret-1234567890\"";
        let redacted = redact_command_text(text, None);
        assert!(!redacted.contains("sk-supersecret-1234567890"));
        assert!(redacted.contains("***REDACTED***"));
    }

    #[test]
    fn redacts_jwt_with_three_segments() {
        let text = "AUTH_TOKEN=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let redacted = redact_command_text(text, None);
        assert!(!redacted.contains("eyJhbGciOi"));
        assert!(redacted.contains("***REDACTED***"));
    }

    #[test]
    fn respects_custom_placeholder() {
        let text = "Authorization: Bearer abc12345";
        let redacted = redact_command_text(text, Some("[HIDDEN]"));
        assert!(redacted.contains("[HIDDEN]"));
        assert!(!redacted.contains("***REDACTED***"));
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(redact_command_text("", None), "");
    }

    #[test]
    fn preserves_unrelated_text() {
        let text = "echo hello";
        assert_eq!(redact_command_text(text, None), text);
    }
}

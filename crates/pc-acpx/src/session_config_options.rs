//! `pc-acpx` session config options — mirrors Node `sessionConfigOptions`,
//! `resultErrorMessage`, `usageBreakdownsEqual`, `renderPaperclipEnvNote`,
//! and `renderApiAccessNote`. These helpers back the runtime override
//! surface area (`set_config_option`) and the human-readable prompt notes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::session_compat::AcpxPreparedRuntimeLite;

/// Single runtime config override. Mirrors the Node
/// `{ key, value }` shape used by `set_config_option`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionConfigOption {
    pub key: String,
    pub value: String,
}

/// Build the list of `set_config_option` calls the engine should make after
/// ensuring the ACPX session. Mirrors Node `sessionConfigOptions`.
///
/// Claude and Codex pre-set their model/effort via the startup env / config
/// file, so applying them again through ACPX would fail the local picker
/// validation. For those agents we only honor overrides that have no
/// conflict — today, none — so the function returns an empty list.
pub fn session_config_options(prepared: &AcpxPreparedRuntimeLite) -> Vec<SessionConfigOption> {
    let mut options: Vec<SessionConfigOption> = Vec::new();
    let agent = prepared.acpx_agent.as_str();
    if agent != "claude" && agent != "codex" {
        if let Some(model) = prepared
            .requested_model
            .as_deref()
            .filter(|v| !v.is_empty())
        {
            options.push(SessionConfigOption {
                key: "model".to_string(),
                value: model.to_string(),
            });
        }
    }
    if agent != "codex" {
        if let Some(effort) = prepared
            .requested_thinking_effort
            .as_deref()
            .filter(|v| !v.is_empty())
        {
            options.push(SessionConfigOption {
                key: "effort".to_string(),
                value: effort.to_string(),
            });
        }
    }
    if agent != "codex" && prepared.fast_mode {
        options.push(SessionConfigOption {
            key: "service_tier".to_string(),
            value: "fast".to_string(),
        });
        options.push(SessionConfigOption {
            key: "features.fast_mode".to_string(),
            value: "true".to_string(),
        });
    }
    options
}

/// Extract the user-visible error message from a turn result. Mirrors Node
/// `resultErrorMessage` — `None` for a clean turn, `Some(message)` when the
/// runtime reported an error.
pub fn result_error_message(err: &Option<String>) -> Option<String> {
    err.as_ref().filter(|value| !value.is_empty()).cloned()
}

/// Compare two usage-breakdown slices for equality. The slices are sorted
/// and zipped before comparison so order does not matter. Mirrors Node
/// `usageBreakdownsEqual` (post-sort, post-filter of zero entries).
pub fn usage_breakdowns_equal(left: &[(String, f64)], right: &[(String, f64)]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut sorted_left = left.to_vec();
    let mut sorted_right = right.to_vec();
    sorted_left.sort_by(|a, b| a.0.cmp(&b.0));
    sorted_right.sort_by(|a, b| a.0.cmp(&b.0));
    sorted_left
        .iter()
        .zip(sorted_right.iter())
        .all(|(a, b)| a.0 == b.0 && (a.1 - b.1).abs() < f64::EPSILON)
}

/// Render a short note listing the `PAPERCLIP_*` environment variables that
/// are exported into the run. Mirrors Node `renderPaperclipEnvNote`.
///
/// Accepts `&BTreeMap<String, String>` so adapters can pass `context.env`
/// without copying, matching the convention of `env_helpers::has_non_empty_env_value`.
/// The rendered note ends in two trailing newlines (`\n\n`) so it composes
/// cleanly when `join_prompt_sections` drops it between sections.
pub fn render_paperclip_env_note(env: &BTreeMap<String, String>) -> String {
    let mut keys: Vec<&str> = env
        .keys()
        .filter(|key| key.starts_with("PAPERCLIP_"))
        .map(String::as_str)
        .collect();
    keys.sort();
    if keys.is_empty() {
        return String::new();
    }
    format!(
        "Paperclip runtime note:\n\
         The following PAPERCLIP_* environment variables are available in this run: {keys}\n\
         Do not assume these variables are missing without checking your shell environment.\n\n\n",
        keys = keys.join(", ")
    )
}

/// Render a short note showing how to call the Paperclip API. Mirrors Node
/// `renderApiAccessNote` (per-adapter shape).
///
/// Returns `""` unless both `PAPERCLIP_API_URL` and `PAPERCLIP_API_KEY` are
/// present with non-whitespace values. The note ends in two trailing newlines
/// (`\n\n`) so adapters can drop it into a multi-section prompt without
/// spurious blank-line collapsing.
pub fn render_api_access_note(env: &BTreeMap<String, String>) -> String {
    let api_url_present = env
        .get("PAPERCLIP_API_URL")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let api_key_present = env
        .get("PAPERCLIP_API_KEY")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if !api_url_present || !api_key_present {
        return String::new();
    }
    let mut lines: Vec<String> = vec![
        "Paperclip API access note:".to_string(),
        "Use terminal commands with curl to make Paperclip API requests.".to_string(),
        "Normalize the base URL before adding API paths:".to_string(),
        "  PAPERCLIP_API_BASE=\"${PAPERCLIP_API_URL%/}\"; PAPERCLIP_API_BASE=\"${PAPERCLIP_API_BASE%/api}\"".to_string(),
        "GET example:".to_string(),
        "  curl -s -H \"Authorization: Bearer $PAPERCLIP_API_KEY\" \"$PAPERCLIP_API_BASE/api/agents/me\"".to_string(),
    ];
    if let Some(task_id) = env
        .get("PAPERCLIP_TASK_ID")
        .filter(|value| !value.trim().is_empty())
    {
        lines.push("Scoped issue comment example:".to_string());
        lines.push(format!(
            "  curl -s -X POST -H \"Authorization: Bearer $PAPERCLIP_API_KEY\" -H \"Content-Type: application/json\" -H \"X-Paperclip-Run-Id: $PAPERCLIP_RUN_ID\" -d '{{\"body\":\"Status update from agent.\"}}' \"$PAPERCLIP_API_BASE/api/issues/{task_id}/comments\"",
            task_id = task_id.as_str()
        ));
    } else {
        lines.push(
            "Use a real issue id from the current context before making issue write requests."
                .to_string(),
        );
    }
    lines.push(String::new());
    lines.push(String::new());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn render_paperclip_env_note_empty_returns_empty_string() {
        let env = env_from(&[("PATH", "/usr/bin")]);
        assert_eq!(render_paperclip_env_note(&env), "");
    }

    #[test]
    fn render_paperclip_env_note_lists_keys_sorted_without_values() {
        let env = env_from(&[
            ("PAPERCLIP_RUN_ID", "run-1"),
            ("PAPERCLIP_API_KEY", "key-1"),
            ("PAPERCLIP_TASK_ID", "task-1"),
        ]);
        let note = render_paperclip_env_note(&env);
        assert!(note.contains("PAPERCLIP_API_KEY, PAPERCLIP_RUN_ID, PAPERCLIP_TASK_ID"));
        // 仅列变量名，不应包含任何值。
        assert!(!note.contains("run-1"));
        assert!(!note.contains("key-1"));
        assert!(!note.contains("task-1"));
    }

    #[test]
    fn render_paperclip_env_note_ends_with_two_newlines() {
        let env = env_from(&[("PAPERCLIP_RUN_ID", "run-1")]);
        let note = render_paperclip_env_note(&env);
        assert!(note.ends_with("\n\n"));
    }

    #[test]
    fn render_paperclip_env_note_ignores_non_paperclip_keys() {
        let env = env_from(&[
            ("PAPERCLIP_RUN_ID", "run-1"),
            ("NOT_PAPERCLIP", "value"),
            ("PAPERCLIP_TOKEN", "tok-1"),
        ]);
        let note = render_paperclip_env_note(&env);
        assert!(note.contains("PAPERCLIP_RUN_ID"));
        assert!(note.contains("PAPERCLIP_TOKEN"));
        assert!(!note.contains("NOT_PAPERCLIP"));
    }

    #[test]
    fn render_api_access_note_requires_both_url_and_key() {
        assert_eq!(render_api_access_note(&env_from(&[])), "");
        assert_eq!(
            render_api_access_note(&env_from(&[("PAPERCLIP_API_URL", "https://api.test")])),
            ""
        );
        assert_eq!(
            render_api_access_note(&env_from(&[("PAPERCLIP_API_KEY", "sk-test")])),
            ""
        );
    }

    #[test]
    fn render_api_access_note_ignores_whitespace_only_values() {
        let env = env_from(&[
            ("PAPERCLIP_API_URL", "  "),
            ("PAPERCLIP_API_KEY", "sk-test"),
        ]);
        assert_eq!(render_api_access_note(&env), "");
        let env = env_from(&[
            ("PAPERCLIP_API_URL", "https://api.test"),
            ("PAPERCLIP_API_KEY", "  "),
        ]);
        assert_eq!(render_api_access_note(&env), "");
    }

    #[test]
    fn render_api_access_note_with_credentials_returns_get_example() {
        let env = env_from(&[
            ("PAPERCLIP_API_URL", "https://api.test"),
            ("PAPERCLIP_API_KEY", "sk-test"),
        ]);
        let note = render_api_access_note(&env);
        assert!(note.contains("Paperclip API access note"));
        assert!(note.contains("curl"));
        assert!(note.contains("GET example"));
        assert!(note.contains("/api/agents/me"));
        assert!(!note.contains("sk-test"));
    }

    #[test]
    fn render_api_access_note_includes_task_id_comment_example() {
        let env = env_from(&[
            ("PAPERCLIP_API_URL", "https://api.test"),
            ("PAPERCLIP_API_KEY", "sk-test"),
            ("PAPERCLIP_TASK_ID", "issue-42"),
        ]);
        let note = render_api_access_note(&env);
        assert!(note.contains("Scoped issue comment example"));
        assert!(note.contains("issue-42"));
        assert!(note.contains("X-Paperclip-Run-Id"));
    }

    #[test]
    fn render_api_access_note_falls_back_to_real_id_warning_without_task_id() {
        let env = env_from(&[
            ("PAPERCLIP_API_URL", "https://api.test"),
            ("PAPERCLIP_API_KEY", "sk-test"),
        ]);
        let note = render_api_access_note(&env);
        assert!(note.contains("Use a real issue id from the current context"));
        assert!(!note.contains("Scoped issue comment example"));
    }
}

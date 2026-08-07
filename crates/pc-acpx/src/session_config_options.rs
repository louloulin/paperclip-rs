//! `pc-acpx` session config options — mirrors Node `sessionConfigOptions`,
//! `resultErrorMessage`, `usageBreakdownsEqual`, `renderPaperclipEnvNote`,
//! and `renderApiAccessNote`. These helpers back the runtime override
//! surface area (`set_config_option`) and the human-readable prompt notes.

use std::collections::HashMap;

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
pub fn render_paperclip_env_note(env: &HashMap<String, String>) -> String {
    let mut keys: Vec<&str> = env
        .keys()
        .filter(|key| key.starts_with("PAPERCLIP_"))
        .map(String::as_str)
        .collect();
    keys.sort();
    if keys.is_empty() {
        return String::new();
    }
    [
        "Paperclip runtime note:",
        &format!(
            "The following PAPERCLIP_* environment variables are available in this run: {}",
            keys.join(", ")
        ),
        "Do not assume these variables are missing without checking your shell environment.",
    ]
    .join("\n")
}

/// Render a short note showing how to call the Paperclip API. Mirrors Node
/// `renderApiAccessNote`.
pub fn render_api_access_note(env: &HashMap<String, String>) -> String {
    let _api_url = match env.get("PAPERCLIP_API_URL") {
        Some(value) if !value.is_empty() => value,
        _ => return String::new(),
    };
    let api_key = match env.get("PAPERCLIP_API_KEY") {
        Some(value) if !value.is_empty() => value,
        _ => return String::new(),
    };
    let mut lines: Vec<String> = vec![
        "Paperclip API access note:".to_string(),
        "Use terminal commands with curl to make Paperclip API requests."
            .to_string(),
        "Normalize the base URL before adding API paths:".to_string(),
        format!(
            "  PAPERCLIP_API_BASE=\"${{{api_url_env}%/}}\"; PAPERCLIP_API_BASE=\"${{{api_url_env}%/api}}\"",
            api_url_env = "PAPERCLIP_API_URL"
        ),
        "GET example:".to_string(),
        format!(
            "  curl -s -H \"Authorization: Bearer {api_key}\" \"$PAPERCLIP_API_BASE/api/agents/me\""
        ),
    ];
    if let Some(task_id) = env.get("PAPERCLIP_TASK_ID").filter(|v| !v.is_empty()) {
        lines.push("Scoped issue comment example:".to_string());
        lines.push(format!(
            "  curl -s -X POST -H \"Authorization: Bearer {api_key}\" -H \"Content-Type: application/json\" -H \"X-Paperclip-Run-Id: $PAPERCLIP_RUN_ID\" -d '{{\"body\":\"Status update from agent.\"}}' \"$PAPERCLIP_API_BASE/api/issues/{task_id}/comments\""
        ));
    } else {
        lines.push(
            "Use a real issue id from the current context before making issue write requests."
                .to_string(),
        );
    }
    lines.join("\n")
}

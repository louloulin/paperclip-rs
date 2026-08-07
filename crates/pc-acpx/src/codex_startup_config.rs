//! `pc-acpx` codex startup config — pure helper that mirrors Node
//! `buildCodexStartupConfig`. Builds a JSON `config.toml` payload that
//! overlays the runtime-requested model / effort / fast mode on top of the
//! caller's existing config, while flagging malformed existing configs so the
//! engine can surface a warning before launching the runtime.

use serde::{Deserialize, Serialize};

/// Input to [`build_codex_startup_config`]. All fields are raw strings — the
/// helper decides which entries to include in the merged output based on
/// whether each is non-empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexStartupConfigInput {
    /// Existing config.toml contents (already read from disk). `None` means
    /// the file was absent; `Some("")` is treated identically.
    pub existing_config: Option<String>,
    /// Requested model name (e.g. `gpt-5`). Empty means "do not set".
    pub requested_model: String,
    /// Requested reasoning effort (e.g. `high`, `medium`). Empty means
    /// "do not set".
    pub requested_thinking_effort: String,
    /// Whether fast mode was requested.
    pub fast_mode: bool,
}

/// Output of [`build_codex_startup_config`].
///
/// `value` is `None` when no runtime override was requested (model,
/// effort, and fast mode all empty/false). The engine can then avoid
/// rewriting the on-disk config file at all.
///
/// `invalid_existing_config` is `true` when the existing config was
/// provided but failed JSON parsing; the helper still synthesizes a fresh
/// payload so the runtime has something to launch with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexStartupConfigOutput {
    pub value: Option<String>,
    pub invalid_existing_config: bool,
}

/// Build a codex config payload that merges the runtime-requested overrides
/// on top of the existing on-disk config. Mirrors Node
/// `buildCodexStartupConfig`.
pub fn build_codex_startup_config(input: CodexStartupConfigInput) -> CodexStartupConfigOutput {
    let has_runtime_config = !input.requested_model.is_empty()
        || !input.requested_thinking_effort.is_empty()
        || input.fast_mode;
    if !has_runtime_config {
        return CodexStartupConfigOutput {
            value: None,
            invalid_existing_config: false,
        };
    }

    let mut existing: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let mut invalid_existing_config = false;
    if let Some(raw) = input.existing_config.as_ref() {
        if !raw.trim().is_empty() {
            match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(serde_json::Value::Object(map)) => existing = map,
                _ => {
                    invalid_existing_config = true;
                }
            }
        }
    }

    if !input.requested_model.is_empty() {
        existing.insert(
            "model".to_string(),
            serde_json::json!(input.requested_model),
        );
    }
    if !input.requested_thinking_effort.is_empty() {
        existing.insert(
            "model_reasoning_effort".to_string(),
            serde_json::json!(input.requested_thinking_effort),
        );
    }
    if input.fast_mode {
        existing.insert("service_tier".to_string(), serde_json::json!("fast"));
        let features = existing
            .entry("features".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let serde_json::Value::Object(features_map) = features {
            features_map.insert("fast_mode".to_string(), serde_json::json!(true));
        }
    }

    let value = serde_json::to_string(&serde_json::Value::Object(existing))
        .expect("map serialization is infallible");
    CodexStartupConfigOutput {
        value: Some(value),
        invalid_existing_config,
    }
}

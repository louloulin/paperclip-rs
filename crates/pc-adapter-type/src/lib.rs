#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Agent adapter type validation.
//!
//! R555: Direct port of `paperclip/packages/shared/src/adapter-type.ts` (15 LOC).
//! Provides helpers for validating and normalizing agent adapter type strings.

/// Default adapter type, mirroring `agentAdapterTypeSchema.default("process")`.
pub const DEFAULT_AGENT_ADAPTER_TYPE: &str = "process";

/// Built-in adapter types known to paperclip. External adapters may register
/// additional non-empty string types at runtime — see
/// `AGENT_ADAPTER_TYPES` in `constants.ts` for the canonical list.
pub const KNOWN_BUILTIN_ADAPTER_TYPES: &[&str] = &[
    "process",
    "claude_local",
    "codex_local",
    "cursor_local",
    "cursor_cloud",
    "gemini_local",
    "grok_local",
    "hermes",
    "hermes_gateway",
    "openclaw_gateway",
    "opencode_local",
    "pi_local",
];

/// Normalize a raw adapter-type string:
///
/// 1. trim leading/trailing whitespace
/// 2. if empty, return the default ("process")
/// 3. convert hyphens to underscores (matches the convention used by all
///    built-in adapter crates + `KNOWN_BUILTIN_ADAPTER_TYPES` + Node
///    upstream `AGENT_ADAPTER_TYPES`)
///
/// Mirrors `z.string().trim().min(1).default("process")` + post-trim
/// normalization.
pub fn normalize_agent_adapter_type(raw: Option<&str>) -> String {
    let trimmed = raw.map_or("", str::trim);
    if trimmed.is_empty() {
        return DEFAULT_AGENT_ADAPTER_TYPE.to_string();
    }
    trimmed.replace('-', "_")
}

/// Validate that `value` is a non-empty trimmed string. Returns `Some(trimmed)`
/// on success, `None` on failure.
pub fn validate_agent_adapter_type(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Validate an optional adapter type. Returns `Some(trimmed)` if non-empty,
/// `None` if missing/empty.
pub fn validate_optional_agent_adapter_type(value: Option<&str>) -> Option<String> {
    let raw = value?;
    validate_agent_adapter_type(raw)
}

/// Returns true iff `value` is one of the built-in adapter types.
pub fn is_builtin_adapter_type(value: &str) -> bool {
    KNOWN_BUILTIN_ADAPTER_TYPES.contains(&value)
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn normalize_handles_empty() {
        assert_eq!(
            normalize_agent_adapter_type(None),
            DEFAULT_AGENT_ADAPTER_TYPE
        );
        assert_eq!(
            normalize_agent_adapter_type(Some("")),
            DEFAULT_AGENT_ADAPTER_TYPE
        );
    }

    #[test]
    fn normalize_trims_whitespace() {
        assert_eq!(
            normalize_agent_adapter_type(Some("  claude_local  ")),
            "claude_local"
        );
    }

    #[test]
    fn normalize_passes_through() {
        assert_eq!(
            normalize_agent_adapter_type(Some("codex_local")),
            "codex_local"
        );
    }

    #[test]
    fn validate_accepts_non_empty() {
        // validate_* functions only TRIM — they do NOT normalize format.
        // Hyphens in input stay hyphens. Use normalize_agent_adapter_type
        // for hyphen→underscore normalization (which is currently a no-op
        // since adapters use underscore by convention; see R564).
        assert_eq!(
            validate_agent_adapter_type("claude-local"),
            Some("claude-local".into())
        );
    }

    #[test]
    fn validate_rejects_empty() {
        assert_eq!(validate_agent_adapter_type(""), None);
        assert_eq!(validate_agent_adapter_type("   "), None);
    }

    #[test]
    fn validate_optional_passes_through() {
        assert_eq!(
            validate_optional_agent_adapter_type(Some("claude-local")),
            Some("claude-local".into())
        );
        assert_eq!(validate_optional_agent_adapter_type(None), None);
        assert_eq!(validate_optional_agent_adapter_type(Some("")), None);
    }

    #[test]
    fn builtin_recognized() {
        assert!(is_builtin_adapter_type("claude_local"));
        assert!(is_builtin_adapter_type("process"));
    }

    #[test]
    fn custom_adapter_not_builtin() {
        assert!(!is_builtin_adapter_type("my-custom-adapter"));
    }
}

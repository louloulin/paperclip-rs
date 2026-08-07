//! `pc-acpx::session_compaction` - port of `session-compaction.ts` from Node
//! `paperclip/packages/adapter-utils/src/`.
//!
//! Session compaction policy resolution for adapter runs. Determines
//! whether and when to rotate an adapter session based on run count,
//! token usage, and age thresholds.

use serde_json::Value;

/// A session compaction policy.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionCompactionPolicy {
    pub enabled: bool,
    pub max_session_runs: u64,
    pub max_raw_input_tokens: u64,
    pub max_session_age_hours: u64,
}

/// Native context management capability of an adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeContextManagement {
    Confirmed,
    Likely,
    Unknown,
    None,
}

impl NativeContextManagement {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Likely => "likely",
            Self::Unknown => "unknown",
            Self::None => "none",
        }
    }
}

/// Adapter session management metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct AdapterSessionManagement {
    pub supports_session_resume: bool,
    pub native_context_management: NativeContextManagement,
    pub default_session_compaction: SessionCompactionPolicy,
}

/// The source of a resolved policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCompactionSource {
    AdapterDefault,
    AgentOverride,
    LegacyFallback,
}

impl SessionCompactionSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AdapterDefault => "adapter_default",
            Self::AgentOverride => "agent_override",
            Self::LegacyFallback => "legacy_fallback",
        }
    }
}

/// A fully resolved session compaction policy.
#[derive(Debug, Clone)]
pub struct ResolvedSessionCompactionPolicy {
    pub policy: SessionCompactionPolicy,
    pub adapter_session_management: Option<AdapterSessionManagement>,
    pub explicit_override: PartialSessionCompactionPolicy,
    pub source: SessionCompactionSource,
}

/// A partial session compaction policy (for overrides).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PartialSessionCompactionPolicy {
    pub enabled: Option<bool>,
    pub max_session_runs: Option<u64>,
    pub max_raw_input_tokens: Option<u64>,
    pub max_session_age_hours: Option<u64>,
}

impl PartialSessionCompactionPolicy {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.max_session_runs.is_none()
            && self.max_raw_input_tokens.is_none()
            && self.max_session_age_hours.is_none()
    }
}

/// Default session compaction policy.
pub fn default_session_compaction_policy() -> SessionCompactionPolicy {
    SessionCompactionPolicy {
        enabled: true,
        max_session_runs: 200,
        max_raw_input_tokens: 2_000_000,
        max_session_age_hours: 72,
    }
}

/// Adapter-managed session policy (no threshold-based compaction).
pub fn adapter_managed_session_policy() -> SessionCompactionPolicy {
    SessionCompactionPolicy {
        enabled: true,
        max_session_runs: 0,
        max_raw_input_tokens: 0,
        max_session_age_hours: 0,
    }
}

/// Legacy adapter types that support session resume.
pub const LEGACY_SESSIONED_ADAPTER_TYPES: &[&str] = &[
    "claude_local",
    "codex_local",
    "cursor_cloud",
    "cursor",
    "gemini_local",
    "hermes_local",
    "opencode_local",
    "pi_local",
];

/// Get adapter session management metadata for a given adapter type.
#[must_use]
pub fn get_adapter_session_management(
    adapter_type: Option<&str>,
) -> Option<AdapterSessionManagement> {
    let adapter_type = adapter_type?;
    match adapter_type {
        "claude_local" => Some(AdapterSessionManagement {
            supports_session_resume: true,
            native_context_management: NativeContextManagement::Confirmed,
            default_session_compaction: adapter_managed_session_policy(),
        }),
        "codex_local" => Some(AdapterSessionManagement {
            supports_session_resume: true,
            native_context_management: NativeContextManagement::Confirmed,
            default_session_compaction: adapter_managed_session_policy(),
        }),
        "cursor_cloud" | "cursor" | "gemini_local" | "opencode_local" | "pi_local" => {
            Some(AdapterSessionManagement {
                supports_session_resume: true,
                native_context_management: NativeContextManagement::Unknown,
                default_session_compaction: default_session_compaction_policy(),
            })
        }
        "hermes_local" => Some(AdapterSessionManagement {
            supports_session_resume: true,
            native_context_management: NativeContextManagement::Confirmed,
            default_session_compaction: adapter_managed_session_policy(),
        }),
        _ => None,
    }
}

fn is_record(value: &Value) -> bool {
    value.is_object()
}

fn read_boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Number(n) if n.as_f64() == Some(1.0) => Some(true),
        Value::Number(n) if n.as_f64() == Some(0.0) => Some(false),
        Value::String(s) => {
            let normalized = s.trim().to_lowercase();
            match normalized.as_str() {
                "true" | "1" | "yes" | "on" => Some(true),
                "false" | "0" | "no" | "off" => Some(false),
                _ => None,
            }
        }
        _ => None,
    }
}

fn read_number(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => {
            let f = n.as_f64()?;
            if !f.is_finite() {
                return None;
            }
            Some(f.max(0.0).floor() as u64)
        }
        Value::String(s) => {
            let trimmed = s.trim();
            let parsed: f64 = trimmed.parse().ok()?;
            if !parsed.is_finite() {
                return None;
            }
            Some(parsed.max(0.0).floor() as u64)
        }
        _ => None,
    }
}

/// Read session compaction override from a runtime config.
/// Mirrors Node `readSessionCompactionOverride`.
#[must_use]
pub fn read_session_compaction_override(runtime_config: &Value) -> PartialSessionCompactionPolicy {
    let runtime = if is_record(runtime_config) {
        runtime_config
    } else {
        &Value::Null
    };
    let heartbeat = runtime
        .get("heartbeat")
        .filter(|v| is_record(v))
        .unwrap_or(&Value::Null);
    let compaction = heartbeat
        .get("sessionCompaction")
        .or_else(|| heartbeat.get("sessionRotation"))
        .or_else(|| runtime.get("sessionCompaction"))
        .filter(|v| is_record(v))
        .unwrap_or(&Value::Null);

    let mut explicit = PartialSessionCompactionPolicy::default();
    if let Some(enabled) = read_boolean(&compaction.get("enabled").unwrap_or(&Value::Null)) {
        explicit.enabled = Some(enabled);
    }
    if let Some(max_runs) = read_number(&compaction.get("maxSessionRuns").unwrap_or(&Value::Null)) {
        explicit.max_session_runs = Some(max_runs);
    }
    if let Some(max_tokens) =
        read_number(&compaction.get("maxRawInputTokens").unwrap_or(&Value::Null))
    {
        explicit.max_raw_input_tokens = Some(max_tokens);
    }
    if let Some(max_age) =
        read_number(&compaction.get("maxSessionAgeHours").unwrap_or(&Value::Null))
    {
        explicit.max_session_age_hours = Some(max_age);
    }
    explicit
}

/// Resolve the session compaction policy for an adapter type and runtime config.
/// Mirrors Node `resolveSessionCompactionPolicy`.
#[must_use]
pub fn resolve_session_compaction_policy(
    adapter_type: Option<&str>,
    runtime_config: &Value,
) -> ResolvedSessionCompactionPolicy {
    let adapter_session_management = get_adapter_session_management(adapter_type);
    let explicit_override = read_session_compaction_override(runtime_config);
    let has_explicit_override = !explicit_override.is_empty();
    let fallback_enabled = adapter_type
        .map(|t| LEGACY_SESSIONED_ADAPTER_TYPES.contains(&t))
        .unwrap_or(false);

    let base_policy = adapter_session_management
        .as_ref()
        .map(|a| a.default_session_compaction.clone())
        .unwrap_or_else(|| {
            let mut p = default_session_compaction_policy();
            p.enabled = fallback_enabled;
            p
        });

    let policy = SessionCompactionPolicy {
        enabled: explicit_override.enabled.unwrap_or(base_policy.enabled),
        max_session_runs: explicit_override
            .max_session_runs
            .unwrap_or(base_policy.max_session_runs),
        max_raw_input_tokens: explicit_override
            .max_raw_input_tokens
            .unwrap_or(base_policy.max_raw_input_tokens),
        max_session_age_hours: explicit_override
            .max_session_age_hours
            .unwrap_or(base_policy.max_session_age_hours),
    };

    let source = if has_explicit_override {
        SessionCompactionSource::AgentOverride
    } else if adapter_session_management.is_some() {
        SessionCompactionSource::AdapterDefault
    } else {
        SessionCompactionSource::LegacyFallback
    };

    ResolvedSessionCompactionPolicy {
        policy,
        adapter_session_management,
        explicit_override,
        source,
    }
}

/// Check if a policy has any active thresholds.
/// Mirrors Node `hasSessionCompactionThresholds`.
#[must_use]
pub fn has_session_compaction_thresholds(policy: &SessionCompactionPolicy) -> bool {
    policy.max_session_runs > 0
        || policy.max_raw_input_tokens > 0
        || policy.max_session_age_hours > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_policy_has_expected_values() {
        let p = default_session_compaction_policy();
        assert!(p.enabled);
        assert_eq!(p.max_session_runs, 200);
        assert_eq!(p.max_raw_input_tokens, 2_000_000);
        assert_eq!(p.max_session_age_hours, 72);
    }

    #[test]
    fn adapter_managed_policy_has_zero_thresholds() {
        let p = adapter_managed_session_policy();
        assert!(p.enabled);
        assert_eq!(p.max_session_runs, 0);
        assert!(!has_session_compaction_thresholds(&p));
    }

    #[test]
    fn get_adapter_session_management_claude() {
        let m = get_adapter_session_management(Some("claude_local")).unwrap();
        assert!(m.supports_session_resume);
        assert_eq!(
            m.native_context_management,
            NativeContextManagement::Confirmed
        );
    }

    #[test]
    fn get_adapter_session_management_gemini() {
        let m = get_adapter_session_management(Some("gemini_local")).unwrap();
        assert!(m.supports_session_resume);
        assert_eq!(
            m.native_context_management,
            NativeContextManagement::Unknown
        );
    }

    #[test]
    fn get_adapter_session_management_unknown_returns_none() {
        assert!(get_adapter_session_management(Some("unknown_adapter")).is_none());
        assert!(get_adapter_session_management(None).is_none());
    }

    #[test]
    fn read_boolean_parses_various_formats() {
        assert_eq!(read_boolean(&json!(true)), Some(true));
        assert_eq!(read_boolean(&json!(false)), Some(false));
        assert_eq!(read_boolean(&json!(1)), Some(true));
        assert_eq!(read_boolean(&json!(0)), Some(false));
        assert_eq!(read_boolean(&json!("true")), Some(true));
        assert_eq!(read_boolean(&json!("yes")), Some(true));
        assert_eq!(read_boolean(&json!("on")), Some(true));
        assert_eq!(read_boolean(&json!("false")), Some(false));
        assert_eq!(read_boolean(&json!("no")), Some(false));
        assert_eq!(read_boolean(&json!("off")), Some(false));
        assert_eq!(read_boolean(&json!("maybe")), None);
    }

    #[test]
    fn read_number_parses_various_formats() {
        assert_eq!(read_number(&json!(42)), Some(42));
        assert_eq!(read_number(&json!(3.7)), Some(3));
        assert_eq!(read_number(&json!(-5)), Some(0));
        assert_eq!(read_number(&json!("100")), Some(100));
        assert_eq!(read_number(&json!("3.9")), Some(3));
        assert_eq!(read_number(&json!("abc")), None);
    }

    #[test]
    fn read_override_from_heartbeat_session_compaction() {
        let config = json!({
            "heartbeat": {
                "sessionCompaction": {
                    "enabled": false,
                    "maxSessionRuns": 50
                }
            }
        });
        let o = read_session_compaction_override(&config);
        assert_eq!(o.enabled, Some(false));
        assert_eq!(o.max_session_runs, Some(50));
    }

    #[test]
    fn read_override_from_heartbeat_session_rotation_alias() {
        let config = json!({
            "heartbeat": {
                "sessionRotation": {
                    "maxRawInputTokens": 1000000
                }
            }
        });
        let o = read_session_compaction_override(&config);
        assert_eq!(o.max_raw_input_tokens, Some(1_000_000));
    }

    #[test]
    fn read_override_from_top_level_session_compaction() {
        let config = json!({
            "sessionCompaction": {
                "maxSessionAgeHours": 24
            }
        });
        let o = read_session_compaction_override(&config);
        assert_eq!(o.max_session_age_hours, Some(24));
    }

    #[test]
    fn read_override_empty_when_no_config() {
        let o = read_session_compaction_override(&json!({}));
        assert!(o.is_empty());
    }

    #[test]
    fn resolve_policy_adapter_default_for_claude() {
        let resolved = resolve_session_compaction_policy(Some("claude_local"), &json!({}));
        assert_eq!(resolved.source, SessionCompactionSource::AdapterDefault);
        assert!(resolved.adapter_session_management.is_some());
        // Claude uses adapter-managed policy
        assert!(!has_session_compaction_thresholds(&resolved.policy));
    }

    #[test]
    fn resolve_policy_adapter_default_for_gemini() {
        let resolved = resolve_session_compaction_policy(Some("gemini_local"), &json!({}));
        assert_eq!(resolved.source, SessionCompactionSource::AdapterDefault);
        assert!(has_session_compaction_thresholds(&resolved.policy));
        assert_eq!(resolved.policy.max_session_runs, 200);
    }

    #[test]
    fn resolve_policy_legacy_fallback_for_unknown_adapter() {
        let resolved = resolve_session_compaction_policy(Some("unknown"), &json!({}));
        assert_eq!(resolved.source, SessionCompactionSource::LegacyFallback);
        assert!(resolved.adapter_session_management.is_none());
    }

    #[test]
    fn resolve_policy_agent_override_wins_over_default() {
        let config = json!({
            "heartbeat": {
                "sessionCompaction": {
                    "maxSessionRuns": 10
                }
            }
        });
        let resolved = resolve_session_compaction_policy(Some("gemini_local"), &config);
        assert_eq!(resolved.source, SessionCompactionSource::AgentOverride);
        assert_eq!(resolved.policy.max_session_runs, 10);
    }

    #[test]
    fn resolve_policy_legacy_fallback_enabled_for_known_legacy_types() {
        for adapter_type in LEGACY_SESSIONED_ADAPTER_TYPES {
            let resolved = resolve_session_compaction_policy(Some(adapter_type), &json!({}));
            // Either adapter_default or legacy_fallback, but policy should be enabled
            assert!(
                resolved.policy.enabled,
                "{adapter_type} should have enabled policy"
            );
        }
    }

    #[test]
    fn has_thresholds_detects_positive_values() {
        assert!(has_session_compaction_thresholds(
            &SessionCompactionPolicy {
                enabled: true,
                max_session_runs: 1,
                max_raw_input_tokens: 0,
                max_session_age_hours: 0,
            }
        ));
        assert!(has_session_compaction_thresholds(
            &SessionCompactionPolicy {
                enabled: true,
                max_session_runs: 0,
                max_raw_input_tokens: 1,
                max_session_age_hours: 0,
            }
        ));
        assert!(!has_session_compaction_thresholds(
            &SessionCompactionPolicy {
                enabled: true,
                max_session_runs: 0,
                max_raw_input_tokens: 0,
                max_session_age_hours: 0,
            }
        ));
    }
}

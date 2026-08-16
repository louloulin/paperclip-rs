#![forbid(unsafe_code)]

//! Heartbeat misc pure helpers \u2014 1:1 port of paperclip/server/src/services/heartbeat.ts
//!
//! R725: zero-DB helpers for transient fallback resolution, error family
//! classification, env binding validation, and recovery metadata merging.

use serde_json::{Map, Value};

/// Codex transient fallback modes (Node CodexTransientFallbackMode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexTransientFallbackMode {
    NativeResume,
    FreshSpawn,
    Disabled,
}

/// Resolve Codex transient fallback mode based on attempt number.
///
/// Node parity: resolveCodexTransientFallbackMode(attempt) \u2014 attempt 1 uses
/// native resume, attempts 2-3 fresh spawn, otherwise disabled.
pub fn resolve_codex_transient_fallback_mode(attempt: u32) -> CodexTransientFallbackMode {
    match attempt {
        1 => CodexTransientFallbackMode::NativeResume,
        2 | 3 => CodexTransientFallbackMode::FreshSpawn,
        _ => CodexTransientFallbackMode::Disabled,
    }
}

/// Determine whether a run hit maximum turn exhaustion.
///
/// Node parity: isMaxTurnExhaustionRun \u2014 inspects resultJson for a
///  marker.
pub fn is_max_turn_exhaustion_run(result_json: Option<&Value>) -> bool {
    let Some(v) = result_json else { return false; };
    if let Some(code) = v.get("code").and_then(Value::as_str) {
        if code == "max_turns_exceeded" { return true; }
    }
    if let Some(reason) = v.get("reason").and_then(Value::as_str) {
        if reason.contains("max_turns") || reason.contains("turn limit") { return true; }
    }
    false
}

/// Determine whether a failure message looks like a spawn failure.
///
/// Node parity: isSpawnLikeFailureMessage(value) \u2014 matches known phrases.
pub fn is_spawn_like_failure_message(value: Option<&str>) -> bool {
    let Some(s) = value else { return false; };
    let lower = s.to_lowercase();
    let phrases = ["spawn", "enoent", "failed to start", "executable not found"];
    phrases.iter().any(|p| lower.contains(p))
}

/// Determine whether a context snapshot represents an interaction continuation wake.
///
/// Node parity: isResolvedInteractionContinuationWakeContext(contextSnapshot).
pub fn is_resolved_interaction_continuation_wake_context(context_snapshot: Option<&Value>) -> bool {
    let Some(v) = context_snapshot else { return false; };
    v.get("kind").and_then(Value::as_str) == Some("interaction_continuation")
        || v.get("interaction").and_then(Value::as_object).is_some()
}

/// Test whether an env binding value is considered configured (non-empty, non-null).
///
/// Node parity: isConfiguredEnvBindingValue(binding).
pub fn is_configured_env_binding_value(binding: Option<&Value>) -> bool {
    match binding {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        _ => true,
    }
}

/// Test whether the desired skills list contains the github PR workflow skill.
///
/// Node parity: hasGithubPrWorkflowSkill(desiredSkills).
pub fn has_github_pr_workflow_skill(desired_skills: &[String]) -> bool {
    desired_skills.iter().any(|s| {
        let lower = s.to_lowercase();
        lower == "github-pr-workflow" || lower == "github_pr_workflow" || lower.contains("github-pr")
    })
}

/// Merge adapter recovery metadata into a base map.
///
/// Node parity: mergeAdapterRecoveryMetadata(input).
pub fn merge_adapter_recovery_metadata(
    base: Option<Map<String, Value>>,
    recovery: Option<Map<String, Value>>,
) -> Map<String, Value> {
    let mut out = base.unwrap_or_default();
    if let Some(r) = recovery {
        for (k, v) in r {
            out.insert(k, v);
        }
    }
    out
}

/// Strip forbidden env bindings (anything outside the allowed list).
///
/// Node parity: stripForbiddenEnvBindings(envValue) \u2014 returns the cleaned
/// object or null if the input was not an object.
pub fn strip_forbidden_env_bindings(
    env_value: Option<&Value>,
    allowed_keys: &[&str],
) -> Option<Map<String, Value>> {
    let v = env_value?;
    let obj = v.as_object()?;
    let mut out = Map::new();
    for k in obj.keys() {
        if allowed_keys.iter().any(|a| *a == k) {
            if let Some(val) = obj.get(k) {
                out.insert(k.clone(), val.clone());
            }
        }
    }
    Some(out)
}

/// Strip forbidden env entries from an adapter config.
///
/// Node parity: stripForbiddenEnvFromAdapterConfig(config).
pub fn strip_forbidden_env_from_adapter_config(
    config: Option<Map<String, Value>>,
    allowed_keys: &[&str],
) -> Option<Map<String, Value>> {
    let cfg = config?;
    let env_value = cfg.get("env")?;
    let cleaned = strip_forbidden_env_bindings(Some(env_value), allowed_keys)?;
    let mut out = cfg.clone();
    out.insert("env".to_string(), Value::Object(cleaned));
    Some(out)
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_codex_transient_fallback_mode_cases() {
        assert_eq!(resolve_codex_transient_fallback_mode(1), CodexTransientFallbackMode::NativeResume);
        assert_eq!(resolve_codex_transient_fallback_mode(2), CodexTransientFallbackMode::FreshSpawn);
        assert_eq!(resolve_codex_transient_fallback_mode(3), CodexTransientFallbackMode::FreshSpawn);
        assert_eq!(resolve_codex_transient_fallback_mode(4), CodexTransientFallbackMode::Disabled);
        assert_eq!(resolve_codex_transient_fallback_mode(0), CodexTransientFallbackMode::Disabled);
    }

    #[test]
    fn is_max_turn_exhaustion_via_code() {
        assert!(is_max_turn_exhaustion_run(Some(&json!({"code": "max_turns_exceeded"}))));
    }

    #[test]
    fn is_max_turn_exhaustion_via_reason() {
        assert!(is_max_turn_exhaustion_run(Some(&json!({"reason": "Hit max_turns limit"}))));
        assert!(!is_max_turn_exhaustion_run(Some(&json!({"reason": "Some other error"}))));
    }

    #[test]
    fn is_max_turn_exhaustion_none() {
        assert!(!is_max_turn_exhaustion_run(None));
    }

    #[test]
    fn is_spawn_like_failure_message_known_phrases() {
        assert!(is_spawn_like_failure_message(Some("spawn failed: ENOENT")));
        assert!(is_spawn_like_failure_message(Some("Failed to start process")));
        assert!(!is_spawn_like_failure_message(Some("Network error")));
        assert!(!is_spawn_like_failure_message(None));
    }

    #[test]
    fn is_resolved_interaction_continuation_basic() {
        let v = json!({"kind": "interaction_continuation"});
        assert!(is_resolved_interaction_continuation_wake_context(Some(&v)));
        assert!(!is_resolved_interaction_continuation_wake_context(Some(&json!({}))));
        assert!(!is_resolved_interaction_continuation_wake_context(None));
    }

    #[test]
    fn is_configured_env_binding_value_variants() {
        assert!(!is_configured_env_binding_value(None));
        assert!(!is_configured_env_binding_value(Some(&Value::Null)));
        assert!(!is_configured_env_binding_value(Some(&json!(""))));
        assert!(!is_configured_env_binding_value(Some(&json!("   "))));
        assert!(is_configured_env_binding_value(Some(&json!("value"))));
        assert!(is_configured_env_binding_value(Some(&json!(0))));
        assert!(is_configured_env_binding_value(Some(&json!(false))));
    }

    #[test]
    fn has_github_pr_workflow_skill_match() {
        let skills = vec!["github-pr-workflow".into(), "other".into()];
        assert!(has_github_pr_workflow_skill(&skills));
        let skills = vec!["GitHub-PR".into()];
        assert!(has_github_pr_workflow_skill(&skills));
        assert!(!has_github_pr_workflow_skill(&vec![]));
    }

    #[test]
    fn merge_adapter_recovery_metadata_merges_keys() {
        let base = Some(Map::new());
        let mut recovery = Map::new();
        recovery.insert("retry_after".into(), json!(30));
        let out = merge_adapter_recovery_metadata(base, Some(recovery));
        assert_eq!(out.get("retry_after"), Some(&json!(30)));
    }

    #[test]
    fn merge_adapter_recovery_metadata_empty_inputs() {
        let out = merge_adapter_recovery_metadata(None, None);
        assert!(out.is_empty());
    }

    #[test]
    fn strip_forbidden_env_bindings_keeps_allowed() {
        let env = json!({"FOO": "1", "BAR": "2", "BAZ": "3"});
        let cleaned = strip_forbidden_env_bindings(Some(&env), &["FOO", "BAZ"]).unwrap();
        assert_eq!(cleaned.get("FOO"), Some(&json!("1")));
        assert_eq!(cleaned.get("BAZ"), Some(&json!("3")));
        assert!(cleaned.get("BAR").is_none());
    }

    #[test]
    fn strip_forbidden_env_bindings_non_object() {
        assert!(strip_forbidden_env_bindings(Some(&json!("not an object")), &[]).is_none());
        assert!(strip_forbidden_env_bindings(None, &[]).is_none());
    }

    #[test]
    fn strip_forbidden_env_from_adapter_config_returns_cleaned() {
        let mut cfg = Map::new();
        cfg.insert("env".into(), json!({"A": "1", "B": "2"}));
        let out = strip_forbidden_env_from_adapter_config(Some(cfg), &["A"]).unwrap();
        let env = out.get("env").unwrap().as_object().unwrap();
        assert!(env.contains_key("A"));
        assert!(!env.contains_key("B"));
    }
}

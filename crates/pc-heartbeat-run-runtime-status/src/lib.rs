//! Heartbeat run runtime status.
//!
//! 1:1 port of Node `paperclip/server/src/services/heartbeat-run-runtime-status.ts`.
//!
//! Tracks an ephemeral, in-memory snapshot of "what is the run doing
//! right now" for each active heartbeat run. The status is sanitised
//! (whitespace-normalised + secret-redacted + length-capped) before
//! being stored, and is auto-expired after a 90s TTL (configurable).
//!
//! The store is process-local and thread-safe. Tests can inject a
//! custom [`RuntimeStatusStore`] to avoid relying on the global
//! singleton.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use regex::Regex;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------

pub const HEARTBEAT_RUN_RUNTIME_STATUS_TTL_MS: i64 = 90_000;
pub const MAX_HEARTBEAT_RUN_RUNTIME_STATUS_MESSAGE_CHARS: usize = 180;
pub const MAX_HEARTBEAT_RUN_RUNTIME_TOOL_NAME_CHARS: usize = 80;
pub const MAX_HEARTBEAT_RUN_RUNTIME_ASSISTANT_SNIPPET_CHARS: usize = 220;

pub const REDACTED_EVENT_VALUE: &str = "***REDACTED***";

// ---------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------

/// Phases of a heartbeat run. Mirrors `HeartbeatRunStatusPhase` from
/// `@paperclipai/shared` 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatRunStatusPhase {
    GitSync,
    ConfigSync,
    AdapterStartup,
    Restore,
    Export,
    Finalize,
    #[default]
    RunActivity,
}

impl HeartbeatRunStatusPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitSync => "git_sync",
            Self::ConfigSync => "config_sync",
            Self::AdapterStartup => "adapter_startup",
            Self::Restore => "restore",
            Self::Export => "export",
            Self::Finalize => "finalize",
            Self::RunActivity => "run_activity",
        }
    }
}

/// Snapshot of a run's runtime state. `updatedAt` and `lastEventAt` are
/// serialised as ISO-8601 strings for cross-process compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRunRuntimeStatus {
    pub company_id: String,
    pub issue_id: Option<String>,
    pub agent_id: String,
    pub run_id: String,
    pub phase: HeartbeatRunStatusPhase,
    pub message: String,
    pub updated_at: DateTime<Utc>,
    pub current_tool_name: Option<String>,
    pub last_assistant_snippet: Option<String>,
    pub last_event_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct SetStatusInput {
    pub company_id: String,
    pub issue_id: Option<String>,
    pub agent_id: String,
    pub run_id: String,
    pub phase: HeartbeatRunStatusPhase,
    pub message: String,
    pub updated_at: Option<DateTime<Utc>>,
    pub current_tool_name: Option<String>,
    pub last_assistant_snippet: Option<String>,
    pub last_event_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct TouchStatusInput {
    pub company_id: String,
    pub issue_id: Option<String>,
    pub agent_id: String,
    pub run_id: String,
    pub at: Option<DateTime<Utc>>,
    pub fallback_phase: Option<HeartbeatRunStatusPhase>,
    pub fallback_message: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GetStatusExpected<'a> {
    pub company_id: Option<&'a str>,
    pub issue_id: Option<&'a str>,
    pub agent_id: Option<&'a str>,
    pub now: Option<DateTime<Utc>>,
    pub ttl_ms: Option<i64>,
}

// ---------------------------------------------------------------------
// Store trait
// ---------------------------------------------------------------------

/// Storage backend for runtime statuses. The default in-memory
/// implementation lives in [`InMemoryStore`]; tests can inject a
/// recording / asserting implementation.
pub trait RuntimeStatusStore: Send + Sync {
    fn get(&self, run_id: &str) -> Option<HeartbeatRunRuntimeStatus>;
    fn set(&self, status: HeartbeatRunRuntimeStatus);
    fn delete(&self, run_id: &str) -> bool;
    fn clear(&self);
    fn snapshot(&self) -> Vec<(String, HeartbeatRunRuntimeStatus)>;
}

/// Default process-local store.
#[derive(Default)]
pub struct InMemoryStore {
    inner: RwLock<HashMap<String, HeartbeatRunRuntimeStatus>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl RuntimeStatusStore for InMemoryStore {
    fn get(&self, run_id: &str) -> Option<HeartbeatRunRuntimeStatus> {
        self.inner.read().get(run_id).cloned()
    }
    fn set(&self, status: HeartbeatRunRuntimeStatus) {
        self.inner.write().insert(status.run_id.clone(), status);
    }
    fn delete(&self, run_id: &str) -> bool {
        self.inner.write().remove(run_id).is_some()
    }
    fn clear(&self) {
        self.inner.write().clear();
    }
    fn snapshot(&self) -> Vec<(String, HeartbeatRunRuntimeStatus)> {
        self.inner
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

static DEFAULT_STORE: Lazy<Arc<InMemoryStore>> = Lazy::new(|| Arc::new(InMemoryStore::new()));

/// Accessor for the default process-local store.
pub fn default_store() -> Arc<InMemoryStore> {
    DEFAULT_STORE.clone()
}

// ---------------------------------------------------------------------
// Sanitisation
// ---------------------------------------------------------------------

/// Whitespace-normalise + redact secrets + truncate to `max_chars`.
pub fn sanitize_runtime_status_text(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let redacted = redact_sensitive_text(&normalized);
    if redacted.chars().count() <= max_chars {
        return redacted;
    }
    // Truncate on a char boundary, leaving room for "..." suffix.
    let take = max_chars.saturating_sub(3);
    let mut out: String = redacted.chars().take(take).collect();
    out.push_str("...");
    out
}

pub fn sanitize_heartbeat_run_runtime_status_message(message: &str) -> String {
    sanitize_runtime_status_text(message, MAX_HEARTBEAT_RUN_RUNTIME_STATUS_MESSAGE_CHARS)
}

pub fn sanitize_heartbeat_run_runtime_tool_name(tool_name: &str) -> String {
    sanitize_runtime_status_text(tool_name, MAX_HEARTBEAT_RUN_RUNTIME_TOOL_NAME_CHARS)
}

pub fn sanitize_heartbeat_run_runtime_assistant_snippet(snippet: &str) -> String {
    sanitize_runtime_status_text(snippet, MAX_HEARTBEAT_RUN_RUNTIME_ASSISTANT_SNIPPET_CHARS)
}

static SECRET_TEXT_HINTS: &[&str] = &[
    "api", "key", "token", "auth", "bearer", "secret", "pass", "credential", "jwt", "private",
    "cookie", "connectionstring", "sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_",
];

fn maybe_contains_secret_text(input: &str) -> bool {
    let lower = input.to_lowercase();
    SECRET_TEXT_HINTS.iter().any(|h| lower.contains(h)) || input.contains('.')
}

static JSON_SECRET_FIELD_RE: Lazy<Regex> = Lazy::new(|| {
    // Capture group 1 = key + colon + opening quote of value,
    // capture group 2 = closing quote of value.
    let pattern = r#"(?i)("[A-Za-z0-9_-]*(?:api[-_]?key|access[-_]?token|auth(?:_?token)?|token|authorization|bearer|secret|passwd|password|credential|jwt|private[-_]?key|cookie|connectionstring)[A-Za-z0-9_-]*"\s*:\s*")([^"]*)(")"#;
    Regex::new(pattern).expect("JSON_SECRET_FIELD_RE")
});

/// Lightweight equivalent of Node `redactSensitiveText`. Replaces
/// `"secretLikeKey": "value"` with `"secretLikeKey": "***REDACTED***"`.
pub fn redact_sensitive_text(input: &str) -> String {
    if !maybe_contains_secret_text(input) {
        return input.to_string();
    }
    JSON_SECRET_FIELD_RE
        .replace_all(input, |caps: &regex::Captures| {
            let open = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let close = caps.get(3).map(|m| m.as_str()).unwrap_or("");
            format!("{open}{REDACTED_EVENT_VALUE}{close}")
        })
        .to_string()
}

// ---------------------------------------------------------------------
// Public API (operate on any store)
// ---------------------------------------------------------------------

fn is_expired(
    status: &HeartbeatRunRuntimeStatus,
    now: DateTime<Utc>,
    ttl_ms: i64,
) -> bool {
    (now - status.updated_at).num_milliseconds() > ttl_ms
}

/// Set a new runtime status. Returns the stored clone, or `None` if
/// the sanitised message was empty (in which case any prior status for
/// the run is cleared).
pub fn set_heartbeat_run_runtime_status(
    store: &dyn RuntimeStatusStore,
    input: SetStatusInput,
) -> Option<HeartbeatRunRuntimeStatus> {
    let message = sanitize_heartbeat_run_runtime_status_message(&input.message);
    if message.is_empty() {
        clear_heartbeat_run_runtime_status(store, &input.run_id);
        return None;
    }

    let now = input.updated_at.unwrap_or_else(Utc::now);
    let status = HeartbeatRunRuntimeStatus {
        company_id: input.company_id,
        issue_id: input.issue_id,
        agent_id: input.agent_id,
        run_id: input.run_id,
        phase: input.phase,
        message,
        updated_at: now,
        current_tool_name: input
            .current_tool_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(sanitize_heartbeat_run_runtime_tool_name),
        last_assistant_snippet: input
            .last_assistant_snippet
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(sanitize_heartbeat_run_runtime_assistant_snippet),
        last_event_at: input.last_event_at,
    };
    store.set(status.clone());
    Some(status)
}

/// Refresh the activity timestamp of an existing status. If the status
/// has expired or the (company, agent) no longer matches, a new
/// fallback status is created.
pub fn touch_heartbeat_run_runtime_status(
    store: &dyn RuntimeStatusStore,
    input: TouchStatusInput,
) -> Option<HeartbeatRunRuntimeStatus> {
    let at = input.at.unwrap_or_else(Utc::now);
    if let Some(existing) = store.get(&input.run_id) {
        let expired = is_expired(&existing, at, HEARTBEAT_RUN_RUNTIME_STATUS_TTL_MS);
        let company_match = existing.company_id == input.company_id;
        let agent_match = existing.agent_id == input.agent_id;
        if !expired && company_match && agent_match {
            let mut updated = existing.clone();
            if at > updated.updated_at {
                updated.updated_at = at;
            }
            match updated.last_event_at {
                Some(prev) if at <= prev => {}
                _ => updated.last_event_at = Some(at),
            }
            store.set(updated.clone());
            return Some(updated);
        }
    }
    set_heartbeat_run_runtime_status(
        store,
        SetStatusInput {
            company_id: input.company_id,
            issue_id: input.issue_id,
            agent_id: input.agent_id,
            run_id: input.run_id,
            phase: input
                .fallback_phase
                .unwrap_or(HeartbeatRunStatusPhase::RunActivity),
            message: input
                .fallback_message
                .unwrap_or_else(|| "Receiving agent output".to_string()),
            updated_at: Some(at),
            current_tool_name: None,
            last_assistant_snippet: None,
            last_event_at: Some(at),
        },
    )
}

/// Look up a runtime status. If `expected` is supplied, the stored
/// record must match on (company, issue, agent) and must not be
/// expired.
pub fn get_heartbeat_run_runtime_status(
    store: &dyn RuntimeStatusStore,
    run_id: &str,
    expected: Option<GetStatusExpected<'_>>,
) -> Option<HeartbeatRunRuntimeStatus> {
    let status = store.get(run_id)?;
    let expected = expected.unwrap_or_default();
    let now = expected.now.unwrap_or_else(Utc::now);
    let ttl = expected.ttl_ms.unwrap_or(HEARTBEAT_RUN_RUNTIME_STATUS_TTL_MS);
    if is_expired(&status, now, ttl) {
        store.delete(run_id);
        return None;
    }
    if let Some(c) = expected.company_id {
        if status.company_id != c {
            return None;
        }
    }
    if let Some(i) = expected.issue_id {
        if status.issue_id.as_deref() != Some(i) {
            return None;
        }
    }
    if let Some(a) = expected.agent_id {
        if status.agent_id != a {
            return None;
        }
    }
    Some(status)
}

pub fn clear_heartbeat_run_runtime_status(
    store: &dyn RuntimeStatusStore,
    run_id: &str,
) -> bool {
    store.delete(run_id)
}

pub fn clear_all_heartbeat_run_runtime_statuses(store: &dyn RuntimeStatusStore) {
    store.clear();
}

/// Sweep expired statuses from the store. Returns the count of removed
/// entries.
pub fn sweep_expired_heartbeat_run_runtime_statuses(
    store: &dyn RuntimeStatusStore,
    now: Option<DateTime<Utc>>,
    ttl_ms: Option<i64>,
) -> usize {
    let now = now.unwrap_or_else(Utc::now);
    let ttl = ttl_ms.unwrap_or(HEARTBEAT_RUN_RUNTIME_STATUS_TTL_MS);
    let snap = store.snapshot();
    let mut swept = 0usize;
    for (run_id, status) in snap {
        if is_expired(&status, now, ttl) {
            if store.delete(&run_id) {
                swept += 1;
            }
        }
    }
    swept
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> InMemoryStore {
        InMemoryStore::new()
    }

    fn input_for_run(run_id: &str) -> SetStatusInput {
        SetStatusInput {
            company_id: "company-1".to_string(),
            issue_id: Some("issue-1".to_string()),
            agent_id: "agent-1".to_string(),
            run_id: run_id.to_string(),
            phase: HeartbeatRunStatusPhase::RunActivity,
            message: "Working on it".to_string(),
            ..Default::default()
        }
    }

    // -------- sanitisation --------

    #[test]
    fn sanitize_preserves_short_text() {
        let out = sanitize_heartbeat_run_runtime_status_message("Hello, world");
        assert_eq!(out, "Hello, world");
    }

    #[test]
    fn sanitize_normalises_whitespace() {
        let out = sanitize_heartbeat_run_runtime_status_message("hello\n\n\tworld   foo");
        assert_eq!(out, "hello world foo");
    }

    #[test]
    fn sanitize_truncates_long_message() {
        let long = "a".repeat(500);
        let out = sanitize_heartbeat_run_runtime_status_message(&long);
        assert_eq!(out.chars().count(), MAX_HEARTBEAT_RUN_RUNTIME_STATUS_MESSAGE_CHARS);
        assert!(out.ends_with("..."));
    }

    #[test]
    fn sanitize_truncates_tool_name() {
        let long = "x".repeat(200);
        let out = sanitize_heartbeat_run_runtime_tool_name(&long);
        assert_eq!(out.chars().count(), MAX_HEARTBEAT_RUN_RUNTIME_TOOL_NAME_CHARS);
    }

    #[test]
    fn sanitize_truncates_assistant_snippet() {
        let long = "y".repeat(500);
        let out = sanitize_heartbeat_run_runtime_assistant_snippet(&long);
        assert_eq!(out.chars().count(), MAX_HEARTBEAT_RUN_RUNTIME_ASSISTANT_SNIPPET_CHARS);
    }

    #[test]
    fn sanitize_redacts_api_key_in_message() {
        let out = sanitize_heartbeat_run_runtime_status_message(
            r#"Using token "apiKey": "sk-abc123" to call the API"#,
        );
        assert!(out.contains(REDACTED_EVENT_VALUE));
        assert!(!out.contains("sk-abc123"));
    }

    #[test]
    fn sanitize_passes_through_normal_text() {
        let out = sanitize_heartbeat_run_runtime_status_message("Working on PR review");
        assert_eq!(out, "Working on PR review");
    }

    // -------- set / get --------

    #[test]
    fn set_stores_status_and_returns_clone() {
        let s = fresh();
        let stored = set_heartbeat_run_runtime_status(&s, input_for_run("run-1")).unwrap();
        assert_eq!(stored.message, "Working on it");
        assert_eq!(stored.phase, HeartbeatRunStatusPhase::RunActivity);
        let got = get_heartbeat_run_runtime_status(&s, "run-1", None);
        assert_eq!(got.unwrap().message, "Working on it");
    }

    #[test]
    fn set_with_empty_message_clears_existing() {
        let s = fresh();
        set_heartbeat_run_runtime_status(&s, input_for_run("run-1")).unwrap();
        let mut input = input_for_run("run-1");
        input.message = "   \n  ".to_string();
        let res = set_heartbeat_run_runtime_status(&s, input);
        assert!(res.is_none());
        assert!(get_heartbeat_run_runtime_status(&s, "run-1", None).is_none());
    }

    #[test]
    fn set_sanitises_message_before_storage() {
        let s = fresh();
        let mut input = input_for_run("run-1");
        input.message = "  hello   world  ".to_string();
        let stored = set_heartbeat_run_runtime_status(&s, input).unwrap();
        assert_eq!(stored.message, "hello world");
    }

    #[test]
    fn set_sanitises_tool_name_and_snippet() {
        let s = fresh();
        let mut input = input_for_run("run-1");
        input.current_tool_name = Some("   tool   name   ".to_string());
        input.last_assistant_snippet = Some("  snippet  ".to_string());
        let stored = set_heartbeat_run_runtime_status(&s, input).unwrap();
        assert_eq!(stored.current_tool_name.as_deref(), Some("tool name"));
        assert_eq!(stored.last_assistant_snippet.as_deref(), Some("snippet"));
    }

    #[test]
    fn set_omits_empty_tool_name_and_snippet() {
        let s = fresh();
        let mut input = input_for_run("run-1");
        input.current_tool_name = Some("".to_string());
        input.last_assistant_snippet = Some("".to_string());
        let stored = set_heartbeat_run_runtime_status(&s, input).unwrap();
        assert!(stored.current_tool_name.is_none());
        assert!(stored.last_assistant_snippet.is_none());
    }

    // -------- get filters --------

    #[test]
    fn get_filters_by_company_mismatch() {
        let s = fresh();
        set_heartbeat_run_runtime_status(&s, input_for_run("run-1")).unwrap();
        let got = get_heartbeat_run_runtime_status(
            &s,
            "run-1",
            Some(GetStatusExpected {
                company_id: Some("other-company"),
                ..Default::default()
            }),
        );
        assert!(got.is_none());
    }

    #[test]
    fn get_filters_by_agent_mismatch() {
        let s = fresh();
        set_heartbeat_run_runtime_status(&s, input_for_run("run-1")).unwrap();
        let got = get_heartbeat_run_runtime_status(
            &s,
            "run-1",
            Some(GetStatusExpected {
                agent_id: Some("other-agent"),
                ..Default::default()
            }),
        );
        assert!(got.is_none());
    }

    #[test]
    fn get_filters_by_issue_mismatch() {
        let s = fresh();
        set_heartbeat_run_runtime_status(&s, input_for_run("run-1")).unwrap();
        let got = get_heartbeat_run_runtime_status(
            &s,
            "run-1",
            Some(GetStatusExpected {
                issue_id: Some("other-issue"),
                ..Default::default()
            }),
        );
        assert!(got.is_none());
    }

    #[test]
    fn get_returns_null_for_unknown_run() {
        let s = fresh();
        assert!(get_heartbeat_run_runtime_status(&s, "missing", None).is_none());
    }

    #[test]
    fn get_clears_expired_entry() {
        let s = fresh();
        set_heartbeat_run_runtime_status(&s, input_for_run("run-1")).unwrap();
        // Force expiration by passing a `now` well past the TTL.
        let later = Utc::now() + chrono::Duration::seconds(600);
        let got = get_heartbeat_run_runtime_status(
            &s,
            "run-1",
            Some(GetStatusExpected {
                now: Some(later),
                ..Default::default()
            }),
        );
        assert!(got.is_none());
        // The expired entry was deleted.
        assert!(get_heartbeat_run_runtime_status(&s, "run-1", None).is_none());
    }

    // -------- touch --------

    #[test]
    fn touch_refreshes_existing_status() {
        let s = fresh();
        let initial = set_heartbeat_run_runtime_status(&s, input_for_run("run-1")).unwrap();
        let later = initial.updated_at + chrono::Duration::seconds(5);
        let updated = touch_heartbeat_run_runtime_status(
            &s,
            TouchStatusInput {
                company_id: "company-1".to_string(),
                issue_id: Some("issue-1".to_string()),
                agent_id: "agent-1".to_string(),
                run_id: "run-1".to_string(),
                at: Some(later),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.updated_at, later);
        assert_eq!(updated.last_event_at, Some(later));
    }

    #[test]
    fn touch_creates_fallback_when_no_existing() {
        let s = fresh();
        let res = touch_heartbeat_run_runtime_status(
            &s,
            TouchStatusInput {
                company_id: "c".to_string(),
                issue_id: None,
                agent_id: "a".to_string(),
                run_id: "r".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(res.phase, HeartbeatRunStatusPhase::RunActivity);
        assert_eq!(res.message, "Receiving agent output");
    }

    #[test]
    fn touch_creates_fallback_when_expired() {
        let s = fresh();
        let mut input = input_for_run("run-1");
        input.updated_at = Some(Utc::now() - chrono::Duration::seconds(600));
        set_heartbeat_run_runtime_status(&s, input).unwrap();
        let res = touch_heartbeat_run_runtime_status(
            &s,
            TouchStatusInput {
                company_id: "company-1".to_string(),
                issue_id: Some("issue-1".to_string()),
                agent_id: "agent-1".to_string(),
                run_id: "run-1".to_string(),
                at: Some(Utc::now()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(res.message, "Receiving agent output");
    }

    #[test]
    fn touch_creates_fallback_on_agent_mismatch() {
        let s = fresh();
        set_heartbeat_run_runtime_status(&s, input_for_run("run-1")).unwrap();
        let res = touch_heartbeat_run_runtime_status(
            &s,
            TouchStatusInput {
                company_id: "company-1".to_string(),
                issue_id: Some("issue-1".to_string()),
                agent_id: "different-agent".to_string(),
                run_id: "run-1".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(res.message, "Receiving agent output");
    }

    #[test]
    fn touch_does_not_move_updated_at_backwards() {
        let s = fresh();
        let initial = set_heartbeat_run_runtime_status(&s, input_for_run("run-1")).unwrap();
        let earlier = initial.updated_at - chrono::Duration::seconds(5);
        let updated = touch_heartbeat_run_runtime_status(
            &s,
            TouchStatusInput {
                company_id: "company-1".to_string(),
                issue_id: Some("issue-1".to_string()),
                agent_id: "agent-1".to_string(),
                run_id: "run-1".to_string(),
                at: Some(earlier),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.updated_at, initial.updated_at);
    }

    // -------- clear / sweep --------

    #[test]
    fn clear_returns_true_for_existing_run() {
        let s = fresh();
        set_heartbeat_run_runtime_status(&s, input_for_run("run-1")).unwrap();
        assert!(clear_heartbeat_run_runtime_status(&s, "run-1"));
    }

    #[test]
    fn clear_returns_false_for_missing_run() {
        let s = fresh();
        assert!(!clear_heartbeat_run_runtime_status(&s, "nope"));
    }

    #[test]
    fn clear_all_removes_everything() {
        let s = fresh();
        set_heartbeat_run_runtime_status(&s, input_for_run("r1")).unwrap();
        set_heartbeat_run_runtime_status(&s, input_for_run("r2")).unwrap();
        clear_all_heartbeat_run_runtime_statuses(&s);
        assert_eq!(s.snapshot().len(), 0);
    }

    #[test]
    fn sweep_removes_expired_and_keeps_fresh() {
        let s = fresh();
        // expired
        let mut old = input_for_run("r1");
        old.updated_at = Some(Utc::now() - chrono::Duration::seconds(600));
        set_heartbeat_run_runtime_status(&s, old).unwrap();
        // fresh
        set_heartbeat_run_runtime_status(&s, input_for_run("r2")).unwrap();

        let swept = sweep_expired_heartbeat_run_runtime_statuses(&s, None, None);
        assert_eq!(swept, 1);
        let snap: Vec<String> = s.snapshot().into_iter().map(|(k, _)| k).collect();
        assert_eq!(snap, vec!["r2".to_string()]);
    }

    #[test]
    fn sweep_with_custom_ttl_respects_value() {
        let s = fresh();
        let mut input = input_for_run("r1");
        input.updated_at = Some(Utc::now() - chrono::Duration::seconds(10));
        set_heartbeat_run_runtime_status(&s, input).unwrap();
        // Default TTL is 90s; with 5s TTL the entry is expired.
        let swept = sweep_expired_heartbeat_run_runtime_statuses(
            &s,
            Some(Utc::now()),
            Some(5_000),
        );
        assert_eq!(swept, 1);
    }

    // -------- serde --------

    #[test]
    fn phase_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(HeartbeatRunStatusPhase::RunActivity).unwrap(),
            serde_json::json!("run_activity")
        );
    }

    #[test]
    fn status_dto_serializes_camel_case() {
        let s = fresh();
        let stored = set_heartbeat_run_runtime_status(&s, input_for_run("r")).unwrap();
        let v = serde_json::to_value(&stored).unwrap();
        assert!(v["companyId"].is_string());
        assert!(v["updatedAt"].is_string());
        assert_eq!(v["phase"], serde_json::json!("run_activity"));
    }
}

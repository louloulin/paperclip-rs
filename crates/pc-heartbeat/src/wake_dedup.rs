//! Heartbeat wakeup dedup & coalesce (pure function part).
//!
//! Mirrors Node `services/heartbeat.ts`:
//! - `mergeWakeCommentIds(...values)` —— dedup + merge multi-source comment IDs
//! - `enqueueWakeup(...)` `findActiveWakeupRequest` + `coalescedCount++` pattern
//! - `enrichWakeContextSnapshot(...)` —— merge context fields from multiple sources
//!
//! Design:
//! - Pure functions with no side effects (except `existing: Option<&WakeSnapshot>` input)
//! - `WakeAction` three-state decision: `Create { row }` | `Coalesce { into_id, merged_payload, increment }` | `Skip { reason }`
//! - Fully decoupled from SQL / actor / IO; caller is responsible for execution
//! - Single responsibility: dedup decision + payload merge; idempotency key generation is a separate concern (handled in readiness module)

use serde_json::Value;

// ============================================================================
// Constants
// ============================================================================

/// Wakeup payload keys that go through incoming-priority merge (other fields use incoming overwrite).
///
/// Node-side `enrichWakeContextSnapshot` extracts these fields from payload / contextSnapshot:
/// - issueId / taskId / taskKey / projectId / commentId / wakeCommentId / wakeReason
pub const WAKE_CONTEXT_KEYS: &[&str] = &[
    "issueId",
    "taskId",
    "taskKey",
    "projectId",
    "commentId",
    "wakeCommentId",
    "wakeReason",
];

/// payload field name storing merged comment ID list (mirrors Node `wakeCommentIds` 1:1).
pub const WAKE_COMMENT_IDS_KEY: &str = "wakeCommentIds";

// ============================================================================
// Types
// ============================================================================

/// Wake decision input: existing wakeup snapshot (minimum subset needed for decision).
#[derive(Debug, Clone)]
pub struct WakeSnapshot {
    pub id: String,
    pub agent_id: String,
    pub company_id: String,
    pub status: String,
    pub coalesced_count: i32,
    pub payload: Option<Value>,
}

/// Wake decision input: new incoming wakeup.
#[derive(Debug, Clone)]
pub struct WakeInput {
    pub agent_id: String,
    pub company_id: String,
    pub source: String,
    pub reason: Option<String>,
    pub payload: Option<Value>,
    pub idempotency_key: Option<String>,
}

/// Wake dedup decision output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeAction {
    /// No active wakeup exists -> create new row
    Create,
    /// Active wakeup exists -> merge into existing (payload merge + coalesced_count +1)
    Coalesce { into_id: String, increment: i32 },
    /// Skip creation (e.g. agent mismatch, idempotency_key conflict)
    Skip { reason: String },
}

impl WakeAction {
    pub fn is_create(&self) -> bool {
        matches!(self, WakeAction::Create)
    }

    pub fn is_coalesce(&self) -> bool {
        matches!(self, WakeAction::Coalesce { .. })
    }

    pub fn is_skip(&self) -> bool {
        matches!(self, WakeAction::Skip { .. })
    }
}

// ============================================================================
// Decision
// ============================================================================

/// Decide whether to coalesce into the existing wakeup.
///
/// Rules (mirror Node `enqueueWakeup` 1:1):
/// 1. No existing -> Create
/// 2. existing.status not in active set (queued/requested/claimed/deferred_issue_execution) -> Create
/// 3. existing.agent_id != incoming.agent_id -> Skip
/// 4. existing.company_id != incoming.company_id -> Skip
/// 5. Otherwise -> Coalesce (coalesced_count +1, payload merged)
pub fn decide_wake_action(existing: Option<&WakeSnapshot>, incoming: &WakeInput) -> WakeAction {
    let Some(existing) = existing else {
        return WakeAction::Create;
    };

    if !is_active_wakeup_status(&existing.status) {
        return WakeAction::Create;
    }

    if existing.company_id != incoming.company_id {
        return WakeAction::Skip {
            reason: format!(
                "company mismatch: existing={} incoming={}",
                existing.company_id, incoming.company_id
            ),
        };
    }

    if existing.agent_id != incoming.agent_id {
        return WakeAction::Skip {
            reason: format!(
                "agent mismatch: existing={} incoming={}",
                existing.agent_id, incoming.agent_id
            ),
        };
    }

    WakeAction::Coalesce {
        into_id: existing.id.clone(),
        increment: 1,
    }
}

/// Active wakeup status set (equivalent to Node `status IN ('requested', 'claimed')`,
/// additionally includes `queued` / `deferred_issue_execution` to align with Rust repo state).
pub fn is_active_wakeup_status(status: &str) -> bool {
    matches!(
        status,
        "queued" | "requested" | "claimed" | "deferred_issue_execution"
    )
}

// ============================================================================
// Payload merge
// ============================================================================

/// Merge two wakeup payloads.
///
/// Rules:
/// - `wakeCommentIds`: union + dedup (dedup preserves first-occurrence order)
/// - Other WAKE_CONTEXT_KEYS: incoming overwrites existing when incoming is non-null
/// - Other fields: incoming wins; existing kept when incoming is missing
///
/// Returns new Value (does not modify inputs).
pub fn merge_wake_payloads(existing: Option<&Value>, incoming: Option<&Value>) -> Value {
    let existing = existing.cloned().unwrap_or(Value::Null);
    let incoming = incoming.cloned().unwrap_or(Value::Null);

    match (existing, incoming) {
        (Value::Null, Value::Null) => Value::Null,
        (Value::Null, v) => v,
        (v, Value::Null) => v,
        (Value::Object(mut a), Value::Object(b)) => {
            for key in WAKE_CONTEXT_KEYS {
                if let Some(incoming_val) = b.get(*key) {
                    if !incoming_val.is_null() {
                        a.insert((*key).to_string(), incoming_val.clone());
                    }
                }
            }
            if let Some(incoming_ids) = b.get(WAKE_COMMENT_IDS_KEY).and_then(|v| v.as_array()) {
                let merged = merge_wake_comment_ids_from(
                    a.get(WAKE_COMMENT_IDS_KEY).and_then(|v| v.as_array()),
                    Some(incoming_ids),
                );
                a.insert(WAKE_COMMENT_IDS_KEY.to_string(), Value::Array(merged));
            }
            for (key, val) in b {
                if WAKE_CONTEXT_KEYS.contains(&key.as_str()) || key == WAKE_COMMENT_IDS_KEY {
                    continue;
                }
                a.insert(key, val);
            }
            Value::Object(a)
        }
        (_, v) => v,
    }
}

/// Merge wake comment IDs from multiple sources.
///
/// - Accepts any iterator of `&Value` (or `Value` via `&` deref)
/// - String: used directly as ID
/// - Array: flatten each entry
/// - Object: recursively extract `wakeCommentIds` / `commentId` / `wakeCommentId`
/// - Dedup preserves first-occurrence order
pub fn merge_wake_comment_ids<'a, I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a Value>,
{
    let mut merged: Vec<String> = Vec::new();
    for v in values {
        collect_comment_ids_from(v, &mut merged);
    }
    merged
}

fn collect_comment_ids_from(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Null => {}
        Value::String(s) => push_unique(out, s),
        Value::Array(arr) => {
            for entry in arr {
                collect_comment_ids_from(entry, out);
            }
        }
        Value::Object(obj) => {
            if let Some(Value::Array(ids)) = obj.get(WAKE_COMMENT_IDS_KEY) {
                for id in ids {
                    collect_comment_ids_from(id, out);
                }
                return;
            }
            for key in ["wakeCommentId", "commentId"] {
                if let Some(Value::String(s)) = obj.get(key) {
                    push_unique(out, s);
                }
            }
        }
        _ => {}
    }
}

fn merge_wake_comment_ids_from(
    existing: Option<&Vec<Value>>,
    incoming: Option<&Vec<Value>>,
) -> Vec<Value> {
    let mut merged: Vec<String> = Vec::new();
    if let Some(arr) = existing {
        for v in arr {
            collect_comment_ids_from(v, &mut merged);
        }
    }
    if let Some(arr) = incoming {
        for v in arr {
            collect_comment_ids_from(v, &mut merged);
        }
    }
    merged.into_iter().map(Value::String).collect()
}

fn push_unique(out: &mut Vec<String>, s: &str) {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return;
    }
    if !out.iter().any(|x| x == trimmed) {
        out.push(trimmed.to_string());
    }
}

// ============================================================================
// Suppression resolution
// ============================================================================

/// Suppression inputs (mirror Node `SuppressionSnapshot` 1:1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuppressionInputs {
    /// env.PAPERCLIP_IN_WORKTREE
    pub in_worktree: bool,
    /// env.PAPERCLIP_DATABASE_RESTORE_IN_PROGRESS || env.PAPERCLIP_RESTORE_IN_PROGRESS
    pub database_restore_in_progress: bool,
    /// DB experimental.enableWorktreeRunExecution (true = DB override disables suppression)
    pub db_worktree_override_armed: bool,
}

impl SuppressionInputs {
    pub fn from_env(env: &std::collections::HashMap<String, String>) -> Self {
        fn truthy(v: Option<&String>) -> bool {
            matches!(v.map(|s| s.as_str()), Some("true" | "1" | "yes" | "on"))
        }
        Self {
            in_worktree: truthy(env.get("PAPERCLIP_IN_WORKTREE")),
            database_restore_in_progress: truthy(env.get("PAPERCLIP_DATABASE_RESTORE_IN_PROGRESS"))
                || truthy(env.get("PAPERCLIP_RESTORE_IN_PROGRESS")),
            db_worktree_override_armed: false,
        }
    }

    /// R557：从 DB experimental flag 装载 `db_worktree_override_armed`。
    ///
    /// 与 Node `experimental.enableWorktreeRunExecution` 字段对齐:
    /// 当 DB 行 `experimental.enable_worktree_run_execution = true` 时,
    /// 即使 `in_worktree` 也不会被抑制。
    ///
    /// 这是默认 `from_env` 不读取的字段(env 没有该信号),
    /// 调用方在加载 DB 后调用此方法覆盖。
    pub fn with_db_override(mut self, armed: bool) -> Self {
        self.db_worktree_override_armed = armed;
        self
    }

    /// R557：从 `experimental_flags` 行(若存在)装载 DB override。
    ///
    /// 接受 `Option<bool>`:`None` 表示 DB 行不存在(保持当前值),
    /// `Some(b)` 表示装载具体值。
    pub fn with_db_override_opt(mut self, armed: Option<bool>) -> Self {
        if let Some(b) = armed {
            self.db_worktree_override_armed = b;
        }
        self
    }
}

/// Suppression decision result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppressionDecision {
    pub suppressed: bool,
    pub reason: SuppressionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressionReason {
    /// Not suppressed.
    None,
    /// DB backup restore in progress, force-suppressed.
    DatabaseRestoreInProgress,
    /// Worktree instance without DB override armed.
    WorktreeInstance,
}

impl SuppressionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::DatabaseRestoreInProgress => "database_restore_in_progress",
            Self::WorktreeInstance => "worktree_instance",
        }
    }
}

/// Resolve heartbeat scheduling suppression.
///
/// Priority (mirror Node `resolveHeartbeatSchedulingSuppression` 1:1):
/// 1. DB restore in progress -> suppress (highest priority, override cannot lift)
/// 2. Worktree instance + DB override armed -> not suppressed
/// 3. Worktree instance + DB override not armed -> suppress
/// 4. Non-worktree instance -> not suppressed
pub fn resolve_suppression(inputs: &SuppressionInputs) -> SuppressionDecision {
    if inputs.database_restore_in_progress {
        return SuppressionDecision {
            suppressed: true,
            reason: SuppressionReason::DatabaseRestoreInProgress,
        };
    }
    if inputs.in_worktree {
        if inputs.db_worktree_override_armed {
            return SuppressionDecision {
                suppressed: false,
                reason: SuppressionReason::None,
            };
        }
        return SuppressionDecision {
            suppressed: true,
            reason: SuppressionReason::WorktreeInstance,
        };
    }
    SuppressionDecision {
        suppressed: false,
        reason: SuppressionReason::None,
    }
}

// ============================================================================
// Idempotency key builders
// ============================================================================

/// Build idempotency key for issue assignment wake.
///
/// Format: `issue_assignment_wake:<company_id>:<agent_id>:<issue_id>`
pub fn build_issue_assignment_wake_key(company_id: &str, agent_id: &str, issue_id: &str) -> String {
    format!("issue_assignment_wake:{company_id}:{agent_id}:{issue_id}")
}

/// Build idempotency key for decision continuation wake.
///
/// Format: `decision_continuation:<decision_id>`
pub fn build_decision_continuation_wake_key(decision_id: &str) -> String {
    format!("decision_continuation:{decision_id}")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn existing(id: &str, status: &str, coalesced: i32) -> WakeSnapshot {
        WakeSnapshot {
            id: id.to_string(),
            agent_id: "a-1".to_string(),
            company_id: "co-1".to_string(),
            status: status.to_string(),
            coalesced_count: coalesced,
            payload: None,
        }
    }

    fn incoming() -> WakeInput {
        WakeInput {
            agent_id: "a-1".to_string(),
            company_id: "co-1".to_string(),
            source: "assignment".to_string(),
            reason: Some("issue_assigned".to_string()),
            payload: Some(json!({"issueId": "iss-1"})),
            idempotency_key: None,
        }
    }

    #[test]
    fn decide_create_when_no_existing() {
        let action = decide_wake_action(None, &incoming());
        assert!(action.is_create());
    }

    #[test]
    fn decide_create_when_existing_is_terminal() {
        let action = decide_wake_action(Some(&existing("w-1", "completed", 0)), &incoming());
        assert!(action.is_create());
    }

    #[test]
    fn decide_coalesce_into_existing_queued() {
        let action = decide_wake_action(Some(&existing("w-1", "queued", 0)), &incoming());
        match action {
            WakeAction::Coalesce { into_id, increment } => {
                assert_eq!(into_id, "w-1");
                assert_eq!(increment, 1);
            }
            _ => panic!("expected Coalesce, got {action:?}"),
        }
    }

    #[test]
    fn decide_coalesce_into_existing_requested() {
        let action = decide_wake_action(Some(&existing("w-1", "requested", 2)), &incoming());
        assert!(action.is_coalesce());
    }

    #[test]
    fn decide_skip_when_agent_mismatch() {
        let mut inc = incoming();
        inc.agent_id = "a-2".to_string();
        let action = decide_wake_action(Some(&existing("w-1", "queued", 0)), &inc);
        match action {
            WakeAction::Skip { reason } => {
                assert!(reason.contains("agent mismatch"), "reason: {reason}");
            }
            _ => panic!("expected Skip, got {action:?}"),
        }
    }

    #[test]
    fn decide_skip_when_company_mismatch() {
        let mut inc = incoming();
        inc.company_id = "co-2".to_string();
        let action = decide_wake_action(Some(&existing("w-1", "queued", 0)), &inc);
        match action {
            WakeAction::Skip { reason } => {
                assert!(reason.contains("company mismatch"), "reason: {reason}");
            }
            _ => panic!("expected Skip, got {action:?}"),
        }
    }

    #[test]
    fn merge_payloads_incoming_overrides_existing_for_context_keys() {
        let existing = json!({
            "issueId": "iss-old",
            "taskKey": "tk-1",
            "extra_field": "keep-me"
        });
        let incoming = json!({
            "issueId": "iss-new",
            "wakeCommentId": "c-1"
        });
        let merged = merge_wake_payloads(Some(&existing), Some(&incoming));
        assert_eq!(merged["issueId"], "iss-new");
        assert_eq!(merged["taskKey"], "tk-1");
        assert_eq!(merged["wakeCommentId"], "c-1");
        assert_eq!(merged["extra_field"], "keep-me");
    }

    #[test]
    fn merge_payloads_unions_wake_comment_ids_dedup() {
        let existing = json!({
            "wakeCommentIds": ["c-1", "c-2"]
        });
        let incoming = json!({
            "wakeCommentIds": ["c-2", "c-3"]
        });
        let merged = merge_wake_payloads(Some(&existing), Some(&incoming));
        let ids: Vec<&str> = merged["wakeCommentIds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["c-1", "c-2", "c-3"]);
    }

    #[test]
    fn merge_payloads_handles_null_sides() {
        assert_eq!(merge_wake_payloads(None, None), Value::Null);
        let only = json!({"a": 1});
        assert_eq!(merge_wake_payloads(None, Some(&only)), only);
        assert_eq!(merge_wake_payloads(Some(&only), None), only);
    }

    #[test]
    fn merge_payloads_incoming_null_context_key_does_not_override() {
        let existing = json!({"issueId": "iss-1"});
        let incoming = json!({"issueId": null, "taskKey": "tk-1"});
        let merged = merge_wake_payloads(Some(&existing), Some(&incoming));
        assert_eq!(merged["issueId"], "iss-1");
        assert_eq!(merged["taskKey"], "tk-1");
    }

    #[test]
    fn merge_wake_comment_ids_from_mixed_sources() {
        let values = vec![
            json!("c-1"),
            json!(["c-1", "c-2"]),
            json!({"wakeCommentId": "c-3"}),
            json!({"commentId": "c-4"}),
            json!({"wakeCommentIds": ["c-2", "c-5"]}),
        ];
        let merged = merge_wake_comment_ids(values.iter());
        assert_eq!(merged, vec!["c-1", "c-2", "c-3", "c-4", "c-5"]);
    }

    #[test]
    fn merge_wake_comment_ids_skips_empty_strings() {
        let values = vec![json!(""), json!("  "), json!("c-1"), json!(null)];
        let merged = merge_wake_comment_ids(values.iter());
        assert_eq!(merged, vec!["c-1"]);
    }

    #[test]
    fn merge_wake_comment_ids_dedup_preserves_first() {
        let values = vec![json!("c-1"), json!("c-2"), json!("c-1"), json!("c-2")];
        let merged = merge_wake_comment_ids(values.iter());
        assert_eq!(merged, vec!["c-1", "c-2"]);
    }

    #[test]
    fn suppression_none_when_no_flags() {
        let inputs = SuppressionInputs::default();
        let decision = resolve_suppression(&inputs);
        assert!(!decision.suppressed);
        assert_eq!(decision.reason, SuppressionReason::None);
    }

    #[test]
    fn suppression_database_restore_blocks_all() {
        let inputs = SuppressionInputs {
            in_worktree: false,
            database_restore_in_progress: true,
            db_worktree_override_armed: false,
        };
        let decision = resolve_suppression(&inputs);
        assert!(decision.suppressed);
        assert_eq!(
            decision.reason,
            SuppressionReason::DatabaseRestoreInProgress
        );
    }

    #[test]
    fn suppression_worktree_without_override() {
        let inputs = SuppressionInputs {
            in_worktree: true,
            database_restore_in_progress: false,
            db_worktree_override_armed: false,
        };
        let decision = resolve_suppression(&inputs);
        assert!(decision.suppressed);
        assert_eq!(decision.reason, SuppressionReason::WorktreeInstance);
    }

    #[test]
    fn suppression_worktree_with_db_override_is_allowed() {
        let inputs = SuppressionInputs {
            in_worktree: true,
            database_restore_in_progress: false,
            db_worktree_override_armed: true,
        };
        let decision = resolve_suppression(&inputs);
        assert!(!decision.suppressed);
        assert_eq!(decision.reason, SuppressionReason::None);
    }

    #[test]
    fn suppression_database_restore_takes_priority_over_worktree_override() {
        let inputs = SuppressionInputs {
            in_worktree: true,
            database_restore_in_progress: true,
            db_worktree_override_armed: true,
        };
        let decision = resolve_suppression(&inputs);
        assert!(decision.suppressed);
        assert_eq!(
            decision.reason,
            SuppressionReason::DatabaseRestoreInProgress
        );
    }

    #[test]
    fn suppression_from_env_parses_truthy_values() {
        let mut env = std::collections::HashMap::new();
        env.insert("PAPERCLIP_IN_WORKTREE".to_string(), "true".to_string());
        env.insert(
            "PAPERCLIP_DATABASE_RESTORE_IN_PROGRESS".to_string(),
            "yes".to_string(),
        );
        let inputs = SuppressionInputs::from_env(&env);
        assert!(inputs.in_worktree);
        assert!(inputs.database_restore_in_progress);
        assert!(!inputs.db_worktree_override_armed);
    }

    #[test]
    fn suppression_from_env_treats_falsy_as_false() {
        let mut env = std::collections::HashMap::new();
        env.insert("PAPERCLIP_IN_WORKTREE".to_string(), "false".to_string());
        env.insert(
            "PAPERCLIP_DATABASE_RESTORE_IN_PROGRESS".to_string(),
            "0".to_string(),
        );
        let inputs = SuppressionInputs::from_env(&env);
        assert!(!inputs.in_worktree);
        assert!(!inputs.database_restore_in_progress);
    }

    #[test]
    fn idempotency_key_for_assignment_is_deterministic() {
        let a = build_issue_assignment_wake_key("co-1", "a-1", "iss-1");
        let b = build_issue_assignment_wake_key("co-1", "a-1", "iss-1");
        assert_eq!(a, b);
        assert_eq!(a, "issue_assignment_wake:co-1:a-1:iss-1");
    }

    #[test]
    fn idempotency_key_for_decision_continuation() {
        let key = build_decision_continuation_wake_key("dec-1");
        assert_eq!(key, "decision_continuation:dec-1");
    }

    #[test]
    fn active_wakeup_status_recognises_all_states() {
        assert!(is_active_wakeup_status("queued"));
        assert!(is_active_wakeup_status("requested"));
        assert!(is_active_wakeup_status("claimed"));
        assert!(is_active_wakeup_status("deferred_issue_execution"));
        assert!(!is_active_wakeup_status("completed"));
        assert!(!is_active_wakeup_status("failed"));
        assert!(!is_active_wakeup_status("cancelled"));
        assert!(!is_active_wakeup_status("skipped"));
        assert!(!is_active_wakeup_status("coalesced"));
    }

    #[test]
    fn end_to_end_dedup_flow_creates_then_coalesces() {
        let action1 = decide_wake_action(None, &incoming());
        assert!(action1.is_create());

        let action2 = decide_wake_action(Some(&existing("w-1", "queued", 0)), &incoming());
        assert!(action2.is_coalesce());

        let existing_payload = json!({"wakeCommentIds": ["c-1"]});
        let incoming_payload = json!({"wakeCommentIds": ["c-2"], "issueId": "iss-1"});
        let merged = merge_wake_payloads(Some(&existing_payload), Some(&incoming_payload));
        let ids: Vec<&str> = merged["wakeCommentIds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["c-1", "c-2"]);
        assert_eq!(merged["issueId"], "iss-1");
    }

    #[test]
    fn end_to_end_skip_when_recovering_stale_claim() {
        let action = decide_wake_action(Some(&existing("w-1", "completed", 5)), &incoming());
        assert!(action.is_create());
    }

    // ============== R557: with_db_override + cross-process idempotency ==============

    #[test]
    fn r557_with_db_override_lifts_worktree_suppression() {
        let env = std::collections::HashMap::from([("PAPERCLIP_IN_WORKTREE".into(), "true".into())]);
        let inputs = SuppressionInputs::from_env(&env).with_db_override(true);
        let decision = resolve_suppression(&inputs);
        assert!(!decision.suppressed);
        assert_eq!(decision.reason, SuppressionReason::None);
    }

    #[test]
    fn r557_with_db_override_disabled_keeps_worktree_suppression() {
        let env = std::collections::HashMap::from([("PAPERCLIP_IN_WORKTREE".into(), "true".into())]);
        let inputs = SuppressionInputs::from_env(&env).with_db_override(false);
        let decision = resolve_suppression(&inputs);
        assert!(decision.suppressed);
        assert_eq!(decision.reason, SuppressionReason::WorktreeInstance);
    }

    #[test]
    fn r557_db_override_opt_none_keeps_default_false() {
        let env = std::collections::HashMap::from([("PAPERCLIP_IN_WORKTREE".into(), "true".into())]);
        let inputs = SuppressionInputs::from_env(&env).with_db_override_opt(None);
        // 默认 false,与原 from_env 行为一致
        assert!(!inputs.db_worktree_override_armed);
        assert!(resolve_suppression(&inputs).suppressed);
    }

    #[test]
    fn r557_db_override_opt_some_applies_value() {
        let env = std::collections::HashMap::new();
        let inputs = SuppressionInputs::from_env(&env).with_db_override_opt(Some(true));
        assert!(inputs.db_worktree_override_armed);
    }

    #[test]
    fn r557_db_restore_in_progress_overrides_worktree_override() {
        // 即使 DB override armed,database_restore_in_progress 仍然是最高优先级
        let env = std::collections::HashMap::from([
            ("PAPERCLIP_IN_WORKTREE".into(), "true".into()),
            ("PAPERCLIP_DATABASE_RESTORE_IN_PROGRESS".into(), "true".into()),
        ]);
        let inputs = SuppressionInputs::from_env(&env).with_db_override(true);
        let decision = resolve_suppression(&inputs);
        assert!(decision.suppressed);
        assert_eq!(decision.reason, SuppressionReason::DatabaseRestoreInProgress);
    }

    #[test]
    fn r557_idempotency_key_issue_assignment_format() {
        let k = build_issue_assignment_wake_key("co-1", "a-1", "iss-1");
        assert_eq!(k, "issue_assignment_wake:co-1:a-1:iss-1");
    }

    #[test]
    fn r557_idempotency_key_decision_continuation_format() {
        let k = build_decision_continuation_wake_key("d-42");
        assert_eq!(k, "decision_continuation:d-42");
    }

    #[test]
    fn r557_cross_source_wake_comment_id_merge() {
        // 模拟多源 comment 列表合并 (issue / decision / status card)
        let a = json!({"wakeCommentIds": ["c-1", "c-2"]});
        let b = json!({"wakeCommentIds": ["c-2", "c-3"]});
        let c = json!({"commentId": "c-4"});
        let merged = merge_wake_comment_ids([&a, &b, &c]);
        // c-2 在两源中出现,只保留首次出现
        assert_eq!(merged, vec!["c-1", "c-2", "c-3", "c-4"]);
    }

    #[test]
    fn r557_dedup_with_payload_merge_end_to_end() {
        // 真实场景: 同一 agent 短时间内被 wake 两次,带不同的 reason + 共享 issue
        let first_existing = Some(WakeSnapshot {
            id: "w-1".into(),
            agent_id: "a-1".into(),
            company_id: "co-1".into(),
            status: "queued".into(),
            coalesced_count: 0,
            payload: Some(json!({
                "wakeReason": "first call",
                "issueId": "iss-1",
                "wakeCommentIds": ["c-1"],
            })),
        });
        let incoming = WakeInput {
            agent_id: "a-1".into(),
            company_id: "co-1".into(),
            source: "issue_assignment".into(),
            reason: Some("second call".into()),
            payload: Some(json!({
                "wakeReason": "second call",
                "wakeCommentIds": ["c-2"],
            })),
            idempotency_key: Some(build_issue_assignment_wake_key("co-1", "a-1", "iss-1")),
        };
        // 1. Decision: 应当 Coalesce
        let action = decide_wake_action(first_existing.as_ref(), &incoming);
        let coalesce = match action {
            WakeAction::Coalesce { into_id, increment } => (into_id, increment),
            other => panic!("expected Coalesce, got {other:?}"),
        };
        assert_eq!(coalesce.0, "w-1");
        assert_eq!(coalesce.1, 1);

        // 2. Payload merge: 验证 wakeReason 被覆盖,wakeCommentIds 合并
        let merged = merge_wake_payloads(
            first_existing.unwrap().payload.as_ref(),
            incoming.payload.as_ref(),
        );
        assert_eq!(merged["wakeReason"], "second call");
        assert_eq!(merged["issueId"], "iss-1"); // 保留 first
        let ids: Vec<&str> = merged["wakeCommentIds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["c-1", "c-2"]);
    }
}

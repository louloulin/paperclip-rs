//! Successful run handoff state hydrate + resolve（对齐 Node `server/src/services/successful-run-handoff-state.ts`，128 行）。
//!
//! 单一职责：
//! - `hydrateSuccessfulRunHandoffLiveness`：并行拉 active heartbeat_runs / agent_wakeup_requests，
//!   在传入的 `Map<issue_id, state>` 上原地更新 `hasLiveContinuation` / `liveRunId`
//! - `resolveRequiredSuccessfulRunHandoffOnValidPath`：从 activity_log 找最近 handoff，
//!   当且仅当 latest 是 `required` 时写一条 `resolved` 日志
//!
//! 不持有任何业务状态；所有 IO 通过 `&Db` 完成。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::types::Json;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::activity::{ActivityRepo, NewActivity};
use crate::Db;

// ---- 常量（与 Node 1:1 对齐） ----

/// `heartbeat_runs.status` 取值集合（与 Node `SUCCESSFUL_RUN_HANDOFF_LIVE_RUN_STATUSES` 1:1 对齐）。
pub const SUCCESSFUL_RUN_HANDOFF_LIVE_RUN_STATUSES: &[&str] =
    &["queued", "running", "scheduled_retry"];

/// `agent_wakeup_requests.status` 取值集合（与 Node `SUCCESSFUL_RUN_HANDOFF_LIVE_WAKE_STATUSES` 1:1 对齐）。
pub const SUCCESSFUL_RUN_HANDOFF_LIVE_WAKE_STATUSES: &[&str] =
    &["queued", "deferred_issue_execution", "claimed"];

// ---- 类型 ----

/// Handoff state kind（与 Node `SuccessfulRunHandoffStateKind` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SuccessfulRunHandoffStateKind {
    Required,
    Resolved,
    Escalated,
}

impl SuccessfulRunHandoffStateKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Resolved => "resolved",
            Self::Escalated => "escalated",
        }
    }
}

/// Successful run handoff state（与 Node `SuccessfulRunHandoffState` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuccessfulRunHandoffState {
    pub state: SuccessfulRunHandoffStateKind,
    pub required: bool,
    #[serde(rename = "hasLiveContinuation", default)]
    pub has_live_continuation: bool,
    #[serde(rename = "liveRunId", skip_serializing_if = "Option::is_none")]
    pub live_run_id: Option<String>,
    #[serde(rename = "sourceRunId")]
    pub source_run_id: Option<String>,
    #[serde(rename = "correctiveRunId")]
    pub corrective_run_id: Option<String>,
    #[serde(rename = "assigneeAgentId")]
    pub assignee_agent_id: Option<String>,
    #[serde(rename = "detectedProgressSummary")]
    pub detected_progress_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<JsonDateTime>,
}

/// `Date | string | null` 兼容类型（与 Node `Date | string | null` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonDateTime {
    DateTime(DateTime<Utc>),
    String(String),
}

// ---- 公开 API ----

/// Hydrate required handoff states with live continuation liveness
/// （与 Node `hydrateSuccessfulRunHandoffLiveness` 1:1 对齐）。
///
/// 行为：
/// 1. 过滤出 `state.kind == "required"` 的 issue_id 列表
/// 2. 并发拉 active heartbeat_runs + agent_wakeup_requests
/// 3. 构造 `Map<issue_id, run_id>` (live runs) + `Set<issue_id>` (live wakes)
/// 4. 对每个 required state，更新 `hasLiveContinuation`（= live run OR live wake）
///    以及可选的 `liveRunId`（取 live run 的 run id）
/// 5. 返回更新后的 map（与 Node 行为一致：原地修改 + 返回）
pub async fn hydrate_successful_run_handoff_liveness(
    db: &Db,
    company_id: Uuid,
    mut states: HashMap<String, SuccessfulRunHandoffState>,
) -> Result<HashMap<String, SuccessfulRunHandoffState>, sqlx::Error> {
    let required_issue_ids: Vec<String> = states
        .iter()
        .filter(|(_, s)| s.state == SuccessfulRunHandoffStateKind::Required)
        .map(|(id, _)| id.clone())
        .collect();
    if required_issue_ids.is_empty() {
        return Ok(states);
    }

    let issue_uuid_id_strings: Vec<String> = required_issue_ids.clone();

    let (active_runs, active_wakes) = tokio::try_join!(
        fetch_active_runs(db, company_id, &issue_uuid_id_strings),
        fetch_active_wakes(db, company_id, &issue_uuid_id_strings),
    )?;

    let mut live_run_by_issue_id: HashMap<String, String> = HashMap::new();
    for row in active_runs {
        if let Some(issue_id) = row.issue_id {
            if !live_run_by_issue_id.contains_key(&issue_id) {
                live_run_by_issue_id.insert(issue_id, row.id);
            }
        }
    }
    let live_wake_issue_ids: HashSet<String> = active_wakes
        .into_iter()
        .filter_map(|r| r.issue_id)
        .collect();

    for issue_id in required_issue_ids {
        if let Some(state) = states.get_mut(&issue_id) {
            let live_run_id = live_run_by_issue_id.get(&issue_id).cloned();
            state.has_live_continuation = live_run_id.is_some() || live_wake_issue_ids.contains(&issue_id);
            if let Some(rid) = live_run_id {
                state.live_run_id = Some(rid);
            }
        }
    }

    Ok(states)
}

/// Resolve a required handoff on a valid path
/// （与 Node `resolveRequiredSuccessfulRunHandoffOnValidPath` 1:1 对齐）。
///
/// 行为：
/// 1. 查 activity_log 找 latest handoff（`required` / `resolved` / `escalated` 三种 action）
/// 2. latest 不是 `required` → 返回 `false`
/// 3. 否则在 activity_log 写一条 `resolved` 日志（actor=system/heartbeat），并返回 `true`
pub async fn resolve_required_successful_run_handoff_on_valid_path(
    db: &Db,
    input: ResolveRequiredHandoffInput,
) -> Result<bool, sqlx::Error> {
    let latest = fetch_latest_handoff_activity(
        db,
        input.company_id,
        input.issue_id,
        &[
            "issue.successful_run_handoff_required",
            "issue.successful_run_handoff_resolved",
            "issue.successful_run_handoff_escalated",
        ],
    )
    .await?;
    if let Some(ref handoff) = latest {
        if handoff.action != "issue.successful_run_handoff_required" {
            return Ok(false);
        }
    } else {
        return Ok(false);
    }
    let handoff = latest.expect("checked above");
    let details_obj = handoff
        .details
        .as_ref()
        .and_then(|d| d.as_object())
        .cloned()
        .unwrap_or_default();
    let source_run_id: Option<String> = [
        details_obj.get("sourceRunId").and_then(|v| v.as_str()),
        details_obj.get("source_run_id").and_then(|v| v.as_str()),
        details_obj.get("resumeFromRunId").and_then(|v| v.as_str()),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|s| !s.is_empty())
    .map(str::to_owned)
    .or(handoff.run_id);

    let mut details = serde_json::Map::new();
    details.insert("label".to_string(), JsonValue::String("Successful run handoff continuation confirmed".to_string()));
    if let Some(sid) = source_run_id {
        details.insert("sourceRunId".to_string(), JsonValue::String(sid));
    }
    details.insert(
        "resolvedByRunId".to_string(),
        JsonValue::String(input.run_id.to_string()),
    );
    details.insert(
        "resolvedBySkipReason".to_string(),
        JsonValue::String(input.skip_reason),
    );
    let mut issue_obj = serde_json::Map::new();
    issue_obj.insert("id".to_string(), JsonValue::String(input.issue_id.to_string()));
    if let Some(ident) = input.issue_identifier {
        issue_obj.insert("identifier".to_string(), JsonValue::String(ident));
    }
    details.insert("issue".to_string(), JsonValue::Object(issue_obj));

    let new_activity = NewActivity {
        company_id: input.company_id,
        actor_type: crate::activity::ActorType::System,
        actor_id: "heartbeat".to_string(),
        action: "issue.successful_run_handoff_resolved".to_string(),
        entity_type: "issue".to_string(),
        entity_id: input.issue_id.to_string(),
        agent_id: Some(input.agent_id),
        run_id: Some(input.run_id),
        responsible_user_id: None,
        details: Some(JsonValue::Object(details)),
    };
    ActivityRepo::new(db)
        .record(&new_activity)
        .await
        .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    Ok(true)
}

/// `resolveRequiredSuccessfulRunHandoffOnValidPath` 输入。
#[derive(Debug, Clone)]
pub struct ResolveRequiredHandoffInput {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub issue_identifier: Option<String>,
    pub agent_id: Uuid,
    pub run_id: Uuid,
    pub skip_reason: String,
}

// ---- private DB helpers ----

struct ActiveRunRow {
    id: String,
    issue_id: Option<String>,
}

struct ActiveWakeRow {
    issue_id: Option<String>,
}

struct LatestHandoffRow {
    action: String,
    run_id: Option<String>,
    details: Option<JsonValue>,
}

async fn fetch_active_runs(
    db: &Db,
    company_id: Uuid,
    issue_id_strings: &[String],
) -> Result<Vec<ActiveRunRow>, sqlx::Error> {
    // 1) heartbeat_runs.contextSnapshot->>'issueId' / 'taskId'
    let rows: Vec<(Uuid, Option<String>)> = sqlx::query_as(
        r#"
        SELECT id, coalesce(
            context_snapshot ->> 'issueId',
            context_snapshot ->> 'taskId'
        ) AS issue_id
        FROM heartbeat_runs
        WHERE company_id = $1
          AND status = ANY($2)
          AND coalesce(
                context_snapshot ->> 'issueId',
                context_snapshot ->> 'taskId'
              ) = ANY($3)
        "#,
    )
    .bind(company_id)
    .bind(SUCCESSFUL_RUN_HANDOFF_LIVE_RUN_STATUSES)
    .bind(issue_id_strings)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, issue_id)| ActiveRunRow {
            id: id.to_string(),
            issue_id,
        })
        .collect())
}

async fn fetch_active_wakes(
    db: &Db,
    company_id: Uuid,
    issue_id_strings: &[String],
) -> Result<Vec<ActiveWakeRow>, sqlx::Error> {
    let rows: Vec<(Option<String>,)> = sqlx::query_as(
        r#"
        SELECT coalesce(
            payload ->> 'issueId',
            payload ->> 'taskId',
            payload -> '_paperclipWakeContext' ->> 'issueId',
            payload -> '_paperclipWakeContext' ->> 'taskId'
        ) AS issue_id
        FROM agent_wakeup_requests
        WHERE company_id = $1
          AND status = ANY($2)
          AND coalesce(
                payload ->> 'issueId',
                payload ->> 'taskId',
                payload -> '_paperclipWakeContext' ->> 'issueId',
                payload -> '_paperclipWakeContext' ->> 'taskId'
              ) = ANY($3)
        "#,
    )
    .bind(company_id)
    .bind(SUCCESSFUL_RUN_HANDOFF_LIVE_WAKE_STATUSES)
    .bind(issue_id_strings)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(issue_id,)| ActiveWakeRow { issue_id })
        .collect())
}

async fn fetch_latest_handoff_activity(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    actions: &[&str],
) -> Result<Option<LatestHandoffRow>, sqlx::Error> {
    let row: Option<(String, Option<Uuid>, Option<Json<JsonValue>>)> = sqlx::query_as(
        r#"
        SELECT action, run_id, details
        FROM activity_log
        WHERE company_id = $1
          AND entity_type = 'issue'
          AND entity_id = $2
          AND action = ANY($3)
        ORDER BY created_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(company_id)
    .bind(issue_id)
    .bind(actions)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(|(action, run_id, details)| LatestHandoffRow {
        action,
        run_id: run_id.map(|u| u.to_string()),
        details: details.map(|j| j.0),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_run_statuses_match_node() {
        for s in ["queued", "running", "scheduled_retry"] {
            assert!(SUCCESSFUL_RUN_HANDOFF_LIVE_RUN_STATUSES.contains(&s));
        }
        assert_eq!(SUCCESSFUL_RUN_HANDOFF_LIVE_RUN_STATUSES.len(), 3);
    }

    #[test]
    fn live_wake_statuses_match_node() {
        for s in ["queued", "deferred_issue_execution", "claimed"] {
            assert!(SUCCESSFUL_RUN_HANDOFF_LIVE_WAKE_STATUSES.contains(&s));
        }
        assert_eq!(SUCCESSFUL_RUN_HANDOFF_LIVE_WAKE_STATUSES.len(), 3);
    }

    #[test]
    fn handoff_state_kind_as_str() {
        assert_eq!(SuccessfulRunHandoffStateKind::Required.as_str(), "required");
        assert_eq!(SuccessfulRunHandoffStateKind::Resolved.as_str(), "resolved");
        assert_eq!(SuccessfulRunHandoffStateKind::Escalated.as_str(), "escalated");
    }

    fn required_state() -> SuccessfulRunHandoffState {
        SuccessfulRunHandoffState {
            state: SuccessfulRunHandoffStateKind::Required,
            required: true,
            has_live_continuation: false,
            live_run_id: None,
            source_run_id: None,
            corrective_run_id: None,
            assignee_agent_id: None,
            detected_progress_summary: None,
            created_at: None,
        }
    }

    fn resolved_state() -> SuccessfulRunHandoffState {
        SuccessfulRunHandoffState {
            state: SuccessfulRunHandoffStateKind::Resolved,
            required: false,
            has_live_continuation: false,
            live_run_id: None,
            source_run_id: None,
            corrective_run_id: None,
            assignee_agent_id: None,
            detected_progress_summary: None,
            created_at: None,
        }
    }

    #[test]
    fn required_state_default_has_no_live_continuation() {
        let s = required_state();
        assert!(!s.has_live_continuation);
        assert!(s.live_run_id.is_none());
    }

    #[test]
    fn state_serializes_with_camel_case() {
        let s = required_state();
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["state"], "required");
        assert_eq!(json["required"], true);
        assert_eq!(json["hasLiveContinuation"], false);
        assert!(json.get("liveRunId").is_none());
        assert_eq!(json["sourceRunId"], JsonValue::Null);
    }

    #[test]
    fn json_datetime_accepts_iso_string() {
        // With `#[serde(untagged)]`, an ISO-8601 string may be parsed either as
        // `DateTime<Utc>` (when chrono recognizes the format) or `String` fallback.
        // Both are acceptable; we just need the round trip to succeed.
        let j: JsonDateTime = serde_json::from_value(JsonValue::String("2026-07-23T18:13:03.000Z".into())).unwrap();
        match j {
            JsonDateTime::DateTime(_) | JsonDateTime::String(_) => {}
        }
    }

    #[test]
    fn json_datetime_accepts_null_as_none() {
        let s = required_state();
        let json = serde_json::to_value(&s).unwrap();
        assert!(json["createdAt"].is_null() || json.get("createdAt").is_none());
    }

    #[test]
    fn resolve_input_carries_all_required_fields() {
        let input = ResolveRequiredHandoffInput {
            company_id: Uuid::nil(),
            issue_id: Uuid::nil(),
            issue_identifier: Some("PAP-1".to_string()),
            agent_id: Uuid::nil(),
            run_id: Uuid::nil(),
            skip_reason: "ok".to_string(),
        };
        assert_eq!(input.issue_identifier.as_deref(), Some("PAP-1"));
        assert_eq!(input.skip_reason, "ok");
    }
}

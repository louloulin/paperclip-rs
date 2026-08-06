//! `scheduleProviderQuotaRecoveryMonitor` 的 DB 接入层。
//!
//! Node 在 issue 仍处于 `in_progress` / `in_review` 且原执行者可继续承担工作时，
//! 不创建 recovery action，也不把 issue 改成 `blocked`，而是把 provider quota
//! reset 时间写入 issue monitor。下一次 heartbeat monitor 到期后再唤醒 owner。
//!
//! 本模块只负责这条持久化边界：retry 时间和分类由上层纯函数决定；这里负责
//! pending monitor 幂等检查、execution policy/state 合并以及 issue 写入。

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use pc_repos::issue::{IssueRepo, UpdateIssuePatch};
use pc_repos::Db;

pub const PROVIDER_QUOTA_MONITOR_SERVICE_NAME: &str = "AI provider quota";

#[derive(Debug, Clone)]
pub struct ScheduleProviderQuotaRecoveryMonitorInput {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub latest_run_id: Uuid,
    pub target_agent_id: Uuid,
    pub retry_at: DateTime<Utc>,
    pub parsed_reset_time: bool,
    pub now: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderQuotaRecoveryMonitorResult {
    pub issue_id: Uuid,
    pub latest_run_id: Uuid,
    pub retry_at: DateTime<Utc>,
    pub parsed_reset_time: bool,
}

/// 在 issue 上建立 provider quota monitor；已有同一 run 的未来 monitor 时返回 `None`。
pub async fn schedule_provider_quota_recovery_monitor(
    db: &Db,
    input: ScheduleProviderQuotaRecoveryMonitorInput,
) -> sqlx::Result<Option<ProviderQuotaRecoveryMonitorResult>> {
    let now = input.now.unwrap_or_else(Utc::now);
    let row: Option<(
        String,
        Option<Uuid>,
        Option<Value>,
        Option<Value>,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        i32,
    )> = sqlx::query_as(
        "SELECT status, assignee_agent_id, execution_policy, execution_state, \
                monitor_next_check_at, monitor_last_triggered_at, monitor_attempt_count \
         FROM issues WHERE id = $1 AND company_id = $2",
    )
    .bind(input.issue_id)
    .bind(input.company_id)
    .fetch_optional(db.pool())
    .await?;
    let Some((
        status,
        _assignee_agent_id,
        existing_policy,
        existing_state,
        next_check_at,
        last_triggered_at,
        attempt_count,
    )) = row
    else {
        return Ok(None);
    };
    if status != "in_progress" && status != "in_review" {
        return Ok(None);
    }

    // The caller normally performs this check while classifying the latest run;
    // repeat it here so this DB boundary cannot attach a monitor to another run.
    let run_agent: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT agent_id FROM heartbeat_runs WHERE id = $1 AND company_id = $2")
            .bind(input.latest_run_id)
            .bind(input.company_id)
            .fetch_optional(db.pool())
            .await?;
    if run_agent.flatten() != Some(input.target_agent_id) {
        return Ok(None);
    }

    if next_check_at.is_some_and(|value| value > now)
        && monitor_ref_matches(existing_policy.as_ref(), input.latest_run_id)
    {
        return Ok(None);
    }

    let retry_at_string = input.retry_at.to_rfc3339();
    let notes = if status == "in_review" {
        if input.parsed_reset_time {
            "Provider usage quota reached; retry the active review participant at the provider reset time."
        } else {
            "Provider usage quota reached; retry the active review participant after the default recovery backoff."
        }
    } else if input.parsed_reset_time {
        "Provider usage quota reached; retry the original assignee at the provider reset time."
    } else {
        "Provider usage quota reached; retry the original assignee after the default recovery backoff."
    };

    let monitor_policy = json!({
        "nextCheckAt": retry_at_string,
        "notes": notes,
        "scheduledBy": "assignee",
        "kind": "external_service",
        "serviceName": PROVIDER_QUOTA_MONITOR_SERVICE_NAME,
        "externalRef": input.latest_run_id,
        "timeoutAt": Value::Null,
        "maxAttempts": Value::Null,
        "recoveryPolicy": "wake_owner",
    });
    let policy = merge_policy_with_monitor(existing_policy, monitor_policy);
    let state = merge_state_with_monitor(
        existing_state,
        last_triggered_at,
        attempt_count,
        notes,
        &retry_at_string,
        input.latest_run_id,
    );

    let patch = UpdateIssuePatch {
        execution_policy: Some(Some(&policy)),
        execution_state: Some(Some(&state)),
        monitor_next_check_at: Some(Some(input.retry_at)),
        monitor_wake_requested_at: Some(None),
        monitor_notes: Some(Some(notes)),
        monitor_scheduled_by: Some(Some("assignee")),
        ..UpdateIssuePatch::default()
    };
    let updated = IssueRepo::new(db)
        .update_full(input.issue_id, &patch)
        .await?;
    if updated.is_none() {
        return Ok(None);
    }
    Ok(Some(ProviderQuotaRecoveryMonitorResult {
        issue_id: input.issue_id,
        latest_run_id: input.latest_run_id,
        retry_at: input.retry_at,
        parsed_reset_time: input.parsed_reset_time,
    }))
}

/// 将 adapter failure 规范化为 provider quota 分类，供后续 sweep 稳定识别。
pub async fn persist_provider_quota_recovery_classification(
    db: &Db,
    company_id: Uuid,
    run_id: Uuid,
    retry_at: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let existing: Option<Option<Value>> = sqlx::query_scalar(
        "SELECT result_json FROM heartbeat_runs WHERE id = $1 AND company_id = $2",
    )
    .bind(run_id)
    .bind(company_id)
    .fetch_optional(db.pool())
    .await?;
    let Some(existing) = existing else {
        return Ok(false);
    };
    let mut result = match existing {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    let retry_at = Value::String(retry_at.to_rfc3339());
    result.insert(
        "errorFamily".to_owned(),
        Value::String("provider_quota".to_owned()),
    );
    result.insert("retryNotBefore".to_owned(), retry_at.clone());
    result.insert("transientRetryNotBefore".to_owned(), retry_at.clone());
    result.insert("providerQuotaRetryNotBefore".to_owned(), retry_at);
    result.insert(
        "recoveryClassification".to_owned(),
        Value::String("provider_quota".to_owned()),
    );
    let updated = sqlx::query(
        "UPDATE heartbeat_runs SET error_code = 'provider_quota', result_json = $1, \
         updated_at = now() WHERE id = $2 AND company_id = $3",
    )
    .bind(Value::Object(result))
    .bind(run_id)
    .bind(company_id)
    .execute(db.pool())
    .await?;
    Ok(updated.rows_affected() == 1)
}

fn monitor_ref_matches(policy: Option<&Value>, run_id: Uuid) -> bool {
    let run_id = run_id.to_string();
    policy
        .and_then(Value::as_object)
        .and_then(|value| value.get("monitor"))
        .and_then(Value::as_object)
        .is_some_and(|monitor| {
            monitor.get("serviceName").and_then(Value::as_str)
                == Some(PROVIDER_QUOTA_MONITOR_SERVICE_NAME)
                && monitor.get("externalRef").and_then(Value::as_str) == Some(run_id.as_str())
        })
}

fn merge_policy_with_monitor(existing: Option<Value>, monitor: Value) -> Value {
    let mut policy = match existing {
        Some(Value::Object(map)) => map,
        _ => {
            let mut map = Map::new();
            map.insert("mode".to_owned(), Value::String("normal".to_owned()));
            map.insert("commentRequired".to_owned(), Value::Bool(true));
            map.insert("stages".to_owned(), Value::Array(Vec::new()));
            map
        }
    };
    policy.insert("monitor".to_owned(), monitor);
    Value::Object(policy)
}

fn merge_state_with_monitor(
    existing: Option<Value>,
    last_triggered_at: Option<DateTime<Utc>>,
    attempt_count: i32,
    notes: &str,
    retry_at: &str,
    run_id: Uuid,
) -> Value {
    let mut state = match existing {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    let previous_monitor = state
        .get("monitor")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let last_triggered = last_triggered_at
        .map(|value| Value::String(value.to_rfc3339()))
        .or_else(|| previous_monitor.get("lastTriggeredAt").cloned())
        .unwrap_or(Value::Null);
    state.insert(
        "monitor".to_owned(),
        json!({
            "status": "scheduled",
            "nextCheckAt": retry_at,
            "lastTriggeredAt": last_triggered,
            "attemptCount": attempt_count,
            "notes": notes,
            "scheduledBy": "assignee",
            "kind": "external_service",
            "serviceName": PROVIDER_QUOTA_MONITOR_SERVICE_NAME,
            "externalRef": run_id,
            "timeoutAt": Value::Null,
            "maxAttempts": Value::Null,
            "recoveryPolicy": "wake_owner",
            "clearedAt": Value::Null,
            "clearReason": Value::Null,
        }),
    );
    Value::Object(state)
}

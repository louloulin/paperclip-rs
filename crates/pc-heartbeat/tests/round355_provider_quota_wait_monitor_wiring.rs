//! Round 355：`ensure_provider_quota_wait_recovery_monitor` 接线。
//!
//! 对齐 Node `services/recovery/service.ts`：
//! - 升级路径中当 recovery action 的 `cause=provider_quota` 且 `owner_agent_id=None` 且
//!   `return_owner_agent_id=Some(retriable_agent)`，必须创建 scheduled_retry run +
//!   queued wakeup，并把 `monitor_policy` 更新为 `{type:"wait_recovery", scheduledRunId, retryAt}`。
//!
//! Rust 已经实现了 `ensure_provider_quota_wait_recovery_monitor` 与 `schedule_provider_quota_recovery_monitor`
//! 两个模块：
//! - `schedule_provider_quota_recovery_monitor` 走 in-place monitor（issue 不变 blocked，不创建 action）
//! - `ensure_provider_quota_wait_recovery_monitor` 走 wait_recovery monitor（创建 scheduled_retry run）
//!
//! 缺口：`ensure_provider_quota_wait_recovery_monitor` 没有被 orchestrator 调用，是一个 orphan 函数。
//! 本轮修复此问题，并通过真实 PostgreSQL 验证。

use pc_heartbeat::recovery::scheduler_db::{
    ensure_source_scoped_recovery_action_for_issue, SchedulerDbInput,
};
use pc_heartbeat::recovery::source_scoped_recovery_action::StrandedRecoveryCause;
use pc_repos::agent::{
    HeartbeatInvocationSource, NewAgentWakeupRequest, WakeupActorType, WakeupRequestStatus,
    WakeupTriggerDetail,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.unwrap()
}

fn wake_template(company_id: Uuid, agent_id: Uuid) -> NewAgentWakeupRequest {
    NewAgentWakeupRequest {
        company_id,
        agent_id,
        source: HeartbeatInvocationSource::OnDemand,
        trigger_detail: Some(WakeupTriggerDetail::Manual),
        reason: None,
        payload: None,
        status: WakeupRequestStatus::Queued,
        coalesced_count: 0,
        requested_by_actor_type: Some(WakeupActorType::System),
        requested_by_actor_id: None,
        idempotency_key: None,
        run_id: None,
        error: None,
    }
}

async fn cleanup(db: &Db, company_id: Uuid) {
    for stmt in [
        "DELETE FROM agent_wakeup_requests WHERE company_id = $1",
        "DELETE FROM issue_recovery_actions WHERE company_id = $1",
        "DELETE FROM heartbeat_runs WHERE company_id = $1",
        "DELETE FROM issue_comments WHERE company_id = $1",
        "DELETE FROM issues WHERE company_id = $1",
        "DELETE FROM agents WHERE company_id = $1",
        "DELETE FROM companies WHERE id = $1",
    ] {
        let _ = sqlx::query(stmt).bind(company_id).execute(db.pool()).await;
    }
}

async fn fixture(db: &Db) -> (Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r355-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r355-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, assignee_agent_id) \
         VALUES ($1, $2, 'r355-issue', 'in_progress', 'normal', 'system', $3, $4)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(format!("r355-fp-{issue_id}"))
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, \
         error, error_code, context_snapshot, started_at) \
         VALUES (gen_random_uuid(), $1, $2, 'manual', 'failed', 'provider quota reached', \
                 'provider_quota', $3, now())",
    )
    .bind(company_id)
    .bind(agent_id)
    .bind(json!({
        "issueId": issue_id,
        "providerQuotaRetryNotBefore": "2099-01-01T00:00:00Z",
    }))
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id, issue_id)
}

/// 主目标：当 SchedulerDbInput 强制 cause=provider_quota + invokable assignee，
/// 调用 `ensure_source_scoped_recovery_action_for_issue` 应该同时产出：
/// 1) recovery action（cause=provider_quota, owner_agent_id=null, wake_policy.type=monitor_only,
///    monitor_policy.type=wait_recovery）
/// 2) 一个 scheduled_retry heartbeat_run（status=scheduled_retry, scheduled_retry_at<future>）
/// 3) 一个 queued wakeup（与 issue + retry_at 关联）
/// 4) action.monitor_policy 已设置 {scheduledRunId, retryAt}
#[tokio::test(flavor = "current_thread")]
async fn action_creation_for_provider_quota_with_invokable_assignee_creates_scheduled_retry() {
    let db = connect().await;
    let (company_id, agent_id, issue_id) = fixture(&db).await;

    let result = ensure_source_scoped_recovery_action_for_issue(
        &db,
        SchedulerDbInput {
            issue_id,
            previous_status: Some("in_progress".to_owned()),
            recovery_cause_override: Some(StrandedRecoveryCause::ProviderQuota),
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap()
    .expect("result Some");
    assert_eq!(result.cause, StrandedRecoveryCause::ProviderQuota);

    let action = &result.result.persisted.action;
    assert_eq!(action.cause, "provider_quota");
    assert!(
        action.owner_agent_id.is_none(),
        "no owner for provider_quota wait"
    );
    assert_eq!(action.return_owner_agent_id, Some(agent_id));
    assert_eq!(
        action
            .wake_policy
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str()),
        Some("monitor_only")
    );
    assert_eq!(
        action
            .monitor_policy
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str()),
        Some("wait_recovery")
    );

    // 验证 scheduled_retry run 被创建
    let (scheduled_run_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM heartbeat_runs \
         WHERE company_id = $1 AND status = 'scheduled_retry' \
           AND context_snapshot->>'issueId' = $2",
    )
    .bind(company_id)
    .bind(issue_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        scheduled_run_count >= 1,
        "expected a scheduled_retry heartbeat_run, found {}",
        scheduled_run_count
    );

    // 验证 wakeup 被创建并处于 queued
    let (wakeup_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM agent_wakeup_requests \
         WHERE company_id = $1 AND status = 'queued' \
           AND payload->>'issueId' = $2",
    )
    .bind(company_id)
    .bind(issue_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        wakeup_count >= 1,
        "expected a queued wakeup with issueId, found {}",
        wakeup_count
    );

    // 验证 monitor_policy.scheduledRunId 与实际 scheduled_run.id 一致
    let action_id = action.id;
    let monitor_policy_json: serde_json::Value =
        sqlx::query_scalar("SELECT monitor_policy FROM issue_recovery_actions WHERE id = $1")
            .bind(action_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let scheduled_run_id_str = monitor_policy_json
        .get("scheduledRunId")
        .and_then(|v| v.as_str())
        .expect("monitor_policy should contain scheduledRunId");
    let scheduled_run_id = Uuid::parse_str(scheduled_run_id_str).unwrap();

    let run_row: (Uuid, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT id, scheduled_retry_at FROM heartbeat_runs WHERE id = $1")
            .bind(scheduled_run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(run_row.0, scheduled_run_id);
    assert!(
        run_row.1.is_some(),
        "scheduled_retry_at should be set on the scheduled_retry run"
    );

    // 验证 action.timeout_at 与 retry_at 一致（用于 issue_blocked_monitor）
    let action_timeout_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT timeout_at FROM issue_recovery_actions WHERE id = $1")
            .bind(action_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(
        action_timeout_at.is_some(),
        "action.timeout_at should be set to retry_at for provider_quota wait"
    );

    cleanup(&db, company_id).await;
}

/// 重复调用幂等：第二次调用 should NOT 再次创建 scheduled_retry 或 wakeup，
/// 应返回已有的 (issue recovery action 已 active + 已有 scheduled_retry)。
#[tokio::test(flavor = "current_thread")]
async fn repeat_invocation_is_idempotent() {
    let db = connect().await;
    let (company_id, agent_id, issue_id) = fixture(&db).await;

    // 第一次
    let _first = ensure_source_scoped_recovery_action_for_issue(
        &db,
        SchedulerDbInput {
            issue_id,
            previous_status: Some("in_progress".to_owned()),
            recovery_cause_override: Some(StrandedRecoveryCause::ProviderQuota),
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap();

    let (scheduled_after_first,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM heartbeat_runs \
         WHERE company_id = $1 AND status = 'scheduled_retry' \
           AND context_snapshot->>'issueId' = $2",
    )
    .bind(company_id)
    .bind(issue_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();

    let (wakeup_after_first,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM agent_wakeup_requests \
         WHERE company_id = $1 AND status = 'queued' \
           AND payload->>'issueId' = $2",
    )
    .bind(company_id)
    .bind(issue_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();

    // 第二次
    let _second = ensure_source_scoped_recovery_action_for_issue(
        &db,
        SchedulerDbInput {
            issue_id,
            previous_status: Some("in_progress".to_owned()),
            recovery_cause_override: Some(StrandedRecoveryCause::ProviderQuota),
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap();

    let (scheduled_after_second,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM heartbeat_runs \
         WHERE company_id = $1 AND status = 'scheduled_retry' \
           AND context_snapshot->>'issueId' = $2",
    )
    .bind(company_id)
    .bind(issue_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        scheduled_after_first, scheduled_after_second,
        "second call must not create an additional scheduled_retry run"
    );
    let (wakeup_after_second,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM agent_wakeup_requests \
         WHERE company_id = $1 AND status = 'queued' \
           AND payload->>'issueId' = $2",
    )
    .bind(company_id)
    .bind(issue_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        wakeup_after_first, wakeup_after_second,
        "second call must not create an additional queued wakeup"
    );

    cleanup(&db, company_id).await;
}

/// 第 3 个测试：当 latest run 自动分类为 ProviderQuota（无显式 override）
/// 时，ensure_source_scoped_recovery_action_for_issue 同样应创建 scheduled_retry run + queued wakeup。
#[tokio::test(flavor = "current_thread")]
async fn auto_classification_via_run_error_code_triggers_monitor_wiring() {
    let db = connect().await;
    let (company_id, agent_id, issue_id) = fixture(&db).await;

    let result = ensure_source_scoped_recovery_action_for_issue(
        &db,
        SchedulerDbInput {
            issue_id,
            previous_status: Some("in_progress".to_owned()),
            recovery_cause_override: None, // 关键：无 override，依赖自动分类
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap()
    .expect("result Some");
    assert_eq!(
        result.cause,
        StrandedRecoveryCause::ProviderQuota,
        "auto-classification should map error_code=provider_quota to cause=ProviderQuota"
    );

    let (scheduled_run_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM heartbeat_runs \
         WHERE company_id = $1 AND status = 'scheduled_retry' \
           AND context_snapshot->>'issueId' = $2",
    )
    .bind(company_id)
    .bind(issue_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        scheduled_run_count >= 1,
        "auto-classified ProviderQuota should produce a scheduled_retry run, found {}",
        scheduled_run_count
    );

    let (wakeup_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM agent_wakeup_requests \
         WHERE company_id = $1 AND status = 'queued' \
           AND payload->>'issueId' = $2",
    )
    .bind(company_id)
    .bind(issue_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        wakeup_count >= 1,
        "auto-classified ProviderQuota should produce a queued wakeup, found {}",
        wakeup_count
    );

    let action = &result.result.persisted.action;
    assert_eq!(action.cause, "provider_quota");
    assert!(action.owner_agent_id.is_none());
    let monitor_policy = action.monitor_policy.as_ref().expect("monitor_policy set");
    assert_eq!(monitor_policy["type"], "wait_recovery");
    assert!(
        monitor_policy
            .get("scheduledRunId")
            .and_then(|v| v.as_str())
            .is_some(),
        "monitor_policy.scheduledRunId must be populated by ensure_provider_quota_wait_recovery_monitor"
    );

    cleanup(&db, company_id).await;
}

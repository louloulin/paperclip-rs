//! `ensureProviderQuotaWaitRecoveryMonitor` DB 模块。
//!
//! 对齐 Node `services/recovery/service.ts` 的 `ensureProviderQuotaWaitRecoveryMonitor`：
//! 当 issue 被 upgrade 到 blocked 且 owner agent 是 `provider_quota` wait 状态时，
//! 创建一个 scheduled_retry heartbeat_run（带 wakeup）等到 provider quota 恢复后
//! 自动重试。
//!
//! 数据流：
//! 1. 检查是否已有 scheduled_retry run（按 company_id + agent_id + issue_id）
//!    - 已有 → 直接返回
//! 2. 计算 retryAt（从 latest_run.result_json.providerQuotaRetryNotBefore 读，或 now + fallback）
//! 3. 事务内：
//!    a. INSERT agent_wakeup_requests（source=automation, reason=provider_quota_recovery）
//!    b. INSERT heartbeat_runs（status=scheduled_retry, scheduledRetryAt=retryAt）
//!    c. UPDATE agent_wakeup_requests SET run_id = scheduledRun.id
//!    d. UPDATE issue_recovery_actions SET monitor_policy, timeout_at = retryAt
//!
//! 设计：
//! - 事务保证原子性：避免部分写入导致孤儿数据
//! - wakeup 与 heartbeat_run INSERT 都走同一事务连接
//! - idempotency_key 包含 retryAt 时间戳 → 同一时间窗口内重复调用幂等
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use pc_repos::Db;

// ============================================================================
// Constants
// ============================================================================

/// `provider_quota_recovery` 默认 retry 间隔（fallback 当 latest_run 无信息时）。
/// 与 Node `readProviderQuotaRetryAt` 默认行为对齐：2 小时。
pub const DEFAULT_PROVIDER_QUOTA_RETRY_AFTER_MS: i64 = 2 * 60 * 60 * 1000;

/// Provider quota 等待时间，从 latest_run.result_json.providerQuotaRetryNotBefore 读取。
pub const PROVIDER_QUOTA_RETRY_NOT_BEFORE_KEY: &str = "providerQuotaRetryNotBefore";

// ============================================================================
// Public types
// ============================================================================

/// `ensure_provider_quota_wait_recovery_monitor` 输入。
#[derive(Debug, Clone)]
pub struct EnsureProviderQuotaMonitorInput {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub agent_id: Uuid,
    pub action_id: Uuid,
    /// Latest run id（写入 retry_of_run_id）。
    pub latest_run_id: Option<Uuid>,
    /// 注入的 now（便于测试）。
    pub now: Option<DateTime<Utc>>,
}

/// 输出：创建的 scheduled_run 信息（如已有 existing 则返回 existing）。
#[derive(Debug, Clone)]
pub struct ProviderQuotaMonitorResult {
    pub scheduled_run_id: Uuid,
    pub wakeup_request_id: Uuid,
    pub retry_at: DateTime<Utc>,
}

// ============================================================================
// Main entry point
// ============================================================================

/// 创建 provider quota wait scheduled_retry run（如果还没有）。
///
/// 与 Node `ensureProviderQuotaWaitRecoveryMonitor` 对齐：
/// - 已存在 scheduled_retry run → 直接返回
/// - 否则事务内创建 wakeup + scheduled_retry run + 更新 action.monitor_policy
///
/// 返回 `ProviderQuotaMonitorResult`：包含 scheduled_run_id + wakeup_request_id + retry_at。
pub async fn ensure_provider_quota_wait_recovery_monitor(
    db: &Db,
    input: EnsureProviderQuotaMonitorInput,
) -> sqlx::Result<Option<ProviderQuotaMonitorResult>> {
    let now = input.now.unwrap_or_else(Utc::now);

    // Step 1: 检查是否已有 scheduled_retry run
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM heartbeat_runs \
         WHERE company_id = $1 \
           AND agent_id = $2 \
           AND status = 'scheduled_retry' \
           AND context_snapshot->>'issueId' = $3 \
         ORDER BY COALESCE(scheduled_retry_at, created_at) DESC LIMIT 1",
    )
    .bind(input.company_id)
    .bind(input.agent_id)
    .bind(input.issue_id.to_string())
    .fetch_optional(db.pool())
    .await?;

    if let Some((existing_id,)) = existing {
        // 已有：返回最关键的信息（避免重复 INSERT）
        let wake_row: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
            "SELECT id, COALESCE(scheduled_retry_at, created_at) FROM heartbeat_runs WHERE id = $1",
        )
        .bind(existing_id)
        .fetch_optional(db.pool())
        .await?;
        if let Some((_, retry_at)) = wake_row {
            // 找 wakeup id
            let wakeup_row: Option<(Uuid,)> = sqlx::query_as(
                "SELECT id FROM agent_wakeup_requests \
                 WHERE run_id = $1 AND company_id = $2 LIMIT 1",
            )
            .bind(existing_id)
            .bind(input.company_id)
            .fetch_optional(db.pool())
            .await?;
            return Ok(Some(ProviderQuotaMonitorResult {
                scheduled_run_id: existing_id,
                wakeup_request_id: wakeup_row.map(|(w,)| w).unwrap_or(Uuid::nil()),
                retry_at,
            }));
        }
    }

    // Step 2: 计算 retryAt
    let retry_at = compute_provider_quota_retry_at(db, input.latest_run_id, now).await?;

    // Step 3: 事务内创建
    let mut tx = db.pool().begin().await?;

    // 3a. 创建 wakeup
    let wakeup_payload = json!({
        "issueId": input.issue_id,
        "retryOfRunId": input.latest_run_id,
        "retryReason": "provider_quota_recovery",
        "providerQuotaRetryNotBefore": retry_at.to_rfc3339(),
    });
    // 必须使用同一个事务连接。若调用 AgentRepo（它使用独立 pool 连接），
    // heartbeat_runs 可能在 wakeup 尚未提交时等待或触发 FK 异常，破坏原子性。
    let wakeup_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_wakeup_requests \
            (company_id, agent_id, source, trigger_detail, reason, payload, status, \
             coalesced_count, requested_by_actor_type, requested_by_actor_id, idempotency_key, \
             run_id, error) \
         VALUES ($1, $2, 'automation', 'system', 'provider_quota_recovery', $3, 'queued', \
                 0, 'system', NULL, $4, NULL, NULL) \
         RETURNING id",
    )
    .bind(input.company_id)
    .bind(input.agent_id)
    .bind(&wakeup_payload)
    .bind(format!(
        "provider_quota_recovery:{}:{}",
        input.issue_id,
        retry_at.to_rfc3339()
    ))
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        eprintln!("ensure_provider_quota_wait_recovery_monitor: wakeup create failed: {e}");
        e
    })?;

    // 3b. 创建 scheduled_retry run
    let snapshot = json!({
        "issueId": input.issue_id,
        "taskId": input.issue_id,
        "wakeReason": "provider_quota_recovery",
        "retryReason": "provider_quota_recovery",
        "providerQuotaRetryNotBefore": retry_at.to_rfc3339(),
    });
    let scheduled_run_id: Uuid = sqlx::query_scalar(
        "INSERT INTO heartbeat_runs \
            (company_id, agent_id, invocation_source, trigger_detail, status, \
             wakeup_request_id, retry_of_run_id, scheduled_retry_at, \
             scheduled_retry_attempt, scheduled_retry_reason, context_snapshot, \
             created_at, updated_at) \
         VALUES ($1, $2, 'automation', 'system', 'scheduled_retry', $3, $4, $5, 1, \
                 'provider_quota_recovery', $6, $7, $7) \
         RETURNING id",
    )
    .bind(input.company_id)
    .bind(input.agent_id)
    .bind(wakeup_id)
    .bind(input.latest_run_id)
    .bind(retry_at)
    .bind(&snapshot)
    .bind(now)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        eprintln!("ensure_provider_quota_wait_recovery_monitor: scheduled run insert failed: {e}");
        e
    })?;

    // 3c. UPDATE wakeup SET run_id
    sqlx::query(
        "UPDATE agent_wakeup_requests SET run_id = $1, updated_at = $2 \
         WHERE id = $3 AND company_id = $4",
    )
    .bind(scheduled_run_id)
    .bind(now)
    .bind(wakeup_id)
    .bind(input.company_id)
    .execute(&mut *tx)
    .await?;

    // 3d. UPDATE issue_recovery_actions monitor_policy + timeout_at
    let monitor_policy = json!({
        "type": "wait_recovery",
        "retryAgentId": input.agent_id,
        "scheduledRunId": scheduled_run_id,
        "retryAt": retry_at.to_rfc3339(),
    });
    sqlx::query(
        "UPDATE issue_recovery_actions \
         SET monitor_policy = $1, timeout_at = $2, updated_at = $3 \
         WHERE id = $4 AND company_id = $5",
    )
    .bind(&monitor_policy)
    .bind(retry_at)
    .bind(now)
    .bind(input.action_id)
    .bind(input.company_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        eprintln!(
            "ensure_provider_quota_wait_recovery_monitor: recovery action update failed: {e}"
        );
        e
    })?;

    tx.commit().await.map_err(|e| {
        eprintln!("ensure_provider_quota_wait_recovery_monitor: commit failed: {e}");
        e
    })?;

    Ok(Some(ProviderQuotaMonitorResult {
        scheduled_run_id,
        wakeup_request_id: wakeup_id,
        retry_at,
    }))
}

// ============================================================================
// Helpers (private)
// ============================================================================

/// 计算 provider_quota retry 时间：
/// - 从 latest_run.result_json.providerQuotaRetryNotBefore 读取
/// - 没有则 fallback 到 now + DEFAULT_PROVIDER_QUOTA_RETRY_AFTER_MS
async fn compute_provider_quota_retry_at(
    db: &Db,
    latest_run_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> sqlx::Result<DateTime<Utc>> {
    let Some(run_id) = latest_run_id else {
        return Ok(now + chrono::Duration::milliseconds(DEFAULT_PROVIDER_QUOTA_RETRY_AFTER_MS));
    };
    let row: Option<(Option<Value>, Option<Value>)> =
        sqlx::query_as("SELECT result_json, context_snapshot FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(db.pool())
            .await?;
    let Some((result_json, context_snapshot)) = row else {
        return Ok(now + chrono::Duration::milliseconds(DEFAULT_PROVIDER_QUOTA_RETRY_AFTER_MS));
    };
    let candidates = [
        result_json
            .as_ref()
            .and_then(|value| value.get("retryNotBefore")),
        result_json
            .as_ref()
            .and_then(|value| value.get("transientRetryNotBefore")),
        result_json
            .as_ref()
            .and_then(|value| value.get(PROVIDER_QUOTA_RETRY_NOT_BEFORE_KEY)),
        context_snapshot
            .as_ref()
            .and_then(|value| value.get(PROVIDER_QUOTA_RETRY_NOT_BEFORE_KEY)),
        context_snapshot
            .as_ref()
            .and_then(|value| value.get("transientRetryNotBefore")),
    ];
    for candidate in candidates.into_iter().flatten() {
        let parsed = parse_retry_at(candidate);
        if parsed.is_some_and(|value| value > now) {
            return Ok(parsed.expect("checked above"));
        }
    }
    Ok(now + chrono::Duration::milliseconds(DEFAULT_PROVIDER_QUOTA_RETRY_AFTER_MS))
}

fn parse_retry_at(value: &Value) -> Option<DateTime<Utc>> {
    if let Some(raw) = value.as_str() {
        return DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|parsed| parsed.with_timezone(&Utc));
    }
    value.as_f64().and_then(|milliseconds| {
        if !milliseconds.is_finite() {
            return None;
        }
        DateTime::<Utc>::from_timestamp_millis(milliseconds.floor() as i64)
    })
}

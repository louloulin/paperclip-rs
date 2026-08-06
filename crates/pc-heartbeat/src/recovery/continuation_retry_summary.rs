//! Continuation 重试摘要的纯计划层 + DB 接入层。
//!
//! 对齐 Node `services/recovery/service.ts` 的：
//! - `summarizeRecentContinuationRetries`：扫描最近 N 次 heartbeat run，
//!   统计连续「`retryReason=issue_continuation_needed` + unsuccessful terminal status + 同 errorCode」
//!   的次数 + 最近一次 finished_at。
//!
//! 边界：
//! - 纯函数 `summarize_continuation_retries_from_rows` 不依赖 DB
//! - DB 接入层 `load_continuation_retry_summary` 走 heartbeat_runs 表
//!
//! 调用方拿到 summary 后决定：
//! - `consecutive >= max_attempts` → 升级到 blocked（escalate）
//! - `consecutive < max_attempts` 且 `base_backoff_ms > 0` 且 elapsed 太短 → 跳过
//! - 否则：重试 wake

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use pc_core::Timestamp;
use pc_repos::Db;

/// Node `UNSUCCESSFUL_HEARTBEAT_RUN_TERMINAL_STATUSES` 常量镜像。
pub const UNSUCCESSFUL_HEARTBEAT_RUN_TERMINAL_STATUSES: &[&str] =
    &["interrupted", "failed", "cancelled", "timed_out"];

/// Node `ISSUE_CONTINUATION_NEEDED_RETRY_REASON` 常量镜像。
pub const ISSUE_CONTINUATION_NEEDED_RETRY_REASON: &str = "issue_continuation_needed";

/// Node `INTERACTION_CONTINUATION_REQUEUE_MAX_ATTEMPTS` 常量镜像。
pub const INTERACTION_CONTINUATION_REQUEUE_MAX_ATTEMPTS: u32 = 3;

/// Node `CONTINUATION_RECOVERY_TRANSIENT_MAX_ATTEMPTS` 常量镜像。
pub const CONTINUATION_RECOVERY_TRANSIENT_MAX_ATTEMPTS: u32 = 3;

/// Node `CONTINUATION_RECOVERY_DEFAULT_MAX_ATTEMPTS` 常量镜像。
pub const CONTINUATION_RECOVERY_DEFAULT_MAX_ATTEMPTS: u32 = 1;

/// Node `CONTINUATION_RECOVERY_TRANSIENT_BASE_BACKOFF_MS` 常量镜像。
pub const CONTINUATION_RECOVERY_TRANSIENT_BASE_BACKOFF_MS: i64 = 60_000;

/// Continuation retry 摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContinuationRetrySummary {
    pub consecutive: u32,
    pub latest_finished_at: Option<Timestamp>,
    pub matched_retry_reason: bool,
    pub reached_terminal_status: bool,
}

/// Continuation retry 行（DB 层传入纯函数的最小快照）。
#[derive(Debug, Clone)]
pub struct ContinuationRunRow {
    pub status: String,
    pub error_code: Option<String>,
    pub retry_reason: Option<String>,
    pub finished_at: Option<Timestamp>,
}

/// 纯函数：计算连续失败次数。
///
/// 与 Node `summarizeRecentContinuationRetries` 完全对齐：
/// 1. 行按 created_at DESC（调用方保证）
/// 2. 第一次不满足 retry_reason=issue_continuation_needed → break
/// 3. status 不在 UNSUCCESSFUL_HEARTBEAT_RUN_TERMINAL_STATUSES → break
/// 4. error_code 不匹配 → break
/// 5. 都满足 → consecutive += 1，记录 finished_at
pub fn summarize_continuation_retries_from_rows(
    rows: &[ContinuationRunRow],
    error_code_to_match: Option<&str>,
) -> ContinuationRetrySummary {
    let mut summary = ContinuationRetrySummary::default();
    for row in rows {
        summary.matched_retry_reason =
            row.retry_reason.as_deref() == Some(ISSUE_CONTINUATION_NEEDED_RETRY_REASON);
        if !summary.matched_retry_reason {
            break;
        }
        if !UNSUCCESSFUL_HEARTBEAT_RUN_TERMINAL_STATUSES.contains(&row.status.as_str()) {
            summary.reached_terminal_status = false;
            break;
        }
        summary.reached_terminal_status = true;
        if row.error_code.as_deref() != error_code_to_match {
            break;
        }
        summary.consecutive += 1;
        if summary.latest_finished_at.is_none() {
            summary.latest_finished_at = row.finished_at;
        }
    }
    summary
}

/// 纯函数：判断是否应跳过重试（backoff 未到）。
///
/// 返回 `true` 表示「应该跳过本次重试」：
/// - `consecutive > 0`
/// - `base_backoff_ms > 0`
/// - `latest_finished_at` 距 now 不足 `base_backoff_ms * 2^(consecutive-1)`
pub fn should_skip_due_to_backoff(
    summary: &ContinuationRetrySummary,
    base_backoff_ms: i64,
    now: DateTime<Utc>,
) -> bool {
    if base_backoff_ms <= 0 || summary.consecutive == 0 {
        return false;
    }
    let Some(latest) = summary.latest_finished_at else {
        return false;
    };
    let required_delay_ms = base_backoff_ms * 2_i64.pow((summary.consecutive - 1).min(20));
    let elapsed_ms = (now - latest.as_datetime()).num_milliseconds();
    elapsed_ms < required_delay_ms
}

/// 纯函数：判断是否应升级（consecutive >= max_attempts）。
pub fn should_escalate_due_to_retry_limit(
    summary: &ContinuationRetrySummary,
    max_attempts: u32,
) -> bool {
    summary.consecutive >= max_attempts
}

// ----------------------------------------------------------------------------
// DB layer
// ----------------------------------------------------------------------------

/// 从 DB 读取最近 N 次 heartbeat run 并计算摘要。
///
/// 与 Node `summarizeRecentContinuationRetries` SQL 对齐：
/// - WHERE company_id=$1 AND agent_id=$2 AND context_snapshot->>'issueId'=$3
/// - 若 since 非空：AND (created_at >= since OR finished_at >= since)
/// - ORDER BY created_at DESC, id DESC
/// - LIMIT 10
pub async fn load_continuation_retry_summary(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    agent_id: Uuid,
    error_code_to_match: Option<&str>,
    since: Option<DateTime<Utc>>,
    limit: i64,
) -> sqlx::Result<ContinuationRetrySummary> {
    let since_clause = since
        .map(|_| "AND (heartbeat_runs.created_at >= $5 OR heartbeat_runs.finished_at >= $5)")
        .unwrap_or("");
    let query = format!(
        "SELECT status::text AS status, error_code, context_snapshot, finished_at \
         FROM heartbeat_runs \
         WHERE company_id = $1 AND agent_id = $2 \
           AND context_snapshot ->> 'issueId' = $3::text \
           {} \
         ORDER BY created_at DESC, id DESC \
         LIMIT $4",
        since_clause
    );
    let rows = if let Some(since_ts) = since {
        sqlx::query(&query)
            .bind(company_id)
            .bind(agent_id)
            .bind(issue_id.to_string())
            .bind(limit.clamp(1, 100))
            .bind(since_ts)
            .fetch_all(db.pool())
            .await?
    } else {
        sqlx::query(&query)
            .bind(company_id)
            .bind(agent_id)
            .bind(issue_id.to_string())
            .bind(limit.clamp(1, 100))
            .fetch_all(db.pool())
            .await?
    };
    let parsed: Vec<ContinuationRunRow> = rows
        .into_iter()
        .map(|row| {
            let snapshot: Option<Value> = row.try_get("context_snapshot").ok();
            let retry_reason = snapshot
                .as_ref()
                .and_then(|v| v.get("retryReason"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned());
            ContinuationRunRow {
                status: row.try_get::<String, _>("status").unwrap_or_default(),
                error_code: row
                    .try_get::<Option<String>, _>("error_code")
                    .ok()
                    .flatten(),
                retry_reason,
                finished_at: row
                    .try_get::<Option<Timestamp>, _>("finished_at")
                    .ok()
                    .flatten(),
            }
        })
        .collect();
    Ok(summarize_continuation_retries_from_rows(
        &parsed,
        error_code_to_match,
    ))
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(epoch_seconds: i64) -> Timestamp {
        Timestamp::from_dt(Utc.timestamp_opt(epoch_seconds, 0).unwrap())
    }

    fn row(
        status: &str,
        error_code: Option<&str>,
        retry_reason: Option<&str>,
        finished_at: Option<Timestamp>,
    ) -> ContinuationRunRow {
        ContinuationRunRow {
            status: status.to_owned(),
            error_code: error_code.map(str::to_owned),
            retry_reason: retry_reason.map(str::to_owned),
            finished_at,
        }
    }

    #[test]
    fn empty_rows_yields_zero_summary() {
        let s = summarize_continuation_retries_from_rows(&[], Some("process_lost"));
        assert_eq!(s.consecutive, 0);
        assert!(s.latest_finished_at.is_none());
        assert!(!s.matched_retry_reason);
        assert!(!s.reached_terminal_status);
    }

    #[test]
    fn breaks_on_non_continuation_retry_reason() {
        let rows = vec![
            row(
                "failed",
                Some("process_lost"),
                Some("issue_continuation_needed"),
                Some(ts(100)),
            ),
            row(
                "failed",
                Some("process_lost"),
                Some("max_turns"),
                Some(ts(90)),
            ),
            row(
                "failed",
                Some("process_lost"),
                Some("issue_continuation_needed"),
                Some(ts(80)),
            ),
        ];
        let s = summarize_continuation_retries_from_rows(&rows, Some("process_lost"));
        assert_eq!(s.consecutive, 1, "must break on max_turns row");
    }

    #[test]
    fn breaks_on_non_terminal_status() {
        let rows = vec![
            row(
                "running",
                Some("process_lost"),
                Some("issue_continuation_needed"),
                Some(ts(100)),
            ),
            row(
                "failed",
                Some("process_lost"),
                Some("issue_continuation_needed"),
                Some(ts(80)),
            ),
        ];
        let s = summarize_continuation_retries_from_rows(&rows, Some("process_lost"));
        assert_eq!(s.consecutive, 0);
        assert!(!s.reached_terminal_status);
    }

    #[test]
    fn breaks_on_error_code_mismatch() {
        // DESC order: newest first; first row has wrong error_code so loop breaks immediately.
        let rows = vec![
            row(
                "failed",
                Some("timeout"),
                Some("issue_continuation_needed"),
                Some(ts(100)),
            ),
            row(
                "failed",
                Some("process_lost"),
                Some("issue_continuation_needed"),
                Some(ts(80)),
            ),
        ];
        let s = summarize_continuation_retries_from_rows(&rows, Some("process_lost"));
        assert_eq!(s.consecutive, 0);

        // Second scenario: two matching rows, then one with different error_code breaks the chain.
        let rows = vec![
            row(
                "failed",
                Some("process_lost"),
                Some("issue_continuation_needed"),
                Some(ts(120)),
            ),
            row(
                "failed",
                Some("process_lost"),
                Some("issue_continuation_needed"),
                Some(ts(100)),
            ),
            row(
                "failed",
                Some("timeout"),
                Some("issue_continuation_needed"),
                Some(ts(80)),
            ),
        ];
        let s = summarize_continuation_retries_from_rows(&rows, Some("process_lost"));
        assert_eq!(s.consecutive, 2, "must stop at first error_code mismatch");
    }

    #[test]
    fn counts_all_consecutive_matching_rows() {
        let rows = vec![
            row(
                "failed",
                Some("process_lost"),
                Some("issue_continuation_needed"),
                Some(ts(100)),
            ),
            row(
                "failed",
                Some("process_lost"),
                Some("issue_continuation_needed"),
                Some(ts(80)),
            ),
            row(
                "interrupted",
                Some("process_lost"),
                Some("issue_continuation_needed"),
                Some(ts(60)),
            ),
        ];
        let s = summarize_continuation_retries_from_rows(&rows, Some("process_lost"));
        assert_eq!(s.consecutive, 3);
        assert_eq!(s.latest_finished_at, Some(ts(100)));
    }

    #[test]
    fn null_error_code_breaks_when_match_required() {
        let rows = vec![row(
            "failed",
            None,
            Some("issue_continuation_needed"),
            Some(ts(100)),
        )];
        let s = summarize_continuation_retries_from_rows(&rows, Some("process_lost"));
        assert_eq!(s.consecutive, 0);
    }

    #[test]
    fn should_skip_backoff_returns_false_when_consecutive_zero() {
        let s = ContinuationRetrySummary::default();
        assert!(!should_skip_due_to_backoff(&s, 60_000, Utc::now()));
    }

    #[test]
    fn should_skip_backoff_returns_true_when_recent_finish() {
        let now = Utc.timestamp_opt(1000, 0).unwrap();
        let s = ContinuationRetrySummary {
            consecutive: 1,
            latest_finished_at: Some(ts(now.timestamp() - 5)),
            ..Default::default()
        };
        assert!(should_skip_due_to_backoff(&s, 60_000, now));
    }

    #[test]
    fn should_skip_backoff_returns_false_when_long_elapsed() {
        let now = Utc.timestamp_opt(1000, 0).unwrap();
        let s = ContinuationRetrySummary {
            consecutive: 1,
            latest_finished_at: Some(ts(now.timestamp() - 600)),
            ..Default::default()
        };
        assert!(!should_skip_due_to_backoff(&s, 60_000, now));
    }

    #[test]
    fn should_escalate_when_consecutive_ge_max() {
        let s = ContinuationRetrySummary {
            consecutive: 3,
            ..Default::default()
        };
        assert!(should_escalate_due_to_retry_limit(&s, 3));
        assert!(should_escalate_due_to_retry_limit(&s, 2));
        assert!(!should_escalate_due_to_retry_limit(&s, 4));
    }
}

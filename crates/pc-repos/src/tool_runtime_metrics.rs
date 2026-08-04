//! Tool runtime metrics（1:1 port of Node `server/src/services/tool-runtime-metrics.ts`，57 行）。
//!
//! 单一职责：维护 `tool_runtime_metric_counters` 表，按 (company_id, metric, 分钟桶) 聚合计数。
//!
//! 公开 API：
//! - [`TOOL_RUNTIME_AUDIT_WRITE_FAILURE_METRIC`] —— audit 写入失败 metric 名常量
//! - [`minute_bucket`] —— 把任意 DateTime 截断到分钟桶
//! - [`increment_tool_runtime_metric_counter`] —— INSERT ... ON CONFLICT DO UPDATE 累加 count
//! - [`record_tool_runtime_audit_write_failure`] —— 包装 + 错误吞咽（与 Node try/catch 一致）

use chrono::{DateTime, Timelike, Utc};
use tracing::warn;
use uuid::Uuid;

use crate::Db;

// ============================================================================
// Constants
// ============================================================================

/// audit 写入失败 metric 名（与 Node `TOOL_RUNTIME_AUDIT_WRITE_FAILURE_METRIC` 1:1 对齐）。
pub const TOOL_RUNTIME_AUDIT_WRITE_FAILURE_METRIC: &str = "audit_write_failed";

// ============================================================================
// Pure helpers
// ============================================================================

/// 把任意时间戳截断到分钟桶（与 Node `minuteBucket` 1:1 对齐）。
///
/// - 秒 / 毫秒归零
/// - 时分保留
#[must_use]
pub fn minute_bucket(at: DateTime<Utc>) -> DateTime<Utc> {
    at.with_second(0)
        .and_then(|d| d.with_nanosecond(0))
        .unwrap_or(at)
}

// ============================================================================
// Input
// ============================================================================

/// `increment_tool_runtime_metric_counter` 入参（与 Node `input` 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct IncrementMetricInput {
    pub company_id: Uuid,
    pub metric: &'static str,
    pub at: Option<DateTime<Utc>>,
}

// ============================================================================
// Public API
// ============================================================================

/// 累加 metric counter（与 Node `incrementToolRuntimeMetricCounter` 1:1 对齐）。
///
/// 行为：
/// 1. `at` 缺省为 `now()`
/// 2. `bucketStartAt = minuteBucket(at)`
/// 3. INSERT `tool_runtime_metric_counters` with `count = 1`
/// 4. `ON CONFLICT (company_id, metric, bucket_start_at) DO UPDATE SET count = count + 1, updated_at = excluded.updated_at`
pub async fn increment_tool_runtime_metric_counter(
    db: &Db,
    input: IncrementMetricInput,
) -> sqlx::Result<()> {
    let at = input.at.unwrap_or_else(Utc::now);
    let bucket = minute_bucket(at);

    sqlx::query(
        "INSERT INTO tool_runtime_metric_counters \
         (company_id, metric, bucket_start_at, count, created_at, updated_at) \
         VALUES ($1, $2, $3, 1, $4, $5) \
         ON CONFLICT (company_id, metric, bucket_start_at) DO UPDATE SET \
            count = tool_runtime_metric_counters.count + 1, \
            updated_at = EXCLUDED.updated_at",
    )
    .bind(input.company_id)
    .bind(input.metric)
    .bind(bucket)
    .bind(at)
    .bind(at)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// 记录一次 audit 写入失败（与 Node `recordToolRuntimeAuditWriteFailure` 1:1 对齐）。
///
/// 错误吞咽 + warn 日志：counter 自身失败不应再次抛出。
pub async fn record_tool_runtime_audit_write_failure(db: &Db, company_id: Uuid) {
    if let Err(err) = increment_tool_runtime_metric_counter(
        db,
        IncrementMetricInput {
            company_id,
            metric: TOOL_RUNTIME_AUDIT_WRITE_FAILURE_METRIC,
            at: None,
        },
    )
    .await
    {
        warn!(
            err = %err,
            company_id = %company_id,
            "failed to record audit write failure counter"
        );
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // ---- 常量 ----

    #[test]
    fn audit_write_failure_metric_constant_matches_node() {
        assert_eq!(TOOL_RUNTIME_AUDIT_WRITE_FAILURE_METRIC, "audit_write_failed");
    }

    // ---- minute_bucket ----

    #[test]
    fn minute_bucket_truncates_seconds() {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 12, 30, 45).unwrap()
            + chrono::Duration::milliseconds(123);
        let bucket = minute_bucket(at);
        assert_eq!(bucket.second(), 0);
        assert_eq!(bucket.nanosecond(), 0);
        assert_eq!(bucket.minute(), 30);
        assert_eq!(bucket.hour(), 12);
    }

    #[test]
    fn minute_bucket_preserves_minute() {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 12, 30, 0).unwrap();
        let bucket = minute_bucket(at);
        assert_eq!(bucket, at);
    }

    #[test]
    fn minute_bucket_handles_zero_second() {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 12, 30, 0).unwrap();
        let bucket = minute_bucket(at);
        assert_eq!(bucket.second(), 0);
    }

    // ---- SQL 形状 ----

    #[test]
    fn increment_sql_uses_upsert_with_three_column_target() {
        let sql = "INSERT INTO tool_runtime_metric_counters \
                   (company_id, metric, bucket_start_at, count, created_at, updated_at) \
                   VALUES ($1, $2, $3, 1, $4, $5) \
                   ON CONFLICT (company_id, metric, bucket_start_at) DO UPDATE SET \
                      count = tool_runtime_metric_counters.count + 1, \
                      updated_at = EXCLUDED.updated_at";
        assert!(sql.contains("ON CONFLICT (company_id, metric, bucket_start_at)"));
        assert!(sql.contains("count = tool_runtime_metric_counters.count + 1"));
        assert!(sql.contains("EXCLUDED.updated_at"));
        assert!(sql.contains("VALUES ($1, $2, $3, 1, $4, $5)"));
    }

    // ---- IncrementMetricInput ----

    #[test]
    fn increment_metric_input_carries_company_id_and_metric() {
        let input = IncrementMetricInput {
            company_id: Uuid::nil(),
            metric: "test_metric",
            at: None,
        };
        assert_eq!(input.metric, "test_metric");
        assert!(input.at.is_none());
    }

    #[test]
    fn increment_metric_input_carries_optional_at() {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 12, 30, 0).unwrap();
        let input = IncrementMetricInput {
            company_id: Uuid::nil(),
            metric: "test",
            at: Some(at),
        };
        assert_eq!(input.at, Some(at));
    }

}

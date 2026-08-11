//! Routine trait: business code implements this to register executable work.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum RoutineError {
    #[error("routine failed: {0}")]
    Failed(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("routine not registered: {0}")]
    NotFound(String),
    #[error("routine timeout after {0:?}")]
    Timeout(std::time::Duration),
}

pub type RoutineResult<T> = Result<T, RoutineError>;

#[derive(Debug, Clone)]
pub struct RoutineContext {
    pub run_id: Uuid,
    pub company_id: Uuid,
    pub config: Value,
    pub secrets: Value,
}

impl RoutineContext {
    #[must_use]
    pub fn new(run_id: Uuid, company_id: Uuid) -> Self {
        Self {
            run_id,
            company_id,
            config: Value::Null,
            secrets: Value::Null,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoutineOutput {
    pub result: Value,
    pub metadata: Value,
}

impl RoutineOutput {
    #[must_use]
    pub fn ok(result: Value) -> Self {
        Self {
            result,
            metadata: Value::Null,
        }
    }
}

// ============================================================================
// R488: Routine execution helpers (复刻 Node `routines.ts::nextResultText`)
// ============================================================================

/// Routine 单次执行结果的人类可读文本（存到 `last_result` 列）。
///
/// 与 Node `nextResultText(status, issueId)` 1:1 对齐：
/// - `issue_created` + 有 `issueId` → "Created execution issue {id}"
/// - `coalesced` → "Coalesced into an existing live execution issue"
/// - `skipped_paused` → "Skipped because the project is paused"
/// - `skipped` → "Skipped because a live execution issue already exists"
/// - `completed` → "Execution issue completed"
/// - `failed` → "Execution failed"
/// - 其它 → 原样返回 status（保持向前兼容：新增 status 不会被吞掉）
///
/// 高内聚：纯字符串格式化；无 IO、无副作用。
/// 低耦合：仅依赖 `&str` 入参；可被任意 routine service 调用。
#[must_use]
pub fn next_result_text(status: &str, issue_id: Option<&str>) -> String {
    if status == "issue_created" {
        if let Some(id) = issue_id {
            return format!("Created execution issue {id}");
        }
    }
    match status {
        "issue_created" => "Created execution issue".to_string(),
        "coalesced" => "Coalesced into an existing live execution issue".to_string(),
        "skipped_paused" => "Skipped because the project is paused".to_string(),
        "skipped" => "Skipped because a live execution issue already exists".to_string(),
        "completed" => "Execution issue completed".to_string(),
        "failed" => "Execution failed".to_string(),
        other => other.to_string(),
    }
}

/// 把 webhook 时间戳字符串归一化为毫秒（Unix epoch）。
///
/// 与 Node `normalizeWebhookTimestampMs(rawTimestamp)` 1:1 对齐：
/// - `Number(raw)` 解析失败或非有限数 → `None`
/// - 解析值 > 1e12 视为毫秒 → 原样返回
/// - 否则视为秒 → 乘 1000
///
/// 用于 webhook 签名校验：先归一再与 `Date.now()` 比较 `replayWindowSec`。
///
/// 高内聚：纯数值解析；无 IO。
/// 低耦合：仅依赖 `&str` → `i64`；无 chrono 依赖（避免 timestamp 类型耦合）。
#[must_use]
pub fn normalize_webhook_timestamp_ms(raw_timestamp: &str) -> Option<i64> {
    let parsed: f64 = raw_timestamp.parse().ok()?;
    if !parsed.is_finite() {
        return None;
    }
    // 1e12 ms ≈ 2001-09-09；1e9 s ≈ 2001-09-09。> 1e12 一定是 ms。
    // Truncation is intentional: webhook timestamps are valid Unix epoch ms,
    // well within i64 range (i64::MAX ≈ 9.2e18, ~2.9e8 years from epoch).
    #[allow(clippy::cast_possible_truncation)]
    let result = if parsed.abs() > 1e12 {
        parsed as i64
    } else {
        (parsed * 1000.0) as i64
    };
    Some(result)
}

#[async_trait]
pub trait Routine: Send + Sync + std::fmt::Debug {
    fn key(&self) -> &'static str;
    fn label(&self) -> &'static str;
    async fn run(&self, ctx: RoutineContext) -> RoutineResult<RoutineOutput>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ R488: next_result_text ============

    #[test]
    fn next_result_text_issue_created_with_id() {
        assert_eq!(
            next_result_text("issue_created", Some("iss-42")),
            "Created execution issue iss-42"
        );
    }

    #[test]
    fn next_result_text_issue_created_without_id() {
        assert_eq!(
            next_result_text("issue_created", None),
            "Created execution issue"
        );
    }

    #[test]
    fn next_result_text_known_statuses() {
        assert_eq!(
            next_result_text("coalesced", None),
            "Coalesced into an existing live execution issue"
        );
        assert_eq!(
            next_result_text("skipped_paused", None),
            "Skipped because the project is paused"
        );
        assert_eq!(
            next_result_text("skipped", None),
            "Skipped because a live execution issue already exists"
        );
        assert_eq!(
            next_result_text("completed", None),
            "Execution issue completed"
        );
        assert_eq!(next_result_text("failed", None), "Execution failed");
    }

    #[test]
    fn next_result_text_unknown_status_passes_through() {
        // 未知 status 原样返回（向前兼容：未来加新 status 不会被吞）
        assert_eq!(
            next_result_text("some_future_status", None),
            "some_future_status"
        );
        assert_eq!(
            next_result_text("some_future_status", Some("iss-1")),
            "some_future_status"
        );
    }

    // ============ R488: normalize_webhook_timestamp_ms ============

    #[test]
    fn normalize_webhook_timestamp_ms_seconds_to_millis() {
        // 1700000000 秒 → 1700000000000 毫秒
        assert_eq!(
            normalize_webhook_timestamp_ms("1700000000"),
            Some(1_700_000_000_000)
        );
    }

    #[test]
    fn normalize_webhook_timestamp_ms_already_millis() {
        // > 1e12 视为毫秒，原样
        assert_eq!(
            normalize_webhook_timestamp_ms("1700000000123"),
            Some(1_700_000_000_123)
        );
    }

    #[test]
    fn normalize_webhook_timestamp_ms_invalid_returns_none() {
        assert_eq!(normalize_webhook_timestamp_ms("not a number"), None);
        assert_eq!(normalize_webhook_timestamp_ms(""), None);
    }

    #[test]
    fn normalize_webhook_timestamp_ms_non_finite_returns_none() {
        // f64 解析 "NaN" / "Infinity" → is_finite() 为 false
        assert_eq!(normalize_webhook_timestamp_ms("NaN"), None);
        assert_eq!(normalize_webhook_timestamp_ms("Infinity"), None);
    }
}

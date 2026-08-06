//! `persistAdapterFailureRecoveryClassification` —— Node `services/recovery/service.ts:3494`。
//!
//! 业务语义：
//! - 将 adapter failure classification 持久化到 `heartbeat_runs` 行：
//!   * 更新 `error_code` = classification.kind
//!   * 合并 `result_json`：
//!     - `errorFamily` / `recoveryClassification` 都设为 classification.kind
//!     - 若 classification 是 `ProviderQuota`：附加 3 个 retry 时间字段
//!       （`retryNotBefore` / `transientRetryNotBefore` / `providerQuotaRetryNotBefore`）
//!     - 若 classification 是 `ConfigurationIncomplete`：仅写 error 分类字段，不写 retry
//!   * 保留已有 result_json 其他字段不变
//! - 用于：
//!   * ProviderQuota 路径（接续 schedule_provider_quota_recovery_monitor）
//!   * ConfigurationIncomplete 路径（escalateStrandedAssignedIssue 之后）
//! - 不存在的 run → 返回 Ok(false)，不抛错
//!
//! 设计原则：
//! - 顶层函数 `persist_adapter_failure_recovery_classification` 是 DB I/O 入口
//! - pure 函数 `build_classified_result_json` 与 `error_code_for_classification` 可独立单测
//! - 单事务原子：read existing → merge → update
use crate::recovery::adapter_failure_classification::AdapterFailureRecoveryClassification;
use pc_repos::Db;
use serde_json::{Map, Value};
use uuid::Uuid;

const RESULT_JSON_KEY_ERROR_FAMILY: &str = "errorFamily";
const RESULT_JSON_KEY_RECOVERY_CLASSIFICATION: &str = "recoveryClassification";
const RESULT_JSON_KEY_RETRY_NOT_BEFORE: &str = "retryNotBefore";
const RESULT_JSON_KEY_TRANSIENT_RETRY_NOT_BEFORE: &str = "transientRetryNotBefore";
const RESULT_JSON_KEY_PROVIDER_QUOTA_RETRY_NOT_BEFORE: &str = "providerQuotaRetryNotBefore";

const ERROR_FAMILY_PROVIDER_QUOTA: &str = "provider_quota";
const ERROR_FAMILY_CONFIGURATION_INCOMPLETE: &str = "configuration_incomplete";

const ERROR_CODE_PROVIDER_QUOTA: &str = "provider_quota";
const ERROR_CODE_CONFIGURATION_INCOMPLETE: &str = "configuration_incomplete";

/// 把 classification 映射成 `heartbeat_runs.error_code` 字符串。
///
/// 与 Node `withAdapterFailureRecoveryClassification` 中 `errorCode = classification.kind` 对齐。
pub fn error_code_for_classification(
    classification: &AdapterFailureRecoveryClassification,
) -> &'static str {
    match classification {
        AdapterFailureRecoveryClassification::ProviderQuota { .. } => ERROR_CODE_PROVIDER_QUOTA,
        AdapterFailureRecoveryClassification::ConfigurationIncomplete => {
            ERROR_CODE_CONFIGURATION_INCOMPLETE
        }
    }
}

/// 把 classification 映射成 `result_json.errorFamily` 字符串。
pub fn error_family_for_classification(
    classification: &AdapterFailureRecoveryClassification,
) -> &'static str {
    match classification {
        AdapterFailureRecoveryClassification::ProviderQuota { .. } => ERROR_FAMILY_PROVIDER_QUOTA,
        AdapterFailureRecoveryClassification::ConfigurationIncomplete => {
            ERROR_FAMILY_CONFIGURATION_INCOMPLETE
        }
    }
}

/// pure：在已有 result_json 上合并 classification 字段，返回新 result_json。
///
/// 行为：
/// - 输入 `existing`：可以是 `Object` / `Null` / 其他；非 `Object` 时按空对象处理
/// - 输出：保留所有已有字段，新增/覆盖：
///   * `errorFamily` = error_family_for_classification
///   * `recoveryClassification` = classification kind 字符串
///   * 若 ProviderQuota：附加 3 个 retry 时间字段（RFC3339 字符串）
pub fn build_classified_result_json(
    existing: Option<&Value>,
    classification: &AdapterFailureRecoveryClassification,
) -> Map<String, Value> {
    let mut result: Map<String, Value> = match existing {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    };
    let kind = error_code_for_classification(classification);
    let family = error_family_for_classification(classification);
    result.insert(
        RESULT_JSON_KEY_ERROR_FAMILY.to_owned(),
        Value::String(family.to_owned()),
    );
    result.insert(
        RESULT_JSON_KEY_RECOVERY_CLASSIFICATION.to_owned(),
        Value::String(kind.to_owned()),
    );
    if let AdapterFailureRecoveryClassification::ProviderQuota { retry_at, .. } = classification {
        let retry_iso = retry_at.to_rfc3339();
        let retry_value = Value::String(retry_iso.clone());
        result.insert(
            RESULT_JSON_KEY_RETRY_NOT_BEFORE.to_owned(),
            retry_value.clone(),
        );
        result.insert(
            RESULT_JSON_KEY_TRANSIENT_RETRY_NOT_BEFORE.to_owned(),
            retry_value.clone(),
        );
        result.insert(
            RESULT_JSON_KEY_PROVIDER_QUOTA_RETRY_NOT_BEFORE.to_owned(),
            retry_value,
        );
    }
    result
}

/// DB 入口：把 classification 持久化到指定 heartbeat_run 行。
///
/// 返回 `Ok(true)` 当行被成功更新；`Ok(false)` 当 run 不存在。
/// 不存在的 run 不抛错（与 Node `withAdapterFailureRecoveryClassification` 调用语义一致：
/// Node 会在分类前先校验 run 存在，但 Rust 端做 defensive return）。
pub async fn persist_adapter_failure_recovery_classification(
    db: &Db,
    company_id: Uuid,
    run_id: Uuid,
    classification: AdapterFailureRecoveryClassification,
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
    let new_result = Value::Object(build_classified_result_json(
        existing.as_ref(),
        &classification,
    ));
    let error_code = error_code_for_classification(&classification);
    let updated = sqlx::query(
        "UPDATE heartbeat_runs SET error_code = $1, result_json = $2, \
         updated_at = now() WHERE id = $3 AND company_id = $4",
    )
    .bind(error_code)
    .bind(&new_result)
    .bind(run_id)
    .bind(company_id)
    .execute(db.pool())
    .await?;
    Ok(updated.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use serde_json::json;

    #[test]
    fn error_code_for_provider_quota() {
        let c = AdapterFailureRecoveryClassification::ProviderQuota {
            retry_at: Utc::now(),
            parsed_reset_time: true,
        };
        assert_eq!(error_code_for_classification(&c), "provider_quota");
    }

    #[test]
    fn error_code_for_configuration_incomplete() {
        let c = AdapterFailureRecoveryClassification::ConfigurationIncomplete;
        assert_eq!(
            error_code_for_classification(&c),
            "configuration_incomplete"
        );
    }

    #[test]
    fn build_classified_provider_quota_merges_retry_fields() {
        let existing = json!({"foo": "bar", "nested": {"x": 1}});
        let retry_at = Utc::now() + Duration::hours(2);
        let c = AdapterFailureRecoveryClassification::ProviderQuota {
            retry_at,
            parsed_reset_time: true,
        };
        let map = build_classified_result_json(Some(&existing), &c);
        assert_eq!(map["errorFamily"], "provider_quota");
        assert_eq!(map["recoveryClassification"], "provider_quota");
        assert_eq!(map["retryNotBefore"], retry_at.to_rfc3339());
        assert_eq!(map["transientRetryNotBefore"], retry_at.to_rfc3339());
        assert_eq!(map["providerQuotaRetryNotBefore"], retry_at.to_rfc3339());
        assert_eq!(map["foo"], "bar");
        assert_eq!(map["nested"]["x"], 1);
    }

    #[test]
    fn build_classified_configuration_incomplete_skips_retry_fields() {
        let existing = json!({"preserve": "me"});
        let c = AdapterFailureRecoveryClassification::ConfigurationIncomplete;
        let map = build_classified_result_json(Some(&existing), &c);
        assert_eq!(map["errorFamily"], "configuration_incomplete");
        assert_eq!(map["recoveryClassification"], "configuration_incomplete");
        assert!(!map.contains_key("retryNotBefore"));
        assert!(!map.contains_key("transientRetryNotBefore"));
        assert!(!map.contains_key("providerQuotaRetryNotBefore"));
        assert_eq!(map["preserve"], "me");
    }

    #[test]
    fn build_classified_handles_null_existing() {
        let c = AdapterFailureRecoveryClassification::ConfigurationIncomplete;
        let map = build_classified_result_json(None, &c);
        assert_eq!(map["errorFamily"], "configuration_incomplete");
        assert_eq!(map["recoveryClassification"], "configuration_incomplete");
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn build_classified_handles_non_object_existing() {
        let existing = Value::String("not an object".to_owned());
        let c = AdapterFailureRecoveryClassification::ConfigurationIncomplete;
        let map = build_classified_result_json(Some(&existing), &c);
        assert_eq!(map["errorFamily"], "configuration_incomplete");
        assert_eq!(map["recoveryClassification"], "configuration_incomplete");
        assert_eq!(map.len(), 2);
    }
}

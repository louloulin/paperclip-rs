//! Round 321：`persistAdapterFailureRecoveryClassification` 的 PostgreSQL 验证。
//!
//! 与 Node `services/recovery/service.ts::persistAdapterFailureRecoveryClassification` 对齐：
//! - ProviderQuota → errorFamily=provider_quota + retryNotBefore/transientRetryNotBefore/
//!   providerQuotaRetryNotBefore/recoveryClassification=provider_quota + error_code=provider_quota
//! - ConfigurationIncomplete → errorFamily=configuration_incomplete +
//!   recoveryClassification=configuration_incomplete + error_code=configuration_incomplete
//! - 不存在的 run → return Ok(false)

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::adapter_failure_classification::AdapterFailureRecoveryClassification;
use pc_heartbeat::recovery::persist_adapter_failure_recovery_classification;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

async fn fixture(db: &Db) -> (Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r321-{company_id}"))
        .bind(format!("R{}", &company_id.simple().to_string()[..8]))
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r321-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id, run_id)
}

async fn insert_run(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    run_id: Uuid,
    error_code: &str,
    result_json: serde_json::Value,
) {
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error_code, \
         result_json, started_at, created_at) \
         VALUES ($1, $2, $3, 'failed', $4, $5, now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(error_code)
    .bind(result_json)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn fetch_run(db: &Db, run_id: Uuid) -> (Option<String>, Option<serde_json::Value>) {
    let row: (Option<String>, Option<serde_json::Value>) =
        sqlx::query_as("SELECT error_code, result_json FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    row
}

/// ProviderQuota 分类：result_json 写入 4 个 retry/quota 字段 + errorFamily + recoveryClassification
/// + error_code='provider_quota'，并保留已有 result_json 其他字段。
#[tokio::test]
async fn persists_provider_quota_classification_with_retry_fields() {
    let db = connect().await;
    let (company_id, agent_id, run_id) = fixture(&db).await;
    let retry_at = Utc::now() + Duration::hours(2);
    insert_run(
        &db,
        company_id,
        agent_id,
        run_id,
        "adapter_failed",
        json!({"foo": "bar", "nested": {"x": 1}}),
    )
    .await;

    let updated = persist_adapter_failure_recovery_classification(
        &db,
        company_id,
        run_id,
        AdapterFailureRecoveryClassification::ProviderQuota {
            retry_at,
            parsed_reset_time: true,
        },
    )
    .await
    .expect("persist should succeed");
    assert!(updated, "should report row updated");

    let (error_code, result_json) = fetch_run(&db, run_id).await;
    assert_eq!(error_code.as_deref(), Some("provider_quota"));
    let json = result_json.expect("result_json should be set");
    assert_eq!(json["errorFamily"], "provider_quota");
    assert_eq!(json["recoveryClassification"], "provider_quota");
    assert_eq!(
        json["retryNotBefore"],
        retry_at.to_rfc3339(),
        "retryNotBefore should equal classification.retry_at"
    );
    assert_eq!(json["transientRetryNotBefore"], retry_at.to_rfc3339());
    assert_eq!(json["providerQuotaRetryNotBefore"], retry_at.to_rfc3339());
    assert_eq!(json["foo"], "bar", "previous fields preserved");
    assert_eq!(json["nested"]["x"], 1);

    cleanup(&db, company_id).await;
}

/// ConfigurationIncomplete 分类：result_json 只写 errorFamily + recoveryClassification
/// + error_code='configuration_incomplete'，**不**写 retry 时间字段。
#[tokio::test]
async fn persists_configuration_incomplete_without_retry_fields() {
    let db = connect().await;
    let (company_id, agent_id, run_id) = fixture(&db).await;
    insert_run(
        &db,
        company_id,
        agent_id,
        run_id,
        "adapter_failed",
        json!({"some": "previous", "value": 42}),
    )
    .await;

    let updated = persist_adapter_failure_recovery_classification(
        &db,
        company_id,
        run_id,
        AdapterFailureRecoveryClassification::ConfigurationIncomplete,
    )
    .await
    .expect("persist should succeed");
    assert!(updated);

    let (error_code, result_json) = fetch_run(&db, run_id).await;
    assert_eq!(error_code.as_deref(), Some("configuration_incomplete"));
    let json = result_json.expect("result_json should be set");
    assert_eq!(json["errorFamily"], "configuration_incomplete");
    assert_eq!(json["recoveryClassification"], "configuration_incomplete");
    assert!(
        json.get("retryNotBefore").is_none(),
        "ConfigurationIncomplete must NOT write retryNotBefore"
    );
    assert!(
        json.get("transientRetryNotBefore").is_none(),
        "ConfigurationIncomplete must NOT write transientRetryNotBefore"
    );
    assert!(
        json.get("providerQuotaRetryNotBefore").is_none(),
        "ConfigurationIncomplete must NOT write providerQuotaRetryNotBefore"
    );
    assert_eq!(json["some"], "previous");
    assert_eq!(json["value"], 42);

    cleanup(&db, company_id).await;
}

/// 不存在的 run → 返回 Ok(false)，不抛错。
#[tokio::test]
async fn returns_false_when_run_missing() {
    let db = connect().await;
    let (company_id, _agent_id, run_id) = fixture(&db).await;
    // 不 insert run

    let updated = persist_adapter_failure_recovery_classification(
        &db,
        company_id,
        run_id,
        AdapterFailureRecoveryClassification::ConfigurationIncomplete,
    )
    .await
    .expect("persist should succeed (not error)");
    assert!(!updated, "missing run should return false");

    cleanup(&db, company_id).await;
}

/// 二次调用幂等：第二次 persist 同样的 classification 不引入新字段或修改保留字段。
#[tokio::test]
async fn second_persist_is_idempotent() {
    let db = connect().await;
    let (company_id, agent_id, run_id) = fixture(&db).await;
    let retry_at = Utc::now() + Duration::hours(1);
    insert_run(
        &db,
        company_id,
        agent_id,
        run_id,
        "adapter_failed",
        json!({"preserve": "me"}),
    )
    .await;

    let classification = AdapterFailureRecoveryClassification::ProviderQuota {
        retry_at,
        parsed_reset_time: true,
    };
    persist_adapter_failure_recovery_classification(
        &db,
        company_id,
        run_id,
        classification.clone(),
    )
    .await
    .unwrap();
    persist_adapter_failure_recovery_classification(&db, company_id, run_id, classification)
        .await
        .unwrap();

    let (_, result_json) = fetch_run(&db, run_id).await;
    let json = result_json.unwrap();
    assert_eq!(json["errorFamily"], "provider_quota");
    assert_eq!(json["recoveryClassification"], "provider_quota");
    assert_eq!(json["retryNotBefore"], retry_at.to_rfc3339());
    assert_eq!(json["preserve"], "me");

    cleanup(&db, company_id).await;
}

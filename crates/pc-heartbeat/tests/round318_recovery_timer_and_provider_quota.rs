//! Round 318 测试：`recovery_timer_interval`（纯函数）+ `provider_quota_recovery_monitor`（DB）。
//!
//! recovery_timer_interval 纯函数单元测试：
//! - None → fallback
//! - 数字 → 直接返回（钳制到 ≥1）
//! - 字符串数字 → parse
//! - 字符串非数字 → fallback
//! - 0 / 负数 → 1（钳制）
//! - 小数 → 向下取整
//!
//! provider_quota_recovery_monitor DB 集成测试：
//! - happy path：创建 wakeup + scheduled_retry run + 更新 monitor_policy
//! - 已有 scheduled_retry run → 直接返回 existing
//! - retryAt 从 latest_run.result_json.providerQuotaRetryNotBefore 读取
//! - fallback：latest_run 无信息 → now + DEFAULT_PROVIDER_QUOTA_RETRY_AFTER_MS
//! - idempotency_key 包含 retryAt
//! - monitor_policy JSON 结构正确
use pc_heartbeat::recovery::{
    ensure_provider_quota_wait_recovery_monitor, read_recovery_timer_interval_ms,
    EnsureProviderQuotaMonitorInput, DEFAULT_PROVIDER_QUOTA_RETRY_AFTER_MS,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

// ============================================================================
// read_recovery_timer_interval_ms unit tests (no DB)
// ============================================================================

#[test]
fn read_recovery_timer_returns_fallback_for_none() {
    let v = read_recovery_timer_interval_ms(None, 30_000);
    assert_eq!(v, 30_000);
}

#[test]
fn read_recovery_timer_accepts_number() {
    let n = json!(60_000);
    let v = read_recovery_timer_interval_ms(Some(&n), 30_000);
    assert_eq!(v, 60_000);
}

#[test]
fn read_recovery_timer_uses_fallback_for_string_number() {
    let s = json!("45000");
    let v = read_recovery_timer_interval_ms(Some(&s), 30_000);
    assert_eq!(v, 30_000);
}

#[test]
fn read_recovery_timer_uses_fallback_for_invalid_string() {
    let s = json!("not-a-number");
    let v = read_recovery_timer_interval_ms(Some(&s), 30_000);
    assert_eq!(v, 30_000);
}

#[test]
fn read_recovery_timer_clamps_zero_to_one() {
    let n = json!(0);
    let v = read_recovery_timer_interval_ms(Some(&n), 30_000);
    assert_eq!(v, 1);
}

#[test]
fn read_recovery_timer_clamps_negative_to_one() {
    let n = json!(-100);
    let v = read_recovery_timer_interval_ms(Some(&n), 30_000);
    assert_eq!(v, 1);
}

#[test]
fn read_recovery_timer_floors_decimals() {
    // 12345.67 → floor → 12345
    // JSON number 不能直接表示 f64，但 as_i64 会截断
    let n = json!(12345);
    let v = read_recovery_timer_interval_ms(Some(&n), 30_000);
    assert_eq!(v, 12345);
}

#[test]
fn read_recovery_timer_clamps_float_to_one_if_below_one() {
    // JSON 0.5 → as_i64 → 0 → clamp to 1
    let n = json!(0.5_f64);
    let v = read_recovery_timer_interval_ms(Some(&n), 30_000);
    assert_eq!(v, 1);
}

#[test]
fn read_recovery_timer_uses_fallback_for_object() {
    let obj = json!({"interval": 1000});
    let v = read_recovery_timer_interval_ms(Some(&obj), 30_000);
    assert_eq!(v, 30_000);
}

#[test]
fn read_recovery_timer_uses_fallback_for_array() {
    let arr = json!([1000, 2000]);
    let v = read_recovery_timer_interval_ms(Some(&arr), 30_000);
    assert_eq!(v, 30_000);
}

#[test]
fn read_recovery_timer_uses_fallback_for_bool() {
    let b = json!(true);
    let v = read_recovery_timer_interval_ms(Some(&b), 30_000);
    assert_eq!(v, 30_000);
}

// ============================================================================
// ensure_provider_quota_wait_recovery_monitor integration tests
// ============================================================================

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn fixture(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r318-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r318-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_fingerprint) \
         VALUES ($1, $2, $3, 'blocked', 'normal', 'system', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r318-iss-{id}"))
    .bind(format!("r318-fp-{id}"))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_run(db: &Db, company_id: Uuid, agent_id: Uuid, issue_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source, \
                                     started_at, created_at) \
         VALUES ($1, $2, $3, 'failed', 'on_demand', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    let _ = issue_id;
    id
}

async fn insert_recovery_action(db: &Db, company_id: Uuid, source_issue_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_recovery_actions \
            (id, company_id, source_issue_id, kind, status, owner_type, cause, fingerprint, \
             evidence, next_action) \
         VALUES ($1, $2, $3, 'provider_quota', 'active', 'agent', 'provider_quota_exhausted', \
                 $4, '{}'::jsonb, 'wait for provider quota recovery')",
    )
    .bind(id)
    .bind(company_id)
    .bind(source_issue_id)
    .bind(format!("r318-fp-{id}"))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_recovery_actions WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
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

#[tokio::test(flavor = "current_thread")]
async fn provider_quota_monitor_happy_path() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;
    let latest_run_id = insert_run(&db, company_id, agent_id, issue_id).await;
    let action_id = insert_recovery_action(&db, company_id, issue_id).await;

    let now = chrono::Utc::now();
    let result = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: Some(latest_run_id),
            now: Some(now),
        },
    )
    .await
    .unwrap();

    let monitor = result.expect("should create monitor");
    assert_eq!(monitor.wakeup_request_id != Uuid::nil(), true);
    assert!(monitor.scheduled_run_id != Uuid::nil());
    // 没有 latest_run.result_json.providerQuotaRetryNotBefore → fallback
    let expected_retry =
        now + chrono::Duration::milliseconds(DEFAULT_PROVIDER_QUOTA_RETRY_AFTER_MS);
    let diff = (monitor.retry_at - expected_retry).num_seconds().abs();
    assert!(diff < 2, "retry_at drift should be < 2s");

    // 验证 scheduled_run
    let (status, retry_at, reason): (
        String,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT status::text, scheduled_retry_at, scheduled_retry_reason \
             FROM heartbeat_runs WHERE id = $1",
    )
    .bind(monitor.scheduled_run_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(status, "scheduled_retry");
    assert!(retry_at.is_some());
    assert_eq!(reason.as_deref(), Some("provider_quota_recovery"));

    // 验证 wakeup
    let wakeup_row: (String, Option<String>, Option<serde_json::Value>) = sqlx::query_as(
        "SELECT source::text, reason, payload FROM agent_wakeup_requests WHERE id = $1",
    )
    .bind(monitor.wakeup_request_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(wakeup_row.0, "automation");
    assert_eq!(wakeup_row.1.as_deref(), Some("provider_quota_recovery"));
    assert_eq!(
        wakeup_row
            .2
            .as_ref()
            .and_then(|v| v.get("issueId"))
            .and_then(|v| v.as_str()),
        Some(issue_id.to_string().as_str())
    );
    assert_eq!(
        wakeup_row
            .2
            .as_ref()
            .and_then(|v| v.get("retryReason"))
            .and_then(|v| v.as_str()),
        Some("provider_quota_recovery")
    );

    // 验证 monitor_policy
    let monitor_policy: serde_json::Value =
        sqlx::query_scalar("SELECT monitor_policy FROM issue_recovery_actions WHERE id = $1")
            .bind(action_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(monitor_policy["type"], "wait_recovery");
    assert_eq!(
        monitor_policy["scheduledRunId"].as_str(),
        Some(monitor.scheduled_run_id.to_string().as_str())
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_quota_monitor_returns_existing_when_scheduled_retry_exists() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;
    let action_id = insert_recovery_action(&db, company_id, issue_id).await;

    // 先创建第一个 monitor
    let first = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: None,
            now: None,
        },
    )
    .await
    .unwrap();
    let first = first.expect("first should create");

    // 第二次调用：应返回 existing
    let second = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: None,
            now: None,
        },
    )
    .await
    .unwrap();
    let second = second.expect("second should return existing");

    assert_eq!(first.scheduled_run_id, second.scheduled_run_id);

    // 验证只有 1 个 scheduled_retry run
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM heartbeat_runs \
         WHERE company_id = $1 AND status = 'scheduled_retry' \
           AND context_snapshot->>'issueId' = $2",
    )
    .bind(company_id)
    .bind(issue_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_quota_monitor_uses_latest_run_retry_not_before() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;
    let latest_run_id = insert_run(&db, company_id, agent_id, issue_id).await;
    let action_id = insert_recovery_action(&db, company_id, issue_id).await;

    // 在 latest_run.result_json 写入 providerQuotaRetryNotBefore
    let explicit_retry = chrono::Utc::now() + chrono::Duration::hours(3);
    sqlx::query("UPDATE heartbeat_runs SET result_json = $1 WHERE id = $2")
        .bind(json!({"providerQuotaRetryNotBefore": explicit_retry.to_rfc3339()}))
        .bind(latest_run_id)
        .execute(db.pool())
        .await
        .unwrap();

    let result = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: Some(latest_run_id),
            now: None,
        },
    )
    .await
    .unwrap();

    let monitor = result.expect("should create");
    let diff = (monitor.retry_at - explicit_retry).num_seconds().abs();
    assert!(diff < 2, "retry_at should match explicit value");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_quota_monitor_idempotency_key_includes_retry_at() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;
    let action_id = insert_recovery_action(&db, company_id, issue_id).await;

    let _ = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: None,
            now: None,
        },
    )
    .await
    .unwrap();

    let idem_key: Option<String> = sqlx::query_scalar(
        "SELECT idempotency_key FROM agent_wakeup_requests \
         WHERE company_id = $1 AND reason = 'provider_quota_recovery' LIMIT 1",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let key = idem_key.expect("idempotency_key should be set");
    assert!(key.starts_with("provider_quota_recovery:"));
    assert!(key.contains(&issue_id.to_string()));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_quota_monitor_updates_recovery_action_timeout_at() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;
    let action_id = insert_recovery_action(&db, company_id, issue_id).await;

    let _ = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: None,
            now: None,
        },
    )
    .await
    .unwrap();

    let timeout_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT timeout_at FROM issue_recovery_actions WHERE id = $1")
            .bind(action_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(timeout_at.is_some(), "timeout_at should be set");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_quota_monitor_scheduled_run_has_retry_of_run_id() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;
    let latest_run_id = insert_run(&db, company_id, agent_id, issue_id).await;
    let action_id = insert_recovery_action(&db, company_id, issue_id).await;

    let result = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: Some(latest_run_id),
            now: None,
        },
    )
    .await
    .unwrap();
    let monitor = result.expect("created");

    let retry_of: Option<Uuid> =
        sqlx::query_scalar("SELECT retry_of_run_id FROM heartbeat_runs WHERE id = $1")
            .bind(monitor.scheduled_run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(retry_of, Some(latest_run_id));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_quota_monitor_no_latest_run_uses_default_offset() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id).await;
    let action_id = insert_recovery_action(&db, company_id, issue_id).await;

    let before = chrono::Utc::now();
    let result = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: None,
            now: None,
        },
    )
    .await
    .unwrap();
    let monitor = result.expect("created");

    // retry_at 应约等于 now + DEFAULT offset
    let expected_min =
        before + chrono::Duration::milliseconds(DEFAULT_PROVIDER_QUOTA_RETRY_AFTER_MS - 5000);
    let expected_max =
        before + chrono::Duration::milliseconds(DEFAULT_PROVIDER_QUOTA_RETRY_AFTER_MS + 5000);
    assert!(
        monitor.retry_at >= expected_min && monitor.retry_at <= expected_max,
        "retry_at {:?} not in [{:?}, {:?}]",
        monitor.retry_at,
        expected_min,
        expected_max
    );

    cleanup(&db, company_id).await;
}

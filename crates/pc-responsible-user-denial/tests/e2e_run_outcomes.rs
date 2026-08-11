//! R732: e2e for `pc-responsible-user-denial-run-outcomes` against real Postgres.

use pc_repos::Db;
use pc_responsible_user_denial::run_outcomes::{
    record_responsible_user_denial_on_active_run, RecordDenialInput,
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &PgPool, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    let suffix = Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(6)
        .collect::<String>();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R732-{tag}-{id}"))
    .bind(format!("R732{tag}-{suffix}"))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_agent(pool: &PgPool, company_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents \
            (id, company_id, name, role, status, adapter_type, adapter_config, \
             runtime_config, permissions, budget_monthly_cents, spent_monthly_cents, created_at, updated_at) \
         VALUES ($1, $2, $3, 'engineer', 'active', 'codex_local', '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, 0, 0, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("R732 agent {tag}"))
    .execute(pool)
    .await
    .expect("insert agent");
    id
}

async fn insert_heartbeat_run(
    pool: &PgPool,
    company_id: Uuid,
    agent_id: Uuid,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs \
            (id, company_id, agent_id, status, invocation_source, \
             created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'on_demand', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(status)
    .execute(pool)
    .await
    .expect("insert heartbeat_run");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn records_code_on_active_run() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "active").await;
    let agent_id = insert_agent(&pool, company_id, "active").await;
    let run_id = insert_heartbeat_run(&pool, company_id, agent_id, "queued").await;

    let outcome = record_responsible_user_denial_on_active_run(
        &db,
        RecordDenialInput {
            run_id: Some(run_id.to_string()),
            agent_id: Some(agent_id.to_string()),
            company_id: Some(company_id.to_string()),
            code: Some(json!("rate_limited")),
        },
    )
    .await
    .expect("record")
    .expect("some");

    assert_eq!(outcome.id, run_id);
    assert_eq!(outcome.company_id, company_id);
    assert_eq!(outcome.agent_id, agent_id);
    assert_eq!(outcome.error_code.as_deref(), Some("rate_limited"));

    // DB row 应该也被更新
    let stored: Option<String> =
        sqlx::query_scalar("SELECT error_code FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(&pool)
            .await
            .expect("select");
    assert_eq!(stored.as_deref(), Some("rate_limited"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn records_code_on_running_run() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "running").await;
    let agent_id = insert_agent(&pool, company_id, "running").await;
    let run_id = insert_heartbeat_run(&pool, company_id, agent_id, "running").await;

    let outcome = record_responsible_user_denial_on_active_run(
        &db,
        RecordDenialInput {
            run_id: Some(run_id.to_string()),
            agent_id: None,
            company_id: None,
            code: Some(json!("quota_exceeded")),
        },
    )
    .await
    .expect("record")
    .expect("some");
    assert_eq!(outcome.error_code.as_deref(), Some("quota_exceeded"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_none_for_terminal_run() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "term").await;
    let agent_id = insert_agent(&pool, company_id, "term").await;
    let run_id = insert_heartbeat_run(&pool, company_id, agent_id, "succeeded").await;

    let result = record_responsible_user_denial_on_active_run(
        &db,
        RecordDenialInput {
            run_id: Some(run_id.to_string()),
            agent_id: Some(agent_id.to_string()),
            company_id: Some(company_id.to_string()),
            code: Some(json!("rate_limited")),
        },
    )
    .await
    .expect("record");
    assert!(result.is_none());

    // DB row should NOT have been updated.
    let stored: (Option<String>,) =
        sqlx::query_as("SELECT error_code FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("select");
    assert!(stored.0.is_none());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn agent_id_mismatch_does_not_update() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "agm").await;
    let agent_a = insert_agent(&pool, company_id, "a").await;
    let agent_b = insert_agent(&pool, company_id, "b").await;
    let run_id = insert_heartbeat_run(&pool, company_id, agent_a, "queued").await;

    // 传入 agent_b 但 run 属于 agent_a → 不应更新
    let result = record_responsible_user_denial_on_active_run(
        &db,
        RecordDenialInput {
            run_id: Some(run_id.to_string()),
            agent_id: Some(agent_b.to_string()),
            company_id: Some(company_id.to_string()),
            code: Some(json!("rate_limited")),
        },
    )
    .await
    .expect("record");
    assert!(result.is_none());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn company_id_mismatch_does_not_update() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let co_a = insert_company(&pool, "ca").await;
    let co_b = insert_company(&pool, "cb").await;
    let agent_a = insert_agent(&pool, co_a, "a").await;
    let run_id = insert_heartbeat_run(&pool, co_a, agent_a, "queued").await;

    let result = record_responsible_user_denial_on_active_run(
        &db,
        RecordDenialInput {
            run_id: Some(run_id.to_string()),
            agent_id: Some(agent_a.to_string()),
            company_id: Some(co_b.to_string()),
            code: Some(json!("rate_limited")),
        },
    )
    .await
    .expect("record");
    assert!(result.is_none());

    cleanup(&pool, co_a).await;
    cleanup(&pool, co_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_none_for_unknown_code() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "unk").await;
    let agent_id = insert_agent(&pool, company_id, "unk").await;
    let run_id = insert_heartbeat_run(&pool, company_id, agent_id, "queued").await;

    let result = record_responsible_user_denial_on_active_run(
        &db,
        RecordDenialInput {
            run_id: Some(run_id.to_string()),
            agent_id: None,
            company_id: None,
            code: Some(json!("totally_made_up_code")),
        },
    )
    .await
    .expect("record");
    assert!(result.is_none());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_none_for_empty_run_id() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let result = record_responsible_user_denial_on_active_run(
        &db,
        RecordDenialInput {
            run_id: Some("".to_string()),
            agent_id: None,
            company_id: None,
            code: Some(json!("rate_limited")),
        },
    )
    .await
    .expect("record");
    assert!(result.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn returns_none_for_invalid_uuid() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let result = record_responsible_user_denial_on_active_run(
        &db,
        RecordDenialInput {
            run_id: Some("not-a-uuid".to_string()),
            agent_id: None,
            company_id: None,
            code: Some(json!("rate_limited")),
        },
    )
    .await
    .expect("record");
    assert!(result.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn returns_none_for_missing_code() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "nc").await;
    let agent_id = insert_agent(&pool, company_id, "nc").await;
    let run_id = insert_heartbeat_run(&pool, company_id, agent_id, "queued").await;

    let result = record_responsible_user_denial_on_active_run(
        &db,
        RecordDenialInput {
            run_id: Some(run_id.to_string()),
            agent_id: None,
            company_id: None,
            code: None,
        },
    )
    .await
    .expect("record");
    assert!(result.is_none());

    cleanup(&pool, company_id).await;
}

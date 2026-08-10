//! R611: pc-inbox e2e service tests (Postgres-backed).
//!
//! Validates:
//! - InboxService: dismiss / snooze / restore dispatch hooks correctly
//! - dismiss / snooze reject nil company + empty user_id / item_key
//! - snooze rejects past timestamps
//! - restore returns false + no hook when row didn't exist
//! - list_for_user / list_active_for_user / get / count_active
//! - InboxAgentPolicyService: get returns defaults (materialized=false) when missing
//! - update with allowlist + dedup + invalid agent id returns InvalidAgents error
//! - update with Open mode ignores allowed_agent_ids

use std::sync::Arc;

use chrono::{Duration, Utc};
use pc_core::Timestamp;
use pc_inbox::{
    InboxAgentPolicyService, InboxAgentPolicyMode, InboxHookEvent, InboxService,
    RecordingInboxHook, UpdateInboxAgentPolicyInput,
};
use pc_repos::Db;
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

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!("R{}", Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>());
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R611ct-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_agent(pool: &PgPool, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, status, role, created_at, updated_at)          VALUES ($1, $2, $3, 'active', 'worker', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("R611ag-{id}"))
    .execute(pool)
    .await
    .expect("insert agent");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM inbox_dismissals WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM user_inbox_agent_policies WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1").bind(company_id).execute(pool).await;
}

#[tokio::test(flavor = "current_thread")]
async fn dismiss_emits_dismissed_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingInboxHook::default());
    let svc = InboxService::with_hooks(db, vec![recorder.clone()]);

    let row = svc.dismiss(company_id, "u1", "issue-1").await.expect("dismiss");
    assert_eq!(row.user_id, "u1");
    assert_eq!(row.item_key, "issue-1");
    assert_eq!(row.kind, "dismiss");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], InboxHookEvent::Dismissed { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn dismiss_rejects_nil_company() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = InboxService::new(db);
    let res = svc.dismiss(Uuid::nil(), "u", "k").await;
    assert!(res.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn dismiss_rejects_empty_user_id() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = InboxService::new(db);
    let res = svc.dismiss(company_id, "", "k").await;
    assert!(res.is_err());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn snooze_emits_snoozed_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingInboxHook::default());
    let svc = InboxService::with_hooks(db, vec![recorder.clone()]);

    let until = Timestamp::from_dt(Utc::now() + Duration::hours(1));
    let row = svc.snooze(company_id, "u1", "issue-1", until).await.expect("snooze");
    assert_eq!(row.kind, "snooze");
    assert_eq!(row.snoozed_until, Some(until));

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        InboxHookEvent::Snoozed { snoozed_until, .. } => {
            assert_eq!(*snoozed_until, until);
        }
        _ => panic!("expected Snoozed"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn snooze_rejects_past_timestamp() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = InboxService::new(db);
    let past = Timestamp::from_dt(Utc::now() - Duration::hours(1));
    let res = svc.snooze(company_id, "u", "k", past).await;
    assert!(res.is_err());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn restore_emits_hook_only_on_actual_delete() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingInboxHook::default());
    let svc = InboxService::with_hooks(db, vec![recorder.clone()]);

    svc.dismiss(company_id, "u", "k").await.expect("dismiss");
    recorder.clear();

    let restored = svc.restore(company_id, "u", "k").await.expect("restore");
    assert!(restored);
    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], InboxHookEvent::Restored { .. }));

    // Second restore returns false and does NOT emit hook
    recorder.clear();
    let again = svc.restore(company_id, "u", "k").await.expect("restore again");
    assert!(!again);
    assert!(recorder.is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn list_for_user_returns_recent_first() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = InboxService::new(db);

    svc.dismiss(company_id, "u1", "a").await.expect("dismiss a");
    svc.dismiss(company_id, "u1", "b").await.expect("dismiss b");
    svc.dismiss(company_id, "u2", "c").await.expect("dismiss c");

    let rows = svc.list_for_user(company_id, "u1").await.expect("list");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.user_id == "u1"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn count_active_for_company_returns_zero_initially() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = InboxService::new(db);
    let n = svc.count_active(company_id, Timestamp::now()).await.expect("count");
    assert_eq!(n, 0);
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn inbox_policy_get_returns_defaults_when_missing() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = InboxAgentPolicyService::new(db);
    let policy = svc.get(company_id, "u1").await.expect("get");
    assert_eq!(policy.company_id, company_id);
    assert_eq!(policy.user_id, "u1");
    assert_eq!(policy.mode, InboxAgentPolicyMode::Open);
    assert!(policy.allowed_agent_ids.is_empty());
    assert!(!policy.materialized);
    assert!(policy.created_at.is_none());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn inbox_policy_update_emits_hook_with_allowed_count() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let a1 = insert_agent(&pool, company_id).await;
    let a2 = insert_agent(&pool, company_id).await;

    let recorder = Arc::new(RecordingInboxHook::default());
    let svc = InboxAgentPolicyService::with_hooks(db, vec![recorder.clone()]);

    let policy = svc
        .update(
            company_id,
            "u1",
            UpdateInboxAgentPolicyInput {
                mode: InboxAgentPolicyMode::Allowlist,
                allowed_agent_ids: vec![a1, a2, a1 /* dedup */],
            },
        )
        .await
        .expect("update");
    assert!(policy.materialized);
    assert_eq!(policy.allowed_agent_ids.len(), 2, "expected dedup to keep 2 ids");
    assert_eq!(policy.mode, InboxAgentPolicyMode::Allowlist);

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        InboxHookEvent::AgentPolicyUpdated { mode, allowed_count, .. } => {
            assert_eq!(*mode, InboxAgentPolicyMode::Allowlist);
            assert_eq!(*allowed_count, 2);
        }
        _ => panic!("expected AgentPolicyUpdated"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn inbox_policy_open_mode_ignores_allowed_ids() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let a1 = insert_agent(&pool, company_id).await;

    let svc = InboxAgentPolicyService::new(db);
    let policy = svc
        .update(
            company_id,
            "u1",
            UpdateInboxAgentPolicyInput {
                mode: InboxAgentPolicyMode::Open,
                allowed_agent_ids: vec![a1],
            },
        )
        .await
        .expect("update");
    assert_eq!(policy.mode, InboxAgentPolicyMode::Open);
    assert!(policy.allowed_agent_ids.is_empty(), "Open mode should drop ids");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn inbox_policy_update_rejects_invalid_agent_id() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let agent_b = insert_agent(&pool, company_b).await;

    let svc = InboxAgentPolicyService::new(db);
    let res = svc
        .update(
            company_a, // wrong company
            "u1",
            UpdateInboxAgentPolicyInput {
                mode: InboxAgentPolicyMode::Allowlist,
                allowed_agent_ids: vec![agent_b],
            },
        )
        .await;
    assert!(res.is_err(), "expected error for agent from other company");

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

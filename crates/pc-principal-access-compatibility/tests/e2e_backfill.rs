//! R723: e2e for `pc-principal-access-compatibility` against real Postgres.

use pc_principal_access_compatibility::{
    backfill_principal_access_compatibility, ensure_human_role_default_grants,
    insert_missing_principal_grants, GrantInput,
};
use pc_repos::Db;
use sqlx::PgPool;
use sqlx::Row;
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
    let prefix = format!("R723{}-{}", tag, Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>());
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R723-ct-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_user(pool: &PgPool, tag: &str) -> String {
    let id = format!("R723u-{tag}-{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) \
         VALUES ($1, $2, $3, false, now(), now())",
    )
    .bind(&id)
    .bind(format!("R723 user {tag}"))
    .bind(format!("{id}@paperclip.test"))
    .execute(pool)
    .await
    .expect("insert user");
    id
}

async fn insert_membership(pool: &PgPool, company_id: Uuid, user_id: &str, role: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_memberships (id, company_id, principal_type, principal_id, status, membership_role, created_at, updated_at) \
         VALUES ($1, $2, 'user', $3, 'active', $4, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(user_id)
    .bind(role)
    .execute(pool)
    .await
    .expect("insert membership");
    id
}

async fn insert_agent(pool: &PgPool, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, status, adapter_type, created_at, updated_at) \
         VALUES ($1, $2, $3, 'worker', $4, 'codex_local', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("R723-agent-{id}"))
    .bind(status)
    .execute(pool)
    .await
    .expect("insert agent");
    id
}

async fn count_grants(pool: &PgPool, company_id: Uuid, principal_id: &str) -> i64 {
    let row = sqlx::query(
        "SELECT COUNT(*)::bigint AS n FROM principal_permission_grants \
         WHERE company_id = $1 AND principal_type = 'user' AND principal_id = $2",
    )
    .bind(company_id)
    .bind(principal_id)
    .fetch_one(pool)
    .await
    .expect("count grants");
    row.get::<i64, _>("n")
}

async fn count_agent_memberships(pool: &PgPool, company_id: Uuid) -> i64 {
    let row = sqlx::query(
        "SELECT COUNT(*)::bigint AS n FROM company_memberships \
         WHERE company_id = $1 AND principal_type = 'agent'",
    )
    .bind(company_id)
    .fetch_one(pool)
    .await
    .expect("count agent memberships");
    row.get::<i64, _>("n")
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM principal_permission_grants WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
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
    let _ = sqlx::query("DELETE FROM \"user\" WHERE id LIKE 'R723u-%'")
        .execute(pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn insert_missing_principal_grants_upserts_idempotently() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "idemp").await;
    let user_id = insert_user(&pool, "idemp").await;
    insert_membership(&pool, company_id, &user_id, "owner").await;

    let input = pc_principal_access_compatibility::InsertGrantsInput {
        company_id,
        principal_type: "user".to_string(),
        principal_id: user_id.clone(),
        grants: vec![
            GrantInput { permission_key: "agents:create".to_string(), scope: None },
            GrantInput { permission_key: "agents:configure".to_string(), scope: None },
        ],
        granted_by_user_id: None,
    };
    let n1 = insert_missing_principal_grants(&db, input.clone()).await.expect("insert1");
    assert_eq!(n1, 2);
    let n2 = insert_missing_principal_grants(&db, input).await.expect("insert2");
    assert_eq!(n2, 0);
    assert_eq!(count_grants(&pool, company_id, &user_id).await, 2);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn ensure_human_role_default_grants_inserts_owner_set() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "owner").await;
    let user_id = insert_user(&pool, "owner").await;
    insert_membership(&pool, company_id, &user_id, "owner").await;

    let n = ensure_human_role_default_grants(
        &db,
        pc_principal_access_compatibility::EnsureHumanGrantsInput {
            company_id,
            principal_id: user_id.clone(),
            membership_role: Some("owner"),
            granted_by_user_id: None,
        },
    )
    .await
    .expect("grants");
    assert!(n >= 7, "owner should get many grants, got {n}");
    assert_eq!(count_grants(&pool, company_id, &user_id).await, n);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn backfill_inserts_agent_memberships_for_non_terminal_agents() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "agents").await;

    let a_idle = insert_agent(&pool, company_id, "idle").await;
    let a_paused = insert_agent(&pool, company_id, "paused").await;
    let a_terminated = insert_agent(&pool, company_id, "terminated").await;
    let a_pending = insert_agent(&pool, company_id, "pending_approval").await;

    let stats = backfill_principal_access_compatibility(&db).await.expect("backfill");
    // At minimum, our 2 non-terminal agents were inserted. (Other tests
    // in the suite may have left behind additional rows that the
    // backfill will also pick up.)
    assert!(stats.agent_memberships_inserted >= 2);

    let agent_count = count_agent_memberships(&pool, company_id).await;
    assert!(agent_count >= 2, "expected >=2 agent memberships, got {agent_count}");

    // Verify that the terminated and pending agents did NOT get memberships.
    let terminated_present: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM company_memberships \
         WHERE company_id = $1 AND principal_type = 'agent' AND principal_id = $2",
    )
    .bind(company_id)
    .bind(a_terminated.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    let pending_present: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM company_memberships \
         WHERE company_id = $1 AND principal_type = 'agent' AND principal_id = $2",
    )
    .bind(company_id)
    .bind(a_pending.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(terminated_present, 0);
    assert_eq!(pending_present, 0);

    // And the non-terminal ones are present.
    let idle_present: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM company_memberships \
         WHERE company_id = $1 AND principal_type = 'agent' AND principal_id = $2",
    )
    .bind(company_id)
    .bind(a_idle.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    let paused_present: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM company_memberships \
         WHERE company_id = $1 AND principal_type = 'agent' AND principal_id = $2",
    )
    .bind(company_id)
    .bind(a_paused.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(idle_present, 1);
    assert_eq!(paused_present, 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn backfill_is_idempotent() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "idem").await;
    let user_id = insert_user(&pool, "idem").await;
    insert_membership(&pool, company_id, &user_id, "operator").await;
    insert_agent(&pool, company_id, "idle").await;

    let s1 = backfill_principal_access_compatibility(&db).await.expect("backfill1");
    let s2 = backfill_principal_access_compatibility(&db).await.expect("backfill2");
    // Second pass should insert 0 new rows.
    assert_eq!(s2.agent_memberships_inserted, 0);
    assert_eq!(s2.human_grants_inserted, 0);
    // First pass should have done real work for our company.
    assert!(s1.agent_memberships_inserted + s1.human_grants_inserted > 0);

    cleanup(&pool, company_id).await;
}

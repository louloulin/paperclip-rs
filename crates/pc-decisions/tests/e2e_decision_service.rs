//! R587: DecisionService 真实 DB 端到端测试。

use std::sync::Arc;

use pc_decisions::{DecisionService, NoopDecisionHook, RecordingDecisionHook};
use pc_repos::Db;
use pc_secrets::DecisionSigningService;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn setup_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect to postgres")
}

async fn setup_signing() -> DecisionSigningService {
    DecisionSigningService::from_secret("0123456789abcdef0123456789abcdef")
        .expect("test signing secret")
}

async fn insert_company_with_agent_issue_run(db: &Db) -> Uuid {
    let company_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("dec-svc-{company_id}"))
    .bind(format!("DC{}", &company_id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");

    // create agent (required by decision.create FK)
    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent {agent_id}"))
    .execute(db.pool())
    .await
    .expect("insert agent");

    // create issue (required by decision.create FK)
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, 'Decision test', 'todo', 'medium', now(), now())",
    )
    .bind(issue_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert issue");

    // create heartbeat_run (required by decision.create FK)
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source, created_at, updated_at) \
         VALUES ($1, $2, $3, 'queued', 'manual_test', now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert run");

    company_id
}

#[tokio::test(flavor = "current_thread")]
async fn r587_decision_create_validates_inputs() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool);
    let signing = setup_signing().await;
    let svc = DecisionService::new(&db, &signing);
    let bad_company = Uuid::new_v4();

    let err1 = svc
        .create(bad_company, "", "body")
        .await
        .expect_err("empty title rejected");
    assert!(matches!(err1, pc_decisions::DecisionServiceError::InvalidInput(_)));

    let err2 = svc
        .create(bad_company, "title", "")
        .await
        .expect_err("empty body rejected");
    assert!(matches!(err2, pc_decisions::DecisionServiceError::InvalidInput(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn r587_decision_create_then_get() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool);
    let signing = setup_signing().await;
    let company_id = insert_company_with_agent_issue_run(&db).await;

    let svc = DecisionService::new(&db, &signing);
    let row = svc
        .create(company_id, "Test decision", "What to do?")
        .await
        .expect("create");
    assert_eq!(row.company_id, company_id);
    // repo 实际使用 "open" 作为初始 status（非 "pending"）
    assert!(matches!(row.status.as_str(), "open" | "pending"));

    let fetched = svc.get(row.id).await.expect("get").expect("found");
    assert_eq!(fetched.title, "Test decision");
}

#[tokio::test(flavor = "current_thread")]
async fn r587_decision_create_fires_on_created_hook() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool);
    let signing = setup_signing().await;
    let company_id = insert_company_with_agent_issue_run(&db).await;

    let recorder = Arc::new(RecordingDecisionHook::default());
    let svc = DecisionService::with_hooks(&db, &signing, vec![recorder.clone()]);
    assert_eq!(svc.hook_count(), 1);

    let row = svc
        .create(company_id, "Hook test", "Body")
        .await
        .expect("create");
    assert_eq!(recorder.created.lock().unwrap().len(), 1);
    assert_eq!(recorder.created.lock().unwrap()[0], row.id);
}

#[tokio::test(flavor = "current_thread")]
async fn r587_decision_decide_changes_status_and_fires_hook() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool);
    let signing = setup_signing().await;
    let company_id = insert_company_with_agent_issue_run(&db).await;

    let recorder = Arc::new(RecordingDecisionHook::default());
    let svc = DecisionService::with_hooks(&db, &signing, vec![recorder.clone()]);

    let row = svc
        .create(company_id, "Decide me", "Pick one")
        .await
        .expect("create");

    // decide 需要签名验证 — repo.create 已经签了名，应该能验签通过
    let decided = svc
        .decide(row.id, "opt-1", Some("user-1"), Some("looks good"), None)
        .await
        .expect("decide");
    assert_eq!(decided.status, "decided");
    assert_eq!(decided.chosen_option_id.as_deref(), Some("opt-1"));
    assert_eq!(decided.decided_by_user_id.as_deref(), Some("user-1"));
    assert_eq!(recorder.decided.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r587_decision_decide_with_tampered_signature_rejected() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool);
    let signing = setup_signing().await;
    let company_id = insert_company_with_agent_issue_run(&db).await;

    let svc = DecisionService::new(&db, &signing);
    let row = svc
        .create(company_id, "Tamper test", "Pick one")
        .await
        .expect("create");

    // 篡改 signed_spec
    sqlx::query("UPDATE decisions SET signed_spec = 'tampered' WHERE id = $1")
        .bind(row.id)
        .execute(db.pool())
        .await
        .expect("tamper");

    let err = svc
        .decide(row.id, "opt-1", Some("user-1"), None, None)
        .await
        .expect_err("tampered should be rejected");
    assert!(
        matches!(err, pc_decisions::DecisionServiceError::SignatureInvalid(_)),
        "expected SignatureInvalid, got {err:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn r587_decision_dismiss_changes_status_and_fires_hook() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool);
    let signing = setup_signing().await;
    let company_id = insert_company_with_agent_issue_run(&db).await;

    let recorder = Arc::new(RecordingDecisionHook::default());
    let svc = DecisionService::with_hooks(&db, &signing, vec![recorder.clone()]);

    let row = svc
        .create(company_id, "Dismiss me", "n/a")
        .await
        .expect("create");

    let dismissed = svc
        .dismiss(row.id, "not needed", "user-1")
        .await
        .expect("dismiss");
    assert_eq!(dismissed.status, "dismissed");
    assert_eq!(recorder.dismissed.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r587_decision_cancel_changes_status_and_fires_hook() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool);
    let signing = setup_signing().await;
    let company_id = insert_company_with_agent_issue_run(&db).await;

    let recorder = Arc::new(RecordingDecisionHook::default());
    let svc = DecisionService::with_hooks(&db, &signing, vec![recorder.clone()]);

    let row = svc
        .create(company_id, "Cancel me", "n/a")
        .await
        .expect("create");

    let cancelled = svc.cancel(row.id).await.expect("cancel");
    assert_eq!(cancelled.status, "cancelled");
    assert_eq!(recorder.cancelled.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r587_decision_list_by_company() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool);
    let signing = setup_signing().await;
    let company_id = insert_company_with_agent_issue_run(&db).await;

    let svc = DecisionService::new(&db, &signing);
    for i in 0..3 {
        svc
            .create(company_id, &format!("D{i}"), "body")
            .await
            .expect("create");
    }

    let all = svc.list_by_company(company_id).await.expect("list");
    assert_eq!(all.len(), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn r587_decision_get_returns_none_for_missing() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool);
    let signing = setup_signing().await;
    let svc = DecisionService::new(&db, &signing);

    let missing = Uuid::new_v4();
    let result = svc.get(missing).await.expect("ok");
    assert!(result.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn r587_decision_delete_returns_true_when_existing() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool);
    let signing = setup_signing().await;
    let company_id = insert_company_with_agent_issue_run(&db).await;

    let svc = DecisionService::new(&db, &signing);
    let row = svc
        .create(company_id, "Delete me", "x")
        .await
        .expect("create");
    let deleted = svc.delete(row.id).await.expect("delete");
    assert!(deleted, "should report deleted");

    let again = svc.delete(row.id).await.expect("delete 2nd");
    assert!(!again, "second delete returns false");
}

#[tokio::test(flavor = "current_thread")]
async fn r587_decision_noop_hook_does_not_block() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool);
    let signing = setup_signing().await;
    let company_id = insert_company_with_agent_issue_run(&db).await;

    let svc = DecisionService::with_hooks(&db, &signing, vec![Arc::new(NoopDecisionHook)]);
    let row = svc
        .create(company_id, "Noop test", "x")
        .await
        .expect("create");
    let dismissed = svc.dismiss(row.id, "x", "user-1").await.expect("dismiss");
    assert_eq!(dismissed.status, "dismissed");
}

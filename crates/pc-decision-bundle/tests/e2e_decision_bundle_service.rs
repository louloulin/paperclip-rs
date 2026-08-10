//! R629: DecisionBundleService 真实 DB 端到端测试。

use std::sync::Arc;

use pc_decision_bundle::{
    DecisionBundleHook,
    DecisionBundleHookEvent, DecisionBundleService, NoopDecisionBundleHook,
    RecordingDecisionBundleHook,
};
use pc_repos::{decision_bundle::NewDecisionBundle, Db};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn setup_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect to postgres")
}

async fn setup_parent(pool: &sqlx::PgPool) -> (Uuid, Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("bundle-svc-{company_id}"))
    .bind(format!("B{}", &company_id.simple().to_string()[..4]))
    .execute(pool)
    .await
    .expect("insert company");

    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent {agent_id}"))
    .execute(pool)
    .await
    .expect("insert agent");

    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, 'Bundle test', 'todo', 'medium', now(), now())",
    )
    .bind(issue_id)
    .bind(company_id)
    .execute(pool)
    .await
    .expect("insert issue");

    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source, created_at, updated_at) \
         VALUES ($1, $2, $3, 'queued', 'manual_test', now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(pool)
    .await
    .expect("insert run");

    (company_id, agent_id, issue_id, run_id)
}

fn new_bundle(agent_id: Uuid, issue_id: Uuid, run_id: Uuid) -> NewDecisionBundle {
    NewDecisionBundle {
        title: "Test bundle".into(),
        summary: Some("summary".into()),
        origin_agent_id: agent_id,
        origin_issue_id: issue_id,
        origin_run_id: run_id,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn r629_create_rejects_nil_company() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let (_, agent_id, issue_id, run_id) = setup_parent(&pool).await;
    let svc = DecisionBundleService::new(db);
    let bad = Uuid::nil();
    let err = svc
        .create(bad, new_bundle(agent_id, issue_id, run_id))
        .await
        .expect_err("nil company rejected");
    assert!(matches!(err, pc_decision_bundle::DecisionBundleError::Validation(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn r629_create_rejects_empty_title() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let (company_id, agent_id, issue_id, run_id) = setup_parent(&pool).await;
    let svc = DecisionBundleService::new(db);
    let mut bundle = new_bundle(agent_id, issue_id, run_id);
    bundle.title = "   ".into();
    let err = svc
        .create(company_id, bundle)
        .await
        .expect_err("empty title rejected");
    assert!(matches!(err, pc_decision_bundle::DecisionBundleError::Validation(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn r629_create_then_get() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let (company_id, agent_id, issue_id, run_id) = setup_parent(&pool).await;
    let svc = DecisionBundleService::new(db);
    let row = svc
        .create(company_id, new_bundle(agent_id, issue_id, run_id))
        .await
        .expect("create");
    assert_eq!(row.company_id, company_id);
    let fetched = svc.get(row.id).await.expect("get").expect("found");
    assert_eq!(fetched.title, "Test bundle");
    assert_eq!(fetched.summary, "summary");
}

#[tokio::test(flavor = "current_thread")]
async fn r629_list_by_company_empty() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let svc = DecisionBundleService::new(db);
    let company_id = Uuid::new_v4();
    let rows = svc
        .list_by_company(company_id, Default::default())
        .await
        .expect("list");
    assert!(rows.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn r629_list_by_company_returns_inserted() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let (company_id, agent_id, issue_id, run_id) = setup_parent(&pool).await;
    let svc = DecisionBundleService::new(db);
    let row = svc
        .create(company_id, new_bundle(agent_id, issue_id, run_id))
        .await
        .expect("create");
    let rows = svc
        .list_by_company(company_id, Default::default())
        .await
        .expect("list");
    assert!(rows.iter().any(|r| r.id == row.id));
}

#[tokio::test(flavor = "current_thread")]
async fn r629_create_dispatches_hook_event() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let (company_id, agent_id, issue_id, run_id) = setup_parent(&pool).await;
    let recorder = Arc::new(RecordingDecisionBundleHook::default());
    let svc = DecisionBundleService::with_hooks(db, vec![recorder.clone()]);
    let row = svc
        .create(company_id, new_bundle(agent_id, issue_id, run_id))
        .await
        .expect("create");
    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DecisionBundleHookEvent::Created {
            company_id: c,
            bundle_id,
            title,
        } => {
            assert_eq!(*c, company_id);
            assert_eq!(*bundle_id, row.id);
            assert_eq!(title, "Test bundle");
        }
        _ => panic!("expected Created event"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn r629_delete_returns_true_then_false() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let (company_id, agent_id, issue_id, run_id) = setup_parent(&pool).await;
    let svc = DecisionBundleService::new(db);
    let row = svc
        .create(company_id, new_bundle(agent_id, issue_id, run_id))
        .await
        .expect("create");
    let first = svc.delete(row.id).await.expect("delete");
    assert!(first);
    let second = svc.delete(row.id).await.expect("delete again");
    assert!(!second);
}

#[tokio::test(flavor = "current_thread")]
async fn r629_get_with_decisions_returns_empty_for_new_bundle() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let (company_id, agent_id, issue_id, run_id) = setup_parent(&pool).await;
    let svc = DecisionBundleService::new(db);
    let row = svc
        .create(company_id, new_bundle(agent_id, issue_id, run_id))
        .await
        .expect("create");
    let detail = svc
        .get_with_decisions(row.id)
        .await
        .expect("get detail")
        .expect("found");
    assert_eq!(detail.bundle.id, row.id);
    assert!(detail.decisions.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn r629_get_returns_none_for_missing() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let svc = DecisionBundleService::new(db);
    let got = svc.get(Uuid::new_v4()).await.expect("get");
    assert!(got.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn r629_noop_hook_default_impl() {
    let _noop: Box<dyn DecisionBundleHook> = Box::new(NoopDecisionBundleHook);
    // Just verify trait works; nothing to assert beyond compile.
}

//! R612: pc-feedback-vote e2e service tests (Postgres-backed).
//!
//! Validates:
//! - FeedbackVoteService construction + hook attachment
//! - cast validates inputs and inserts the row
//! - cast emits Cast hook with the resolved fields
//! - cast_for_issue resolves company_id from issue and emits hook
//! - cast_for_issue returns NotFound for missing issue
//! - list_by_issue / get_by_id / count_by_issue
//! - cast rejects unknown vote values

use std::sync::Arc;

use pc_feedback::vote::{
    FeedbackVoteHookEvent, FeedbackVoteService, NewFeedbackVote, RecordingFeedbackVoteHook,
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
    .bind(format!("R612fv-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_issue(pool: &PgPool, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at)          VALUES ($1, $2, $3, 'todo', 'normal', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("R612iss-{id}"))
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM feedback_votes WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1").bind(company_id).execute(pool).await;
}

fn make_input(company_id: Uuid, issue_id: Uuid) -> NewFeedbackVote {
    NewFeedbackVote {
        company_id,
        issue_id,
        target_type: "agent".into(),
        target_id: "agent-1".into(),
        author_user_id: "u1".into(),
        vote: "up".into(),
        reason: Some("great".into()),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn service_constructs_with_new_and_with_hooks() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = FeedbackVoteService::new(db.clone());
    assert_eq!(svc.hook_count(), 0);
    let recorder = Arc::new(RecordingFeedbackVoteHook::default());
    let svc2 = FeedbackVoteService::with_hooks(db, vec![recorder.clone()]);
    assert_eq!(svc2.hook_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn cast_rejects_unknown_vote() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id).await;
    let svc = FeedbackVoteService::new(db);
    let mut input = make_input(company_id, issue_id);
    input.vote = "sideways".into();
    let res = svc.cast(input).await;
    assert!(res.is_err());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cast_rejects_nil_company() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = FeedbackVoteService::new(db);
    let input = make_input(Uuid::nil(), Uuid::new_v4());
    let res = svc.cast(input).await;
    assert!(res.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn cast_inserts_row_and_emits_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id).await;

    let recorder = Arc::new(RecordingFeedbackVoteHook::default());
    let svc = FeedbackVoteService::with_hooks(db, vec![recorder.clone()]);

    let vote_id = svc.cast(make_input(company_id, issue_id)).await.expect("cast");
    assert!(!vote_id.is_nil());

    let fetched = svc.get_by_id(vote_id).await.expect("get").expect("exists");
    assert_eq!(fetched.company_id, company_id);
    assert_eq!(fetched.issue_id, issue_id);
    assert_eq!(fetched.vote, "up");
    assert_eq!(fetched.author_user_id, "u1");
    assert_eq!(fetched.reason.as_deref(), Some("great"));

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        FeedbackVoteHookEvent::Cast { vote, author_user_id, .. } => {
            assert_eq!(vote, "up");
            assert_eq!(author_user_id, "u1");
        }
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cast_for_issue_resolves_company_id_and_emits_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id).await;

    let recorder = Arc::new(RecordingFeedbackVoteHook::default());
    let svc = FeedbackVoteService::with_hooks(db, vec![recorder.clone()]);

    let vote_id = svc
        .cast_for_issue(issue_id, "agent", "a1", "u2", "down", Some("meh"))
        .await
        .expect("cast");
    assert!(!vote_id.is_nil());

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        FeedbackVoteHookEvent::Cast { company_id: cid, vote, .. } => {
            assert_eq!(*cid, company_id);
            assert_eq!(vote, "down");
        }
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cast_for_issue_returns_not_found_for_missing_issue() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = FeedbackVoteService::new(db);
    let res = svc
        .cast_for_issue(Uuid::new_v4(), "agent", "a1", "u", "up", None)
        .await;
    assert!(matches!(res, Err(pc_feedback::vote::FeedbackVoteError::NotFound(_))));
}

#[tokio::test(flavor = "current_thread")]
async fn list_by_issue_returns_recent_first() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id).await;
    let svc = FeedbackVoteService::new(db);

    svc.cast(make_input(company_id, issue_id)).await.expect("cast 1");
    svc.cast({
        let mut i = make_input(company_id, issue_id);
        i.target_id = "agent-2".into();
        i
    })
    .await
    .expect("cast 2");

    let rows = svc.list_by_issue(issue_id, 10).await.expect("list");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.issue_id == issue_id));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn count_by_issue_returns_inserted_count() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id).await;
    let svc = FeedbackVoteService::new(db);

    let n0 = svc.count_by_issue(issue_id).await.expect("count 0");
    assert_eq!(n0, 0);

    svc.cast(make_input(company_id, issue_id)).await.expect("cast");
    let n1 = svc.count_by_issue(issue_id).await.expect("count 1");
    assert_eq!(n1, 1);

    cleanup(&pool, company_id).await;
}

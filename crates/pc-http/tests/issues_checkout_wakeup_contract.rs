//! `/api/issues/:id/checkout` + `/wakeup` 路由契约测试。

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    routes,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;
use uuid::Uuid;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

fn test_state(db: Db) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    AppState::new(
        db.clone(),
        RuntimeHandles {
            heartbeat: spawn_heartbeat_supervisor(4, actors.clone()),
            agents: pc_agent::spawn_agent_supervisor(db),
            adapters: AdapterRegistry::new(),
            actors,
        },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 3100,
            session_cookie: "paperclip_session".into(),
            api_key_header: "x-paperclip-agent-key".into(),
            csrf_header: "x-paperclip-csrf".into(),
        },
        pc_telemetry::TelemetryOptions::default(),
        Arc::new(WsState {
            realtime: realtime.clone(),
            server_name: "test".into(),
        }),
        realtime,
    )
}

async fn insert_company(db: &Db) -> Uuid {
    let company_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("checkout-test-{company_id}"))
    .bind(format!("CT{}", &company_id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");
    company_id
}

async fn insert_user(db: &Db, user_id: &str) {
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) \
         VALUES ($1, $2, $3, true, now(), now()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(user_id)
    .bind(format!("User {user_id}"))
    .bind(format!("{user_id}@example.com"))
    .execute(db.pool())
    .await
    .expect("insert user");
}

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, runtime_config, permissions, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent {agent_id}"))
    .execute(db.pool())
    .await
    .expect("insert agent");
    agent_id
}

async fn insert_issue(db: &Db, company_id: Uuid, assignee_agent_id: Option<Uuid>) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, assignee_agent_id, created_at, updated_at) \
         VALUES ($1, $2, $3, 'todo', 'medium', $4, now(), now())",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind("Checkout test issue")
    .bind(assignee_agent_id)
    .execute(db.pool())
    .await
    .expect("insert issue");
    issue_id
}

async fn insert_session(db: &Db, user_id: &str) -> String {
    let token = format!("sess_co_{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO session (id, expires_at, token, created_at, updated_at, user_id) \
         VALUES ($1, now() + interval '1 hour', $2, now(), now(), $3) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(format!("sess-co-{user_id}-{}", Uuid::new_v4().simple()))
    .bind(&token)
    .bind(user_id)
    .execute(db.pool())
    .await
    .expect("insert session");
    token
}

async fn insert_heartbeat_run(db: &Db, company_id: Uuid, agent_id: Uuid) -> Uuid {
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source) \
         VALUES ($1, $2, $3, 'queued', 'manual_test') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert heartbeat_run");
    run_id
}

async fn setup_session(db: &Db, user_id: &str) -> String {
    insert_user(db, user_id).await;
    insert_session(db, user_id).await
}

async fn call_with_session(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    session_token: &str,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let payload = body
        .as_ref()
        .map(|v| serde_json::to_vec(v).expect("serialize"))
        .unwrap_or_default();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {session_token}"))
                .uri(path)
                .body(Body::from(payload))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, payload)
}

#[tokio::test(flavor = "current_thread")]
async fn issue_checkout_persists_run_id_and_creates_lock_and_queues_wakeup() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let issue_id = insert_issue(&db, company_id, Some(agent_id)).await;
    let session = setup_session(&db, "board-user-co").await;

    let app = routes::issues_checkout_wakeup::router().with_state(test_state(db.clone()));

    let run_id = insert_heartbeat_run(&db, company_id, agent_id).await;
    let (status, body) = call_with_session(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/checkout"),
        Some(json!({ "runId": run_id, "actorType": "board", "strategy": "merge" })),
        &session,
    )
    .await;
    assert_eq!(status, 200, "checkout: {body}");
    assert_eq!(body["status"], "checked-out");
    assert_eq!(body["runId"], run_id.to_string());
    assert_eq!(body["wakeupQueued"], true);

    let stored: (Option<Uuid>,) =
        sqlx::query_as("SELECT checkout_run_id FROM issues WHERE id = $1")
            .bind(issue_id)
            .fetch_one(db.pool())
            .await
            .expect("load checkout_run_id");
    assert_eq!(stored.0, Some(run_id));

    let lock_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM issue_checkout_locks WHERE issue_id = $1 AND run_id = $2",
    )
    .bind(issue_id)
    .bind(run_id)
    .fetch_one(db.pool())
    .await
    .expect("count locks");
    assert_eq!(lock_count, 1);

    let wake_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent_wakeup_requests \
         WHERE agent_id = $1 AND payload->>'issueId' = $2",
    )
    .bind(agent_id)
    .bind(&issue_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("count wakeups");
    assert!(wake_count >= 1, "wakeup queued");
}

#[tokio::test(flavor = "current_thread")]
async fn issue_wakeup_endpoint_queues_request_without_checkout() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let issue_id = insert_issue(&db, company_id, Some(agent_id)).await;
    let session = setup_session(&db, "board-user-wu").await;

    let app = routes::issues_checkout_wakeup::router().with_state(test_state(db.clone()));

    let (status, body) = call_with_session(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/wakeup"),
        None,
        &session,
    )
    .await;
    assert_eq!(status, 202, "wakeup: {body}");
    assert_eq!(body["issueId"], issue_id.to_string());
    assert_eq!(body["status"], "wakeup-queued");

    let wake_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent_wakeup_requests \
         WHERE agent_id = $1 AND source = 'issue_wakeup'",
    )
    .bind(agent_id)
    .fetch_one(db.pool())
    .await
    .expect("count wakeups");
    assert_eq!(wake_count, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn issue_checkout_404_when_issue_missing() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let _run_id = insert_heartbeat_run(&db, company_id, agent_id).await;
    let session = setup_session(&db, "board-user-404").await;
    let app = routes::issues_checkout_wakeup::router().with_state(test_state(db.clone()));

    let missing = Uuid::new_v4();
    let (status, body) = call_with_session(
        &app,
        "POST",
        &format!("/api/issues/{missing}/checkout"),
        Some(json!({ "runId": _run_id })),
        &session,
    )
    .await;
    assert_eq!(status, 404, "checkout missing: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn issue_checkout_handles_existing_lock_gracefully() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let issue_id = insert_issue(&db, company_id, Some(agent_id)).await;
    let existing_run = insert_heartbeat_run(&db, company_id, agent_id).await;
    sqlx::query(
        "UPDATE issues SET checkout_run_id = $1, execution_locked_at = now() WHERE id = $2",
    )
    .bind(existing_run)
    .bind(issue_id)
    .execute(db.pool())
    .await
    .expect("seed lock");
    sqlx::query(
        "INSERT INTO issue_checkout_locks (issue_id, run_id, actor_type, strategy, status) \
         VALUES ($1, $2, 'board', 'merge', 'active')",
    )
    .bind(issue_id)
    .bind(existing_run)
    .execute(db.pool())
    .await
    .expect("seed lock row");

    let session = setup_session(&db, "board-user-409").await;
    let app = routes::issues_checkout_wakeup::router().with_state(test_state(db.clone()));

    let new_run = insert_heartbeat_run(&db, company_id, agent_id).await;
    let (status, _body) = call_with_session(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/checkout"),
        Some(json!({ "runId": new_run, "actorType": "board" })),
        &session,
    )
    .await;
    assert!(
        status == 200 || status == 409,
        "checkout with stale run_id: status={status}"
    );
}

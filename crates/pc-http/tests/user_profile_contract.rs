//! 用户资料(/profile)路由契约测试。

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
        Arc::new(WsState::new(realtime.clone(), "test".to_string())),
        realtime,
    )
}

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("up-{id}"))
    .bind(format!("UP{}", &id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_user(db: &Db, user_id: &str, name: &str, email: &str) {
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) \
         VALUES ($1, $2, $3, true, now(), now()) ON CONFLICT (id) DO NOTHING",
    )
    .bind(user_id)
    .bind(name)
    .bind(email)
    .execute(db.pool())
    .await
    .expect("insert user");
}

async fn insert_membership(db: &Db, company_id: Uuid, user_id: &str) {
    sqlx::query(
        "INSERT INTO company_memberships (company_id, principal_type, principal_id, status, membership_role, created_at, updated_at) \
         VALUES ($1, 'user', $2, 'active', 'member', now(), now()) \
         ON CONFLICT (company_id, principal_type, principal_id) DO NOTHING",
    )
    .bind(company_id)
    .bind(user_id)
    .execute(db.pool())
    .await
    .expect("insert membership");
}

async fn insert_session(db: &Db, user_id: &str) -> String {
    let token = format!("sess_up_{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO session (id, expires_at, token, created_at, updated_at, user_id) \
         VALUES ($1, now() + interval '1 hour', $2, now(), now(), $3) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(format!("sess-up-{user_id}-{}", Uuid::new_v4().simple()))
    .bind(&token)
    .bind(user_id)
    .execute(db.pool())
    .await
    .expect("insert session");
    token
}

async fn call_with_session(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    session: &str,
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
                .header("authorization", format!("Bearer {session}"))
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

async fn call(app: &axum::Router, method: &str, path: &str) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .uri(path)
                .body(Body::empty())
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
async fn profile_requires_authentication() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::user_profiles::router().with_state(test_state(db));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/users/some-user/profile"),
    )
    .await;
    assert!(
        status == 401 || status == 403,
        "should be auth challenge, got {status}: {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn profile_returns_identity_and_windows_for_existing_membership() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let slug = "alicecooper".to_string();
    let user_id = format!("user-up-{}", Uuid::new_v4().simple());
    insert_user(&db, &user_id, "AliceCooper", "alice@example.com").await;
    insert_membership(&db, company_id, &user_id).await;
    let session = insert_session(&db, &user_id).await;
    let app = routes::user_profiles::router().with_state(test_state(db));

    let (status, body) = call_with_session(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/users/{slug}/profile"),
        None,
        &session,
    )
    .await;
    assert_eq!(status, 200, "profile: {body}");
    assert_eq!(body["user"]["id"], user_id);
    assert!(!body["user"]["slug"].is_null(), "slug present: {body}");
    // Stats windows: last7 / last30 / all
    let stats = body["stats"].as_array().expect("stats array");
    assert_eq!(stats.len(), 3, "expected 3 windows: {body}");
    let keys: Vec<&str> = stats
        .iter()
        .map(|s| s["key"].as_str().unwrap_or(""))
        .collect();
    assert!(keys.contains(&"last7"));
    assert!(keys.contains(&"last30"));
    assert!(keys.contains(&"all"));
    // Empty company → all counters zero
    assert_eq!(body["recentIssues"].as_array().unwrap().len(), 0);
    assert_eq!(body["recentActivity"].as_array().unwrap().len(), 0);
    assert_eq!(body["topAgents"].as_array().unwrap().len(), 0);
    assert_eq!(body["topProviders"].as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn profile_returns_404_for_missing_membership() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let user_id = format!("user-up-floating-{}", Uuid::new_v4().simple());
    insert_user(&db, &user_id, "Eve Outsider", "eve@example.com").await;
    let session = insert_session(&db, &user_id).await;
    let app = routes::user_profiles::router().with_state(test_state(db));

    let (status, body) = call_with_session(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/users/unknown-slug/profile"),
        None,
        &session,
    )
    .await;
    assert_eq!(status, 404, "expected 404: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn profile_resolves_by_email_slug_fallback() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let email_handle = "bobbuilder".to_string();
    let user_id = format!("user-up-{email_handle}");
    insert_user(
        &db,
        &user_id,
        "Bob Builder",
        &format!("{email_handle}@example.com"),
    )
    .await;
    insert_membership(&db, company_id, &user_id).await;
    let session = insert_session(&db, &user_id).await;
    let app = routes::user_profiles::router().with_state(test_state(db));

    // Look up by email local part (before @) which slugifies
    let (status, body) = call_with_session(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/users/{email_handle}/profile"),
        None,
        &session,
    )
    .await;
    assert_eq!(status, 200, "profile by email slug: {body}");
    assert_eq!(body["user"]["email"], format!("{email_handle}@example.com"));
    assert_eq!(body["user"]["id"], user_id);
}

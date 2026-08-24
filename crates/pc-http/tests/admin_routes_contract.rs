//! `/api/admin/*` 路由契约测试 (R800)。
//!
//! 测试 V6 admin routes 与 TypeScript server 的格式兼容：
//! - GET  /api/admin/users                     → flat array
//! - GET  /api/admin/users/:user_id/company-access
//! - PUT  /api/admin/users/:user_id/company-access
//! - POST /api/admin/users/:user_id/promote-instance-admin
//! - POST /api/admin/users/:user_id/demote-instance-admin

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
use pc_db::Migrator;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;
use uuid::Uuid;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn ensure_migrated(db: &Db) {
    if let Err(e) = Migrator::run(db).await {
        eprintln!("migration note: {e}");
    }
}

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

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let payload = body
        .as_ref()
        .map(|v| serde_json::to_vec(v).expect("serialize"))
        .unwrap_or_default();
    let mut builder = Request::builder()
        .method(method)
        .header("content-type", "application/json")
        .uri(path);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(payload)).expect("request"))
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

/// 签入并返回 session token。
async fn sign_in(app: &axum::Router, email: &str) -> String {
    let (status, body) = call(
        app,
        "POST",
        "/api/auth/sign-in",
        Some(json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, 200, "sign-in: {body}");
    body["session_token"].as_str().expect("session_token").to_string()
}

/// 授予 instance_admin 角色（绕过 auth 做 setup）。
async fn make_admin(db: &Db, user_id: &str) {
    sqlx::query(
        "INSERT INTO instance_user_roles (user_id, role) VALUES ($1, 'instance_admin') \
         ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .execute(db.pool())
    .await
    .expect("make admin");
}

// ── GET /api/admin/users ────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r800_admin_users_returns_flat_array() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));

    let email = format!("admin-list-test-{}@example.com", Uuid::new_v4().simple());
    let token = sign_in(&app, &email).await;

    let (status, body) = call(&app, "GET", "/api/admin/users", None, Some(&token)).await;
    assert_eq!(status, 200, "admin list: {body}");
    // TypeScript returns a flat array, not {items, count}
    assert!(
        body.is_array(),
        "admin users should return flat array, got: {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn r800_admin_users_includes_is_instance_admin_and_membership_count() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));

    let email = format!("admin-fields-test-{}@example.com", Uuid::new_v4().simple());
    let (sign_status, sign_body) = call(
        &app,
        "POST",
        "/api/auth/sign-in",
        Some(json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(sign_status, 200);
    let token = sign_body["session_token"].as_str().expect("token");
    let user_id = sign_body["user_id"].as_str().expect("user_id");

    // Make this user an admin
    make_admin(&db, user_id).await;

    let (status, body) = call(&app, "GET", "/api/admin/users", None, Some(&token)).await;
    assert_eq!(status, 200, "admin list: {body}");
    let items = body.as_array().expect("array response");

    // Find this user in the list
    let me = items
        .iter()
        .find(|item| item["id"].as_str() == Some(user_id))
        .expect("user should be in list");
    assert!(
        me["isInstanceAdmin"].as_bool().unwrap_or(false),
        "current user should be instance admin"
    );
    assert!(
        me["activeCompanyMembershipCount"].is_number(),
        "activeCompanyMembershipCount should be present"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn r800_admin_users_query_filter() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));

    let email = format!("admin-filter-test-{}@example.com", Uuid::new_v4().simple());
    let token = sign_in(&app, &email).await;

    // Filter by non-matching query
    let (status, body) = call(
        &app,
        "GET",
        "/api/admin/users?query=nonexistent-email-xyz",
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "filtered admin list: {body}");
    let items = body.as_array().expect("array response");
    assert_eq!(
        items.len(), 0,
        "filter should return empty for non-matching query"
    );
}

// ── POST /api/admin/users/:user_id/promote-instance-admin ─────────────────

#[tokio::test(flavor = "current_thread")]
async fn r800_promote_returns_userid_role_createdat() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));

    // Admin user
    let admin_email = format!("promote-admin-{}@example.com", Uuid::new_v4().simple());
    let admin_token = sign_in(&app, &admin_email).await;
    let admin_id = {
        let (s, b) = call(&app, "GET", "/api/auth/get-session", None, Some(&admin_token)).await;
        assert_eq!(s, 200, "get-session: {b}");
        b["session"]["user_id"].as_str().unwrap().to_string()
    };
    make_admin(&db, &admin_id).await;

    // Non-admin user to promote
    let target_email = format!("promote-target-{}@example.com", Uuid::new_v4().simple());
    let (_, target_body) = call(
        &app,
        "POST",
        "/api/auth/sign-in",
        Some(json!({ "email": target_email })),
        None,
    )
    .await;
    let target_id = target_body["user_id"].as_str().expect("target user_id");

    let path = format!("/api/admin/users/{target_id}/promote-instance-admin");
    let (status, body) = call(&app, "POST", &path, None, Some(&admin_token)).await;
    assert_eq!(status, 200, "promote: {body}");

    // TypeScript returns { userId, role, createdAt }
    assert_eq!(body["userId"], target_id, "should return target userId");
    assert_eq!(
        body["role"].as_str(),
        Some("instance_admin"),
        "role should be instance_admin"
    );
    assert!(
        body["createdAt"].is_string() || body["createdAt"].is_null(),
        "createdAt should be present (ISO string or null)"
    );
}

// ── POST /api/admin/users/:user_id/demote-instance-admin ───────────────────

#[tokio::test(flavor = "current_thread")]
async fn r800_demote_returns_userid_role_createdat() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));

    // Admin user
    let admin_email = format!("demote-admin-{}@example.com", Uuid::new_v4().simple());
    let admin_token = sign_in(&app, &admin_email).await;
    let admin_id = {
        let (s, b) = call(&app, "GET", "/api/auth/get-session", None, Some(&admin_token)).await;
        assert_eq!(s, 200, "get-session: {b}");
        b["session"]["user_id"].as_str().unwrap().to_string()
    };
    make_admin(&db, &admin_id).await;

    // Promote then demote
    let path = format!("/api/admin/users/{admin_id}/promote-instance-admin");
    let (s, _) = call(&app, "POST", &path, None, Some(&admin_token)).await;
    assert_eq!(s, 200, "promote should succeed");

    let demote_path = format!("/api/admin/users/{admin_id}/demote-instance-admin");
    let (status, body) = call(&app, "POST", &demote_path, None, Some(&admin_token)).await;
    assert_eq!(status, 200, "demote: {body}");

    // TypeScript returns { userId, role, createdAt } from deleted row
    assert_eq!(body["userId"].as_str(), Some(admin_id.as_str()), "should return userId");
    assert_eq!(
        body["role"].as_str(),
        Some("instance_admin"),
        "role should be instance_admin"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn r800_demote_nonexistent_returns_404() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));

    let admin_email = format!("demote-404-admin-{}@example.com", Uuid::new_v4().simple());
    let admin_token = sign_in(&app, &admin_email).await;
    let admin_id = {
        let (s, b) = call(&app, "GET", "/api/auth/get-session", None, Some(&admin_token)).await;
        assert_eq!(s, 200, "get-session: {b}");
        b["session"]["user_id"].as_str().unwrap().to_string()
    };
    make_admin(&db, &admin_id).await;

    // Try to demote a non-admin user
    let fake_id = Uuid::new_v4().to_string();
    let path = format!("/api/admin/users/{fake_id}/demote-instance-admin");
    let (status, body) = call(&app, "POST", &path, None, Some(&admin_token)).await;
    assert_eq!(status, 404, "demote non-admin should return 404: {body}");
}

// ── GET /api/admin/users/:user_id/company-access ─────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r800_company_access_returns_user_and_companyaccess() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));

    let admin_email = format!("company-access-admin-{}@example.com", Uuid::new_v4().simple());
    let admin_token = sign_in(&app, &admin_email).await;
    let admin_id = {
        let (s, b) = call(&app, "GET", "/api/auth/get-session", None, Some(&admin_token)).await;
        assert_eq!(s, 200, "get-session: {b}");
        b["session"]["user_id"].as_str().unwrap().to_string()
    };
    make_admin(&db, &admin_id).await;

    let path = format!("/api/admin/users/{admin_id}/company-access");
    let (status, body) = call(&app, "GET", &path, None, Some(&admin_token)).await;
    assert_eq!(status, 200, "company access: {body}");

    // TypeScript returns { user: {...}, companyAccess: [...] }
    assert!(
        body["user"].is_object(),
        "should have 'user' field: {body}"
    );
    assert!(
        body["companyAccess"].is_array(),
        "should have 'companyAccess' field: {body}"
    );
    let user = &body["user"];
    assert_eq!(user["id"].as_str(), Some(admin_id.as_str()), "user id should match");
}

// ── PUT /api/admin/users/:user_id/company-access ────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn r800_put_company_access_returns_same_format_as_get() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect db");
    ensure_migrated(&db).await;
    let app = routes::router().with_state(test_state(db.clone()));

    let admin_email = format!("put-company-access-admin-{}@example.com", Uuid::new_v4().simple());
    let admin_token = sign_in(&app, &admin_email).await;
    let admin_id = {
        let (s, b) = call(&app, "GET", "/api/auth/get-session", None, Some(&admin_token)).await;
        assert_eq!(s, 200, "get-session: {b}");
        b["session"]["user_id"].as_str().unwrap().to_string()
    };
    make_admin(&db, &admin_id).await;

    // PUT with empty company list
    let put_path = format!("/api/admin/users/{admin_id}/company-access");
    let (status, body) = call(
        &app,
        "PUT",
        &put_path,
        Some(json!({ "companyIds": [] })),
        Some(&admin_token),
    )
    .await;
    assert_eq!(status, 200, "put company access: {body}");

    // TypeScript returns same format as GET
    assert!(
        body["user"].is_object(),
        "PUT should return same format as GET: {body}"
    );
    assert!(
        body["companyAccess"].is_array(),
        "PUT should return companyAccess array: {body}"
    );
}

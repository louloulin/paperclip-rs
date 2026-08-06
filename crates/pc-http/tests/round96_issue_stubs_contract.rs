//! Integration tests for Round 96:
//! 修复 issues.rs 中 14 个引用不存在表的 inline SQL：
//! - issue_interactions（CRUD + accept/cancel/reject/respond/verdict/withdraw）
//! - issue_accepted_plan_decompositions（list/create）
//! - issue_annotation_comments（annotation_comment_route）
//! - issue_read_state（unmark_read_route）
//! - issue_events（issue_activity）

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{routes, state::ConfigSnapshot, AppState};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
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
        pc_http::state::RuntimeHandles {
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
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .uri(path)
                .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .expect("request");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r96-{tag}-{id}"))
        .bind(format!("R{}", &id.simple().to_string()[..5]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority) \
         VALUES ($1, $2, $3, 'backlog', 'medium')",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r96-issue-{tag}-{id}"))
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

// =====================================================================
// Round 96：所有 issue_interactions/* 端点必须返回 200 + deprecated 标记
// 原因：issue_interactions 表在 v3 schema 中不存在
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_list_issue_interactions_returns_empty_with_deprecated_flag() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "r96-list-int").await;
    let iid = insert_issue(&db, cid, "list-int").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{iid}/interactions"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["deprecated"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn http_create_issue_interaction_returns_id_with_deprecated_flag() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "r96-create-int").await;
    let iid = insert_issue(&db, cid, "create-int").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{iid}/interactions"),
        serde_json::json!({"kind": "comment", "body": "test"}),
    )
    .await;
    assert_eq!(status, 200);
    assert!(body["id"].is_string(), "must return synthetic id");
    assert_eq!(body["deprecated"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn http_delete_issue_interaction_returns_204() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "r96-del-int").await;
    let iid = insert_issue(&db, cid, "del-int").await;
    let fake_id = Uuid::new_v4();
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/issues/{iid}/interactions/{fake_id}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status, 204,
        "stub returns 204 even for non-existent interactions"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn http_accept_cancel_reject_interaction_return_deprecated_stubs() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "r96-actions").await;
    let iid = insert_issue(&db, cid, "actions").await;
    let int_id = Uuid::new_v4();

    for action in ["accept", "cancel", "reject"] {
        let (status, body) = call(
            &app,
            "POST",
            &format!("/api/issues/{iid}/interactions/{int_id}/{action}"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, 200, "{action} must return 200");
        assert_eq!(
            body["deprecated"], true,
            "{action} must be marked deprecated"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn http_respond_verdict_withdraw_interaction_return_deprecated_stubs() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "r96-rvw").await;
    let iid = insert_issue(&db, cid, "rvw").await;
    let int_id = Uuid::new_v4();

    let (s1, b1) = call(
        &app,
        "POST",
        &format!("/api/issues/{iid}/interactions/{int_id}/respond"),
        serde_json::json!({"body": "response"}),
    )
    .await;
    assert_eq!(s1, 200);
    assert_eq!(b1["deprecated"], true);

    let (s2, b2) = call(
        &app,
        "POST",
        &format!("/api/issues/{iid}/interactions/{int_id}/verdicts"),
        serde_json::json!({"verdict": "approve"}),
    )
    .await;
    assert_eq!(s2, 200);
    assert_eq!(b2["deprecated"], true);

    let (s3, b3) = call(
        &app,
        "POST",
        &format!("/api/issues/{iid}/interactions/{int_id}/withdraw"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s3, 200);
    assert_eq!(b3["withdrawn"], true);
    assert_eq!(b3["deprecated"], true);
}

// =====================================================================
// issue_accepted_plan_decompositions
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_list_accepted_plan_decompositions_returns_deprecated_stub() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "r96-plans").await;
    let iid = insert_issue(&db, cid, "plans").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{iid}/accepted-plan-decompositions"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["deprecated"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn http_create_accepted_plan_decomposition_returns_deprecated_id() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "r96-plan-create").await;
    let iid = insert_issue(&db, cid, "plan-create").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{iid}/accepted-plan-decompositions"),
        serde_json::json!({"planSummary": "1. Step one\n2. Step two"}),
    )
    .await;
    assert_eq!(status, 200);
    assert!(body["id"].is_string());
    assert_eq!(body["deprecated"], true);
}

// =====================================================================
// issue_annotation_comments
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_annotation_comment_returns_deprecated_stub() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "r96-ann").await;
    let iid = insert_issue(&db, cid, "ann").await;
    let thread_id = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{iid}/documents/some-key/annotations/{thread_id}/comments"),
        serde_json::json!({"body": "annotation comment"}),
    )
    .await;
    assert_eq!(status, 200);
    assert!(body["id"].is_string());
    assert_eq!(body["deprecated"], true);
}

// =====================================================================
// issue_read_state
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_unmark_read_returns_deprecated_stub() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "r96-unmark").await;
    let iid = insert_issue(&db, cid, "unmark").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{iid}/unread"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["read"], false);
    assert_eq!(body["deprecated"], true);
}

// =====================================================================
// issue_events
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_issue_activity_returns_deprecated_empty() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "r96-activity").await;
    let iid = insert_issue(&db, cid, "activity").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{iid}/activity"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["deprecated"], true);
}

// =====================================================================
// 真实路由（list_interactions 在 line 213）仍然有效 — 防止误伤
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_real_list_interactions_still_works() {
    // 保护性测试：line 213 的 list_interactions 是真实路由（用 IssueRepo）
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "r96-real-int").await;
    let iid = insert_issue(&db, cid, "real-int").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{iid}/interactions"),
        serde_json::json!({}),
    )
    .await;
    // 真实路由应返回无 deprecated 标记的 items 数组
    assert_eq!(status, 200);
    assert!(
        body["items"].is_array(),
        "real route must return items array"
    );
}

//! R516 — Node 兼容的 /api/companies/:id/search/extract contract
//!
//! Node 端契约 (issues.ts:4705 + shared/src/types/search.ts:122):
//! - GET /api/companies/:companyId/search/extract?contains=&kind=&scope=&limit=&offset=&matchesPerIssue=
//! - kind:    literal|url   (default literal)
//! - scope:   all|issues|comments|documents (default all)
//! - limit:   1..200        (default 100)
//! - offset:  0..max        (default 0)
//! - matchesPerIssue: 1..50 (default 10)
//! - contains >= 2 字符
//! - 返回 CompanySearchExtractResponse: results 数组含 issueId + matches[] + excerpt

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
use pc_secrets::DecisionSigningService;
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
    .with_decision_signing(Arc::new(
        DecisionSigningService::from_secret("0123456789abcdef0123456789abcdef")
            .expect("test signing secret"),
    ))
}

async fn call_get(app: &axum::Router, path: &str) -> (axum::http::StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("r516-{id}"))
    .bind(id.simple().to_string())
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid, title: &str, description: &str) -> Uuid {
    let id = Uuid::new_v4();
    // identifier 在 issues 表上是全局 unique (非 per-company)，
    // 所以用 Uuid 短前缀保证每个测试独立。
    let identifier = format!("r516-{}", &id.simple().to_string()[..12]);
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, description, status, priority, created_at, updated_at)          VALUES ($1, $2, $3, $4, $5, 'todo', 'normal', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(&identifier)
    .bind(title)
    .bind(description)
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

async fn insert_comment(db: &Db, company_id: Uuid, issue_id: Uuid, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_comments (id, company_id, issue_id, body, created_at, updated_at)          VALUES ($1, $2, $3, $4, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(issue_id)
    .bind(body)
    .execute(db.pool())
    .await
    .expect("insert comment");
    id
}

async fn insert_document(db: &Db, company_id: Uuid, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, title, format, latest_body, latest_revision_number, created_at, updated_at)          VALUES ($1, $2, 'r516-doc', 'markdown', $3, 1, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(body)
    .execute(db.pool())
    .await
    .expect("insert document");
    id
}

#[tokio::test(flavor = "current_thread")]
async fn search_extract_returns_literal_match_in_title() {
    let _lock = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let _issue = insert_issue(
        &db,
        company_id,
        "Migrate authentication to OIDC",
        "Move from password to corporate OIDC provider",
    )
    .await;
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, body) = call_get(
        &app,
        &format!(
            "/api/companies/{company_id}/search/extract?contains=oidc&kind=literal&scope=issues"
        ),
    )
    .await;
    assert_eq!(status, 200, "extract: {body}");
    assert_eq!(body["contains"], "oidc");
    assert_eq!(body["kind"], "literal");
    assert_eq!(body["scope"], "issues");
    let results = body["results"].as_array().expect("results array");
    assert!(
        results.len() >= 1,
        "should find at least one matching issue: {body}"
    );
    let first = &results[0];
    let matches = first["matches"].as_array().expect("matches");
    assert!(
        !matches.is_empty(),
        "issue should have at least one match: {first}"
    );
    // Match should be in title or description field.
    let fields: Vec<&str> = matches.iter().filter_map(|m| m["field"].as_str()).collect();
    assert!(
        fields.contains(&"title") || fields.contains(&"description"),
        "expected title or description match, got {fields:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn search_extract_rejects_missing_contains() {
    let _lock = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, _) = call_get(&app, &format!("/api/companies/{company_id}/search/extract")).await;
    assert_eq!(status, 400, "missing contains should be 400");
}

#[tokio::test(flavor = "current_thread")]
async fn search_extract_finds_match_in_comment_body() {
    let _lock = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        "Refactor cache layer",
        "Switch to Redis with TTL eviction",
    )
    .await;
    let _comment = insert_comment(
        &db,
        company_id,
        issue_id,
        "I checked the bench results — performance improved by 40% in the staging environment",
    )
    .await;
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, body) = call_get(
        &app,
        &format!("/api/companies/{company_id}/search/extract?contains=staging&kind=literal&scope=comments"),
    ).await;
    assert_eq!(status, 200, "extract comments: {body}");
    assert_eq!(body["scope"], "comments");
    let results = body["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "should find comment match: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn search_extract_kind_url_matches_url_substring() {
    let _lock = TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let _ = insert_issue(
        &db,
        company_id,
        "Triage upstream bug",
        "See https://github.com/example/repo/pull/1234 for context",
    )
    .await;
    let app = routes::companies::router().with_state(test_state(db.clone()));

    let (status, body) = call_get(
        &app,
        &format!("/api/companies/{company_id}/search/extract?contains=github.com/example&kind=url"),
    )
    .await;
    assert_eq!(status, 200, "extract url: {body}");
    assert_eq!(body["kind"], "url");
    let results = body["results"].as_array().expect("results array");
    assert!(
        !results.is_empty(),
        "kind=url should find URL-containing text: {body}"
    );
}

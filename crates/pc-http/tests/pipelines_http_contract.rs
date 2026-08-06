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

fn unique_prefix(suffix: &str) -> String {
    let u = Uuid::new_v4().simple().to_string();
    let t: String = u.chars().take(8).collect();
    format!("{t}{suffix}")
}

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("pl-{id}"))
        .bind(unique_prefix("PL"))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_pipeline(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO pipelines (id, company_id, key, name) VALUES ($1,$2,$3,$4)")
        .bind(id)
        .bind(company_id)
        .bind(format!("p-{id}"))
        .bind("Test Pipeline")
        .execute(db.pool())
        .await
        .expect("insert pipeline");
    id
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

#[tokio::test(flavor = "current_thread")]
async fn pipeline_stages_crud() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let pipeline_id = insert_pipeline(&db, company_id).await;
    let app = routes::pipelines::router().with_state(test_state(db.clone()));

    // create stage
    let (s, b) = call(
        &app,
        "POST",
        &format!("/api/pipelines/{pipeline_id}/stages"),
        serde_json::json!({ "key": "open", "name": "Open", "kind": "working", "position": 0 }),
    )
    .await;
    assert_eq!(s, 201, "create: {b}");
    let stage_id = b["id"].as_str().expect("id").to_string();

    // list
    let (_, b) = call(
        &app,
        "GET",
        &format!("/api/pipelines/{pipeline_id}/stages"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(b.as_array().unwrap().len(), 1);

    // patch
    let (s, b) = call(
        &app,
        "PATCH",
        &format!("/api/pipelines/{pipeline_id}/stages/{stage_id}"),
        serde_json::json!({ "name": "Open Stage" }),
    )
    .await;
    assert_eq!(s, 200, "patch: {b}");
    assert_eq!(b["name"], "Open Stage");

    // delete
    let (s, _) = call(
        &app,
        "DELETE",
        &format!("/api/pipelines/{pipeline_id}/stages/{stage_id}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, 204);
}

#[tokio::test(flavor = "current_thread")]
async fn pipeline_transitions_and_cases() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let pipeline_id = insert_pipeline(&db, company_id).await;
    let app = routes::pipelines::router().with_state(test_state(db.clone()));

    // 2 stages
    let (_, open) = call(
        &app,
        "POST",
        &format!("/api/pipelines/{pipeline_id}/stages"),
        serde_json::json!({ "key":"open", "name":"Open", "kind":"working", "position":0 }),
    )
    .await;
    let (_, done) = call(
        &app,
        "POST",
        &format!("/api/pipelines/{pipeline_id}/stages"),
        serde_json::json!({ "key":"done", "name":"Done", "kind":"done", "position":1 }),
    )
    .await;
    let open_id = open["id"].as_str().expect("stage id").to_string();
    let done_id = done["id"].as_str().unwrap().to_string();

    // transition
    let (s, b) = call(
        &app,
        "POST",
        &format!("/api/pipelines/{pipeline_id}/transitions"),
        serde_json::json!({ "from_stage_id": open_id, "to_stage_id": done_id, "label": "finish" }),
    )
    .await;
    assert_eq!(s, 201, "transition: {b}");

    // case
    let (s, b) = call(
        &app,
        "POST",
        &format!("/api/pipelines/{pipeline_id}/cases"),
        serde_json::json!({ "stage_id": open_id, "case_key": "CASE-1", "title": "First case" }),
    )
    .await;
    assert_eq!(s, 201, "case: {b}");
    let case_id = b["id"].as_str().unwrap().to_string();
    assert_eq!(b["version"], 1);

    // transition case
    let (s, b) = call(
        &app,
        "POST",
        &format!("/api/cases/{case_id}/transition"),
        serde_json::json!({ "to_stage_id": done_id }),
    )
    .await;
    assert_eq!(s, 200, "case transition: {b}");
    assert_eq!(b["version"], 2);
    assert_eq!(b["terminal_kind"], "done");
    assert!(b["terminal_at"].is_string());

    // events
    let (_, b) = call(
        &app,
        "GET",
        &format!("/api/cases/{case_id}/events"),
        serde_json::json!({}),
    )
    .await;
    let events = b.as_array().expect("array");
    eprintln!(
        "DEBUG events count: {}, first: {}",
        events.len(),
        serde_json::to_string(events.first().unwrap_or(&serde_json::Value::Null))
            .unwrap_or_default()
    );
    assert!(events.iter().any(|e| e["type"] == "transitioned"));

    // claim / release
    let (s, b) = call(
        &app,
        "POST",
        &format!("/api/cases/{case_id}/claim"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, 200, "claim: {b}");
    assert_eq!(b["lease_owner_type"], "user");
    let (s, b) = call(
        &app,
        "POST",
        &format!("/api/cases/{case_id}/release"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, 200, "release: {b}");
    assert!(b["lease_owner_type"].is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn pipeline_case_issue_links() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let pipeline_id = insert_pipeline(&db, company_id).await;
    let issue_id = Uuid::new_v4();
    sqlx::query("INSERT INTO issues (id, company_id, title, origin_kind, origin_fingerprint) VALUES ($1,$2,$3,'user',$4)")
        .bind(issue_id).bind(company_id).bind("linked issue").bind(format!("fp-{issue_id}"))
        .execute(db.pool()).await.expect("insert issue");

    // stage + case
    let app = routes::pipelines::router().with_state(test_state(db.clone()));
    let (_, stage) = call(
        &app,
        "POST",
        &format!("/api/pipelines/{pipeline_id}/stages"),
        serde_json::json!({ "key":"open", "name":"Open", "kind":"working", "position":0 }),
    )
    .await;
    let (_, case) = call(
        &app,
        "POST",
        &format!("/api/pipelines/{pipeline_id}/cases"),
        serde_json::json!({ "stage_id": stage["id"], "case_key":"C-1", "title":"C" }),
    )
    .await;
    let case_id = case["id"].as_str().unwrap().to_string();

    let (s, b) = call(
        &app,
        "POST",
        &format!("/api/cases/{case_id}/issue-links"),
        serde_json::json!({ "issue_id": issue_id, "role": "work" }),
    )
    .await;
    assert_eq!(s, 201, "link: {b}");
    let link_id = b["id"].as_str().unwrap().to_string();

    let (_, b) = call(
        &app,
        "GET",
        &format!("/api/cases/{case_id}/issue-links"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(b.as_array().unwrap().len(), 1);

    let (s, _) = call(
        &app,
        "DELETE",
        &format!("/api/cases/{case_id}/issue-links/{link_id}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, 204);
}

#[tokio::test(flavor = "current_thread")]
async fn pipeline_archive() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let pipeline_id = insert_pipeline(&db, company_id).await;
    let app = routes::pipelines::router().with_state(test_state(db.clone()));

    let (s, b) = call(
        &app,
        "POST",
        &format!("/api/pipelines/{pipeline_id}/archive"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, 200, "archive: {b}");
    assert!(b["archivedAt"].is_string() || b["archived_at"].is_string());
}

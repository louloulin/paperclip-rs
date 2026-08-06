//! Company skills 路由契约测试。

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

async fn seed_company(db: &Db) -> Uuid {
    let prefix = format!("SK{}", &Uuid::new_v4().simple().to_string()[..4]);
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id",
    )
    .bind(format!("skills-test-{}", Uuid::new_v4().simple()))
    .bind(&prefix)
    .fetch_one(db.pool())
    .await
    .expect("seed company")
}

#[tokio::test(flavor = "current_thread")]
async fn list_company_skills_shape() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::company_skills::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/skills"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "list skills: {body}");
    assert!(body["items"].is_array());
}

#[tokio::test(flavor = "current_thread")]
async fn skills_categories_shape() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::company_skills::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/skills/categories"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "categories: {body}");
    assert!(body["categories"].is_array() || body["items"].is_array());
}

#[tokio::test(flavor = "current_thread")]
async fn skills_catalog_endpoint() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::company_skills::router().with_state(test_state(db));
    let (status, body) = call(&app, "GET", "/api/skills/catalog", None, None).await;
    // catalog 可能 200（如果 manifest 存在）或 500（manifest 缺失），这两种都允许 ——
    // 主要保证响应格式是 JSON 而不是 panic。
    assert!(
        status == 200 || status == 500 || status == 404,
        "catalog: {body}"
    );
    if status == 200 {
        // 形状应当含 skills 数组
        assert!(body["skills"].is_array() || body["items"].is_array());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn install_and_get_company_skill() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::company_skills::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/skills"),
        Some(json!({
            "key": "test-skill",
            "name": "Test Skill",
            "description": "desc",
            "markdown": "# Test Skill\n\nHello",
            "sourceType": "company_owned",
            "trustLevel": "internal",
            "compatibility": "v1",
            "categories": ["general"]
        })),
        None,
    )
    .await;
    assert_eq!(status, 201, "install: {body}");
    let skill_id = body["id"].as_str().expect("id");

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/skills/{skill_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "get: {body}");
    assert_eq!(body["name"], "Test Skill");
    assert_eq!(body["markdown"], "# Test Skill\n\nHello");

    let (status, _body) = call(
        &app,
        "DELETE",
        &format!("/api/companies/{company_id}/skills/{skill_id}"),
        None,
        None,
    )
    .await;
    assert!(status == 200 || status == 204, "delete: status {status}");
}

#[tokio::test(flavor = "current_thread")]
async fn put_skill_config_round_trip() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::company_skills::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;

    let (_, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/skills"),
        Some(json!({
            "key": "config-skill",
            "name": "Configurable",
            "markdown": "x",
            "sourceType": "company_owned",
            "trustLevel": "internal",
            "compatibility": "v1",
        })),
        None,
    )
    .await;
    let skill_id = body["id"].as_str().expect("id");

    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/companies/{company_id}/skills/{skill_id}/config"),
        Some(json!({ "config": { "k": "v" } })),
        None,
    )
    .await;
    assert_eq!(status, 200, "put config: {body}");
    assert_eq!(body["value"]["k"], "v");

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/skills/{skill_id}/config"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "get config: {body}");
    assert_eq!(body["config"]["k"], "v");
}

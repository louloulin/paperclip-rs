//! Execution workspace 路由契约测试。
//!
//! 覆盖：
//! * list by company / overview / get single / patch
//! * close-readiness 形状
//! * workspace-operations 形状（rebuild/reset/reconcile/archive）
//! * runtime service action 队列（enqueue → action log 出现）
//! * runtime command action 队列
//! * reconcile-branch 状态切换 + enqueue
//! * runtime services list（空 workspace）
//! * runtime service lifecycle 状态切换
//! * 404 unknown workspace

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
        Arc::new(WsState::new(
            realtime.clone(),
            "test".to_string(),
        )),
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
    // create a company with a unique issue_prefix
    let prefix = format!("EW{}", &Uuid::new_v4().simple().to_string()[..4]);
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id",
    )
    .bind(format!("exec-ws-test-{}", Uuid::new_v4().simple()))
    .bind(&prefix)
    .fetch_one(db.pool())
    .await
    .expect("seed company")
}

async fn seed_project(db: &Db, company_id: Uuid) -> Uuid {
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO projects (company_id, name, status) \
         VALUES ($1, 'exec-ws project', 'active') RETURNING id",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .expect("seed project")
}

async fn seed_workspace(db: &Db, company_id: Uuid, project_id: Uuid) -> Uuid {
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO execution_workspaces \
            (company_id, project_id, mode, strategy_type, name, status) \
         VALUES ($1, $2, 'execution', 'worktree', 'test-ws', 'active') RETURNING id",
    )
    .bind(company_id)
    .bind(project_id)
    .fetch_one(db.pool())
    .await
    .expect("seed workspace")
}

#[tokio::test(flavor = "current_thread")]
async fn list_workspaces_returns_items() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    let ws_id = seed_workspace(&db, company_id, project_id).await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/execution-workspaces"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "list: {body}");
    let items = body["items"].as_array().expect("items array");
    assert!(items.iter().any(|it| it["id"] == json!(ws_id.to_string())));
    assert!(items.iter().any(|it| it["status"] == "active"));
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_overview_returns_summary() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    seed_workspace(&db, company_id, project_id).await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/workspace-overview"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "overview: {body}");
    assert_eq!(body["companyId"], json!(company_id.to_string()));
    assert!(body["activeWorkspaces"].as_i64().unwrap() >= 1);
    assert!(body["recentRuns"].as_i64().is_some());
    assert!(body["needsAttention"].as_i64().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn get_unknown_workspace_returns_404() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db));
    let (status, _body) = call(
        &app,
        "GET",
        &format!("/api/execution-workspaces/{}", Uuid::new_v4()),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404, "404 expected");
}

#[tokio::test(flavor = "current_thread")]
async fn patch_workspace_updates_name() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    let ws_id = seed_workspace(&db, company_id, project_id).await;

    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/execution-workspaces/{ws_id}"),
        Some(json!({ "name": "renamed-ws" })),
        None,
    )
    .await;
    assert_eq!(status, 200, "patch: {body}");
    assert_eq!(body["status"], "updated");
    let actual: String = sqlx::query_scalar("SELECT name FROM execution_workspaces WHERE id = $1")
        .bind(ws_id)
        .fetch_one(db.pool())
        .await
        .expect("query");
    assert_eq!(actual, "renamed-ws");
}

#[tokio::test(flavor = "current_thread")]
async fn close_readiness_shape() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    let ws_id = seed_workspace(&db, company_id, project_id).await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/execution-workspaces/{ws_id}/close-readiness"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "close-readiness: {body}");
    assert_eq!(body["id"], json!(ws_id.to_string()));
    assert!(body["ready"].is_boolean());
    assert!(body["checks"].is_array());
    assert_eq!(body["uncommittedChanges"], json!(0));
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_operations_shape() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    let ws_id = seed_workspace(&db, company_id, project_id).await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/execution-workspaces/{ws_id}/workspace-operations"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "operations: {body}");
    let ops = body["operations"].as_array().expect("operations array");
    let keys: Vec<&str> = ops
        .iter()
        .map(|o| o["key"].as_str().unwrap_or(""))
        .collect();
    assert!(keys.contains(&"rebuild"));
    assert!(keys.contains(&"reset"));
    assert!(keys.contains(&"reconcile"));
    assert!(keys.contains(&"archive"));
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_service_action_enqueues_log_entry() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    let ws_id = seed_workspace(&db, company_id, project_id).await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/execution-workspaces/{ws_id}/runtime-services/start"),
        Some(json!({ "serviceName": "dev-server" })),
        None,
    )
    .await;
    assert_eq!(status, 202, "service action: {body}");
    assert_eq!(body["kind"], "service");
    assert_eq!(body["action"], "start");
    assert_eq!(body["status"], "queued");

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/execution-workspaces/{ws_id}/action-log"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "action-log: {body}");
    let items = body["items"].as_array().expect("items array");
    assert!(items.iter().any(|it| it["kind"] == "service" && it["action"] == "start"));
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_command_action_enqueues_log_entry() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    let ws_id = seed_workspace(&db, company_id, project_id).await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/execution-workspaces/{ws_id}/runtime-commands/exec"),
        Some(json!({ "command": "ls -la", "timeoutMs": 30000 })),
        None,
    )
    .await;
    assert_eq!(status, 202, "command action: {body}");
    assert_eq!(body["kind"], "command");
    assert_eq!(body["action"], "exec");
    assert_eq!(body["status"], "queued");
}

#[tokio::test(flavor = "current_thread")]
async fn reconcile_branch_enqueues_and_flips_status() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    let ws_id = seed_workspace(&db, company_id, project_id).await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/execution-workspaces/{ws_id}/reconcile-branch"),
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(status, 202, "reconcile: {body}");
    assert_eq!(body["kind"], "reconcile");
    let status_now: String =
        sqlx::query_scalar("SELECT status FROM execution_workspaces WHERE id = $1")
            .bind(ws_id)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert_eq!(status_now, "reconciling");
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_services_list_is_empty_when_unused() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    let ws_id = seed_workspace(&db, company_id, project_id).await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/execution-workspaces/{ws_id}/runtime-services"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "runtime services: {body}");
    assert_eq!(body["workspaceId"], json!(ws_id.to_string()));
    assert!(body["items"].is_array());
    // we don't seed any runtime services for this ws, so list may be empty
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_service_lifecycle_round_trip() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    // seed a runtime service directly
    let ws_id = seed_workspace(&db, company_id, project_id).await;
    let svc_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace_runtime_services \
            (id, company_id, project_id, scope_type, scope_id, service_name, status, lifecycle, provider) \
         VALUES (gen_random_uuid(), $1, $2, 'execution_workspace', $3, 'web', 'idle', 'fresh', 'local_process') RETURNING id"
    )
    .bind(company_id)
    .bind(project_id)
    .bind(ws_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("seed service");

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/runtime-services/{svc_id}/lifecycle"),
        Some(json!({ "lifecycle": "started" })),
        None,
    )
    .await;
    assert_eq!(status, 200, "lifecycle: {body}");
    assert_eq!(body["lifecycle"], "started");

    // unknown service → 404
    let (status, _body) = call(
        &app,
        "POST",
        &format!("/api/runtime-services/{}/lifecycle", Uuid::new_v4()),
        Some(json!({ "lifecycle": "stopped" })),
        None,
    )
    .await;
    assert_eq!(status, 404, "404 unknown");
}

#[tokio::test(flavor = "current_thread")]
async fn lease_acquire_renew_release_round_trip() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    let ws_id = seed_workspace(&db, company_id, project_id).await;

    // acquire
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/execution-workspaces/{ws_id}/lease/acquire"),
        Some(json!({ "agentId": Uuid::new_v4(), "ttlSecs": 60 })),
        None,
    )
    .await;
    assert_eq!(status, 200, "acquire: {body}");
    let lease_id = body["id"].as_str().expect("lease id");
    let token = body["token"].as_str().expect("lease token");
    assert_eq!(body["state"], "holding");

    // active 查询
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/execution-workspaces/{ws_id}/lease"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "active: {body}");
    assert_eq!(body["id"], json!(lease_id));

    // 二次 acquire 应当 409（已被占用）
    let (status, _body) = call(
        &app,
        "POST",
        &format!("/api/execution-workspaces/{ws_id}/lease/acquire"),
        Some(json!({ "agentId": Uuid::new_v4() })),
        None,
    )
    .await;
    assert_eq!(status, 409, "second acquire must conflict");

    // renew
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/execution-workspaces/{ws_id}/lease/renew"),
        Some(json!({ "leaseId": lease_id, "token": token, "newTtlSecs": 600 })),
        None,
    )
    .await;
    assert_eq!(status, 200, "renew: {body}");

    // release
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/execution-workspaces/{ws_id}/lease/release"),
        Some(json!({ "leaseId": lease_id, "token": token })),
        None,
    )
    .await;
    assert_eq!(status, 200, "release: {body}");
    assert_eq!(body["status"], "released");

    // 释放后可重新 acquire
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/execution-workspaces/{ws_id}/lease/acquire"),
        Some(json!({ "agentId": Uuid::new_v4() })),
        None,
    )
    .await;
    assert_eq!(status, 200, "post-release acquire must succeed");
}

#[tokio::test(flavor = "current_thread")]
async fn lease_active_404_for_unleased_workspace() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::execution_workspaces::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let project_id = seed_project(&db, company_id).await;
    let ws_id = seed_workspace(&db, company_id, project_id).await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/execution-workspaces/{ws_id}/lease"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404, "no lease → 404: {body}");
}

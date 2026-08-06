//! Integration tests for Round 99:
//! 修复 tool_access.rs 中 `tool_applications` 表列名漂移：
//! - 原 SQL 引用不存在的列 `kind / description / config`
//! - 真实 schema：`tool_applications(id, company_id, name, type, metadata, ...)`
//!   * `kind` → 写到 `type`
//!   * `description` + `config` → 合并到 jsonb `metadata`
//!
//! 涉及 4 个路由：
//! - `POST   /api/companies/:cid/tools/applications`        (create)
//! - `GET    /api/companies/:cid/tools/applications`        (list)
//! - `GET    /api/tool-applications/:aid`                   (get by id)
//! - `PATCH  /api/companies/:cid/tools/applications/:aid`   (patch)

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
        .bind(format!("r99-{tag}-{id}"))
        .bind(format!("R99{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_tool_application(
    db: &Db,
    company_id: Uuid,
    kind: &str,
    description: &str,
    config: serde_json::Value,
) -> Uuid {
    let id = Uuid::new_v4();
    let mut metadata = config.clone();
    if let serde_json::Value::Object(ref mut map) = metadata {
        map.insert("description".into(), serde_json::json!(description));
    } else {
        let mut map = serde_json::Map::new();
        map.insert("description".into(), serde_json::json!(description));
        metadata = serde_json::Value::Object(map);
    }
    sqlx::query(
        "INSERT INTO tool_applications (id, company_id, name, type, metadata) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("app-{id}"))
    .bind(kind)
    .bind(metadata)
    .execute(db.pool())
    .await
    .expect("insert tool_application");
    id
}

// =====================================================================
// 1. list_tool_applications：验证 type 列投影 + metadata 拆出
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_list_tool_applications_returns_kind_and_metadata_split() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "list").await;
    insert_tool_application(
        &db,
        cid,
        "mcp",
        "first app",
        serde_json::json!({"endpoint": "https://example.com"}),
    )
    .await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{cid}/tools/applications"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    let items = body["items"].as_array().expect("items array");
    assert!(!items.is_empty(), "expected at least 1 item");
    let app0 = &items[0];
    assert_eq!(app0["kind"], "mcp");
    assert_eq!(app0["description"], "first app");
    assert_eq!(app0["config"]["endpoint"], "https://example.com");
}

// =====================================================================
// 2. create_tool_application：验证 INSERT 用 type + metadata
//    输入 body 提供 name/kind/description/config；响应回送原字段
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_create_tool_application_writes_type_and_metadata() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "create").await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{cid}/tools/applications"),
        serde_json::json!({
            "name": "my-mcp-app",
            "kind": "stdio",
            "description": "writes to stdout",
            "config": {"command": "echo hello", "timeout": 30}
        }),
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["name"], "my-mcp-app");
    assert_eq!(body["kind"], "stdio");
    assert_eq!(body["description"], "writes to stdout");
    assert_eq!(body["config"]["command"], "echo hello");

    // 反查 DB 验证：kind 写到 type；metadata 内嵌 description + config
    let row: (String, serde_json::Value) =
        sqlx::query_as("SELECT type, metadata FROM tool_applications WHERE id = $1")
            .bind(body["id"].as_str().expect("id string"))
            .fetch_one(db.pool())
            .await
            .expect("query row");
    let (db_kind, db_meta) = row;
    assert_eq!(db_kind, "stdio");
    assert_eq!(db_meta["description"], "writes to stdout");
    assert_eq!(db_meta["config"]["command"], "echo hello");
}

// =====================================================================
// 3. get_tool_application：验证根据 id 反查、并把 metadata 拆回
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_get_tool_application_returns_kind_and_metadata_split() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "get").await;
    let aid = insert_tool_application(
        &db,
        cid,
        "http",
        "http app",
        serde_json::json!({"url": "https://api.test"}),
    )
    .await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/tool-applications/{aid}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["id"], serde_json::json!(aid));
    assert_eq!(body["kind"], "http");
    assert_eq!(body["description"], "http app");
    assert_eq!(body["config"]["url"], "https://api.test");
}

// =====================================================================
// 4. patch_tool_application：验证 metadata || patch 用 jsonb 合并
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_patch_tool_application_merges_metadata_jsonb() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "patch").await;
    let aid =
        insert_tool_application(&db, cid, "mcp", "before", serde_json::json!({"flag": true})).await;

    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{cid}/tools/applications/{aid}"),
        serde_json::json!({
            "description": "after",
            "config": {"flag": false, "added": 1}
        }),
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["updated"], true);

    // 验证 metadata 在 DB 中确实被 jsonb 合并：
    //   description 由 "before" → "after"
    //   config 由 {flag: true} → {flag: false, added: 1}
    let (db_meta,): (serde_json::Value,) =
        sqlx::query_as("SELECT metadata FROM tool_applications WHERE id = $1")
            .bind(aid)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert_eq!(db_meta["description"], "after");
    assert_eq!(db_meta["config"]["flag"], false);
    assert_eq!(db_meta["config"]["added"], 1);
}

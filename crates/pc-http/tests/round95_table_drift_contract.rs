//! Integration tests for Round 95:
//! 修复 3 个表名漂移 bug — schema 在迭代中改名但路由 SQL 没跟上。
//!
//! - `secrets.rs::patch_provider_config`: `secret_provider_configs` → `company_secret_provider_configs`，列 `label` → `display_name`
//! - `issues.rs::{list,create}_issue_feedback_vote`: `issue_feedback_votes` → `feedback_votes`，列 `voter_kind/score` → `target_type/vote`
//! - `tool_access.rs::list_connection_grants`: `tool_oauth_grants` → `connection_grants`，列 `scope/expires_at` → `kind/subject_user_id/status`

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
        .bind(format!("drift-{tag}-{id}"))
        .bind(format!("D{}", &id.simple().to_string()[..5]))
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
    .bind(format!("drift-issue-{tag}-{id}"))
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

async fn insert_provider_config(db: &Db, company_id: Uuid, label: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_secret_provider_configs \
         (id, company_id, provider, display_name, status, config) \
         VALUES ($1, $2, 'aws-secrets', $3, 'ready', '{}'::jsonb)",
    )
    .bind(id)
    .bind(company_id)
    .bind(label)
    .execute(db.pool())
    .await
    .expect("insert provider config");
    id
}

async fn insert_connection(db: &Db, company_id: Uuid, kind: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tool_connections (id, company_id, kind, display_name, status) \
         VALUES ($1, $2, $3, 'test', 'ready')",
    )
    .bind(id)
    .bind(company_id)
    .bind(kind)
    .execute(db.pool())
    .await
    .expect("insert connection");
    id
}

async fn insert_connection_grant(db: &Db, company_id: Uuid, connection_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO connection_grants \
         (id, company_id, connection_id, kind, status) \
         VALUES ($1, $2, $3, 'workspace', 'active')",
    )
    .bind(id)
    .bind(company_id)
    .bind(connection_id)
    .execute(db.pool())
    .await
    .expect("insert grant");
    id
}

// =====================================================================
// secrets.rs: patch_provider_config
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_patch_provider_config_uses_real_table_and_display_name_column() {
    // Round 95 修复：原 SQL 引用 secret_provider_configs + label 列
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "drift-pc").await;
    let pid = insert_provider_config(&db, cid, "original-label").await;

    let (status, _body) = call(
        &app,
        "PATCH",
        &format!("/api/secrets/provider-configs/{pid}"),
        serde_json::json!({"label": "renamed-label", "status": "ready"}),
    )
    .await;
    assert_eq!(status, 200, "patch must succeed (was 500 before fix)");

    let row: (String, String) = sqlx::query_as(
        "SELECT display_name, status FROM company_secret_provider_configs WHERE id = $1",
    )
    .bind(pid)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "renamed-label", "display_name updated");
    assert_eq!(row.1, "ready");
}

// =====================================================================
// issues.rs: list_issue_feedback_votes / create_issue_feedback_vote
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_create_feedback_vote_uses_feedback_votes_table() {
    // Round 95 修复：表名 issue_feedback_votes → feedback_votes
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "drift-fb").await;
    let iid = insert_issue(&db, cid, "v1").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{iid}/feedback-votes"),
        serde_json::json!({"voterKind": "user", "vote": "up", "reason": "looks good"}),
    )
    .await;
    assert_eq!(status, 200);
    let vote_id = body["id"].as_str().expect("vote id");
    // 验证真的写入 feedback_votes 表（不是不存在的 issue_feedback_votes）
    let row: (String, String, String, Option<String>) = sqlx::query_as(
        "SELECT target_type, target_id, vote, reason FROM feedback_votes WHERE id = $1::uuid",
    )
    .bind(vote_id)
    .fetch_one(db.pool())
    .await
    .expect("feedback_votes row exists");
    assert_eq!(row.0, "user");
    assert_eq!(row.2, "up");
    assert_eq!(row.3.as_deref(), Some("looks good"));
}

#[tokio::test(flavor = "current_thread")]
async fn http_list_feedback_votes_reads_from_feedback_votes() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "drift-fb-list").await;
    let iid = insert_issue(&db, cid, "v2").await;
    // 直接往 feedback_votes 写一行
    sqlx::query(
        "INSERT INTO feedback_votes (company_id, issue_id, target_type, target_id, author_user_id, vote) \
         VALUES ($1, $2, 'user', 'tester', 'tester', 'up')",
    )
    .bind(cid)
    .bind(iid)
    .execute(db.pool())
    .await
    .unwrap();
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/issues/{iid}/feedback-votes"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["voterKind"], "user");
    assert_eq!(items[0]["vote"], "up");
}

#[tokio::test(flavor = "current_thread")]
async fn http_create_feedback_vote_for_missing_issue_returns_404() {
    // Round 95 修复：必须先查 company_id（必填列）；找不到 issue → 404 而不是 500
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let missing = Uuid::new_v4();
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/issues/{missing}/feedback-votes"),
        serde_json::json!({"voterKind": "user", "vote": "up"}),
    )
    .await;
    assert_eq!(status, 404);
}

// =====================================================================
// tool_access.rs: list_connection_grants / list_application_grants
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_list_connection_grants_uses_real_table() {
    // Round 95 修复：表名 tool_oauth_grants → connection_grants，列 scope/expires_at → kind/subject_user_id/status
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "drift-cg").await;
    let conn_id = insert_connection(&db, cid, "github").await;
    insert_connection_grant(&db, cid, conn_id).await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/tool-connections/{conn_id}/grants"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["connectionId"], serde_json::json!(conn_id));
    assert_eq!(items[0]["kind"], "workspace");
    assert_eq!(items[0]["status"], "active");
}

#[tokio::test(flavor = "current_thread")]
async fn http_list_application_grants_is_now_deprecated_stub() {
    // Round 95 修复：application 概念在 v3 schema 已删除；端点保留 URL 兼容
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let app_id = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/tool-applications/{app_id}/grants"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["deprecated"], true);
}

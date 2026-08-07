//! Round 358: HTTP `/api/issues/:id/comments` 端点 round-trip 验证
//! `presentation` / `metadata` 字段。
//!
//! 业务背景：
//! - Rust 端 RPC 路径（`escalate_db.rs::create_comment_with_display`）已经会写
//!   `presentation`（含 title 行/rows）+ `metadata`（recovery action_id 引用等）
//! - HTTP 路由层 `add_comment` 之前的 `CommentBody` 不接受这俩字段，所以
//!   系统评论的展示元数据无法通过 HTTP 上传 → 端到端 gap
//! - 本轮闭合：扩展 `CommentBody` 入参 + 端到端 round-trip 测试
//!
//! Node 参考：`apps/server/src/http/issues/*` 的 `POST /issues/:id/comments`
//! 支持任意 JSON body，序列化保留所有 `presentation` / `metadata` 字段

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
        Arc::new(WsState::new(realtime.clone(), "test")),
        realtime,
    )
}

fn unique_issue_prefix(suffix: &str) -> String {
    let unique = Uuid::new_v4().simple().to_string();
    let trimmed: String = unique.chars().take(8).collect();
    format!("{trimmed}{suffix}")
}

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r358-c-{id}"))
        .bind(unique_issue_prefix("R358"))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_user(db: &Db) -> String {
    let id = format!("r358-user-{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO \"user\" (id, email, name, created_at, updated_at) \
         VALUES ($1, $2, $3, now(), now()) ON CONFLICT (id) DO NOTHING",
    )
    .bind(&id)
    .bind(format!("{id}@example.com"))
    .bind(&id)
    .execute(db.pool())
    .await
    .expect("insert user");
    id
}

async fn insert_session(db: &Db, user_id: &str) -> String {
    let token = format!("r358-sess-{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO session (id, user_id, token, expires_at, created_at, updated_at) \
         VALUES ($1, $2, $3, now() + interval '1 day', now(), now())",
    )
    .bind(Uuid::new_v4().simple().to_string())
    .bind(user_id)
    .bind(&token)
    .execute(db.pool())
    .await
    .expect("insert session");
    token
}

async fn insert_issue(db: &Db, company_id: Uuid, title: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, origin_kind, origin_fingerprint) \
         VALUES ($1, $2, $3, 'user', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(title)
    .bind(format!("r358-fp-{id}"))
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_comments WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM session WHERE user_id LIKE 'r358-user-%'")
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM \"user\" WHERE id LIKE 'r358-user-%'")
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
    session: Option<&str>,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let mut req = Request::builder()
        .method(method)
        .header("content-type", "application/json")
        .uri(path);
    if let Some(tok) = session {
        req = req.header("cookie", format!("paperclip_session={tok}"));
    }
    let response = app
        .clone()
        .oneshot(
            req.body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .expect("request");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn call_no_body(
    app: &axum::Router,
    method: &str,
    path: &str,
    session: Option<&str>,
) -> (u16, Value) {
    call(app, method, path, json!({}), session).await
}

/// 端到端：POST 带 presentation/metadata 的 comment → GET 回来完全保留
#[tokio::test(flavor = "current_thread")]
async fn post_comment_with_presentation_and_metadata_round_trips() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let user = insert_user(&db).await;
    let token = insert_session(&db, &user).await;
    let issue_id = insert_issue(&db, company_id, "r358-presentation-metadata").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    let presentation = json!({
        "tone": "warning",
        "title": "Recovery: source escalation (system notice)",
        "rows": [
            {"label": "Cause", "value": "successful_run_missing_state"},
            {"label": "Latest run", "value": "run-12345"},
        ]
    });
    let metadata = json!({
        "category": "recovery_notice",
        "recovery_action_id": "00000000-0000-0000-0000-000000000abc",
        "actor": "system"
    });

    let (post_status, post_body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/comments"),
        json!({
            "body": "system: source escalation required",
            "author_user_id": user,
            "presentation": presentation,
            "metadata": metadata,
        }),
        Some(&token),
    )
    .await;
    assert_eq!(post_status, 201, "POST comment: {post_body}");

    // POST 响应里应该立刻回写 presentation/metadata
    assert_eq!(
        post_body["presentation"], presentation,
        "post returned presentation differs"
    );
    assert_eq!(
        post_body["metadata"], metadata,
        "post returned metadata differs"
    );

    // GET 端点拉回来的数组里也必须保留
    let (get_status, get_body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/comments"),
        None,
    )
    .await;
    assert_eq!(get_status, 200, "GET list: {get_body}");
    let arr = get_body.as_array().expect("array");
    assert_eq!(arr.len(), 1, "expected 1 comment: {get_body}");
    assert_eq!(
        arr[0]["presentation"], presentation,
        "GET presentation differs"
    );
    assert_eq!(arr[0]["metadata"], metadata, "GET metadata differs");
    assert_eq!(arr[0]["body"], "system: source escalation required");
    assert_eq!(arr[0]["author_user_id"], user);

    cleanup(&db, company_id).await;
}

/// Backward-compatible：旧客户端不传 presentation/metadata → 响应里是 null
#[tokio::test(flavor = "current_thread")]
async fn post_comment_without_presentation_metadata_still_works() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let user = insert_user(&db).await;
    let token = insert_session(&db, &user).await;
    let issue_id = insert_issue(&db, company_id, "r358-legacy").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    let (post_status, post_body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/comments"),
        json!({
            "body": "plain text comment",
            "author_user_id": user,
        }),
        Some(&token),
    )
    .await;
    assert_eq!(post_status, 201, "POST: {post_body}");
    assert!(
        post_body["presentation"].is_null(),
        "presentation should be null"
    );
    assert!(post_body["metadata"].is_null(), "metadata should be null");

    let (_, get_body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/comments"),
        None,
    )
    .await;
    let arr = get_body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert!(arr[0]["presentation"].is_null());
    assert!(arr[0]["metadata"].is_null());

    cleanup(&db, company_id).await;
}

/// presentation/metadata 各自独立 → 缺一不互相影响
#[tokio::test(flavor = "current_thread")]
async fn presentation_only_and_metadata_only_each_round_trip() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let user = insert_user(&db).await;
    let token = insert_session(&db, &user).await;
    let issue_id = insert_issue(&db, company_id, "r358-partial").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // 只有 presentation
    let presentation = json!({"title": "Recovery: workspace validation failed"});
    let (s1, b1) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/comments"),
        json!({
            "body": "with-presentation",
            "author_user_id": user,
            "presentation": presentation,
        }),
        Some(&token),
    )
    .await;
    assert_eq!(s1, 201, "post-presentation: {b1}");
    assert_eq!(b1["presentation"], presentation);
    assert!(b1["metadata"].is_null());

    // 只有 metadata
    let metadata = json!({"category": "lifecycle_event", "actor": "system"});
    let (s2, b2) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/comments"),
        json!({
            "body": "with-metadata",
            "author_user_id": user,
            "metadata": metadata,
        }),
        Some(&token),
    )
    .await;
    assert_eq!(s2, 201, "post-metadata: {b2}");
    assert!(b2["presentation"].is_null());
    assert_eq!(b2["metadata"], metadata);

    // GET 拉回来两条都必须各自正确
    let (_, get_body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/comments"),
        None,
    )
    .await;
    let arr = get_body.as_array().expect("array");
    assert_eq!(arr.len(), 2);

    let by_body: std::collections::HashMap<&str, &Value> = arr
        .iter()
        .map(|c| (c["body"].as_str().unwrap_or(""), c))
        .collect();
    let with_pres = by_body["with-presentation"];
    let with_meta = by_body["with-metadata"];
    assert_eq!(with_pres["presentation"], presentation);
    assert!(with_pres["metadata"].is_null());
    assert!(with_meta["presentation"].is_null());
    assert_eq!(with_meta["metadata"], metadata);

    cleanup(&db, company_id).await;
}

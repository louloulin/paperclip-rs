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
        Arc::new(WsState::new(
            realtime.clone(),
            "test",
        )),
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
        .bind(format!("http-issues-{id}"))
        .bind(unique_issue_prefix("ISU"))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_user(db: &Db) -> String {
    let id = format!("user-{}", Uuid::new_v4().simple());
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

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, title, adapter_type, status) \
         VALUES ($1, $2, $3, 'worker', 'Worker', 'process', 'idle') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("agent-{id}"))
    .execute(db.pool())
    .await
    .expect("insert agent");
    id
}

async fn insert_session(db: &Db, user_id: &str) -> String {
    let token = format!("sess-{}", Uuid::new_v4().simple());
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
    .bind(format!("fp-{id}"))
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: serde_json::Value,
    session: Option<&str>,
) -> (u16, serde_json::Value) {
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
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn call_no_body(
    app: &axum::Router,
    method: &str,
    path: &str,
    session: Option<&str>,
) -> (u16, serde_json::Value) {
    call(app, method, path, serde_json::json!({}), session).await
}

#[tokio::test(flavor = "current_thread")]
async fn issue_children_lifecycle() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let parent = insert_issue(&db, company_id, "Parent").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{parent}/children"),
        serde_json::json!({ "title": "Child A", "priority": "high" }),
        None,
    )
    .await;
    assert_eq!(status, 201, "create child 1: {body}");
    let child_id = body["id"].as_str().expect("child id").to_string();
    let child_uuid: Uuid = child_id.parse().expect("uuid");

    let (_, body2) = call(
        &app,
        "POST",
        &format!("/api/issues/{parent}/children"),
        serde_json::json!({ "title": "Child B" }),
        None,
    )
    .await;
    assert_eq!(
        body2["parent_id"].as_str(),
        Some(parent.to_string()).as_deref()
    );

    let (status, body) =
        call_no_body(&app, "GET", &format!("/api/issues/{parent}/children"), None).await;
    assert_eq!(status, 200, "list children: {body}");
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    let titles: Vec<&str> = arr
        .iter()
        .map(|v| v["title"].as_str().unwrap_or(""))
        .collect();
    assert!(titles.contains(&"Child A"));
    assert!(titles.contains(&"Child B"));

    // 删 parent 不会自动级联，但应能列出
    let _ = child_uuid;
}

#[tokio::test(flavor = "current_thread")]
async fn issue_comments_crud() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let user = insert_user(&db).await;
    let token = insert_session(&db, &user).await;
    let issue_id = insert_issue(&db, company_id, "Commented issue").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // POST comment
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/comments"),
        serde_json::json!({
            "body": "first comment",
            "author_user_id": user,
        }),
        Some(&token),
    )
    .await;
    assert_eq!(status, 201, "post comment: {body}");
    let cid = body["id"].as_str().expect("comment id").to_string();

    // GET comments
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/comments"),
        None,
    )
    .await;
    assert_eq!(status, 200, "list comments: {body}");
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["body"], "first comment");

    // PATCH comment
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/issues/{issue_id}/comments/{cid}"),
        serde_json::json!({ "body": "edited", "author_user_id": user }),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "edit comment: {body}");
    assert_eq!(body["body"], "edited");

    // DELETE comment
    let (status, _) = call_no_body(
        &app,
        "DELETE",
        &format!("/api/issues/{issue_id}/comments/{cid}"),
        Some(&token),
    )
    .await;
    assert_eq!(status, 204);

    let (_, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/comments"),
        None,
    )
    .await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn issue_labels_crud_and_assignment() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Labeled issue").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // POST label
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/labels"),
        serde_json::json!({ "name": "bug", "color": "#ff0000" }),
        None,
    )
    .await;
    assert_eq!(status, 201, "create label: {body}");
    let label_id = body["id"].as_str().expect("label id").to_string();

    // GET labels
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/labels"),
        None,
    )
    .await;
    assert_eq!(status, 200, "list labels: {body}");
    let arr = body.as_array().expect("array");
    assert!(arr.iter().any(|v| v["name"] == "bug"));

    // assign label
    let (status, _) = call_no_body(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/labels/{label_id}"),
        None,
    )
    .await;
    assert_eq!(status, 204, "assign label");

    // unassign
    let (status, _) = call_no_body(
        &app,
        "DELETE",
        &format!("/api/issues/{issue_id}/labels/{label_id}"),
        None,
    )
    .await;
    assert_eq!(status, 204, "unassign label");

    // delete label
    let (status, _) = call_no_body(&app, "DELETE", &format!("/api/labels/{label_id}"), None).await;
    assert_eq!(
        status, 404,
        "label deletion via /api/labels/:id without company_id should 404"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn issue_read_state_upsert_and_get() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let user = insert_user(&db).await;
    let token = insert_session(&db, &user).await;
    let issue_id = insert_issue(&db, company_id, "Read state issue").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // initial GET → null
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/read"),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "get read: {body}");
    assert!(body.is_null(), "expected null before upsert, got {body}");

    // PUT upsert
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/issues/{issue_id}/read"),
        serde_json::json!({}),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "upsert read: {body}");
    assert!(body["last_read_at"].is_string());

    // GET after upsert
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/read"),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "get read after: {body}");
    assert_eq!(body["user_id"], user);
}

#[tokio::test(flavor = "current_thread")]
async fn issue_inbox_archive_lifecycle() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let user = insert_user(&db).await;
    let token = insert_session(&db, &user).await;
    let issue_id = insert_issue(&db, company_id, "Inbox issue").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    let (status, _) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/inbox-archive"),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200);

    // PUT archive
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/issues/{issue_id}/inbox-archive"),
        serde_json::json!({}),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "archive: {body}");
    assert_eq!(body["issue_id"], issue_id.to_string());

    // DELETE unarchive
    let (status, _) = call_no_body(
        &app,
        "DELETE",
        &format!("/api/issues/{issue_id}/inbox-archive"),
        Some(&token),
    )
    .await;
    assert_eq!(status, 204, "unarchive");
}

#[tokio::test(flavor = "current_thread")]
async fn issue_release_and_force_release() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Release issue").await;
    let run_id = Uuid::new_v4();

    // 先创建 agent + heartbeat_run 以满足 checkout_run_id 的 FK 约束
    let agent_id = insert_agent(&db, company_id).await;
    let run_uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source) \
         VALUES ($1, $2, $3, 'queued', 'manual_test') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(run_uuid)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert heartbeat_run");

    // 设置 checkout 锁
    sqlx::query(
        "UPDATE issues SET checkout_run_id = $1, execution_locked_at = now() WHERE id = $2",
    )
    .bind(run_uuid)
    .bind(issue_id)
    .execute(db.pool())
    .await
    .expect("checkout");
    let run_id = run_uuid;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // release 匹配 run_id
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/release"),
        serde_json::json!({ "run_id": run_id }),
        None,
    )
    .await;
    assert_eq!(status, 200, "release: {body}");
    assert!(
        body["checkout_run_id"].is_null(),
        "checkout_run_id should be cleared"
    );

    // 重新 checkout，再 force-release
    let run_uuid2 = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source) \
         VALUES ($1, $2, $3, 'queued', 'manual_test') ON CONFLICT (id) DO NOTHING",
    )
    .bind(run_uuid2)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert heartbeat_run 2");
    sqlx::query(
        "UPDATE issues SET checkout_run_id = $1, execution_locked_at = now() WHERE id = $2",
    )
    .bind(run_uuid2)
    .bind(issue_id)
    .execute(db.pool())
    .await
    .expect("re-checkout");

    let (status, body) = call_no_body(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/admin/force-release"),
        None,
    )
    .await;
    assert_eq!(status, 200, "force-release: {body}");
    assert!(body["checkout_run_id"].is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn issue_watchdog_upsert_get_delete() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Watched issue").await;
    let agent_id = insert_agent(&db, company_id).await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // 初始无 watchdog
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/watchdog"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.is_null(), "expected null watchdog, got {body}");

    // PUT upsert
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/issues/{issue_id}/watchdog"),
        serde_json::json!({
            "watchdog_agent_id": agent_id,
            "instructions": "watch this issue",
        }),
        None,
    )
    .await;
    assert_eq!(status, 200, "upsert: {body}");
    assert_eq!(body["status"], "active");
    let wid = body["id"].as_str().expect("watchdog id").to_string();

    // GET 后存在
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/watchdog"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["id"].as_str(), Some(wid.as_str()));

    // DELETE
    let (status, body) = call_no_body(
        &app,
        "DELETE",
        &format!("/api/issues/{issue_id}/watchdog"),
        None,
    )
    .await;
    assert_eq!(status, 200, "delete: {body}");
    assert_eq!(body["ok"], true);

    // GET 再次为 null
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/watchdog"),
        None,
    )
    .await;
    assert!(body.is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn issue_work_products_crud() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "WP issue").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // POST work product
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/work-products"),
        serde_json::json!({
            "type": "pull_request",
            "title": "Initial PR",
            "summary": "first draft",
            "is_primary": true,
        }),
        None,
    )
    .await;
    assert_eq!(status, 201, "create: {body}");
    let wp_id = body["id"].as_str().expect("wp id").to_string();
    assert_eq!(body["title"], "Initial PR");
    assert_eq!(body["is_primary"], true);
    assert_eq!(body["provider"], "paperclip");

    // GET via issue endpoint
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/work-products"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);

    // GET via /api/work-products/:id
    let (status, body) =
        call_no_body(&app, "GET", &format!("/api/work-products/{wp_id}"), None).await;
    assert_eq!(status, 200, "get: {body}");
    assert_eq!(body["id"].as_str(), Some(wp_id.as_str()));

    // PATCH
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/work-products/{wp_id}"),
        serde_json::json!({ "title": "Updated PR", "review_state": "approved" }),
        None,
    )
    .await;
    assert_eq!(status, 200, "patch: {body}");
    assert_eq!(body["title"], "Updated PR");
    assert_eq!(body["review_state"], "approved");

    // DELETE
    let (status, _) =
        call_no_body(&app, "DELETE", &format!("/api/work-products/{wp_id}"), None).await;
    assert_eq!(status, 204);

    // GET 404
    let (status, _) = call_no_body(&app, "GET", &format!("/api/work-products/{wp_id}"), None).await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "current_thread")]
async fn issue_recovery_actions_list_and_resolve() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Recovery issue").await;
    let agent_id = insert_agent(&db, company_id).await;

    // 直接在 SQL 中插入一个 active recovery action
    let action_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_recovery_actions \
            (id, company_id, source_issue_id, kind, status, owner_type, owner_agent_id, \
             cause, fingerprint, evidence, next_action) \
         VALUES ($1, $2, $3, 'restart', 'active', 'agent', $4, 'stalled', $5, '{}'::jsonb, 'reassign')",
    )
    .bind(action_id)
    .bind(company_id)
    .bind(issue_id)
    .bind(agent_id)
    .bind(format!("fp-{action_id}"))
    .execute(db.pool())
    .await
    .expect("insert recovery action");

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // GET 列表
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/recovery-actions"),
        None,
    )
    .await;
    assert_eq!(status, 200, "list: {body}");
    assert!(body["active"].is_object(), "expected active object");
    assert_eq!(body["actions"].as_array().unwrap().len(), 1);

    // POST resolve
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/recovery-actions/resolve"),
        serde_json::json!({
            "action_id": action_id,
            "outcome": "reassigned",
            "resolution_note": "reassigned to new agent",
        }),
        None,
    )
    .await;
    assert_eq!(status, 200, "resolve: {body}");
    assert_eq!(body["status"], "resolved");
    assert_eq!(body["outcome"], "reassigned");

    // GET 列表应为空
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/recovery-actions"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body["active"].is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn issue_documents_crud_and_revisions() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Doc issue").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // PUT 首次创建 document
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/issues/{issue_id}/documents/plan"),
        serde_json::json!({ "title": "Plan", "body": "# v1 content" }),
        None,
    )
    .await;
    assert_eq!(status, 200, "create: {body}");
    assert_eq!(body["title"], "Plan");
    assert_eq!(body["latest_revision_number"], 1);
    let doc_id = body["id"].as_str().expect("doc id").to_string();

    // PUT 第二次 = 更新 + 新 revision
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/issues/{issue_id}/documents/plan"),
        serde_json::json!({ "body": "# v2 content" }),
        None,
    )
    .await;
    assert_eq!(status, 200, "update: {body}");
    assert_eq!(body["latest_revision_number"], 2);
    assert!(body["latest_body"].as_str().unwrap_or("").contains("v2"));

    // GET document
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/documents/plan"),
        None,
    )
    .await;
    assert_eq!(status, 200, "get: {body}");
    assert_eq!(body["id"].as_str(), Some(doc_id.as_str()));

    // LIST documents
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/documents"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);

    // GET revisions
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/documents/plan/revisions"),
        None,
    )
    .await;
    assert_eq!(status, 200, "revs: {body}");
    let revs = body.as_array().expect("array");
    assert_eq!(revs.len(), 2);

    // RESTORE revision 1
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/documents/plan/revisions"),
        serde_json::json!({ "revision_number": 1 }),
        None,
    )
    .await;
    assert_eq!(status, 200, "restore: {body}");
    assert_eq!(body["revision_number"], 3);
    assert!(body["body"].as_str().unwrap_or("").contains("v1"));
}

#[tokio::test(flavor = "current_thread")]
async fn issue_document_lock_unlock() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Lock issue").await;
    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // 先创建 document
    call(
        &app,
        "PUT",
        &format!("/api/issues/{issue_id}/documents/spec"),
        serde_json::json!({ "body": "spec body" }),
        None,
    )
    .await;

    // LOCK
    let (status, body) = call_no_body(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/documents/spec/lock"),
        None,
    )
    .await;
    assert_eq!(status, 200, "lock: {body}");
    assert!(body["locked_at"].is_string());

    // 再次 LOCK 应冲突
    let (status, _) = call_no_body(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/documents/spec/lock"),
        None,
    )
    .await;
    assert_eq!(status, 409, "second lock should conflict");

    // UNLOCK
    let (status, body) = call_no_body(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/documents/spec/unlock"),
        None,
    )
    .await;
    assert_eq!(status, 200, "unlock: {body}");
    assert!(body["locked_at"].is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn issue_document_annotations_crud() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Annot issue").await;
    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // 先创建 document
    call(
        &app,
        "PUT",
        &format!("/api/issues/{issue_id}/documents/readme"),
        serde_json::json!({ "body": "Hello world" }),
        None,
    )
    .await;

    // POST annotation thread + 首条 comment
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/documents/readme/annotations"),
        serde_json::json!({
            "selected_text": "Hello",
            "normalized_start": 0,
            "normalized_end": 5,
            "markdown_start": 0,
            "markdown_end": 5,
            "body": "first comment"
        }),
        None,
    )
    .await;
    assert_eq!(status, 201, "create thread: {body}");
    let thread_id = body["id"].as_str().expect("thread id").to_string();
    assert_eq!(body["status"], "open");

    // LIST threads
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/documents/readme/annotations"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);

    // POST comment on thread
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/documents/readme/annotations/{thread_id}"),
        serde_json::json!({ "body": "second comment" }),
        None,
    )
    .await;
    assert_eq!(status, 201, "comment: {body}");
    assert_eq!(body["body"], "second comment");

    // GET thread with comments
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/documents/readme/annotations/{thread_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "get thread: {body}");
    let comments = body["comments"].as_array().expect("comments");
    assert_eq!(comments.len(), 2);

    // PATCH resolve
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/issues/{issue_id}/documents/readme/annotations/{thread_id}"),
        serde_json::json!({}),
        None,
    )
    .await;
    assert_eq!(status, 200, "resolve: {body}");
    assert_eq!(body["status"], "resolved");
}

#[tokio::test(flavor = "current_thread")]
async fn issue_document_delete() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Del doc issue").await;
    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    call(
        &app,
        "PUT",
        &format!("/api/issues/{issue_id}/documents/temp"),
        serde_json::json!({ "body": "tmp" }),
        None,
    )
    .await;

    let (status, _) = call_no_body(
        &app,
        "DELETE",
        &format!("/api/issues/{issue_id}/documents/temp"),
        None,
    )
    .await;
    assert_eq!(status, 204);

    let (status, _) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/documents/temp"),
        None,
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "current_thread")]
async fn issue_approvals_link_list_decide() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Approvals issue").await;
    let agent_id = insert_agent(&db, company_id).await;

    // 先创建一个 approval
    let approval_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO approvals (id, company_id, type, status, payload) \
         VALUES ($1, $2, 'cost', 'pending', '{}'::jsonb)",
    )
    .bind(approval_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert approval");

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // POST link
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/approvals"),
        serde_json::json!({ "approval_id": approval_id, "linked_by_user_id": "u1" }),
        None,
    )
    .await;
    assert_eq!(status, 200, "link: {body}");
    assert_eq!(body["approval_id"], approval_id.to_string());

    // GET list
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/approvals"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert!(arr[0]["approval"].is_object(), "expected approval detail");

    // PATCH decide
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/issues/{issue_id}/approvals/{approval_id}"),
        serde_json::json!({ "decision": "approved", "decision_note": "lgtm" }),
        None,
    )
    .await;
    assert_eq!(status, 200, "decide: {body}");
    assert_eq!(body["status"], "approved");
    assert_eq!(body["decisionNote"], "lgtm");
    let _ = agent_id; // silence unused

    // DELETE unlink
    let (status, _) = call_no_body(
        &app,
        "DELETE",
        &format!("/api/issues/{issue_id}/approvals/{approval_id}"),
        None,
    )
    .await;
    assert_eq!(status, 204);
}

#[tokio::test(flavor = "current_thread")]
async fn issue_thread_interactions_crud() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Interactions issue").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // POST create
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/interactions"),
        serde_json::json!({
            "kind": "approval_request",
            "title": "Need approval",
            "summary": "please review",
            "payload": { "context": "test" },
        }),
        None,
    )
    .await;
    assert_eq!(status, 201, "create: {body}");
    let iid = body["id"].as_str().expect("id").to_string();
    assert_eq!(body["status"], "pending");

    // GET list
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/interactions"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body.as_array().unwrap().len(), 1);

    // GET one
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/interactions/{iid}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "get: {body}");
    assert_eq!(body["kind"], "approval_request");

    // PATCH resolve
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/issues/{issue_id}/interactions/{iid}"),
        serde_json::json!({ "status": "accepted", "result": { "decision": "approved" } }),
        None,
    )
    .await;
    assert_eq!(status, 200, "resolve: {body}");
    assert_eq!(body["status"], "accepted");
    assert!(body["resolved_at"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn issue_feedback_votes_crud() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Feedback issue").await;

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // POST vote
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/feedback-votes"),
        serde_json::json!({
            "target_type": "issue",
            "target_id": issue_id.to_string(),
            "vote": "up",
            "reason": "looks good",
            "author_user_id": "reviewer-1",
        }),
        None,
    )
    .await;
    assert_eq!(status, 201, "vote: {body}");
    assert_eq!(body["vote"], "up");
    assert_eq!(body["author_user_id"], "reviewer-1");

    // GET list
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/feedback-votes"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["reason"], "looks good");
}

#[tokio::test(flavor = "current_thread")]
async fn issue_attachments_crud() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Attach issue").await;
    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // POST attachment
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/attachments"),
        serde_json::json!({
            "provider": "local",
            "object_key": "uploads/test.png",
            "content_type": "image/png",
            "byte_size": 1024,
            "sha256": "abc123",
            "original_filename": "test.png"
        }),
        None,
    )
    .await;
    assert_eq!(status, 201, "create: {body}");
    let attachment_id = body["id"].as_str().expect("attach id").to_string();
    assert!(body["asset"].is_object(), "expected asset detail");

    // LIST
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/attachments"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body.as_array().unwrap().len(), 1);

    // GET
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/attachments/{attachment_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "get: {body}");
    assert_eq!(body["asset"]["sha256"], "abc123");

    // DELETE
    let (status, _) = call_no_body(
        &app,
        "DELETE",
        &format!("/api/attachments/{attachment_id}"),
        None,
    )
    .await;
    assert_eq!(status, 204);

    // GET 404
    let (status, _) = call_no_body(
        &app,
        "GET",
        &format!("/api/attachments/{attachment_id}"),
        None,
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "current_thread")]
async fn issue_count_and_search() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    insert_issue(&db, company_id, "Search alpha").await;
    insert_issue(&db, company_id, "Search beta").await;
    insert_issue(&db, company_id, "Other gamma").await;
    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // count all
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/issues/count"),
        None,
    )
    .await;
    assert_eq!(status, 200, "count: {body}");
    assert!(body["count"].as_i64().unwrap() >= 3);

    // search
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/search?q=Search"),
        None,
    )
    .await;
    assert_eq!(status, 200, "search: {body}");
    assert!(body["count"].as_i64().unwrap() >= 2);
    let results = body["results"].as_array().expect("array");
    assert!(results.len() >= 2);

    // search empty query → 400
    let (status, _) = call_no_body(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/search?q="),
        None,
    )
    .await;
    assert_eq!(status, 400);
}

#[tokio::test(flavor = "current_thread")]
async fn issue_diagnostics_and_external_objects() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Diag issue").await;

    // 创建一个子 issue 作为 blocker
    let blocker = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, parent_id, title, origin_kind, origin_fingerprint) \
         VALUES ($1, $2, $3, 'Blocker A', 'user', $4)",
    )
    .bind(blocker)
    .bind(company_id)
    .bind(issue_id)
    .bind(format!("fp-{blocker}"))
    .execute(db.pool())
    .await
    .expect("insert blocker");

    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // diagnostics/blockers
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/diagnostics/blockers"),
        None,
    )
    .await;
    assert_eq!(status, 200, "blockers: {body}");
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"].as_str(), Some(blocker.to_string().as_str()));

    // diagnostics/wakes
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/diagnostics/wakes?limit=5"),
        None,
    )
    .await;
    assert_eq!(status, 200, "wakes: {body}");
    assert!(body.is_array());

    // diagnostics/subtree
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/diagnostics/subtree"),
        None,
    )
    .await;
    assert_eq!(status, 200, "subtree: {body}");
    assert!(body["children"].is_array());
    assert_eq!(body["children"].as_array().unwrap().len(), 1);

    // external-objects
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/external-objects"),
        None,
    )
    .await;
    assert_eq!(status, 200, "external: {body}");
    assert!(body.is_array());

    // external-object-summary
    let (status, body) = call_no_body(
        &app,
        "GET",
        &format!("/api/issues/{issue_id}/external-object-summary"),
        None,
    )
    .await;
    assert_eq!(status, 200, "summary: {body}");
    assert_eq!(body["total_objects"], 0);
}

#[tokio::test(flavor = "current_thread")]
async fn issue_tree_control() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "Tree control issue").await;
    let state = test_state(db.clone());
    let app = routes::issues::router().with_state(state);

    // monitor/check-now
    let (status, body) = call_no_body(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/monitor/check-now"),
        None,
    )
    .await;
    assert_eq!(status, 200, "check_now: {body}");
    assert!(body["monitor_wake_requested_at"].is_string());

    // scheduled-retry/retry-now
    let (status, body) = call_no_body(
        &app,
        "POST",
        &format!("/api/issues/{issue_id}/scheduled-retry/retry-now"),
        None,
    )
    .await;
    assert_eq!(status, 200, "retry_now: {body}");
    assert!(body["monitor_next_check_at"].is_string());
    assert!(body["monitor_attempt_count"].as_i64().unwrap() >= 1);
}

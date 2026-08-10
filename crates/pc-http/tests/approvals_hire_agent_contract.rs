//! R584: `/api/approvals/:id/approve` 与 `/reject` 通过 `ApprovalService` 走
//! `HireAgentApprovalHook` + `DbHireAgentOps` 的端到端契约。
//!
//! 这些测试覆盖核心 hire_agent 决策链路：
//! - approve (ActivateExisting 模式)：pending_approval agent → idle
//! - approve (CreateNew 模式)：新建 agent
//! - approve 带 budget：自动创建 budget policy
//! - reject：agent → terminated

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

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("ap-hire-{id}"))
    .bind(format!("AH{}", &id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn insert_user_membership(db: &Db, company_id: Uuid) {
    sqlx::query(
        "INSERT INTO company_memberships (company_id, principal_type, principal_id, membership_role, status, created_at, updated_at) \
         VALUES ($1, 'user', 'user-1', 'admin', 'active', now(), now())",
    )
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert membership");
}

async fn insert_agent_in_status(db: &Db, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, \
         adapter_config, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', $4, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Agent {id}"))
    .bind(status)
    .execute(db.pool())
    .await
    .expect("insert agent");
    id
}

async fn insert_hire_approval(db: &Db, company_id: Uuid, payload: Value) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO approvals (id, company_id, type, status, payload, requested_by_user_id, \
                              created_at, updated_at) \
         VALUES ($1, $2, 'hire_agent', 'pending', $3, 'user-1', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(payload)
    .execute(db.pool())
    .await
    .expect("insert approval");
    id
}

async fn fetch_agent_status(db: &Db, id: Uuid) -> String {
    sqlx::query_scalar::<_, String>("SELECT status FROM agents WHERE id = $1")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .expect("fetch status")
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let payload = body
        .as_ref()
        .map(|v| serde_json::to_vec(v).expect("serialize"))
        .unwrap_or_default();
    // 注入 System AuthContext — 模拟内部 service 调用，绕过 auth/authz 检查
    // （生产环境由 auth_layer 中间件完成同样的事）。
    let mut request = Request::builder()
        .method(method)
        .header("content-type", "application/json")
        .uri(path)
        .body(Body::from(payload))
        .expect("request");
    request
        .extensions_mut()
        .insert(pc_auth::AuthContext::system());
    let response = app.clone().oneshot(request).await.expect("response");
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

// =============================================================================
// R584: hire_agent approval e2e (HTTP layer → ApprovalService → HireAgentApprovalHook → DbHireAgentOps)
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn r584_http_approve_activate_pending_agent_to_idle() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    insert_user_membership(&db, company_id).await;
    let agent_id = insert_agent_in_status(&db, company_id, "pending_approval").await;
    let approval_id =
        insert_hire_approval(&db, company_id, json!({ "agentId": agent_id.to_string() })).await;
    let app = routes::approvals::router().with_state(test_state(db.clone()));

    // approve via HTTP
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/approvals/{approval_id}/approve"),
        Some(json!({ "note": "go", "decided_by": "user-1" })),
    )
    .await;
    assert_eq!(status, 200, "approve: {body}");
    assert_eq!(body["status"], "approved");

    // 副作用：agent 状态变更
    assert_eq!(fetch_agent_status(&db, agent_id).await, "idle");
}

#[tokio::test(flavor = "current_thread")]
async fn r584_http_reject_pending_agent_terminates_it() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    insert_user_membership(&db, company_id).await;
    let agent_id = insert_agent_in_status(&db, company_id, "pending_approval").await;
    let approval_id =
        insert_hire_approval(&db, company_id, json!({ "agentId": agent_id.to_string() })).await;
    let app = routes::approvals::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/approvals/{approval_id}/reject"),
        Some(json!({ "note": "no", "decided_by": "user-1" })),
    )
    .await;
    assert_eq!(status, 200, "reject: {body}");
    assert_eq!(body["status"], "rejected");

    // 副作用：agent 应被 terminate
    assert_eq!(fetch_agent_status(&db, agent_id).await, "terminated");
}

#[tokio::test(flavor = "current_thread")]
async fn r584_http_approve_create_new_agent_with_budget_creates_policy() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    insert_user_membership(&db, company_id).await;
    // payload 无 agentId → CreateNew 路径
    let approval_id = insert_hire_approval(
        &db,
        company_id,
        json!({
            "name": "Auto Bot",
            "role": "general",
            "adapterType": "process",
            "budgetMonthlyCents": 5000
        }),
    )
    .await;
    let app = routes::approvals::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/approvals/{approval_id}/approve"),
        Some(json!({ "decided_by": "user-1" })),
    )
    .await;
    assert_eq!(status, 200, "approve create: {body}");

    // 应有新 agent 被创建
    let new_agent_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agents WHERE company_id = $1 AND name = 'Auto Bot'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .expect("count agents");
    assert_eq!(new_agent_count, 1, "expected exactly one new agent");

    // budget policy 应被创建
    let policy_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM budget_policies WHERE company_id = $1")
            .bind(company_id)
            .fetch_one(db.pool())
            .await
            .expect("count policies");
    assert_eq!(policy_count, 1, "expected budget policy");
}

// =============================================================================
// R585: 其他 service 化端点 e2e
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn r585_http_decide_endpoint_routes_through_service_approve() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    insert_user_membership(&db, company_id).await;
    let agent_id = insert_agent_in_status(&db, company_id, "pending_approval").await;
    let approval_id =
        insert_hire_approval(&db, company_id, json!({ "agentId": agent_id.to_string() })).await;
    let app = routes::approvals::router().with_state(test_state(db.clone()));

    // /decide 是通用端点，应等价于 /approve
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/approvals/{approval_id}/decide"),
        Some(json!({
            "status": "approved",
            "decided_by": "user-1",
            "note": "via decide endpoint"
        })),
    )
    .await;
    assert_eq!(status, 200, "decide: {body}");
    assert_eq!(body["status"], "approved");
    // 副作用：agent 已激活
    assert_eq!(fetch_agent_status(&db, agent_id).await, "idle");
}

#[tokio::test(flavor = "current_thread")]
async fn r585_http_decide_endpoint_routes_through_service_reject() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    insert_user_membership(&db, company_id).await;
    let agent_id = insert_agent_in_status(&db, company_id, "pending_approval").await;
    let approval_id =
        insert_hire_approval(&db, company_id, json!({ "agentId": agent_id.to_string() })).await;
    let app = routes::approvals::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/approvals/{approval_id}/decide"),
        Some(json!({
            "status": "rejected",
            "decided_by": "user-1",
            "note": "nope"
        })),
    )
    .await;
    assert_eq!(status, 200, "decide: {body}");
    assert_eq!(body["status"], "rejected");
    assert_eq!(fetch_agent_status(&db, agent_id).await, "terminated");
}

#[tokio::test(flavor = "current_thread")]
async fn r585_http_request_revision_endpoint_routes_through_service() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    insert_user_membership(&db, company_id).await;
    let approval_id = insert_hire_approval(&db, company_id, json!({ "name": "Test Bot" })).await;
    let app = routes::approvals::router().with_state(test_state(db.clone()));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/approvals/{approval_id}/request-revision"),
        Some(json!({ "decisionNote": "please adjust budget" })),
    )
    .await;
    assert_eq!(status, 200, "request-revision: {body}");
    // service 端实现是"更新 note + 仍为 pending"，验证 row 字段存在
    let body_id = body["id"].as_str().unwrap_or_default();
    assert_eq!(body_id, approval_id.to_string());
}

#[tokio::test(flavor = "current_thread")]
async fn r585_http_comments_endpoints_route_through_service() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    insert_user_membership(&db, company_id).await;
    let approval_id = insert_hire_approval(&db, company_id, json!({ "name": "Comment Bot" })).await;
    let app = routes::approvals::router().with_state(test_state(db.clone()));

    // 先 add 一条
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/approvals/{approval_id}/comments"),
        Some(json!({
            "body": "first review comment",
            "authorUserId": "user-1"
        })),
    )
    .await;
    assert_eq!(status, 201, "add comment: {body}");
    assert!(!body["id"].as_str().unwrap_or_default().is_empty());

    // 再 list
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/approvals/{approval_id}/comments"),
        None,
    )
    .await;
    assert_eq!(status, 200, "list comments: {body}");
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["body"], "first review comment");
    assert_eq!(items[0]["authorUserId"], "user-1");
}

#[tokio::test(flavor = "current_thread")]
async fn r585_http_add_comment_rejects_empty_body() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db).await;
    insert_user_membership(&db, company_id).await;
    let approval_id = insert_hire_approval(&db, company_id, json!({ "name": "Bot" })).await;
    let app = routes::approvals::router().with_state(test_state(db.clone()));

    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/approvals/{approval_id}/comments"),
        Some(json!({ "body": "", "authorUserId": "user-1" })),
    )
    .await;
    assert_eq!(status, 400, "empty body should be 400");
}

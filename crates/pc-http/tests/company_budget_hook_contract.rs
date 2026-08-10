//! R592: CompanyBudgetHook 端到端 contract 测试。
//!
//! 验证 CompanyService.create 触发 CompanyBudgetHook → BudgetService.upsert_policy。
//! 当 budget_monthly_cents > 0 时，公司级月度预算被建立。

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_budgets::BudgetService;
use pc_companies::CompanyService;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    hooks::CompanyBudgetHook,
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
    body: Value,
) -> (u16, Value) {
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

async fn cleanup(db: &Db, id: Uuid) {
    let _ = sqlx::query("DELETE FROM budget_policies WHERE scope_id = $1")
        .bind(id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn r592_create_with_budget_creates_policy() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());

    // 直接通过 service + hook 创建 company，模拟 routes 的 hook 注入
    let db_static: &'static pc_repos::Db = Box::leak(Box::new(state.db.clone()));
    let budget_svc = BudgetService::new(db_static);
    let budget_hook = Arc::new(CompanyBudgetHook::new(Arc::new(budget_svc)));
    let company_svc = CompanyService::with_hooks(&state.db, vec![budget_hook]);

    let created = company_svc
        .create(pc_companies::CreateCompanyInput {
            name: format!("R592-Budget-{}", Uuid::new_v4()),
            description: Some("with budget".into()),
            owner_principal_id: "user-test-budget".into(),
            budget_monthly_cents: Some(50_000), // $500
        })
        .await
        .expect("create");

    // 验证 budget policy 已经在 DB 中
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM budget_policies WHERE scope_id = $1 AND scope_type = 'company'")
            .bind(created.id)
            .fetch_one(db.pool())
            .await
            .expect("count");
    assert_eq!(count.0, 1, "expected 1 budget policy");

    let row: (i32, String) = sqlx::query_as(
        "SELECT amount, window_kind FROM budget_policies WHERE scope_id = $1",
    )
    .bind(created.id)
    .fetch_one(db.pool())
    .await
    .expect("fetch policy");
    assert_eq!(row.0, 50_000);
    assert_eq!(row.1, "calendar_month_utc");

    cleanup(&db, created.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r592_create_without_budget_does_not_create_policy() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());

    let db_static: &'static pc_repos::Db = Box::leak(Box::new(state.db.clone()));
    let budget_svc = BudgetService::new(db_static);
    let budget_hook = Arc::new(CompanyBudgetHook::new(Arc::new(budget_svc)));
    let company_svc = CompanyService::with_hooks(&state.db, vec![budget_hook]);

    let created = company_svc
        .create(pc_companies::CreateCompanyInput {
            name: format!("R592-NoBudget-{}", Uuid::new_v4()),
            description: None,
            owner_principal_id: "u".into(),
            budget_monthly_cents: None,
        })
        .await
        .expect("create");

    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM budget_policies WHERE scope_id = $1")
            .bind(created.id)
            .fetch_one(db.pool())
            .await
            .expect("count");
    assert_eq!(count.0, 0, "no policy expected when budget_monthly_cents is None");

    cleanup(&db, created.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r592_create_with_zero_budget_does_not_create_policy() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());

    let db_static: &'static pc_repos::Db = Box::leak(Box::new(state.db.clone()));
    let budget_svc = BudgetService::new(db_static);
    let budget_hook = Arc::new(CompanyBudgetHook::new(Arc::new(budget_svc)));
    let company_svc = CompanyService::with_hooks(&state.db, vec![budget_hook]);

    let created = company_svc
        .create(pc_companies::CreateCompanyInput {
            name: format!("R592-Zero-{}", Uuid::new_v4()),
            description: None,
            owner_principal_id: "u".into(),
            budget_monthly_cents: Some(0),
        })
        .await
        .expect("create");

    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM budget_policies WHERE scope_id = $1")
            .bind(created.id)
            .fetch_one(db.pool())
            .await
            .expect("count");
    assert_eq!(count.0, 0, "no policy expected when budget = 0");

    cleanup(&db, created.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r592_create_via_http_without_budget_does_not_crash() {
    // 验证 routes/companies.rs 的 create 端点不会因为 hook 注入而崩溃
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::companies::router().with_state(state.clone());

    let (status, body) = call(
        &app,
        "POST",
        "/api/companies",
        json!({
            "name": format!("R592-HTTP-{}", Uuid::new_v4()),
            "description": null,
        }),
    )
    .await;
    assert_eq!(status, 201, "create: {body}");
    let id = Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid");

    cleanup(&db, id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r592_hook_skips_other_events() {
    // CompanyBudgetHook 只响应 Created — 不应处理 Updated / Archived / Removed
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());

    let db_static: &'static pc_repos::Db = Box::leak(Box::new(state.db.clone()));
    let budget_svc = BudgetService::new(db_static);
    let budget_hook = Arc::new(CompanyBudgetHook::new(Arc::new(budget_svc)));
    let company_svc = CompanyService::with_hooks(&state.db, vec![budget_hook.clone()]);

    let created = company_svc
        .create(pc_companies::CreateCompanyInput {
            name: format!("R592-Skip-{}", Uuid::new_v4()),
            description: None,
            owner_principal_id: "u".into(),
            budget_monthly_cents: None,
        })
        .await
        .expect("create");

    // 触发 archive — hook 应 noop
    let _ = company_svc
        .archive(created.id, &pc_companies::CompanyActor::system())
        .await
        .expect("archive");

    // 验证 policy 没被错误创建（archive 不应创建 policy）
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM budget_policies WHERE scope_id = $1")
            .bind(created.id)
            .fetch_one(db.pool())
            .await
            .expect("count");
    assert_eq!(count.0, 0, "archive should not create policy");

    cleanup(&db, created.id).await;
}

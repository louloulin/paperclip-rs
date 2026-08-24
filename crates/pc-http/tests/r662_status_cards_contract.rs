//! R662 — status_cards 路由真实 PG 端到端测试
//!
//! 复刻 Node services/status-cards.ts (917 行) 的 HTTP API：
//! - GET  /api/companies/:company_id/status-cards           -- list
//! - POST /api/companies/:company_id/status-cards           -- create
//! - GET  /api/status-cards/:id                              -- get
//! - PATCH /api/status-cards/:id                             -- patch
//! - DELETE /api/status-cards/:id                           -- delete
//! - GET  /api/status-cards/:id/updates                     -- updates list
//! - POST /api/status-cards/:id/recompile                    -- recompile
//! - POST /api/status-cards/:id/refresh                      -- refresh

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
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

static R662_TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

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
        Arc::new(WsState::new(realtime.clone(), "r662-test".to_string())),
        realtime,
    )
}

async fn try_setup_db() -> Option<Db> {
    Db::connect(TEST_DATABASE_URL, 2, 1).await.ok()
}

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("r662-{id}"))
    .bind(id.simple().to_string())
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM status_card_updates WHERE card_id IN (SELECT id FROM status_cards WHERE company_id = $1)")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM status_cards WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let mut req = Request::builder().method(method).uri(path);
    let req = if let Some(b) = body {
        req.header("content-type", "application/json").body(Body::from(b.to_string())).unwrap()
    } else {
        req.body(Body::empty()).unwrap()
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap_or_default();
    let json: Value = if bytes.is_empty() { Value::Null } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

#[tokio::test(flavor = "current_thread")]
async fn r662_status_cards_crud_full_cycle() {
    let db = match try_setup_db().await {
        Some(d) => d,
        None => { eprintln!("[skip] postgres unreachable"); return; }
    };
    let _guard = R662_TEST_LOCK.lock().await;
    let company_id = insert_company(&db).await;
    let app = routes::status_cards::router().with_state(test_state(db.clone()));

    // 1. 初始 list 应该是空 items 数组
    let (status, body) = call(&app, "GET", &format!("/api/companies/{company_id}/status-cards"), None).await;
    assert_eq!(status, 200, "list empty");
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 0, "should start empty");
    eprintln!("R662 step1: empty list OK");

    // 2. create status_card
    let create_body = json!({
        "title": "R662 status card",
        "titlePinned": false,
        "interestPrompt": "Active issues in this project",
        "refreshPolicy": { "kind": "interval", "seconds": 3600 },
        "queries": [],
    });
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/status-cards"),
        Some(create_body),
    ).await;
    assert_eq!(status, 201, "create returns CREATED");
    let card_id = body["id"].as_str().expect("card.id").to_string();
    assert_eq!(body["companyId"].as_str().unwrap(), &company_id.to_string());
    assert_eq!(body["title"].as_str(), Some("R662 status card"));
    assert_eq!(body["state"].as_str(), Some("compiling"));
    eprintln!("R662 step2: created card id={card_id}");

    // 3. get by id
    let (status, body) = call(&app, "GET", &format!("/api/status-cards/{card_id}"), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["id"].as_str().unwrap(), card_id);
    eprintln!("R662 step3: get by id OK");

    // 4. list now contains 1
    let (status, body) = call(&app, "GET", &format!("/api/companies/{company_id}/status-cards"), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    eprintln!("R662 step4: list 1 item OK");

    // 5. patch title
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/status-cards/{card_id}"),
        Some(json!({ "title": "R662 updated title" })),
    ).await;
    assert_eq!(status, 200);
    assert_eq!(body["title"].as_str(), Some("R662 updated title"));
    eprintln!("R662 step5: patch title OK");

    // 6. updates list (空)
    let (status, body) = call(&app, "GET", &format!("/api/status-cards/{card_id}/updates"), None).await;
    assert_eq!(status, 200);
    assert!(body.is_array());
    eprintln!("R662 step6: updates list OK (empty={})", body.as_array().unwrap().len());

    // 7. delete (returns 204 No Content)
    let (status, _) = call(&app, "DELETE", &format!("/api/status-cards/{card_id}"), None).await;
    assert_eq!(status, 204);
    eprintln!("R662 step7: delete OK (204 No Content)");

    // 8. get 返回 null (404)
    let (status, _) = call(&app, "GET", &format!("/api/status-cards/{card_id}"), None).await;
    assert_eq!(status, 404, "deleted card should 404");
    eprintln!("R662 step8: get deleted card -> 404 OK");

    cleanup(&db, company_id).await;
    eprintln!("R662 PASS: status_cards CRUD full cycle (8 steps)");
}

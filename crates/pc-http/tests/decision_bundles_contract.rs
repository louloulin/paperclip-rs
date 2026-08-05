//! Integration tests for `pc_repos::decision_bundle` + the HTTP layer
//! (`POST/GET /api/companies/:id/decision-bundles`,
//! `GET /api/decision-bundles/:id`).
//!
//! Round 92 验证：把 decisions.rs 中 300-450 行的内联 SQL 抽到
//! `pc-repos::decision_bundle` 模块，路由只做 HTTP 适配。

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{routes, state::ConfigSnapshot, AppState};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::{decision_bundle::*, Db};
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
        Arc::new(WsState {
            realtime: realtime.clone(),
            server_name: "test".into(),
        }),
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
        .bind(format!("db-{tag}-{id}"))
        .bind(format!("D{}", &id.simple().to_string()[..5]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle')",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("agent-{tag}-{id}"))
    .execute(db.pool())
    .await
    .expect("insert agent");
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
    .bind(format!("issue-{tag}-{id}"))
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

async fn insert_heartbeat_run(db: &Db, company_id: Uuid, agent_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status) \
         VALUES ($1, $2, $3, 'manual', 'queued')",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert heartbeat_run");
    id
}

async fn insert_decision(
    db: &Db,
    bundle_id: Uuid,
    company_id: Uuid,
    agent_id: Uuid,
    issue_id: Uuid,
    run_id: Uuid,
    title: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let options = serde_json::json!({"options": [{"id": "yes"}, {"id": "no"}]});
    sqlx::query(
        "INSERT INTO decisions (id, company_id, bundle_id, origin_agent_id, origin_issue_id, \
            origin_run_id, title, body, options, status, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'placeholder', $8::jsonb, 'open', now() + interval '1 day')",
    )
    .bind(id)
    .bind(company_id)
    .bind(bundle_id)
    .bind(agent_id)
    .bind(issue_id)
    .bind(run_id)
    .bind(title)
    .bind(options)
    .execute(db.pool())
    .await
    .expect("insert decision");
    id
}

fn sample_input(title: &str, agent_id: Uuid, issue_id: Uuid, run_id: Uuid) -> NewDecisionBundle {
    NewDecisionBundle {
        title: title.to_string(),
        summary: None,
        origin_agent_id: agent_id,
        origin_issue_id: issue_id,
        origin_run_id: run_id,
    }
}

// =====================================================================
// Repo 层单元测试 (与决策束相关的 CRUD 路径)
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn repo_create_inserts_with_fallback_summary() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "create-fallback").await;
    let agent_id = insert_agent(&db, company_id, "fb").await;
    let issue_id = insert_issue(&db, company_id, "fb").await;
    let run_id = insert_heartbeat_run(&db, company_id, agent_id).await;

    let row = DecisionBundleRepo::new(&db)
        .create(
            company_id,
            sample_input("approve rollout", agent_id, issue_id, run_id),
        )
        .await
        .expect("create");
    assert_eq!(row.company_id, company_id);
    assert_eq!(row.title, "approve rollout");
    // summary 回退到 title
    assert_eq!(row.summary, "approve rollout");
    assert_eq!(row.origin_agent_id, agent_id);
}

#[tokio::test(flavor = "current_thread")]
async fn repo_create_rejects_empty_title() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "create-empty").await;
    let agent_id = insert_agent(&db, company_id, "e").await;
    let issue_id = insert_issue(&db, company_id, "e").await;
    let run_id = insert_heartbeat_run(&db, company_id, agent_id).await;

    let res = DecisionBundleRepo::new(&db)
        .create(
            company_id,
            sample_input("   ", agent_id, issue_id, run_id),
        )
        .await;
    assert!(matches!(
        res.err().expect("must error"),
        DecisionBundleError::EmptyTitle
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn repo_list_filters_by_agent_issue_run() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "list-filter").await;
    let agent_a = insert_agent(&db, company_id, "a").await;
    let agent_b = insert_agent(&db, company_id, "b").await;
    let issue_x = insert_issue(&db, company_id, "x").await;
    let issue_y = insert_issue(&db, company_id, "y").await;
    let run_a = insert_heartbeat_run(&db, company_id, agent_a).await;
    let run_b = insert_heartbeat_run(&db, company_id, agent_b).await;

    let b1 = DecisionBundleRepo::new(&db)
        .create(company_id, sample_input("B1", agent_a, issue_x, run_a))
        .await
        .unwrap();
    let b2 = DecisionBundleRepo::new(&db)
        .create(company_id, sample_input("B2", agent_a, issue_y, run_a))
        .await
        .unwrap();
    let _b3 = DecisionBundleRepo::new(&db)
        .create(company_id, sample_input("B3", agent_b, issue_x, run_b))
        .await
        .unwrap();

    // filter by agent_a → b1 + b2
    let rows = DecisionBundleRepo::new(&db)
        .list_by_company(
            company_id,
            &DecisionBundleFilter {
                agent_id: Some(agent_a),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    assert!(ids.contains(&b1.id) && ids.contains(&b2.id));
    assert_eq!(rows.len(), 2);

    // filter by issue_x → b1 + b3
    let rows = DecisionBundleRepo::new(&db)
        .list_by_company(
            company_id,
            &DecisionBundleFilter {
                issue_id: Some(issue_x),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);

    // filter by run_b → b3
    let rows = DecisionBundleRepo::new(&db)
        .list_by_company(
            company_id,
            &DecisionBundleFilter {
                run_id: Some(run_b),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "B3");
}

#[tokio::test(flavor = "current_thread")]
async fn repo_get_with_decisions_returns_mounted_decisions() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "get-with-decisions").await;
    let agent_id = insert_agent(&db, company_id, "wd").await;
    let issue_id = insert_issue(&db, company_id, "wd").await;
    let run_id = insert_heartbeat_run(&db, company_id, agent_id).await;

    let bundle = DecisionBundleRepo::new(&db)
        .create(
            company_id,
            sample_input("Bundle with decisions", agent_id, issue_id, run_id),
        )
        .await
        .unwrap();
    let d1 = insert_decision(&db, bundle.id, company_id, agent_id, issue_id, run_id, "decision-1").await;
    let d2 = insert_decision(&db, bundle.id, company_id, agent_id, issue_id, run_id, "decision-2").await;

    let detail = DecisionBundleRepo::new(&db)
        .get_with_decisions(bundle.id)
        .await
        .unwrap()
        .expect("bundle present");
    assert_eq!(detail.bundle.id, bundle.id);
    assert_eq!(detail.decisions.len(), 2);
    let ids: Vec<Uuid> = detail.decisions.iter().map(|d| d.id).collect();
    assert!(ids.contains(&d1) && ids.contains(&d2));
    // 按 created_at ASC
    assert_eq!(detail.decisions[0].title, "decision-1");
    assert_eq!(detail.decisions[1].title, "decision-2");
}

#[tokio::test(flavor = "current_thread")]
async fn repo_exists_for_origin_detects_duplicates() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "exists").await;
    let agent_id = insert_agent(&db, company_id, "x").await;
    let issue_id = insert_issue(&db, company_id, "x").await;
    let run_id = insert_heartbeat_run(&db, company_id, agent_id).await;

    assert!(
        !DecisionBundleRepo::new(&db)
            .exists_for_origin(company_id, agent_id, issue_id, run_id)
            .await
            .unwrap()
    );
    DecisionBundleRepo::new(&db)
        .create(company_id, sample_input("X", agent_id, issue_id, run_id))
        .await
        .unwrap();
    assert!(
        DecisionBundleRepo::new(&db)
            .exists_for_origin(company_id, agent_id, issue_id, run_id)
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn repo_delete_returns_true_only_when_row_existed() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "delete").await;
    let agent_id = insert_agent(&db, company_id, "d").await;
    let issue_id = insert_issue(&db, company_id, "d").await;
    let run_id = insert_heartbeat_run(&db, company_id, agent_id).await;

    let bundle = DecisionBundleRepo::new(&db)
        .create(company_id, sample_input("to delete", agent_id, issue_id, run_id))
        .await
        .unwrap();
    assert!(
        DecisionBundleRepo::new(&db)
            .delete(bundle.id)
            .await
            .unwrap()
    );
    assert!(
        !DecisionBundleRepo::new(&db)
            .delete(bundle.id)
            .await
            .unwrap()
    );
}

// =====================================================================
// HTTP 层契约测试
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_create_decision_bundle_returns_201_with_payload() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);

    let company_id = insert_company(&db, "http-create").await;
    let agent_id = insert_agent(&db, company_id, "ha").await;
    let issue_id = insert_issue(&db, company_id, "ha").await;
    let run_id = insert_heartbeat_run(&db, company_id, agent_id).await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/decision-bundles"),
        serde_json::json!({
            "title": "approve canary",
            "summary": "批准灰度",
            "originAgentId": agent_id,
            "originIssueId": issue_id,
            "originRunId": run_id,
        }),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(body["title"], "approve canary");
    assert_eq!(body["summary"], "批准灰度");
    assert_eq!(body["companyId"], serde_json::json!(company_id));
    assert!(body["id"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn http_create_rejects_empty_title() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);

    let company_id = insert_company(&db, "http-empty").await;
    let agent_id = insert_agent(&db, company_id, "eh").await;
    let issue_id = insert_issue(&db, company_id, "eh").await;
    let run_id = insert_heartbeat_run(&db, company_id, agent_id).await;

    let (status, _body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/decision-bundles"),
        serde_json::json!({
            "title": "",
            "originAgentId": agent_id,
            "originIssueId": issue_id,
            "originRunId": run_id,
        }),
    )
    .await;
    assert_eq!(status, 400);
}

#[tokio::test(flavor = "current_thread")]
async fn http_list_decision_bundles_filters_by_agent() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);

    let company_id = insert_company(&db, "http-list").await;
    let agent_a = insert_agent(&db, company_id, "la").await;
    let agent_b = insert_agent(&db, company_id, "lb").await;
    let issue_id = insert_issue(&db, company_id, "l").await;
    let run_a = insert_heartbeat_run(&db, company_id, agent_a).await;
    let run_b = insert_heartbeat_run(&db, company_id, agent_b).await;

    DecisionBundleRepo::new(&db)
        .create(company_id, sample_input("A1", agent_a, issue_id, run_a))
        .await
        .unwrap();
    DecisionBundleRepo::new(&db)
        .create(company_id, sample_input("A2", agent_a, issue_id, run_a))
        .await
        .unwrap();
    DecisionBundleRepo::new(&db)
        .create(company_id, sample_input("B1", agent_b, issue_id, run_b))
        .await
        .unwrap();

    let (status, body) = call(
        &app,
        "GET",
        &format!(
            "/api/companies/{company_id}/decision-bundles?agentId={agent_a}"
        ),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["count"], serde_json::json!(2));
    let titles: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"A1") && titles.contains(&"A2"));
}

#[tokio::test(flavor = "current_thread")]
async fn http_get_decision_bundle_returns_404_for_missing_id() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let missing = Uuid::new_v4();
    let (status, _body) = call(
        &app,
        "GET",
        &format!("/api/decision-bundles/{missing}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "current_thread")]
async fn http_get_decision_bundle_includes_decisions() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);

    let company_id = insert_company(&db, "http-detail").await;
    let agent_id = insert_agent(&db, company_id, "hd").await;
    let issue_id = insert_issue(&db, company_id, "hd").await;
    let run_id = insert_heartbeat_run(&db, company_id, agent_id).await;

    let bundle = DecisionBundleRepo::new(&db)
        .create(company_id, sample_input("with decisions", agent_id, issue_id, run_id))
        .await
        .unwrap();
    insert_decision(
        &db,
        bundle.id,
        company_id,
        agent_id,
        issue_id,
        run_id,
        "first-decision",
    )
    .await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/decision-bundles/{}", bundle.id),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["title"], "with decisions");
    assert_eq!(body["decisionCount"], serde_json::json!(1));
    assert_eq!(body["decisions"][0]["title"], "first-decision");
}

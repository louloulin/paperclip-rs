use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use pc_adapter_api::AdapterRegistry;
use pc_agent::{AgentService, CreateAgent};
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
use tower::ServiceExt;
use uuid::Uuid;

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

fn unique_issue_prefix(suffix: &str) -> String {
    let unique = Uuid::new_v4().simple().to_string();
    let trimmed: String = unique.chars().take(8).collect();
    format!("{trimmed}{suffix}")
}

async fn insert_company(db: &Db) -> Uuid {
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("routine-contract-{company_id}"))
        .bind(unique_issue_prefix("RTN"))
        .execute(db.pool())
        .await
        .expect("insert company");
    company_id
}

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    call_with_headers(app, method, path, body, &[]).await
}

async fn call_with_headers(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .header("content-type", "application/json")
        .header("x-paperclip-user-id", "board-user")
        .uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(
                    serde_json::to_vec(&body).expect("serialize body"),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, payload)
}

#[tokio::test(flavor = "current_thread")]
async fn company_routine_create_uses_ui_contract_and_creates_initial_revision() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let app = routes::routines::router().with_state(test_state(db.clone()));

    let (status, routine) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/routines"),
        json!({
            "title": "Daily review",
            "description": "Review open work",
            "priority": "high",
            "status": "active",
            "concurrencyPolicy": "always_enqueue",
            "catchUpPolicy": "skip_missed",
            "activityGatePolicy": "always",
            "activityGateScope": "company",
            "variables": [],
            "env": null
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "create response: {routine}");
    assert_eq!(routine["companyId"], company_id.to_string());
    assert_eq!(routine["title"], "Daily review");
    assert_eq!(routine["priority"], "high");
    assert_eq!(routine["concurrencyPolicy"], "always_enqueue");
    assert_eq!(routine["latestRevisionNumber"], 1);
    assert!(routine["latestRevisionId"].is_string());
    assert!(routine.get("company_id").is_none());

    let routine_id =
        Uuid::parse_str(routine["id"].as_str().expect("routine id")).expect("uuid routine id");
    let revision: (Uuid, i32, String, Value, Option<String>) = sqlx::query_as(
        "SELECT id, revision_number, title, snapshot, change_summary \
         FROM routine_revisions WHERE routine_id = $1",
    )
    .bind(routine_id)
    .fetch_one(db.pool())
    .await
    .expect("initial revision");
    assert_eq!(
        revision.0,
        Uuid::parse_str(routine["latestRevisionId"].as_str().unwrap()).unwrap()
    );
    assert_eq!(revision.1, 1);
    assert_eq!(revision.2, "Daily review");
    assert_eq!(revision.3["version"], 1);
    assert_eq!(revision.3["routine"]["title"], "Daily review");
    assert_eq!(revision.3["triggers"], json!([]));
    assert_eq!(revision.4.as_deref(), Some("Created routine"));
}

#[tokio::test(flavor = "current_thread")]
async fn company_routine_list_filters_by_project_and_returns_ui_aggregates() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let first_project_id = Uuid::new_v4();
    let second_project_id = Uuid::new_v4();
    for (project_id, name) in [
        (first_project_id, "First project"),
        (second_project_id, "Second project"),
    ] {
        sqlx::query("INSERT INTO projects (id, company_id, name) VALUES ($1,$2,$3)")
            .bind(project_id)
            .bind(company_id)
            .bind(name)
            .execute(db.pool())
            .await
            .expect("insert project");
    }
    let app = routes::routines::router().with_state(test_state(db.clone()));
    for (project_id, title) in [
        (first_project_id, "First routine"),
        (second_project_id, "Second routine"),
    ] {
        let (status, payload) = call(
            &app,
            "POST",
            &format!("/api/companies/{company_id}/routines"),
            json!({ "projectId": project_id, "title": title }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "create response: {payload}");
    }

    let (status, payload) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/routines?projectId={first_project_id}"),
        json!({}),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "list response: {payload}");
    let routines = payload.as_array().expect("routine array");
    assert_eq!(routines.len(), 1, "filtered list: {payload}");
    assert_eq!(routines[0]["projectId"], first_project_id.to_string());
    assert_eq!(routines[0]["title"], "First routine");
    assert_eq!(routines[0]["triggers"], json!([]));
    assert!(routines[0]["lastRun"].is_null());
    assert!(routines[0]["activeIssue"].is_null());
    assert!(routines[0]["managedByPlugin"].is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn routine_detail_includes_description_document_and_relationship_aggregates() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let app = routes::routines::router().with_state(test_state(db.clone()));
    let (create_status, created) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/routines"),
        json!({
            "title": "Documented routine",
            "description": "## Execution\nReview every open item."
        }),
    )
    .await;
    assert_eq!(
        create_status,
        StatusCode::CREATED,
        "create response: {created}"
    );
    let routine_id = created["id"].as_str().expect("routine id");

    let (status, detail) = call(
        &app,
        "GET",
        &format!("/api/routines/{routine_id}"),
        json!({}),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "detail response: {detail}");
    assert_eq!(detail["id"], routine_id);
    assert!(detail["project"].is_null());
    assert!(detail["assignee"].is_null());
    assert!(detail["parentIssue"].is_null());
    assert_eq!(detail["triggers"], json!([]));
    assert_eq!(detail["recentRuns"], json!([]));
    assert!(detail["activeIssue"].is_null());
    assert!(detail["managedByPlugin"].is_null());
    assert_eq!(detail["descriptionDocument"]["routineId"], routine_id);
    assert_eq!(detail["descriptionDocument"]["key"], "description");
    assert_eq!(
        detail["descriptionDocument"]["title"],
        "Routine description"
    );
    assert_eq!(detail["descriptionDocument"]["format"], "markdown");
    assert_eq!(
        detail["descriptionDocument"]["body"],
        "## Execution\nReview every open item."
    );
    assert_eq!(detail["descriptionDocument"]["latestRevisionNumber"], 1);
    assert!(detail["descriptionDocument"]["latestRevisionId"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn routine_revision_restore_uses_revision_id_and_preserves_history() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let app = routes::routines::router().with_state(test_state(db.clone()));
    let (_, created) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/routines"),
        json!({ "title": "Original", "description": "first" }),
    )
    .await;
    let routine_id = created["id"].as_str().expect("routine id").to_owned();
    let first_revision_id = created["latestRevisionId"].as_str().unwrap().to_owned();

    let (update_status, updated) = call(
        &app,
        "PATCH",
        &format!("/api/routines/{routine_id}"),
        json!({ "title": "Updated", "description": "second" }),
    )
    .await;
    assert_eq!(update_status, StatusCode::OK, "update response: {updated}");
    assert_eq!(updated["title"], "Updated");
    assert_eq!(updated["latestRevisionNumber"], 2);
    let second_revision_id = updated["latestRevisionId"].as_str().unwrap().to_owned();
    assert_ne!(first_revision_id, second_revision_id);

    let (list_status, revisions) = call(
        &app,
        "GET",
        &format!("/api/routines/{routine_id}/revisions"),
        json!({}),
    )
    .await;
    assert_eq!(
        list_status,
        StatusCode::OK,
        "revisions response: {revisions}"
    );
    let revisions = revisions.as_array().expect("revision array");
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0]["revisionNumber"], 2);
    assert_eq!(revisions[1]["revisionNumber"], 1);
    assert_eq!(revisions[1]["snapshot"]["routine"]["title"], "Original");

    let (restore_status, restored) = call(
        &app,
        "POST",
        &format!("/api/routines/{routine_id}/revisions/{first_revision_id}/restore"),
        json!({}),
    )
    .await;
    assert_eq!(
        restore_status,
        StatusCode::OK,
        "restore response: {restored}"
    );
    assert_eq!(restored["restoredFromRevisionId"], first_revision_id);
    assert_eq!(restored["restoredFromRevisionNumber"], 1);
    assert_eq!(restored["revision"]["revisionNumber"], 3);
    assert_eq!(
        restored["revision"]["snapshot"]["routine"]["title"],
        "Original"
    );
    assert_eq!(restored["routine"]["title"], "Original");
    assert_eq!(restored["routine"]["latestRevisionNumber"], 3);

    let (detail_status, detail) = call(
        &app,
        "GET",
        &format!("/api/routines/{routine_id}"),
        json!({}),
    )
    .await;
    assert_eq!(detail_status, StatusCode::OK);
    assert_eq!(detail["title"], "Original");
    assert_eq!(detail["description"], "first");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM routine_revisions WHERE routine_id = $1")
            .bind(Uuid::parse_str(&routine_id).unwrap())
            .fetch_one(db.pool())
            .await
            .expect("revision count");
    assert_eq!(count, 3);
}

#[tokio::test(flavor = "current_thread")]
async fn schedule_trigger_creation_appends_revision_and_returns_ui_wrapper() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let app = routes::routines::router().with_state(test_state(db.clone()));
    let (_, routine) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/routines"),
        json!({ "title": "Scheduled routine" }),
    )
    .await;
    let routine_id = routine["id"].as_str().expect("routine id");

    let (status, created) = call(
        &app,
        "POST",
        &format!("/api/routines/{routine_id}/triggers"),
        json!({
            "kind": "schedule",
            "label": "Weekday morning",
            "enabled": true,
            "cronExpression": "0 9 * * 1-5",
            "timezone": "UTC"
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "trigger response: {created}");
    assert_eq!(created["trigger"]["routineId"], routine_id);
    assert_eq!(created["trigger"]["kind"], "schedule");
    assert_eq!(created["trigger"]["label"], "Weekday morning");
    assert_eq!(created["trigger"]["cronExpression"], "0 9 * * 1-5");
    assert_eq!(created["trigger"]["timezone"], "UTC");
    assert!(created["trigger"]["nextRunAt"].is_string());
    assert!(created["secretMaterial"].is_null());
    assert_eq!(created["revision"]["revisionNumber"], 2);
    assert_eq!(
        created["revision"]["changeSummary"],
        "Created schedule trigger"
    );
    assert_eq!(
        created["revision"]["snapshot"]["triggers"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        created["revision"]["snapshot"]["triggers"][0]["id"],
        created["trigger"]["id"]
    );

    let (detail_status, detail) = call(
        &app,
        "GET",
        &format!("/api/routines/{routine_id}"),
        json!({}),
    )
    .await;
    assert_eq!(detail_status, StatusCode::OK);
    assert_eq!(detail["latestRevisionNumber"], 2);
    assert_eq!(detail["triggers"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn trigger_update_and_delete_append_revisions_with_exact_snapshots() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let app = routes::routines::router().with_state(test_state(db.clone()));
    let (_, routine) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/routines"),
        json!({ "title": "Mutable trigger routine" }),
    )
    .await;
    let routine_id = routine["id"].as_str().unwrap();
    let (_, created) = call(
        &app,
        "POST",
        &format!("/api/routines/{routine_id}/triggers"),
        json!({
            "kind": "schedule",
            "label": "Initial",
            "cronExpression": "0 9 * * *",
            "timezone": "UTC"
        }),
    )
    .await;
    let trigger_id = created["trigger"]["id"].as_str().unwrap();

    let (update_status, updated) = call(
        &app,
        "PATCH",
        &format!("/api/routine-triggers/{trigger_id}"),
        json!({
            "label": null,
            "enabled": false,
            "cronExpression": "30 10 * * *",
            "timezone": "UTC"
        }),
    )
    .await;
    assert_eq!(update_status, StatusCode::OK, "update trigger: {updated}");
    assert!(updated["label"].is_null());
    assert_eq!(updated["enabled"], false);
    assert_eq!(updated["cronExpression"], "30 10 * * *");
    assert_eq!(updated["timezone"], "UTC");
    assert!(updated["nextRunAt"].is_string());

    let (_, after_update) = call(
        &app,
        "GET",
        &format!("/api/routines/{routine_id}"),
        json!({}),
    )
    .await;
    assert_eq!(after_update["latestRevisionNumber"], 3);
    assert_eq!(after_update["triggers"][0]["id"], trigger_id);
    assert!(after_update["triggers"][0]["label"].is_null());

    let (delete_status, delete_payload) = call(
        &app,
        "DELETE",
        &format!("/api/routine-triggers/{trigger_id}"),
        json!({}),
    )
    .await;
    assert_eq!(
        delete_status,
        StatusCode::NO_CONTENT,
        "delete trigger: {delete_payload}"
    );

    let (_, after_delete) = call(
        &app,
        "GET",
        &format!("/api/routines/{routine_id}"),
        json!({}),
    )
    .await;
    assert_eq!(after_delete["latestRevisionNumber"], 4);
    assert_eq!(after_delete["triggers"], json!([]));
    let latest_snapshot: Value = sqlx::query_scalar(
        "SELECT snapshot FROM routine_revisions WHERE routine_id=$1 ORDER BY revision_number DESC LIMIT 1",
    )
    .bind(Uuid::parse_str(routine_id).unwrap())
    .fetch_one(db.pool())
    .await
    .expect("latest trigger revision");
    assert_eq!(latest_snapshot["triggers"], json!([]));
}

#[tokio::test(flavor = "current_thread")]
async fn manual_run_creates_execution_issue_heartbeat_and_enriched_run_views() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let agent = AgentService::new(db.clone())
        .create(CreateAgent {
            company_id,
            name: "Routine runner".into(),
            adapter_type: "codex_local".into(),
            ..CreateAgent::default()
        })
        .await
        .expect("create agent");
    let app = routes::routines::router().with_state(test_state(db.clone()));
    let (_, routine) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/routines"),
        json!({
            "title": "Execute daily review",
            "description": "Review blockers",
            "assigneeAgentId": agent.id,
            "priority": "high"
        }),
    )
    .await;
    let routine_id = routine["id"].as_str().unwrap();

    let (status, run) = call(
        &app,
        "POST",
        &format!("/api/routines/{routine_id}/run"),
        json!({
            "source": "manual",
            "payload": { "origin": "contract-test" },
            "idempotencyKey": "manual-run-1"
        }),
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED, "run response: {run}");
    assert_eq!(run["routineId"], routine_id);
    assert_eq!(run["source"], "manual");
    assert_eq!(run["status"], "issue_created");
    assert_eq!(run["idempotencyKey"], "manual-run-1");
    assert_eq!(run["triggerPayload"]["origin"], "contract-test");
    assert_eq!(run["routineRevisionId"], routine["latestRevisionId"]);
    assert!(run["linkedIssueId"].is_string());

    let linked_issue_id = Uuid::parse_str(run["linkedIssueId"].as_str().unwrap()).unwrap();
    let issue: (String, Option<String>, Option<String>, Option<Uuid>, Option<Uuid>, String) =
        sqlx::query_as(
            "SELECT origin_kind, origin_id, origin_run_id, assignee_agent_id, execution_run_id, status \
             FROM issues WHERE id=$1",
        )
        .bind(linked_issue_id)
        .fetch_one(db.pool())
        .await
        .expect("execution issue");
    assert_eq!(issue.0, "routine_execution");
    assert_eq!(issue.1.as_deref(), Some(routine_id));
    assert_eq!(issue.2.as_deref(), run["id"].as_str());
    assert_eq!(issue.3, Some(agent.id));
    assert!(issue.4.is_some());
    assert_eq!(issue.5, "todo");

    let heartbeat: (Uuid, Uuid, String, Option<Uuid>, Value) = sqlx::query_as(
        "SELECT id, agent_id, status, wakeup_request_id, context_snapshot \
         FROM heartbeat_runs WHERE id=$1",
    )
    .bind(issue.4.unwrap())
    .fetch_one(db.pool())
    .await
    .expect("heartbeat run");
    assert_eq!(heartbeat.1, agent.id);
    assert_eq!(heartbeat.2, "queued");
    assert!(heartbeat.3.is_some());
    assert_eq!(heartbeat.4["issueId"], linked_issue_id.to_string());

    let (runs_status, runs) = call(
        &app,
        "GET",
        &format!("/api/routines/{routine_id}/runs?limit=10"),
        json!({}),
    )
    .await;
    assert_eq!(runs_status, StatusCode::OK, "runs response: {runs}");
    assert_eq!(runs.as_array().unwrap().len(), 1);
    assert_eq!(runs[0]["id"], run["id"]);
    assert_eq!(runs[0]["linkedIssue"]["id"], linked_issue_id.to_string());
    assert_eq!(runs[0]["linkedIssue"]["title"], "Execute daily review");
    assert!(runs[0]["trigger"].is_null());

    let (_, detail) = call(
        &app,
        "GET",
        &format!("/api/routines/{routine_id}"),
        json!({}),
    )
    .await;
    assert_eq!(detail["recentRuns"][0]["id"], run["id"]);
    assert_eq!(detail["activeIssue"]["id"], linked_issue_id.to_string());

    let (_, list) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/routines"),
        json!({}),
    )
    .await;
    let listed = list
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == routine_id)
        .expect("listed routine");
    assert_eq!(listed["lastRun"]["id"], run["id"]);
    assert_eq!(listed["activeIssue"]["id"], linked_issue_id.to_string());
}

#[tokio::test(flavor = "current_thread")]
async fn bearer_webhook_trigger_encrypts_secret_and_fires_idempotently() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = insert_company(&db).await;
    let agent = AgentService::new(db.clone())
        .create(CreateAgent {
            company_id,
            name: "Webhook runner".into(),
            adapter_type: "codex_local".into(),
            ..CreateAgent::default()
        })
        .await
        .expect("create agent");
    let app = routes::routines::router().with_state(test_state(db.clone()));
    let (_, routine) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/routines"),
        json!({
            "title": "Webhook routine",
            "assigneeAgentId": agent.id
        }),
    )
    .await;
    let routine_id = routine["id"].as_str().unwrap();

    let (trigger_status, created) = call(
        &app,
        "POST",
        &format!("/api/routines/{routine_id}/triggers"),
        json!({
            "kind": "webhook",
            "label": "Inbound hook",
            "signingMode": "bearer",
            "replayWindowSec": 300
        }),
    )
    .await;
    assert_eq!(
        trigger_status,
        StatusCode::CREATED,
        "webhook trigger: {created}"
    );
    let trigger_id = created["trigger"]["id"].as_str().unwrap();
    let public_id = created["trigger"]["publicId"].as_str().unwrap();
    let webhook_secret = created["secretMaterial"]["webhookSecret"]
        .as_str()
        .expect("one-time webhook secret")
        .to_owned();
    assert!(created["secretMaterial"]["webhookUrl"]
        .as_str()
        .unwrap()
        .ends_with(&format!("/api/routine-triggers/public/{public_id}/fire")));
    assert_eq!(created["revision"]["revisionNumber"], 2);

    let stored: (Option<Uuid>, Value) = sqlx::query_as(
        "SELECT rt.secret_id, csv.material FROM routine_triggers rt \
         JOIN company_secret_versions csv ON csv.secret_id=rt.secret_id AND csv.version=1 \
         WHERE rt.id=$1",
    )
    .bind(Uuid::parse_str(trigger_id).unwrap())
    .fetch_one(db.pool())
    .await
    .expect("encrypted webhook secret");
    assert!(stored.0.is_some());
    assert_eq!(stored.1["scheme"], "local_encrypted_v1");
    assert!(!stored.1.to_string().contains(&webhook_secret));

    let fire_path = format!("/api/routine-triggers/public/{public_id}/fire");
    let (unauthorized_status, _) = call_with_headers(
        &app,
        "POST",
        &fire_path,
        json!({ "event": "opened" }),
        &[("idempotency-key", "webhook-run-1")],
    )
    .await;
    assert_eq!(unauthorized_status, StatusCode::UNAUTHORIZED);

    let authorization = format!("Bearer {webhook_secret}");
    let (first_status, first_run) = call_with_headers(
        &app,
        "POST",
        &fire_path,
        json!({ "event": "opened" }),
        &[
            ("authorization", authorization.as_str()),
            ("idempotency-key", "webhook-run-1"),
        ],
    )
    .await;
    assert_eq!(
        first_status,
        StatusCode::ACCEPTED,
        "first fire: {first_run}"
    );
    assert_eq!(first_run["source"], "webhook");
    assert_eq!(first_run["status"], "issue_created");
    assert_eq!(first_run["triggerId"], trigger_id);
    assert_eq!(first_run["triggerPayload"]["event"], "opened");

    let (second_status, second_run) = call_with_headers(
        &app,
        "POST",
        &fire_path,
        json!({ "event": "opened" }),
        &[
            ("authorization", authorization.as_str()),
            ("idempotency-key", "webhook-run-1"),
        ],
    )
    .await;
    assert_eq!(second_status, StatusCode::ACCEPTED);
    assert_eq!(second_run["id"], first_run["id"]);

    let run_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM routine_runs WHERE routine_id=$1 AND source='webhook'",
    )
    .bind(Uuid::parse_str(routine_id).unwrap())
    .fetch_one(db.pool())
    .await
    .expect("webhook run count");
    assert_eq!(run_count, 1);
}

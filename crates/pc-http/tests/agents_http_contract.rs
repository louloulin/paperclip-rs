use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use pc_adapter_api::AdapterRegistry;
use pc_agent::{AgentInstructionsService, AgentService, CreateAgent};
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    routes,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());
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

async fn call_json(
    app: &axum::Router,
    method: &str,
    path: String,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .uri(path)
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .expect("request");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

#[tokio::test(flavor = "current_thread")]
async fn company_agent_list_uses_ui_path_and_camel_case_payload() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("http-agent-contract-{company_id}"))
        .bind(unique_issue_prefix("CON"))
        .execute(db.pool())
        .await
        .expect("insert company");
    let agent = AgentService::new(db.clone())
        .create(CreateAgent {
            company_id,
            name: "HTTP Agent".into(),
            ..CreateAgent::default()
        })
        .await
        .expect("create agent");

    let response = routes::agents::router()
        .with_state(test_state(db.clone()))
        .oneshot(
            Request::builder()
                .uri(format!("/api/companies/{company_id}/agents"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("request");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();

    sqlx::query("DELETE FROM agents WHERE id=$1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("delete agent");
    sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("delete company");

    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload[0]["companyId"], company_id.to_string());
    assert!(payload[0].get("company_id").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn company_agent_create_accepts_ui_payload_and_returns_full_agent() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("http-agent-create-{company_id}"))
        .bind(unique_issue_prefix("CRE"))
        .execute(db.pool())
        .await
        .expect("insert company");
    let response = routes::agents::router()
        .with_state(test_state(db.clone()))
        .oneshot(
            Request::builder()
                .method("POST")
                .header("content-type", "application/json")
                .uri(format!("/api/companies/{company_id}/agents"))
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "name": "Created over HTTP",
                        "role": "engineer",
                        "adapterType": "codex_local",
                        "adapterConfig": {"model": "gpt-5"},
                        "runtimeConfig": {},
                        "budgetMonthlyCents": 1200
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .expect("request");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    if let Some(agent_id) = payload["id"]
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
    {
        sqlx::query("DELETE FROM agents WHERE id=$1")
            .bind(agent_id)
            .execute(db.pool())
            .await
            .expect("delete agent");
    }
    sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("delete company");

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(payload["companyId"], company_id.to_string());
    assert_eq!(payload["adapterType"], "codex_local");
    assert_eq!(payload["budgetMonthlyCents"], 1200);
    assert_eq!(payload["permissions"]["canCreateSkills"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn patch_records_and_exposes_config_revision_contract() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("http-agent-revision-{company_id}"))
        .bind(unique_issue_prefix("REV"))
        .execute(db.pool())
        .await
        .expect("insert company");
    let agent = AgentService::new(db.clone())
        .create(CreateAgent {
            company_id,
            name: "Revision Agent".into(),
            adapter_type: "codex_local".into(),
            adapter_config: serde_json::json!({"model": "gpt-5"}),
            ..CreateAgent::default()
        })
        .await
        .expect("create agent");
    let app = routes::agents::router().with_state(test_state(db.clone()));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .header("content-type", "application/json")
                .uri(format!("/api/agents/{}", agent.id))
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "name": "Revised Agent",
                        "adapterConfig": {"model": "gpt-5.1"},
                        "budgetMonthlyCents": 2400
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .expect("patch request");
    let patch_status = response.status();
    let patch_payload: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("patch body"),
    )
    .unwrap_or_default();

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/agents/{}/config-revisions", agent.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("revision request");
    let revisions_status = response.status();
    let revisions: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("revision body"),
    )
    .unwrap_or_default();

    sqlx::query("DELETE FROM agents WHERE id=$1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("delete agent");
    sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("delete company");

    assert_eq!(patch_status, StatusCode::OK);
    assert_eq!(patch_payload["adapterConfig"]["model"], "gpt-5.1");
    assert_eq!(patch_payload["budgetMonthlyCents"], 2400);
    assert_eq!(revisions_status, StatusCode::OK);
    assert_eq!(revisions.as_array().map(Vec::len), Some(1));
    assert_eq!(
        revisions[0]["changedKeys"],
        serde_json::json!(["name", "adapterConfig", "budgetMonthlyCents"])
    );
    assert_eq!(revisions[0]["source"], "patch");
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_lifecycle_and_key_routes_match_ui_contract() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("http-agent-actions-{company_id}"))
        .bind(unique_issue_prefix("ACT"))
        .execute(db.pool())
        .await
        .expect("insert company");
    let agent = AgentService::new(db.clone())
        .create(CreateAgent {
            company_id,
            name: "Action Agent".into(),
            ..CreateAgent::default()
        })
        .await
        .expect("create agent");
    let app = routes::agents::router().with_state(test_state(db.clone()));

    let (pause_status, paused) = call_json(
        &app,
        "POST",
        format!("/api/agents/{}/pause", agent.id),
        serde_json::json!({}),
    )
    .await;
    let (resume_status, resumed) = call_json(
        &app,
        "POST",
        format!("/api/agents/{}/resume", agent.id),
        serde_json::json!({}),
    )
    .await;
    let (runtime_status, runtime) = call_json(
        &app,
        "GET",
        format!("/api/agents/{}/runtime-state", agent.id),
        serde_json::json!({}),
    )
    .await;
    let (reset_status, reset) = call_json(
        &app,
        "POST",
        format!("/api/agents/{}/runtime-state/reset-session", agent.id),
        serde_json::json!({}),
    )
    .await;
    let (create_key_status, created_key) = call_json(
        &app,
        "POST",
        format!("/api/agents/{}/keys", agent.id),
        serde_json::json!({"name": "http-key", "scope": {"kind": "standard"}}),
    )
    .await;
    let key_id = created_key["id"]
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok());
    let (list_keys_status, keys) = call_json(
        &app,
        "GET",
        format!("/api/agents/{}/keys", agent.id),
        serde_json::json!({}),
    )
    .await;
    let (revoke_status, revoked) = if let Some(key_id) = key_id {
        call_json(
            &app,
            "DELETE",
            format!("/api/agents/{}/keys/{key_id}", agent.id),
            serde_json::json!({}),
        )
        .await
    } else {
        (StatusCode::NOT_FOUND, serde_json::Value::Null)
    };

    sqlx::query("DELETE FROM agent_api_keys WHERE agent_id=$1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("delete keys");
    sqlx::query("DELETE FROM agent_runtime_state WHERE agent_id=$1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("delete runtime");
    sqlx::query("DELETE FROM agents WHERE id=$1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("delete agent");
    sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("delete company");

    assert_eq!(pause_status, StatusCode::OK);
    assert_eq!(paused["status"], "paused");
    assert_eq!(resume_status, StatusCode::OK);
    assert_eq!(resumed["status"], "idle");
    assert_eq!(runtime_status, StatusCode::OK);
    assert_eq!(runtime["agentId"], agent.id.to_string());
    assert_eq!(reset_status, StatusCode::OK);
    assert_eq!(reset["clearedTaskSessions"], 0);
    assert_eq!(create_key_status, StatusCode::CREATED);
    assert!(created_key["token"]
        .as_str()
        .is_some_and(|value| value.starts_with("pcp_")));
    assert_eq!(list_keys_status, StatusCode::OK);
    assert_eq!(keys.as_array().map(Vec::len), Some(1));
    assert_eq!(revoke_status, StatusCode::OK);
    assert!(revoked["revokedAt"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn hire_approval_and_permissions_use_real_tables_and_state_transitions() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, issue_prefix, require_board_approval_for_new_agents) \
         VALUES ($1,$2,$3,true)",
    )
    .bind(company_id)
    .bind(format!("http-agent-hire-{company_id}"))
    .bind(unique_issue_prefix("HIR"))
    .execute(db.pool())
    .await
    .expect("insert company");
    let app = routes::agents::router().with_state(test_state(db.clone()));

    let (hire_status, hire) = call_json(
        &app,
        "POST",
        format!("/api/companies/{company_id}/agent-hires"),
        serde_json::json!({
            "name": "Pending Hire",
            "role": "engineer",
            "adapterType": "codex_local",
            "adapterConfig": {},
            "runtimeConfig": {}
        }),
    )
    .await;
    let agent_id = hire["agent"]["id"]
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok());
    let approval_id = hire["approval"]["id"]
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok());
    let (permissions_status, permissions) = if let Some(agent_id) = agent_id {
        call_json(
            &app,
            "PATCH",
            format!("/api/agents/{agent_id}/permissions"),
            serde_json::json!({
                "canCreateAgents": true,
                "canCreateSkills": false,
                "canAssignTasks": true
            }),
        )
        .await
    } else {
        (StatusCode::NOT_FOUND, serde_json::Value::Null)
    };
    let (approve_status, approved) = if let Some(agent_id) = agent_id {
        call_json(
            &app,
            "POST",
            format!("/api/agents/{agent_id}/approve"),
            serde_json::json!({}),
        )
        .await
    } else {
        (StatusCode::NOT_FOUND, serde_json::Value::Null)
    };
    let (permissions_after_status, permissions_after) = if let Some(agent_id) = agent_id {
        call_json(
            &app,
            "PATCH",
            format!("/api/agents/{agent_id}/permissions"),
            serde_json::json!({
                "canCreateAgents": true,
                "canCreateSkills": false,
                "canAssignTasks": true
            }),
        )
        .await
    } else {
        (StatusCode::NOT_FOUND, serde_json::Value::Null)
    };
    let access_granted: bool = if let Some(agent_id) = agent_id {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM principal_permission_grants WHERE company_id=$1 \
             AND principal_type='agent' AND principal_id=$2 AND permission_key='tasks:assign')",
        )
        .bind(company_id)
        .bind(agent_id.to_string())
        .fetch_one(db.pool())
        .await
        .expect("read grant")
    } else {
        false
    };

    if let Some(approval_id) = approval_id {
        sqlx::query("DELETE FROM approvals WHERE id=$1")
            .bind(approval_id)
            .execute(db.pool())
            .await
            .expect("delete approval");
    }
    if let Some(agent_id) = agent_id {
        sqlx::query("DELETE FROM principal_permission_grants WHERE principal_id=$1")
            .bind(agent_id.to_string())
            .execute(db.pool())
            .await
            .expect("delete grants");
        sqlx::query(
            "DELETE FROM company_memberships WHERE principal_type='agent' AND principal_id=$1",
        )
        .bind(agent_id.to_string())
        .execute(db.pool())
        .await
        .expect("delete membership");
        sqlx::query("DELETE FROM agents WHERE id=$1")
            .bind(agent_id)
            .execute(db.pool())
            .await
            .expect("delete agent");
    }
    sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("delete company");

    assert_eq!(hire_status, StatusCode::CREATED);
    assert_eq!(hire["agent"]["status"], "pending_approval");
    assert_eq!(hire["approval"]["approvalType"], "hire_agent");
    assert_eq!(permissions_status, StatusCode::CONFLICT);
    assert!(permissions["error"]["message"]
        .as_str()
        .is_some_and(|message| message.to_ascii_lowercase().contains("pending approval")));
    assert_eq!(approve_status, StatusCode::OK);
    assert_eq!(approved["status"], "idle");
    assert_eq!(permissions_after_status, StatusCode::OK);
    assert_eq!(permissions_after["permissions"]["canCreateAgents"], true);
    assert_eq!(permissions_after["permissions"]["canCreateSkills"], false);
    assert!(access_granted);
}

#[tokio::test(flavor = "current_thread")]
async fn instructions_bundle_routes_persist_config_and_reject_path_traversal() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("http-agent-instructions-{company_id}"))
        .bind(unique_issue_prefix("INS"))
        .execute(db.pool())
        .await
        .expect("insert company");
    let agent = AgentService::new(db.clone())
        .create(CreateAgent {
            company_id,
            name: "Instructions Agent".into(),
            ..CreateAgent::default()
        })
        .await
        .expect("create agent");
    let temp = tempfile::tempdir().expect("temp instructions root");
    let state = test_state(db.clone()).with_agent_instructions(Arc::new(
        AgentInstructionsService::new(temp.path().join("instance")),
    ));
    let app = routes::agents::router().with_state(state);

    let (put_status, file) = call_json(
        &app,
        "PUT",
        format!("/api/agents/{}/instructions-bundle/file", agent.id),
        serde_json::json!({"path": "AGENTS.md", "content": "# Agent\n"}),
    )
    .await;
    let (get_status, read) = call_json(
        &app,
        "GET",
        format!(
            "/api/agents/{}/instructions-bundle/file?path=AGENTS.md",
            agent.id
        ),
        serde_json::json!({}),
    )
    .await;
    let (traversal_status, _) = call_json(
        &app,
        "PUT",
        format!("/api/agents/{}/instructions-bundle/file", agent.id),
        serde_json::json!({"path": "../escape.md", "content": "escape"}),
    )
    .await;
    let (delete_entry_status, _) = call_json(
        &app,
        "DELETE",
        format!(
            "/api/agents/{}/instructions-bundle/file?path=AGENTS.md",
            agent.id
        ),
        serde_json::json!({}),
    )
    .await;
    let stored_config: serde_json::Value =
        sqlx::query_scalar("SELECT adapter_config FROM agents WHERE id=$1")
            .bind(agent.id)
            .fetch_one(db.pool())
            .await
            .expect("stored adapter config");

    sqlx::query("DELETE FROM agents WHERE id=$1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("delete agent");
    sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("delete company");

    assert_eq!(put_status, StatusCode::OK);
    assert_eq!(file["path"], "AGENTS.md");
    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(read["content"], "# Agent\n");
    assert_eq!(traversal_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(delete_entry_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(stored_config["instructionsBundleMode"], "managed");
    assert!(stored_config["instructionsFilePath"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn instructions_path_route_syncs_bundle_metadata_and_validates_relative_paths() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect test db");
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("http-agent-instructions-path-{company_id}"))
        .bind(unique_issue_prefix("INP"))
        .execute(db.pool())
        .await
        .expect("insert company");
    let agent = AgentService::new(db.clone())
        .create(CreateAgent {
            company_id,
            name: "Instructions Path Agent".into(),
            ..CreateAgent::default()
        })
        .await
        .expect("create agent");
    let temp = tempfile::tempdir().expect("temp instructions root");
    let external_root = temp.path().join("external");
    tokio::fs::create_dir_all(&external_root).await.unwrap();
    let external_entry = external_root.join("AGENTS.md");
    tokio::fs::write(&external_entry, "# External\n")
        .await
        .unwrap();
    let state = test_state(db.clone()).with_agent_instructions(Arc::new(
        AgentInstructionsService::new(temp.path().join("instance")),
    ));
    let app = routes::agents::router().with_state(state);

    let (relative_status, _) = call_json(
        &app,
        "PATCH",
        format!("/api/agents/{}/instructions-path", agent.id),
        serde_json::json!({"path": "relative/AGENTS.md"}),
    )
    .await;
    let (absolute_status, synced) = call_json(
        &app,
        "PATCH",
        format!("/api/agents/{}/instructions-path", agent.id),
        serde_json::json!({"path": external_entry}),
    )
    .await;
    let stored_config: serde_json::Value =
        sqlx::query_scalar("SELECT adapter_config FROM agents WHERE id=$1")
            .bind(agent.id)
            .fetch_one(db.pool())
            .await
            .expect("stored adapter config");
    let (clear_status, cleared) = call_json(
        &app,
        "PATCH",
        format!("/api/agents/{}/instructions-path", agent.id),
        serde_json::json!({"path": null}),
    )
    .await;

    sqlx::query("DELETE FROM agents WHERE id=$1")
        .bind(agent.id)
        .execute(db.pool())
        .await
        .expect("delete agent");
    sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("delete company");

    assert_eq!(relative_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(absolute_status, StatusCode::OK);
    assert_eq!(synced["adapterConfigKey"], "instructionsFilePath");
    assert_eq!(stored_config["instructionsBundleMode"], "external");
    assert_eq!(stored_config["instructionsEntryFile"], "AGENTS.md");
    assert_eq!(clear_status, StatusCode::OK);
    assert!(cleared["path"].is_null());
}

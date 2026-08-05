//! Integration tests for Round 93 修复：
//! `companies.rs` 中 audit/org/search/agents 子块重构到 Repo 层，
//! 同时修复多个隐藏 bug（`adapter_kind` 列名错误、`activity_log` 字段名错误、
//! `cm.user_id` 引用、`company_built_in_agent_provisions` 表不存在）。

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
        .bind(format!("audit-{tag}-{id}"))
        .bind(format!("A{}", &id.simple().to_string()[..5]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_user(db: &Db, tag: &str) -> String {
    let id = format!("u_audit_{}_{}", tag, Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, image, created_at, updated_at) \
         VALUES ($1, $2, $3, true, NULL, now(), now())",
    )
    .bind(&id)
    .bind(format!("User {tag}"))
    .bind(format!("{id}@test.local"))
    .execute(db.pool())
    .await
    .expect("insert user");
    id
}

async fn add_membership(db: &Db, company_id: Uuid, user_id: &str, role: &str) {
    sqlx::query(
        "INSERT INTO company_memberships \
         (id, company_id, principal_type, principal_id, status, membership_role) \
         VALUES ($1, $2, 'user', $3, 'active', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(user_id)
    .bind(role)
    .execute(db.pool())
    .await
    .expect("insert membership");
}

async fn insert_activity(db: &Db, company_id: Uuid, action: &str, entity_type: &str, entity_id: &str) {
    sqlx::query(
        "INSERT INTO activity_log \
         (company_id, actor_type, actor_id, action, entity_type, entity_id, details) \
         VALUES ($1, 'system', 'sys-test', $2, $3, $4, '{}'::jsonb)",
    )
    .bind(company_id)
    .bind(action)
    .bind(entity_type)
    .bind(entity_id)
    .execute(db.pool())
    .await
    .expect("insert activity_log");
}

async fn insert_issue(db: &Db, company_id: Uuid, title: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority) \
         VALUES ($1, $2, $3, 'backlog', 'medium')",
    )
    .bind(id)
    .bind(company_id)
    .bind(title)
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

// =====================================================================
// Repo 层
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn repo_company_exists_returns_true_when_present() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "exists-yes").await;
    assert!(pc_repos::company::CompanyRepo::new(&db).exists(cid).await.unwrap());
    assert!(!pc_repos::company::CompanyRepo::new(&db).exists(Uuid::new_v4()).await.unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn repo_user_directory_returns_active_members_with_role() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "user-dir").await;
    let u1 = insert_user(&db, "u1").await;
    let u2 = insert_user(&db, "u2").await;
    add_membership(&db, cid, &u1, "owner").await;
    add_membership(&db, cid, &u2, "member").await;

    let entries = pc_repos::company_member::CompanyMemberRepo::new(&db)
        .user_directory(cid)
        .await
        .expect("user_directory");
    assert_eq!(entries.len(), 2);
    let ids: Vec<&str> = entries.iter().map(|e| e.user_id.as_str()).collect();
    assert!(ids.contains(&u1.as_str()) && ids.contains(&u2.as_str()));
    let owner = entries.iter().find(|e| e.user_id == u1).unwrap();
    assert_eq!(owner.role, "owner");
    assert!(owner.email.as_deref().unwrap_or_default().contains("@test.local"));
}

#[tokio::test(flavor = "current_thread")]
async fn repo_user_directory_excludes_archived_memberships() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "user-dir-archived").await;
    let u1 = insert_user(&db, "active").await;
    let u2 = insert_user(&db, "kicked").await;
    add_membership(&db, cid, &u1, "owner").await;
    sqlx::query(
        "INSERT INTO company_memberships \
         (id, company_id, principal_type, principal_id, status, membership_role) \
         VALUES ($1, $2, 'user', $3, 'archived', 'member')",
    )
    .bind(Uuid::new_v4())
    .bind(cid)
    .bind(&u2)
    .execute(db.pool())
    .await
    .expect("archived member");
    let entries = pc_repos::company_member::CompanyMemberRepo::new(&db)
        .user_directory(cid)
        .await
        .unwrap();
    let ids: Vec<&str> = entries.iter().map(|e| e.user_id.as_str()).collect();
    assert!(ids.contains(&u1.as_str()));
    assert!(!ids.contains(&u2.as_str()), "archived user must not appear");
}

#[tokio::test(flavor = "current_thread")]
async fn repo_list_for_org_chart_returns_minimal_columns() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "org-chart").await;
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, reports_to, status, adapter_type) \
         VALUES ($1, $2, 'CEO', 'lead', NULL, 'idle', 'process'), \
                ($3, $2, 'CTO', 'general', $1, 'idle', 'process')",
    )
    .bind(Uuid::new_v4())
    .bind(cid)
    .bind(Uuid::new_v4())
    .execute(db.pool())
    .await
    .expect("insert agents");
    let rows = pc_repos::agent::AgentRepo::new(&db)
        .list_for_org_chart(cid)
        .await
        .expect("list_for_org_chart");
    assert_eq!(rows.len(), 2);
    let ceo = rows.iter().find(|r| r.name == "CEO").unwrap();
    assert!(ceo.reports_to.is_none());
    let cto = rows.iter().find(|r| r.name == "CTO").unwrap();
    assert_eq!(cto.reports_to, Some(ceo.id));
}

#[tokio::test(flavor = "current_thread")]
async fn repo_create_simple_writes_to_adapter_type_not_adapter_kind() {
    // Round 93 修复：原 inline SQL 用了不存在的 adapter_kind 列
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "create-simple").await;
    let row = pc_repos::agent::AgentRepo::new(&db)
        .create_simple(cid, "Test Agent", "engineer")
        .await
        .expect("create_simple");
    assert_eq!(row.name, "Test Agent");
    assert_eq!(row.role, "engineer");
    assert_eq!(row.adapter_type, "codex_local");
    assert_eq!(row.status, "active");
}

#[tokio::test(flavor = "current_thread")]
async fn repo_search_titles_uses_ilike_with_limit() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "search-titles").await;
    insert_issue(&db, cid, "Onboarding rollout").await;
    insert_issue(&db, cid, "Onboarding checklist").await;
    insert_issue(&db, cid, "Database migration").await;
    let rows = pc_repos::issue::IssueRepo::new(&db)
        .search_titles(cid, "onboarding", 10)
        .await
        .expect("search_titles");
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert!(row.title.to_lowercase().contains("onboarding"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn repo_list_events_by_company_supports_kind_filter() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let cid = insert_company(&db, "case-events").await;
    let case_id = Uuid::new_v4();
    // 需要先 insert 一个 case
    sqlx::query(
        "INSERT INTO cases (id, company_id, title, status, priority, kind) \
         VALUES ($1, $2, 'Test Case', 'open', 'medium', 'review')",
    )
    .bind(case_id)
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("insert case");
    for kind in ["created", "updated", "created"] {
        sqlx::query(
            "INSERT INTO case_events \
             (company_id, case_id, kind, actor_type, actor_user_id, payload) \
             VALUES ($1, $2, $3, 'user', 'u-test', '{}'::jsonb)",
        )
        .bind(cid)
        .bind(case_id)
        .bind(kind)
        .execute(db.pool())
        .await
        .expect("insert event");
    }
    let all = pc_repos::case::CaseRepo::new(&db)
        .list_events_by_company(cid, None, 50)
        .await
        .expect("list all");
    assert_eq!(all.len(), 3);
    let only_created = pc_repos::case::CaseRepo::new(&db)
        .list_events_by_company(cid, Some("created"), 50)
        .await
        .expect("list filtered");
    assert_eq!(only_created.len(), 2);
    assert!(only_created.iter().all(|e| e.kind == "created"));
}

// =====================================================================
// HTTP 层契约测试
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_create_agent_uses_adapter_type_column() {
    // Round 93 修复：原 POST /api/companies/:id/agents 用 adapter_kind 列 100% 500
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "http-agent").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{cid}/agents"),
        serde_json::json!({"name": "Live Agent", "role": "engineer"}),
    )
    .await;
    assert_eq!(status, 200, "must succeed (was 500 with adapter_kind bug)");
    assert_eq!(body["adapterType"], "codex_local");
    assert_eq!(body["name"], "Live Agent");
    assert_eq!(body["role"], "engineer");
}

#[tokio::test(flavor = "current_thread")]
async fn http_user_directory_returns_company_users() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "http-user-dir").await;
    let u = insert_user(&db, "u").await;
    add_membership(&db, cid, &u, "owner").await;

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{cid}/user-directory"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["count"], 1);
    assert_eq!(body["items"][0]["role"], "owner");
    assert_eq!(body["items"][0]["userId"], serde_json::json!(u));
}

#[tokio::test(flavor = "current_thread")]
async fn http_activity_uses_real_schema_columns() {
    // Round 93 修复：原 /activity 选了不存在的列 kind/actor_user_id/payload
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "http-activity").await;
    insert_activity(&db, cid, "issue.created", "issue", "iss-1").await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{cid}/activity"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert!(body["count"].as_i64().unwrap() >= 1);
    let item = &body["items"][0];
    assert_eq!(item["action"], "issue.created");
    assert_eq!(item["actorType"], "system");
    assert_eq!(item["entityType"], "issue");
}

#[tokio::test(flavor = "current_thread")]
async fn http_search_extract_finds_matching_titles() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "http-search").await;
    insert_issue(&db, cid, "Onboarding flow").await;
    insert_issue(&db, cid, "Database tweak").await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{cid}/search/extract"),
        serde_json::json!({"query": "onboarding", "limit": 5}),
    )
    .await;
    assert_eq!(status, 200);
    let titles: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["Onboarding flow"]);
}

#[tokio::test(flavor = "current_thread")]
async fn http_provision_built_in_agent_returns_stub() {
    // Round 93：company_built_in_agent_provisions 表不存在 → 返回 stub
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "http-provision").await;
    let bid = Uuid::new_v4();
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{cid}/built-in-agents/{bid}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["provisioned"], false);
    assert_eq!(body["builtInAgentId"], serde_json::json!(bid));
}

#[tokio::test(flavor = "current_thread")]
async fn http_get_org_returns_nodes_and_edges() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let state = test_state(db.clone());
    let app = routes::router().with_state(state);
    let cid = insert_company(&db, "http-org").await;
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, reports_to, status, adapter_type) \
         VALUES ($1, $2, 'Boss', 'lead', NULL, 'idle', 'process'), \
                ($3, $2, 'Worker', 'general', $1, 'idle', 'process')",
    )
    .bind(root)
    .bind(cid)
    .bind(child)
    .execute(db.pool())
    .await
    .expect("insert agents");
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{cid}/org"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(body["edges"].as_array().unwrap().len(), 1);
    assert_eq!(body["roots"].as_array().unwrap().len(), 1);
}

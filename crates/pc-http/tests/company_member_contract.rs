//! Integration tests for `pc_repos::company_member` + the HTTP layer
//! (`GET /api/companies/:id/members`, `PATCH .../members/:id`,
//! `DELETE .../members/:id` 走 archive)。
//!
//! 这些测试同时验证 Round 89 修复：
//! - 原 inline SQL 引用不存在的 `company_members.role` / `archived_at`
//! - 现改用真实的 `company_memberships.membership_role` + `status='archived'`
//! - "user" 表 LEFT JOIN 字段通过 `principal_id` 关联

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::{ActorRegistry, Timestamp};
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{routes, state::ConfigSnapshot, AppState};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::{company_member, Db};
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
        Arc::new(WsState::new(
            realtime.clone(),
            "test".to_string(),
        )),
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

async fn insert_user(db: &Db, tag: &str) -> String {
    let id = format!("u_test_{}_{}", tag, Uuid::new_v4().simple());
    let email = format!("{id}@test.local");
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, image, created_at, updated_at) \
         VALUES ($1, $2, $3, true, NULL, now(), now())",
    )
    .bind(&id)
    .bind(format!("Tester {tag}"))
    .bind(&email)
    .execute(db.pool())
    .await
    .expect("insert user");
    id
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("cm-{tag}-{id}"))
        .bind(format!("C{}", &id.simple().to_string()[..5]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn add_member(
    db: &Db,
    company_id: Uuid,
    user_id: &str,
    role: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_memberships \
         (id, company_id, principal_type, principal_id, status, membership_role) \
         VALUES ($1, $2, 'user', $3, 'active', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(user_id)
    .bind(role)
    .execute(db.pool())
    .await
    .expect("insert member");
    id
}

// =====================================================================
// Repo 层：list_by_company + filter
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn repo_list_returns_only_active_members_with_principal_user() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "list-active").await;
    let owner_id = insert_user(&db, "owner").await;
    let viewer_id = insert_user(&db, "viewer").await;
    let agent_id = Uuid::new_v4();

    let owner_member = add_member(&db, company_id, &owner_id, "owner").await;
    let viewer_member = add_member(&db, company_id, &viewer_id, "member").await;

    // 制造一个 agent principal_type 行（不应被 list 出来）
    sqlx::query(
        "INSERT INTO company_memberships \
         (id, company_id, principal_type, principal_id, status, membership_role) \
         VALUES ($1, $2, 'agent', $3, 'active', 'member')",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("agent-token-{agent_id}"))
    .execute(db.pool())
    .await
    .expect("insert agent");

    let rows = company_member::CompanyMemberRepo::new(&db)
        .list_by_company(company_id, company_member::MemberFilter::user())
        .await
        .expect("list");
    assert_eq!(rows.len(), 2, "exactly 2 user members expected");

    // ORDER BY cm.membership_role ASC；字符串自然顺序（"member" < "owner"）。
    let owner_row = rows.iter().find(|r| r.id == owner_member).expect("owner present");
    let viewer_row = rows.iter().find(|r| r.id == viewer_member).expect("viewer present");
    assert_eq!(owner_row.membership_role, "owner");
    assert_eq!(viewer_row.membership_role, "member");

    // 至少一个 row 的 email 被 LEFT JOIN 进来
    assert!(
        rows.iter().any(|r| r.email.as_deref().unwrap_or_default().contains("@test.local")),
        "LEFT JOIN 'user' must populate email"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn repo_list_with_role_filter_returns_only_matching_role() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "list-role").await;
    let u1 = insert_user(&db, "r1").await;
    let u2 = insert_user(&db, "r2").await;
    add_member(&db, company_id, &u1, "owner").await;
    add_member(&db, company_id, &u2, "member").await;

    let filter = company_member::MemberFilter {
        include_archived: false,
        role: Some("owner"),
        ..company_member::MemberFilter::user()
    };
    let rows = company_member::CompanyMemberRepo::new(&db)
        .list_by_company(company_id, filter)
        .await
        .expect("list with role");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].membership_role, "owner");
}

#[tokio::test(flavor = "current_thread")]
async fn repo_list_include_archived_shows_archived_rows() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "list-incl-arc").await;
    let u1 = insert_user(&db, "a1").await;
    let u2 = insert_user(&db, "a2").await;
    let m1 = add_member(&db, company_id, &u1, "member").await;
    add_member(&db, company_id, &u2, "member").await;

    // 通过 Repo 把 m1 归档
    company_member::CompanyMemberRepo::new(&db)
        .archive(company_id, m1)
        .await
        .expect("archive m1");

    // 默认 filter（仅 active）：应只返回 m2
    let active = company_member::CompanyMemberRepo::new(&db)
        .list_by_company(company_id, company_member::MemberFilter::user())
        .await
        .expect("list active");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].principal_id, u2);

    // include_archived=true：应返回 2 行（含 m1，status='archived'）
    let filter = company_member::MemberFilter {
        include_archived: true,
        role: None,
        ..company_member::MemberFilter::user()
    };
    let all = company_member::CompanyMemberRepo::new(&db)
        .list_by_company(company_id, filter)
        .await
        .expect("list all");
    assert_eq!(all.len(), 2);
    let archived = all.iter().find(|r| r.id == m1).expect("m1 row");
    assert_eq!(archived.status, "archived");
}

// =====================================================================
// Repo 层：patch + archive
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn repo_patch_role_updates_membership_role() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "patch-role").await;
    let u1 = insert_user(&db, "pr").await;
    let m1 = add_member(&db, company_id, &u1, "member").await;

    let updated = company_member::CompanyMemberRepo::new(&db)
        .patch(
            company_id,
            m1,
            company_member::MemberPatch {
                membership_role: Some("admin".to_string()),
                status: None,
            },
        )
        .await
        .expect("patch");
    assert!(updated.is_some());
    assert_eq!(updated.unwrap().membership_role, "admin");

    // find_by_user 验证回写
    let row = company_member::CompanyMemberRepo::new(&db)
        .find_by_user(company_id, &u1)
        .await
        .expect("find");
    let row = row.unwrap();
    assert_eq!(row.membership_role, "admin");
    assert_eq!(row.status, "active");
}

#[tokio::test(flavor = "current_thread")]
async fn repo_archive_is_idempotent() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "archive").await;
    let u1 = insert_user(&db, "ar").await;
    let m1 = add_member(&db, company_id, &u1, "member").await;

    let first = company_member::CompanyMemberRepo::new(&db)
        .archive(company_id, m1)
        .await
        .expect("archive 1");
    assert!(first, "first archive returns true");

    let second = company_member::CompanyMemberRepo::new(&db)
        .archive(company_id, m1)
        .await
        .expect("archive 2");
    assert!(!second, "second archive returns false (idempotent)");

    // status 切到 archived 且 find_by_id 仍能查到
    let row = company_member::CompanyMemberRepo::new(&db)
        .find_by_id(company_id, m1)
        .await
        .expect("find after archive");
    let row = row.expect("row still present");
    assert_eq!(row.status, "archived");
}

// =====================================================================
// HTTP 层：`GET /api/companies/:id/members`
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_list_members_returns_joined_user_fields() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "http-list").await;
    let u1 = insert_user(&db, "h1").await;
    add_member(&db, company_id, &u1, "owner").await;

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/members"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200, "list members: {body}");
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    let m = &items[0];
    assert_eq!(m["userId"], u1);
    assert_eq!(m["role"], "owner");
    assert_eq!(m["status"], "active");
    assert!(
        m["email"].as_str().unwrap_or_default().contains("@test.local"),
        "LEFT JOIN 'user'.email should populate"
    );
    assert_eq!(m["companyId"], company_id.to_string());
}

//! 集成测试：
//! - `pc_repos::principal_permission_grant` 的 upsert/replace/list/revoke 契约
//! - `patch_member_role_and_grants` HTTP handler 把 role + grants 正确落库
//!
//! 验证 Round 91 修复：
//! - 原 inline SQL 引用 `company_members.role` / `company_members.permissions` 列不存在，
//!   现统一改用 `company_memberships.membership_role` + `principal_permission_grants` 两张真表

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{routes, state::ConfigSnapshot, AppState};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::{
    company_member::{CompanyMemberRepo, MemberFilter, MemberPatch, MemberStatus},
    principal_permission_grant::{PermissionGrantInput, PrincipalPermissionGrantRepo},
    Db,
};
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

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("mp-{tag}-{id}"))
        .bind(format!("P{}", &id.simple().to_string()[..5]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_user(db: &Db, tag: &str) -> String {
    let id = format!("u_mp_{tag}_{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, image, created_at, updated_at) VALUES ($1, $2, $3, true, NULL, now(), now())",
    )
    .bind(&id)
    .bind(format!("mp-user-{tag}"))
    .bind(format!("{id}@test.local"))
    .execute(db.pool())
    .await
    .expect("insert user");
    id
}

async fn insert_member(db: &Db, company_id: Uuid, user_id: &str, role: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_memberships (id, company_id, principal_type, principal_id, status, membership_role) VALUES ($1, $2, 'user', $3, 'active', $4)",
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
// Repo 层
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn repo_upsert_one_then_list_returns_row() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "upsert-one").await;
    let user_id = insert_user(&db, "u1").await;

    let repo = PrincipalPermissionGrantRepo::new(&db);
    let input = PermissionGrantInput {
        permission_key: "tasks:assign".to_string(),
        scope: Some(serde_json::json!({"team": "core"})),
        granted_by_user_id: Some("u_admin".to_string()),
    };
    let row = repo
        .upsert_one(company_id, "user", &user_id, input.clone())
        .await
        .expect("upsert");
    assert_eq!(row.permission_key, "tasks:assign");

    let rows = repo
        .list_for_principal(company_id, "user", &user_id)
        .await
        .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].permission_key, "tasks:assign");

    // 二次 upsert 同一 key 应更新 scope（unique idx 防重）
    let input2 = PermissionGrantInput {
        permission_key: "tasks:assign".to_string(),
        scope: Some(serde_json::json!({"team": "edge"})),
        granted_by_user_id: Some("u_other".to_string()),
    };
    let updated = repo
        .upsert_one(company_id, "user", &user_id, input2)
        .await
        .expect("upsert 2");
    let scope = updated.scope.unwrap();
    assert_eq!(scope["team"], "edge");

    let rows = repo
        .list_for_principal(company_id, "user", &user_id)
        .await
        .expect("list 2");
    assert_eq!(rows.len(), 1, "unique idx keeps single row");
}

#[tokio::test(flavor = "current_thread")]
async fn repo_replace_all_clears_old_then_inserts_new() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "replace-all").await;
    let user_id = insert_user(&db, "u2").await;

    let repo = PrincipalPermissionGrantRepo::new(&db);
    for k in &["tasks:assign", "boards:read", "projects:create"] {
        repo.upsert_one(
            company_id,
            "user",
            &user_id,
            PermissionGrantInput {
                permission_key: k.to_string(),
                scope: None,
                granted_by_user_id: None,
            },
        )
        .await
        .expect("preset");
    }
    assert_eq!(
        repo.list_for_principal(company_id, "user", &user_id)
            .await
            .unwrap()
            .len(),
        3
    );

    let mut tx = db.pool().begin().await.expect("tx begin");
    let new_grants = vec![
        PermissionGrantInput {
            permission_key: "tasks:assign".to_string(),
            scope: Some(serde_json::json!({"scope": "primary"})),
            granted_by_user_id: Some("u_admin".to_string()),
        },
        PermissionGrantInput {
            permission_key: "agents:dispatch".to_string(),
            scope: None,
            granted_by_user_id: Some("u_admin".to_string()),
        },
    ];
    let written = repo
        .replace_all_for_principal(
            &mut tx,
            company_id,
            "user",
            &user_id,
            &new_grants,
        )
        .await
        .expect("replace");
    tx.commit().await.expect("commit");

    assert_eq!(written[0].permission_key, "agents:dispatch");
    assert_eq!(written[1].permission_key, "tasks:assign");

    let after = repo
        .list_for_principal(company_id, "user", &user_id)
        .await
        .expect("list 2");
    assert_eq!(after.len(), 2);
    assert!(after.iter().any(|r| r.permission_key == "tasks:assign"));
    assert!(after.iter().any(|r| r.permission_key == "agents:dispatch"));
    assert!(!after.iter().any(|r| r.permission_key == "boards:read"));
}

#[tokio::test(flavor = "current_thread")]
async fn repo_revoke_one_returns_false_when_no_match() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "revoke-noop").await;
    let user_id = insert_user(&db, "u3").await;
    let repo = PrincipalPermissionGrantRepo::new(&db);

    let removed = repo
        .revoke_one(company_id, "user", &user_id, "nonexistent:key")
        .await
        .expect("revoke");
    assert!(!removed);

    repo.upsert_one(
        company_id,
        "user",
        &user_id,
        PermissionGrantInput {
            permission_key: "tasks:assign".to_string(),
            scope: None,
            granted_by_user_id: None,
        },
    )
    .await
    .expect("add");
    let removed = repo
        .revoke_one(company_id, "user", &user_id, "tasks:assign")
        .await
        .expect("revoke 2");
    assert!(removed);

    let after = repo
        .list_for_principal(company_id, "user", &user_id)
        .await
        .expect("list");
    assert!(after.is_empty());
}

// =====================================================================
// HTTP 层
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn http_patch_role_and_grants_writes_role_and_replaces_grants() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "http-rg").await;
    let user_id = insert_user(&db, "u4").await;
    let member_id = insert_member(&db, company_id, &user_id, "member").await;

    let grants_repo = PrincipalPermissionGrantRepo::new(&db);
    grants_repo
        .upsert_one(
            company_id,
            "user",
            &user_id,
            PermissionGrantInput {
                permission_key: "legacy:key".to_string(),
                scope: None,
                granted_by_user_id: None,
            },
        )
        .await
        .expect("legacy grant");

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{company_id}/members/{member_id}/role-and-grants"),
        serde_json::json!({
            "role": "admin",
            "grants": ["tasks:assign", "agents:dispatch"],
            "metadata": {"source": "test"},
        }),
    )
    .await;
    assert_eq!(status, 200, "patch: {body}");
    assert_eq!(body["role"], "admin");
    assert_eq!(body["userId"], user_id);

    let member = CompanyMemberRepo::new(&db)
        .find_by_id(company_id, member_id)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(member.membership_role, "admin");

    let grants = grants_repo
        .list_for_principal(company_id, "user", &user_id)
        .await
        .expect("list");
    let keys: Vec<&str> = grants.iter().map(|r| r.permission_key.as_str()).collect();
    assert!(keys.contains(&"tasks:assign"));
    assert!(keys.contains(&"agents:dispatch"));
    assert!(!keys.contains(&"legacy:key"));
}

#[tokio::test(flavor = "current_thread")]
async fn http_patch_role_and_grants_rejects_empty_role() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "http-err").await;
    let user_id = insert_user(&db, "u5").await;
    let member_id = insert_member(&db, company_id, &user_id, "member").await;

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{company_id}/members/{member_id}/role-and-grants"),
        serde_json::json!({"role": "   ", "grants": []}),
    )
    .await;
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap_or_default().contains("role"));
}

#[tokio::test(flavor = "current_thread")]
async fn http_patch_member_permissions_archives_via_status() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "http-arch").await;
    let user_id = insert_user(&db, "u6").await;
    let member_id = insert_member(&db, company_id, &user_id, "member").await;

    let app = routes::companies::router().with_state(test_state(db.clone()));
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{company_id}/members/{member_id}/permissions"),
        serde_json::json!({"role": "admin", "archived": true}),
    )
    .await;
    assert_eq!(status, 200, "patch: {body}");

    let after = CompanyMemberRepo::new(&db)
        .find_by_id(company_id, member_id)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(after.membership_role, "admin");
    assert_eq!(after.status, "archived");

    let active = CompanyMemberRepo::new(&db)
        .list_by_company(company_id, MemberFilter::user())
        .await
        .expect("list");
    assert!(!active.iter().any(|m| m.id == member_id));
}

#[tokio::test(flavor = "current_thread")]
async fn member_patch_status_to_archived_persists() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = insert_company(&db, "patch-arc").await;
    let user_id = insert_user(&db, "u7").await;
    let member_id = insert_member(&db, company_id, &user_id, "member").await;

    let updated = CompanyMemberRepo::new(&db)
        .patch(
            company_id,
            member_id,
            MemberPatch {
                membership_role: None,
                status: Some(MemberStatus::Archived),
            },
        )
        .await
        .expect("patch")
        .expect("present");
    assert_eq!(updated.status, "archived");
}

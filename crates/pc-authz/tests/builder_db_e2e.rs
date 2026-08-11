//! pc-authz：DB-backed ContextBuilder 端到端测试。
//!
//! 若 `PAPERCLIP_TEST_DATABASE_URL` 或 `DATABASE_URL` 未设置，所有 PG 依赖的测试
//! 自动 skip。这样 `cargo test` 在没有 DB 的开发环境也不会失败。
//!
//! 对照 Node `authorization-service.test.ts` 中的 setup helpers。

use pc_auth::Actor;
use pc_authz::{build_context, Action, CompanyRole, PermissionKey, Resource};
use pc_db::Db;
use pc_repos::company_member::CompanyMemberRepo;
use pc_repos::principal_permission_grant::PrincipalPermissionGrantRepo;
use serde_json::json;
use sqlx::Executor;
use uuid::Uuid;

fn test_db_url() -> Option<String> {
    if let Ok(url) = std::env::var("PAPERCLIP_TEST_DATABASE_URL") {
        if !url.is_empty() {
            return Some(url);
        }
    }
    if let Ok(url) = std::env::var("DATABASE_URL") {
        if !url.is_empty() {
            return Some(url);
        }
    }
    None
}

async fn try_connect() -> Option<Db> {
    let url = test_db_url()?;
    Db::connect(&url, 2, 1).await.ok()
}

async fn cleanup(db: &Db) {
    // Best-effort cleanup of test rows.
    let _ = db.pool()
        .execute("DELETE FROM principal_permission_grants WHERE granted_by_user_id LIKE 'pc-authz-test-%'")
        .await;
    let _ = db
        .pool()
        .execute("DELETE FROM company_memberships WHERE principal_id LIKE 'pc-authz-test-%'")
        .await;
    let _ = db
        .pool()
        .execute("DELETE FROM companies WHERE name LIKE 'pc-authz-test-%'")
        .await;
}

async fn ensure_company(db: &Db, label: &str) -> Uuid {
    let name = format!("pc-authz-test-{label}-{}", Uuid::new_v4());
    let row: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO companies (id, name, issue_prefix, status, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, 'active', now(), now()) RETURNING id",
    )
    .bind(&name)
    .bind("az")
    .fetch_optional(db.pool())
    .await
    .expect("insert company");
    row.expect("company id").0
}

#[tokio::test]
async fn builder_loads_user_membership_and_role() {
    let Some(db) = try_connect().await else {
        eprintln!("[skip] DATABASE_URL not set; skipping PG-dependent test");
        return;
    };
    cleanup(&db).await;
    let company_id = ensure_company(&db, "user_role").await;
    let user_id = format!("pc-authz-test-{}", Uuid::new_v4());

    // 插 active membership, role=admin
    sqlx::query(
        "INSERT INTO company_memberships (id, company_id, principal_id, principal_type, role, status, created_at, updated_at)          VALUES (gen_random_uuid(), $1, $2, 'user', $3, 'active', now(), now())",
    )
    .bind(company_id)
    .bind(&user_id)
    .bind("admin")
    .execute(db.pool())
    .await
    .expect("insert membership");

    let actor = Actor::User {
        id: user_id.clone(),
        name: None,
        email: None,
        is_instance_admin: false,
        company_ids: vec![],
        memberships: vec![],
        run_id: None,
    };
    let ctx = build_context(&db, &actor).await;
    assert_eq!(ctx.memberships.len(), 1);
    assert_eq!(ctx.memberships[0].company_id, company_id);
    assert_eq!(ctx.role, Some(CompanyRole::Admin));
    assert!(!ctx.is_instance_admin);

    cleanup(&db).await;
}

#[tokio::test]
async fn builder_loads_user_grants() {
    let Some(db) = try_connect().await else {
        eprintln!("[skip] DATABASE_URL not set; skipping PG-dependent test");
        return;
    };
    cleanup(&db).await;
    let company_id = ensure_company(&db, "grants").await;
    let user_id = format!("pc-authz-test-{}", Uuid::new_v4());

    sqlx::query(
        "INSERT INTO company_memberships (id, company_id, principal_id, principal_type, role, status, created_at, updated_at)          VALUES (gen_random_uuid(), $1, $2, 'user', 'viewer', 'active', now(), now())",
    )
    .bind(company_id)
    .bind(&user_id)
    .execute(db.pool())
    .await
    .expect("insert membership");

    PrincipalPermissionGrantRepo::new(&db)
        .upsert_one(
            company_id,
            "user",
            &user_id,
            pc_repos::principal_permission_grant::PermissionGrantInput {
                permission_key: PermissionKey::JoinsApprove.as_str().into(),
                scope: Some(json!({"consentedChange": true})),
                granted_by_user_id: Some(format!("pc-authz-test-{}", Uuid::new_v4())),
            },
        )
        .await
        .expect("upsert grant");

    let actor = Actor::User {
        id: user_id.clone(),
        name: None,
        email: None,
        is_instance_admin: false,
        company_ids: vec![],
        memberships: vec![],
        run_id: None,
    };
    let ctx = build_context(&db, &actor).await;
    assert!(ctx.grants.contains(&PermissionKey::JoinsApprove));

    // 验证：viewer 角色 + JoinsApprove grant → allow
    let decision = pc_authz::evaluate(
        &actor,
        &ctx,
        &Resource::Company { company_id },
        Action::Permission(PermissionKey::JoinsApprove),
    );
    assert!(decision.allowed);
    assert_eq!(decision.reason, pc_authz::Reason::AllowExplicitGrant);

    cleanup(&db).await;
}

#[tokio::test]
async fn builder_loads_agent_membership() {
    let Some(db) = try_connect().await else {
        eprintln!("[skip] DATABASE_URL not set; skipping PG-dependent test");
        return;
    };
    cleanup(&db).await;
    let company_id = ensure_company(&db, "agent").await;
    let agent_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO company_memberships (id, company_id, principal_id, principal_type, role, status, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, 'agent', 'member', 'active', now(), now())",
    )
    .bind(company_id)
    .bind(agent_id.to_string())
    .execute(db.pool())
    .await
    .expect("insert agent membership");

    let actor = Actor::Agent {
        id: agent_id,
        company_id,
        key_id: None,
        key_scope: Default::default(),
        run_id: None,
        on_behalf_of_user_id: None,
        on_behalf_of_memberships: vec![],
    };
    let ctx = build_context(&db, &actor).await;
    assert!(!ctx.memberships.is_empty());
    assert_eq!(ctx.memberships[0].company_id, company_id);

    cleanup(&db).await;
}

#[tokio::test]
async fn builder_instance_admin_short_circuits() {
    let Some(_db) = try_connect().await else {
        eprintln!("[skip] DATABASE_URL not set; skipping PG-dependent test");
        return;
    };
    // 即便没有 DB 行，instance_admin 应该短路
    let actor = Actor::User {
        id: "any-admin".into(),
        name: None,
        email: None,
        is_instance_admin: true,
        company_ids: vec![],
        memberships: vec![],
        run_id: None,
    };
    let db = _db;
    let ctx = build_context(&db, &actor).await;
    assert!(ctx.is_instance_admin);
    // 即便没有 membership 也 allow
    let decision = pc_authz::evaluate(
        &actor,
        &ctx,
        &Resource::Company {
            company_id: Uuid::new_v4(),
        },
        Action::Permission(PermissionKey::ToolsAdmin),
    );
    assert!(decision.allowed);
    assert_eq!(decision.reason, pc_authz::Reason::AllowInstanceAdmin);
}

#[tokio::test]
async fn builder_anonymous_returns_empty_context() {
    let Some(db) = try_connect().await else {
        eprintln!("[skip] DATABASE_URL not set; skipping PG-dependent test");
        return;
    };
    let ctx = build_context(&db, &Actor::Anonymous).await;
    assert!(ctx.memberships.is_empty());
    assert!(ctx.grants.is_empty());
    assert!(!ctx.is_instance_admin);
}

#[tokio::test]
async fn builder_system_returns_empty_context() {
    let Some(db) = try_connect().await else {
        eprintln!("[skip] DATABASE_URL not set; skipping PG-dependent test");
        return;
    };
    let ctx = build_context(&db, &Actor::System).await;
    assert!(ctx.memberships.is_empty());
    assert!(ctx.grants.is_empty());
}

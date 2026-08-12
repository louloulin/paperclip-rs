//! R626 集成测试：company_member.rs 5 个 query 修复回归保护。
//!
//! 覆盖 R625 修复的 5 处 SQL（user_id → principal_type='user' + principal_id）：
//! - `is_active_member`
//! - `list_company_ids_for_user`
//! - `list_for_user_with_company`（含 cm.role → cm.membership_role 修正）
//! - `replace_user_companies`（DELETE）
//! - `replace_user_companies`（INSERT）
//!
//! 测试环境：复用现有约定（postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos）
//! 每个测试用 unique company_id + unique user_id（uuid v4），互不污染。

use pc_db::Db;
use pc_repos::company_member::CompanyMemberRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect paperclip_repos")
}

/// 插入 unique 公司（不依赖 company_service 业务逻辑，纯 schema 测试）
async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r626-{tag}-{id}"))
        .bind(format!("R626{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

/// 插入 unique user（fake principal_id，模拟 "u_xxx" 格式）
async fn insert_fake_user(db: &Db, tag: &str) -> String {
    let id = format!("u_{}_{}", tag, Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) \
         VALUES ($1, $2, $3, false, now(), now())",
    )
    .bind(&id)
    .bind(format!("r626-user-{tag}"))
    .bind(format!(
        "r626-{}-{}@test.local",
        tag,
        Uuid::new_v4().simple()
    ))
    .execute(db.pool())
    .await
    .expect("insert user");
    id
}

/// 插入 owner 成员（principal_type='user' + principal_id）
async fn add_owner_member(db: &Db, company_id: Uuid, user_id: &str) {
    sqlx::query(
        "INSERT INTO company_memberships (company_id, principal_type, principal_id, status, membership_role) \
         VALUES ($1, 'user', $2, 'active', 'owner')",
    )
    .bind(company_id)
    .bind(user_id)
    .execute(db.pool())
    .await
    .expect("insert owner membership");
}

// ===== 1. is_active_member =====

/// Bug 修复后：active owner → true
#[tokio::test(flavor = "current_thread")]
async fn r626_is_active_member_owner_returns_true() {
    let db = db().await;
    let cid = insert_company(&db, "iam-owner").await;
    let uid = insert_fake_user(&db, "iam-owner").await;
    add_owner_member(&db, cid, &uid).await;

    let repo = CompanyMemberRepo::new(&db);
    let ok = repo
        .is_active_member(&uid, cid)
        .await
        .expect("is_active_member");
    assert!(ok, "owner should be active member");
}

/// Bug 修复后：active owner 但 status='archived' → false
#[tokio::test(flavor = "current_thread")]
async fn r626_is_active_member_archived_returns_false() {
    let db = db().await;
    let cid = insert_company(&db, "iam-archived").await;
    let uid = insert_fake_user(&db, "iam-archived").await;
    add_owner_member(&db, cid, &uid).await;
    sqlx::query(
        "UPDATE company_memberships SET status='archived' WHERE company_id=$1 AND principal_id=$2",
    )
    .bind(cid)
    .bind(&uid)
    .execute(db.pool())
    .await
    .expect("archive");

    let repo = CompanyMemberRepo::new(&db);
    let ok = repo
        .is_active_member(&uid, cid)
        .await
        .expect("is_active_member");
    assert!(!ok, "archived member should NOT be active");
}

/// Bug 修复后：user 是 agent 成员（principal_type='agent'）→ 不是 human member → false
#[tokio::test(flavor = "current_thread")]
async fn r626_is_active_member_agent_principal_excluded() {
    let db = db().await;
    let cid = insert_company(&db, "iam-agent").await;
    let agent_id = format!("agent_{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO company_memberships (company_id, principal_type, principal_id, status, membership_role) \
         VALUES ($1, 'agent', $2, 'active', 'member')",
    )
    .bind(cid).bind(&agent_id)
    .execute(db.pool()).await.expect("insert agent member");

    let repo = CompanyMemberRepo::new(&db);
    let ok = repo
        .is_active_member(&agent_id, cid)
        .await
        .expect("is_active_member");
    assert!(!ok, "agent principal should NOT count as user member");
}

// ===== 2. list_company_ids_for_user =====

/// Bug 修复后：user 拥有 2 个 company → 返回 2 个
#[tokio::test(flavor = "current_thread")]
async fn r626_list_company_ids_for_user_returns_all_active() {
    let db = db().await;
    let uid = insert_fake_user(&db, "lciu").await;
    let c1 = insert_company(&db, "lciu-1").await;
    let c2 = insert_company(&db, "lciu-2").await;
    add_owner_member(&db, c1, &uid).await;
    add_owner_member(&db, c2, &uid).await;

    let repo = CompanyMemberRepo::new(&db);
    let mut ids = repo.list_company_ids_for_user(&uid).await.expect("list");
    ids.sort();
    let mut want = vec![c1, c2];
    want.sort();
    assert_eq!(ids, want, "should return both active companies");
}

// ===== 3. list_for_user_with_company =====

/// Bug 修复后：返回 (company_id, name, membership_role, status) 四元组，role 不为 NULL
#[tokio::test(flavor = "current_thread")]
async fn r626_list_for_user_with_company_returns_membership_role() {
    let db = db().await;
    let uid = insert_fake_user(&db, "lfuwc").await;
    let cid = insert_company(&db, "lfuwc").await;
    add_owner_member(&db, cid, &uid).await;

    let repo = CompanyMemberRepo::new(&db);
    let rows = repo
        .list_for_user_with_company(&uid)
        .await
        .expect("list_for_user_with_company");
    let hit = rows.iter().find(|(id, _, _, _)| *id == cid).expect("found");
    assert_eq!(hit.2.as_deref(), Some("owner"), "membership_role = owner");
    assert_eq!(hit.3.as_deref(), Some("active"), "status = active");
}

// ===== 4. replace_user_companies (DELETE) =====

/// Bug 修复后：replace_user_companies 删掉 user 在所有 company 的 membership
#[tokio::test(flavor = "current_thread")]
async fn r626_replace_user_companies_clears_all_memberships() {
    let db = db().await;
    let uid = insert_fake_user(&db, "rucd").await;
    let c1 = insert_company(&db, "rucd-1").await;
    let c2 = insert_company(&db, "rucd-2").await;
    add_owner_member(&db, c1, &uid).await;
    add_owner_member(&db, c2, &uid).await;

    // 保留 0 个 company（应删除 c1 + c2 的 owner 关系）
    let repo = CompanyMemberRepo::new(&db);
    repo.replace_user_companies(&uid, &[])
        .await
        .expect("replace");

    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM company_memberships WHERE principal_type='user' AND principal_id=$1",
    )
    .bind(&uid)
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(remaining, 0, "all user memberships should be deleted");
}

/// Bug 修复后：replace_user_companies 正确 INSERT (principal_type, principal_id, company_id, membership_role, status)
#[tokio::test(flavor = "current_thread")]
async fn r626_replace_user_companies_inserts_with_principal_columns() {
    let db = db().await;
    let uid = insert_fake_user(&db, "ruci").await;
    let c1 = insert_company(&db, "ruci").await;

    let repo = CompanyMemberRepo::new(&db);
    repo.replace_user_companies(&uid, &[c1])
        .await
        .expect("replace");

    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT principal_type, principal_id, membership_role, status \
         FROM company_memberships WHERE company_id=$1",
    )
    .bind(c1)
    .fetch_optional(db.pool())
    .await
    .expect("select");
    let (pt, pid, role, st) = row.expect("membership row exists");
    assert_eq!(pt, "user", "principal_type");
    assert_eq!(pid, uid, "principal_id");
    assert_eq!(role, "member", "membership_role");
    assert_eq!(st, "active", "status");
}

// ===== 5. Sanity: 确认修复后用错的列 (user_id) 确实不存在 =====
/// 防回归：如果将来 schema 重新引入 user_id 列，这测试会提醒我们清理。
#[tokio::test(flavor = "current_thread")]
async fn r626_company_memberships_has_no_user_id_column() {
    let db = db().await;
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT column_name FROM information_schema.columns \
         WHERE table_name='company_memberships' AND column_name='user_id'",
    )
    .fetch_optional(db.pool())
    .await
    .expect("check schema");
    assert!(
        row.is_none(),
        "company_memberships should NOT have user_id column"
    );
}

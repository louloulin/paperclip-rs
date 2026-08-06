//! Round 133 集成测试：companies.rs 收尾 5 个 SQL 全部仓储化。
//!
//! 覆盖：
//! - FolderRepo::ensure_personal_root（get-or-create 'personal' kind）
//! - FolderRepo::create_with_kind_str（任意 kind 字符串）
//! - FolderRepo::next_position_for_kind（任意 kind 字符串的 MAX+1）
//! - CompanyRepo::create_owner_membership（owner 自动 active + ON CONFLICT 升级）

use pc_db::Db;
use pc_repos::company::CompanyRepo;
use pc_repos::folder::FolderRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r133-{tag}-{id}"))
        .bind(format!("R133{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

// ===== FolderRepo::ensure_personal_root =====

/// 1. ensure_personal_root — 首次创建。
#[tokio::test(flavor = "current_thread")]
async fn ensure_personal_root_creates_when_missing() {
    let db = db().await;
    let cid = insert_company(&db, "epc").await;
    let repo = FolderRepo::new(&db);
    let (row, created) = repo.ensure_personal_root(cid).await.expect("ensure");
    assert!(created);
    assert_eq!(row.kind, "personal");
    assert_eq!(row.name, "Personal");
    assert_eq!(row.position, 0);
}

/// 2. ensure_personal_root — 已存在时返回现有 + created=false。
#[tokio::test(flavor = "current_thread")]
async fn ensure_personal_root_idempotent() {
    let db = db().await;
    let cid = insert_company(&db, "epi").await;
    let repo = FolderRepo::new(&db);
    let (first, _) = repo.ensure_personal_root(cid).await.expect("first");
    let (second, created) = repo.ensure_personal_root(cid).await.expect("second");
    assert!(!created);
    assert_eq!(first.id, second.id);
}

/// 3. ensure_personal_root — 多 company 各自隔离。
#[tokio::test(flavor = "current_thread")]
async fn ensure_personal_root_isolates_tenants() {
    let db = db().await;
    let a = insert_company(&db, "ea").await;
    let b = insert_company(&db, "eb").await;
    let repo = FolderRepo::new(&db);
    let (ra, _) = repo.ensure_personal_root(a).await.expect("a");
    let (rb, _) = repo.ensure_personal_root(b).await.expect("b");
    assert_ne!(ra.id, rb.id);
}

// ===== FolderRepo::create_with_kind_str =====

/// 4. create_with_kind_str — 任意 kind 字符串。
#[tokio::test(flavor = "current_thread")]
async fn create_with_kind_str_accepts_arbitrary_kind() {
    let db = db().await;
    let cid = insert_company(&db, "cks").await;
    let repo = FolderRepo::new(&db);
    let row = repo
        .create_with_kind_str(cid, "personal", "My Personal", Some("#ff0000"), 5)
        .await
        .expect("create");
    assert_eq!(row.kind, "personal");
    assert_eq!(row.name, "My Personal");
    assert_eq!(row.color.as_deref(), Some("#ff0000"));
    assert_eq!(row.position, 5);
}

/// 5. create_with_kind_str — name 空白 trim。
#[tokio::test(flavor = "current_thread")]
async fn create_with_kind_str_trims_name() {
    let db = db().await;
    let cid = insert_company(&db, "ckt").await;
    let repo = FolderRepo::new(&db);
    let row = repo
        .create_with_kind_str(cid, "personal", "  spaced  ", None, 0)
        .await
        .expect("create");
    assert_eq!(row.name, "spaced");
}

/// 6. create_with_kind_str — 空 name 拒绝。
#[tokio::test(flavor = "current_thread")]
async fn create_with_kind_str_rejects_empty_name() {
    let db = db().await;
    let cid = insert_company(&db, "cke").await;
    let repo = FolderRepo::new(&db);
    let res = repo
        .create_with_kind_str(cid, "personal", "   ", None, 0)
        .await;
    assert!(res.is_err());
}

// ===== FolderRepo::next_position_for_kind =====

/// 7. next_position_for_kind — 空集合返回 1。
#[tokio::test(flavor = "current_thread")]
async fn next_position_for_kind_empty_returns_one() {
    let db = db().await;
    let cid = insert_company(&db, "npe").await;
    let repo = FolderRepo::new(&db);
    let p = repo
        .next_position_for_kind(cid, "personal")
        .await
        .expect("p");
    assert_eq!(p, 1);
}

/// 8. next_position_for_kind — 递增。
#[tokio::test(flavor = "current_thread")]
async fn next_position_for_kind_increments() {
    let db = db().await;
    let cid = insert_company(&db, "npi").await;
    let repo = FolderRepo::new(&db);
    repo.create_with_kind_str(cid, "personal", "a", None, 10)
        .await
        .expect("a");
    repo.create_with_kind_str(cid, "personal", "b", None, 11)
        .await
        .expect("b");
    let p = repo
        .next_position_for_kind(cid, "personal")
        .await
        .expect("p");
    assert_eq!(p, 12);
}

// ===== CompanyRepo::create_owner_membership =====

/// 9. create_owner_membership — 新用户创建 owner 行。
#[tokio::test(flavor = "current_thread")]
async fn create_owner_membership_inserts_new() {
    let db = db().await;
    let cid = insert_company(&db, "omi").await;
    let user = format!("u-{}", Uuid::new_v4());
    CompanyRepo::new(&db)
        .create_owner_membership(cid, &user)
        .await
        .expect("create");
    let row: (String, String) = sqlx::query_as(
        "SELECT status, membership_role FROM company_memberships WHERE company_id=$1 AND principal_id=$2",
    )
    .bind(cid).bind(&user)
    .fetch_one(db.pool()).await.expect("fetch");
    assert_eq!(row.0, "active");
    assert_eq!(row.1, "owner");
}

/// 10. create_owner_membership — 已存在时 ON CONFLICT 升级到 active。
#[tokio::test(flavor = "current_thread")]
async fn create_owner_membership_upgrades_existing() {
    let db = db().await;
    let cid = insert_company(&db, "omu").await;
    let user = format!("u-{}", Uuid::new_v4());
    sqlx::query("INSERT INTO company_memberships (company_id, principal_type, principal_id, status, membership_role) VALUES ($1,'user',$2,'inactive','viewer')")
        .bind(cid).bind(&user)
        .execute(db.pool()).await.expect("seed");
    CompanyRepo::new(&db)
        .create_owner_membership(cid, &user)
        .await
        .expect("upgrade");
    let row: (String, String) = sqlx::query_as(
        "SELECT status, membership_role FROM company_memberships WHERE company_id=$1 AND principal_id=$2",
    )
    .bind(cid).bind(&user)
    .fetch_one(db.pool()).await.expect("fetch");
    assert_eq!(row.0, "active");
    assert_eq!(row.1, "viewer"); // COALESCE 保留已有 role
}

/// 11. create_owner_membership — 已存在的 owner role 不被覆盖。
#[tokio::test(flavor = "current_thread")]
async fn create_owner_membership_preserves_existing_owner() {
    let db = db().await;
    let cid = insert_company(&db, "omo").await;
    let user = format!("u-{}", Uuid::new_v4());
    sqlx::query("INSERT INTO company_memberships (company_id, principal_type, principal_id, status, membership_role) VALUES ($1,'user',$2,'active','owner')")
        .bind(cid).bind(&user)
        .execute(db.pool()).await.expect("seed");
    CompanyRepo::new(&db)
        .create_owner_membership(cid, &user)
        .await
        .expect("noop");
    let row: (String, String) = sqlx::query_as(
        "SELECT status, membership_role FROM company_memberships WHERE company_id=$1 AND principal_id=$2",
    )
    .bind(cid).bind(&user)
    .fetch_one(db.pool()).await.expect("fetch");
    assert_eq!(row.1, "owner");
}

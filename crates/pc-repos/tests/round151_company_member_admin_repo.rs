//! Round 151 集成测试：admin 用户的 company access 仓储（replace + list）。
//!
//! 覆盖：
//! - CompanyMemberRepo::list_for_user_with_company
//! - CompanyMemberRepo::replace_user_companies（事务化）

use pc_db::Db;
use pc_repos::company_member::CompanyMemberRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r151-c-{tag}-{id}"))
        .bind(format!("R151{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_user(db: &Db, tag: &str) -> String {
    let id = format!("u_r151_{}_{}", tag, Uuid::new_v4().simple());
    sqlx::query(r#"INSERT INTO "user" (id, name, email) VALUES ($1, $2, $3)"#)
        .bind(&id)
        .bind(format!("r151-{tag}"))
        .bind(format!("r151_{tag}_{id}@x"))
        .execute(db.pool())
        .await
        .expect("user");
    id
}

async fn insert_membership(db: &Db, user_id: &str, company_id: Uuid) {
    sqlx::query(
        r#"INSERT INTO company_memberships (user_id, company_id, role, status)
         VALUES ($1, $2, 'member', 'active')"#,
    )
    .bind(user_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("membership");
}

// ===== list_for_user_with_company =====

/// 1. list_for_user_with_company — 返回 (company_id, name, role, status)。
#[tokio::test(flavor = "current_thread")]
async fn list_for_user_with_company_returns_rows() {
    let db = db().await;
    let user = insert_user(&db, "lw1").await;
    let c1 = insert_company(&db, "lw1a").await;
    let c2 = insert_company(&db, "lw1b").await;
    insert_membership(&db, &user, c1).await;
    insert_membership(&db, &user, c2).await;
    let repo = CompanyMemberRepo::new(&db);
    let rows = repo.list_for_user_with_company(&user).await.expect("list");
    assert_eq!(rows.len(), 2);
    let cids: std::collections::HashSet<Uuid> = rows.iter().map(|(cid, _, _, _)| *cid).collect();
    assert!(cids.contains(&c1));
    assert!(cids.contains(&c2));
    for (_, _, role, status) in &rows {
        assert_eq!(role.as_deref(), Some("member"));
        assert_eq!(status.as_deref(), Some("active"));
    }
}

/// 2. list_for_user_with_company — 无 membership 返回空。
#[tokio::test(flavor = "current_thread")]
async fn list_for_user_with_company_empty() {
    let db = db().await;
    let user = insert_user(&db, "lw2").await;
    let repo = CompanyMemberRepo::new(&db);
    let rows = repo.list_for_user_with_company(&user).await.expect("list");
    assert!(rows.is_empty());
}

// ===== replace_user_companies =====

/// 3. replace_user_companies — 删除全部并插入新集合。
#[tokio::test(flavor = "current_thread")]
async fn replace_user_companies_swaps_set() {
    let db = db().await;
    let user = insert_user(&db, "rp1").await;
    let old_c = insert_company(&db, "rp1-old").await;
    let new_c1 = insert_company(&db, "rp1-n1").await;
    let new_c2 = insert_company(&db, "rp1-n2").await;
    insert_membership(&db, &user, old_c).await;

    let repo = CompanyMemberRepo::new(&db);
    repo.replace_user_companies(&user, &[new_c1, new_c2])
        .await
        .expect("replace");

    let rows = repo.list_for_user_with_company(&user).await.expect("list");
    let cids: std::collections::HashSet<Uuid> = rows.iter().map(|(cid, _, _, _)| *cid).collect();
    assert!(!cids.contains(&old_c));
    assert!(cids.contains(&new_c1));
    assert!(cids.contains(&new_c2));
    assert_eq!(rows.len(), 2);
}

/// 4. replace_user_companies — 空集合会清空。
#[tokio::test(flavor = "current_thread")]
async fn replace_user_companies_empty_clears() {
    let db = db().await;
    let user = insert_user(&db, "rp2").await;
    let c = insert_company(&db, "rp2-c").await;
    insert_membership(&db, &user, c).await;

    let repo = CompanyMemberRepo::new(&db);
    repo.replace_user_companies(&user, &[]).await.expect("replace");

    let rows = repo.list_for_user_with_company(&user).await.expect("list");
    assert!(rows.is_empty());
}

/// 5. replace_user_companies — 重复调用幂等（ON CONFLICT DO NOTHING）。
#[tokio::test(flavor = "current_thread")]
async fn replace_user_companies_idempotent() {
    let db = db().await;
    let user = insert_user(&db, "rp3").await;
    let c = insert_company(&db, "rp3-c").await;

    let repo = CompanyMemberRepo::new(&db);
    repo.replace_user_companies(&user, &[c]).await.expect("first");
    repo.replace_user_companies(&user, &[c]).await.expect("second");

    let rows = repo.list_for_user_with_company(&user).await.expect("list");
    assert_eq!(rows.len(), 1);
}

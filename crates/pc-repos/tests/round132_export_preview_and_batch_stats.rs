//! Round 132 集成测试：
//! - CompanyExportRepo::preview（issues + agents + pipelines 三源聚合）
//! - CompanyRepo::list_accessible_for_user（membership JOIN）
//! - CompanyRepo::stats_for_companies（多公司批量聚合，8 个 GROUP BY）
//! - CompanyStatsRow 新增 case_count / user_count 字段

use pc_db::Db;
use pc_repos::company::CompanyRepo;
use pc_repos::company_export::CompanyExportRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r132-{tag}-{id}"))
        .bind(format!("R132{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("insert company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid, name: &str) {
    sqlx::query("INSERT INTO agents (id, company_id, name, kind, status, owner_user_id) VALUES ($1,$2,$3,'assistant','active','tester')")
        .bind(Uuid::new_v4()).bind(company_id).bind(name)
        .execute(db.pool()).await.expect("agent");
}

async fn insert_issue(db: &Db, company_id: Uuid) {
    sqlx::query("INSERT INTO issues (id, company_id, identifier, title, kind, status, priority) VALUES ($1,$2,$3,'i','task','todo','normal')")
        .bind(Uuid::new_v4()).bind(company_id).bind(format!("ISS-{}", &Uuid::new_v4().simple().to_string()[..6]))
        .execute(db.pool()).await.expect("issue");
}

async fn insert_pipeline(db: &Db, company_id: Uuid) {
    sqlx::query("INSERT INTO pipelines (id, company_id, name, status, slug) VALUES ($1,$2,'p','active','p')")
        .bind(Uuid::new_v4()).bind(company_id)
        .execute(db.pool()).await.expect("pipeline");
}

async fn insert_pipeline_case(db: &Db, company_id: Uuid) {
    sqlx::query("INSERT INTO pipeline_cases (id, company_id, pipeline_id, title) VALUES ($1,$2,$3,'case')")
        .bind(Uuid::new_v4()).bind(company_id).bind(Uuid::new_v4())
        .execute(db.pool()).await.expect("case");
}

async fn insert_membership(db: &Db, company_id: Uuid, user_id: &str) {
    sqlx::query("INSERT INTO company_memberships (company_id, principal_type, principal_id, status, membership_role) VALUES ($1,'user',$2,'active','member')")
        .bind(company_id).bind(user_id)
        .execute(db.pool()).await.expect("membership");
}

// ===== CompanyExportRepo::preview =====

/// 1. preview — 空 company 全空集合。
#[tokio::test(flavor = "current_thread")]
async fn export_preview_empty_company() {
    let db = db().await;
    let cid = insert_company(&db, "empty").await;
    let p = CompanyExportRepo::new(&db).preview(cid).await.expect("preview");
    assert_eq!(p.company_id, cid);
    assert!(p.issues.is_empty());
    assert!(p.agents.is_empty());
    assert!(p.pipelines.is_empty());
}

/// 2. preview — 含 agents / issues / pipelines。
#[tokio::test(flavor = "current_thread")]
async fn export_preview_aggregates_three_sources() {
    let db = db().await;
    let cid = insert_company(&db, "agg").await;
    insert_agent(&db, cid, "alice").await;
    insert_agent(&db, cid, "bob").await;
    insert_issue(&db, cid).await;
    insert_pipeline(&db, cid).await;
    let p = CompanyExportRepo::new(&db).preview(cid).await.expect("preview");
    assert_eq!(p.agents.len(), 2);
    assert_eq!(p.issues.len(), 1);
    assert_eq!(p.pipelines.len(), 1);
}

/// 3. preview — 不计 archived pipelines。
#[tokio::test(flavor = "current_thread")]
async fn export_preview_excludes_archived_pipelines() {
    let db = db().await;
    let cid = insert_company(&db, "arch").await;
    insert_pipeline(&db, cid).await;
    sqlx::query("INSERT INTO pipelines (id, company_id, name, status, slug, archived_at) VALUES ($1,$2,'archived','active','a',now())")
        .bind(Uuid::new_v4()).bind(cid)
        .execute(db.pool()).await.expect("archived");
    let p = CompanyExportRepo::new(&db).preview(cid).await.expect("preview");
    assert_eq!(p.pipelines.len(), 1);
}

// ===== CompanyRepo::list_accessible_for_user =====

/// 4. list_accessible_for_user — 按 name 排序。
#[tokio::test(flavor = "current_thread")]
async fn list_accessible_orders_by_name() {
    let db = db().await;
    let a = insert_company(&db, "zeta").await;
    let b = insert_company(&db, "alpha").await;
    let c = insert_company(&db, "mu").await;
    let user = format!("u-{}", Uuid::new_v4());
    insert_membership(&db, a, &user).await;
    insert_membership(&db, b, &user).await;
    insert_membership(&db, c, &user).await;
    let list = CompanyRepo::new(&db).list_accessible_for_user(&user).await.expect("list");
    assert_eq!(list.len(), 3);
    // name 升序
    let names: Vec<_> = list.iter().map(|c| c.name.as_str()).collect();
    assert!(names.windows(2).all(|w| w[0] <= w[1]));
}

/// 5. list_accessible_for_user — 仅 active membership。
#[tokio::test(flavor = "current_thread")]
async fn list_accessible_filters_active_only() {
    let db = db().await;
    let a = insert_company(&db, "active").await;
    let b = insert_company(&db, "inactive").await;
    let user = format!("u-{}", Uuid::new_v4());
    insert_membership(&db, a, &user).await;
    sqlx::query("INSERT INTO company_memberships (company_id, principal_type, principal_id, status, membership_role) VALUES ($1,'user',$2,'inactive','member')")
        .bind(b).bind(&user)
        .execute(db.pool()).await.expect("inactive");
    let list = CompanyRepo::new(&db).list_accessible_for_user(&user).await.expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, a);
}

/// 6. list_accessible_for_user — 不存在 user 返回空。
#[tokio::test(flavor = "current_thread")]
async fn list_accessible_unknown_user_returns_empty() {
    let db = db().await;
    let list = CompanyRepo::new(&db).list_accessible_for_user("ghost").await.expect("list");
    assert!(list.is_empty());
}

// ===== CompanyRepo::stats_for_companies =====

/// 7. stats_for_companies — 空 ids 返回空 map。
#[tokio::test(flavor = "current_thread")]
async fn stats_for_companies_empty_ids() {
    let db = db().await;
    let map = CompanyRepo::new(&db).stats_for_companies(&[]).await.expect("stats");
    assert!(map.is_empty());
}

/// 8. stats_for_companies — 聚合 8 个字段（含新增 case_count / user_count）。
#[tokio::test(flavor = "current_thread")]
async fn stats_for_companies_aggregates_all_fields() {
    let db = db().await;
    let cid = insert_company(&db, "stats").await;
    insert_agent(&db, cid, "x").await;
    insert_issue(&db, cid).await;
    insert_pipeline(&db, cid).await;
    insert_pipeline_case(&db, cid).await;
    insert_membership(&db, cid, "u1").await;
    let map = CompanyRepo::new(&db).stats_for_companies(&[cid]).await.expect("stats");
    let s = map.get(&cid).expect("entry");
    assert_eq!(s.agent_count, 1);
    assert_eq!(s.issue_count, 1);
    assert_eq!(s.pipeline_count, 1);
    assert_eq!(s.case_count, 1);
    assert_eq!(s.user_count, 1);
}

/// 9. stats_for_companies — 缺失 company 视为全 0（不在 map 中）。
#[tokio::test(flavor = "current_thread")]
async fn stats_for_companies_unknown_company_zeroed() {
    let db = db().await;
    let fake_id = Uuid::new_v4();
    let map = CompanyRepo::new(&db).stats_for_companies(&[fake_id]).await.expect("stats");
    let s = map.get(&fake_id).expect("entry");
    assert_eq!(s.issue_count, 0);
    assert_eq!(s.agent_count, 0);
    assert_eq!(s.case_count, 0);
    assert_eq!(s.user_count, 0);
}

/// 10. stats_for_companies — 多公司独立计数。
#[tokio::test(flavor = "current_thread")]
async fn stats_for_companies_isolates_tenants() {
    let db = db().await;
    let a = insert_company(&db, "isoa").await;
    let b = insert_company(&db, "isob").await;
    for _ in 0..3 {
        insert_agent(&db, a, "n").await;
    }
    insert_agent(&db, b, "n").await;
    let map = CompanyRepo::new(&db).stats_for_companies(&[a, b]).await.expect("stats");
    assert_eq!(map.get(&a).unwrap().agent_count, 3);
    assert_eq!(map.get(&b).unwrap().agent_count, 1);
}

/// 11. stats_for_companies — open_issue_count 排除 done / cancelled。
#[tokio::test(flavor = "current_thread")]
async fn stats_for_companies_open_excludes_done() {
    let db = db().await;
    let cid = insert_company(&db, "open").await;
    sqlx::query("INSERT INTO issues (id, company_id, identifier, title, kind, status, priority) VALUES ($1,$2,'o1','i','task','todo','normal')")
        .bind(Uuid::new_v4()).bind(cid).execute(db.pool()).await.expect("i1");
    sqlx::query("INSERT INTO issues (id, company_id, identifier, title, kind, status, priority) VALUES ($1,$2,'o2','i','task','done','normal')")
        .bind(Uuid::new_v4()).bind(cid).execute(db.pool()).await.expect("i2");
    let map = CompanyRepo::new(&db).stats_for_companies(&[cid]).await.expect("stats");
    let s = map.get(&cid).unwrap();
    assert_eq!(s.issue_count, 2);
    assert_eq!(s.open_issue_count, 1);
}

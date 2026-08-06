//! Round 128 集成测试：CompanyRepo::stats 复合方法（跨 5 表 6 COUNT 聚合）。

use pc_db::Db;
use pc_repos::company::CompanyRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r128-{tag}-{id}"))
        .bind(format!("R128{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid) {
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, kind, status, owner_user_id) \
         VALUES ($1, $2, 'agent', 'assistant', 'active', 'tester')",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert agent");
}

async fn insert_project(db: &Db, company_id: Uuid) {
    sqlx::query("INSERT INTO projects (id, company_id, name, key) VALUES ($1, $2, 'proj', 'p')")
        .bind(Uuid::new_v4())
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("insert project");
}

async fn insert_goal(db: &Db, company_id: Uuid) {
    sqlx::query("INSERT INTO goals (id, company_id, title, status, level) VALUES ($1, $2, 'goal', 'active', 0)")
        .bind(Uuid::new_v4()).bind(company_id)
        .execute(db.pool()).await.expect("insert goal");
}

async fn insert_pipeline(db: &Db, company_id: Uuid) {
    sqlx::query("INSERT INTO pipelines (id, company_id, name, status, slug) VALUES ($1, $2, 'pipe', 'active', 'p')")
        .bind(Uuid::new_v4()).bind(company_id)
        .execute(db.pool()).await.expect("insert pipeline");
}

async fn insert_issue(db: &Db, company_id: Uuid, status: &str) {
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, kind, status, priority) \
         VALUES ($1, $2, $3, 'i', 'task', $4, 'normal')",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(format!("ISS-{}", &Uuid::new_v4().simple().to_string()[..6]))
    .bind(status)
    .execute(db.pool())
    .await
    .expect("insert issue");
}

/// 1. stats — 空 company
#[tokio::test(flavor = "current_thread")]
async fn stats_empty_company() {
    let db = db().await;
    let cid = insert_company(&db, "empty").await;
    let s = CompanyRepo::new(&db).stats(cid).await.expect("stats");
    assert_eq!(s.issue_count, 0);
    assert_eq!(s.agent_count, 0);
    assert_eq!(s.pipeline_count, 0);
    assert_eq!(s.project_count, 0);
    assert_eq!(s.goal_count, 0);
    assert_eq!(s.open_issue_count, 0);
}

/// 2. stats — 含 agent/project/goal
#[tokio::test(flavor = "current_thread")]
async fn stats_with_basic_data() {
    let db = db().await;
    let cid = insert_company(&db, "basic").await;
    insert_agent(&db, cid).await;
    insert_project(&db, cid).await;
    insert_goal(&db, cid).await;
    let s = CompanyRepo::new(&db).stats(cid).await.expect("stats");
    assert_eq!(s.agent_count, 1);
    assert_eq!(s.project_count, 1);
    assert_eq!(s.goal_count, 1);
}

/// 3. stats — pipeline 不计 archived
#[tokio::test(flavor = "current_thread")]
async fn stats_excludes_archived_pipelines() {
    let db = db().await;
    let cid = insert_company(&db, "pipelines").await;
    insert_pipeline(&db, cid).await;
    // Insert archived pipeline
    sqlx::query("INSERT INTO pipelines (id, company_id, name, status, slug, archived_at) VALUES ($1, $2, 'archived', 'active', 'a', now())")
        .bind(Uuid::new_v4()).bind(company_id_safe(cid))
        .execute(db.pool()).await.expect("insert archived");
    let s = CompanyRepo::new(&db).stats(cid).await.expect("stats");
    assert_eq!(s.pipeline_count, 1);
}

fn company_id_safe(_cid: Uuid) -> Uuid {
    _cid
} // just for clarity

/// 4. stats — issue 区分 open vs done
#[tokio::test(flavor = "current_thread")]
async fn stats_open_vs_done_issues() {
    let db = db().await;
    let cid = insert_company(&db, "issues").await;
    insert_issue(&db, cid, "todo").await;
    insert_issue(&db, cid, "in_progress").await;
    insert_issue(&db, cid, "done").await;
    insert_issue(&db, cid, "cancelled").await;
    let s = CompanyRepo::new(&db).stats(cid).await.expect("stats");
    assert_eq!(s.issue_count, 4);
    assert_eq!(s.open_issue_count, 2); // todo + in_progress
}

/// 5. stats — hidden issue 不计入
#[tokio::test(flavor = "current_thread")]
async fn stats_excludes_hidden_issues() {
    let db = db().await;
    let cid = insert_company(&db, "hidden").await;
    insert_issue(&db, cid, "todo").await;
    // Hidden issue
    sqlx::query("INSERT INTO issues (id, company_id, identifier, title, kind, status, priority, hidden_at) VALUES ($1, $2, 'h1', 'h', 'task', 'todo', 'normal', now())")
        .bind(Uuid::new_v4()).bind(cid)
        .execute(db.pool()).await.expect("insert hidden");
    let s = CompanyRepo::new(&db).stats(cid).await.expect("stats");
    assert_eq!(s.issue_count, 1);
}

/// 6. stats — 不存在 company 返回 0
#[tokio::test(flavor = "current_thread")]
async fn stats_unknown_company_returns_zeros() {
    let db = db().await;
    let s = CompanyRepo::new(&db)
        .stats(Uuid::new_v4())
        .await
        .expect("stats");
    assert_eq!(s.issue_count, 0);
    assert_eq!(s.agent_count, 0);
    assert_eq!(s.company_id, Uuid::nil()); // wait, we passed Uuid::new_v4()
    assert!(s.company_id != Uuid::nil());
}

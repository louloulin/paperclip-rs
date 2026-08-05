//! Round 136 集成测试：issues.rs relations 子模块仓储化（list_issue_cases / list_issue_runs）。
//!
//! 覆盖：
//! - CaseRepo::list_issue_cases
//! - HeartbeatRepo::list_runs_by_issue

use pc_db::Db;
use pc_repos::case::CaseRepo;
use pc_repos::heartbeat::HeartbeatRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r136-{tag}-{id}"))
        .bind(format!("R136{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("company");
    id
}

async fn insert_project(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO projects (id, company_id, name, key) VALUES ($1, $2, 'p', 'p')")
        .bind(id).bind(company_id)
        .execute(db.pool()).await.expect("project");
    id
}

async fn insert_case(db: &Db, company_id: Uuid, project_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO cases (id, company_id, project_id, title, status) VALUES ($1, $2, $3, 'case', 'active')")
        .bind(id).bind(company_id).bind(project_id)
        .execute(db.pool()).await.expect("case");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO issues (id, company_id, identifier, title, kind, status, priority) VALUES ($1,$2,$3,'i','task','todo','normal')")
        .bind(id).bind(company_id).bind(format!("ISS-{}", &id.simple().to_string()[..6]))
        .execute(db.pool()).await.expect("issue");
    id
}

async fn link_case_to_issue(db: &Db, company_id: Uuid, case_id: Uuid, issue_id: Uuid, role: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO case_issue_links (id, company_id, case_id, issue_id, role) VALUES ($1, $2, $3, $4, $5)")
        .bind(id).bind(company_id).bind(case_id).bind(issue_id).bind(role)
        .execute(db.pool()).await.expect("link");
    id
}

// ===== CaseRepo::list_issue_cases =====

/// 1. list_issue_cases — 空 issue 返回空集合。
#[tokio::test(flavor = "current_thread")]
async fn list_issue_cases_empty() {
    let db = db().await;
    let cid = insert_company(&db, "e").await;
    let iid = insert_issue(&db, cid).await;
    let list = CaseRepo::new(&db).list_issue_cases(iid).await.expect("list");
    assert!(list.is_empty());
}

/// 2. list_issue_cases — 列出 issue 关联的所有 case。
#[tokio::test(flavor = "current_thread")]
async fn list_issue_cases_returns_links() {
    let db = db().await;
    let cid = insert_company(&db, "links").await;
    let pid = insert_project(&db, cid).await;
    let c1 = insert_case(&db, cid, pid).await;
    let c2 = insert_case(&db, cid, pid).await;
    let iid = insert_issue(&db, cid).await;
    link_case_to_issue(&db, cid, c1, iid, "primary").await;
    link_case_to_issue(&db, cid, c2, iid, "secondary").await;
    let list = CaseRepo::new(&db).list_issue_cases(iid).await.expect("list");
    assert_eq!(list.len(), 2);
    let roles: Vec<_> = list.iter().map(|l| l.role.as_str()).collect();
    assert!(roles.contains(&"primary"));
    assert!(roles.contains(&"secondary"));
}

/// 3. list_issue_cases — 跨 issue 隔离。
#[tokio::test(flavor = "current_thread")]
async fn list_issue_cases_isolates() {
    let db = db().await;
    let cid = insert_company(&db, "iso").await;
    let pid = insert_project(&db, cid).await;
    let c = insert_case(&db, cid, pid).await;
    let i1 = insert_issue(&db, cid).await;
    let i2 = insert_issue(&db, cid).await;
    link_case_to_issue(&db, cid, c, i1, "primary").await;
    assert_eq!(CaseRepo::new(&db).list_issue_cases(i1).await.expect("i1").len(), 1);
    assert_eq!(CaseRepo::new(&db).list_issue_cases(i2).await.expect("i2").len(), 0);
}

/// 4. list_issue_cases — 字段投影含 caseStatus / projectId。
#[tokio::test(flavor = "current_thread")]
async fn list_issue_cases_full_fields() {
    let db = db().await;
    let cid = insert_company(&db, "ff").await;
    let pid = insert_project(&db, cid).await;
    let c = insert_case(&db, cid, pid).await;
    let iid = insert_issue(&db, cid).await;
    link_case_to_issue(&db, cid, c, iid, "primary").await;
    let list = CaseRepo::new(&db).list_issue_cases(iid).await.expect("list");
    assert_eq!(list.len(), 1);
    let row = &list[0];
    assert_eq!(row.case_id, c);
    assert_eq!(row.project_id, Some(pid));
    assert_eq!(row.status.as_deref(), Some("active"));
    assert_eq!(row.role, "primary");
}

// ===== HeartbeatRepo::list_runs_by_issue =====

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO agents (id, company_id, name, kind, status, owner_user_id) VALUES ($1,$2,'a','assistant','active','tester')")
        .bind(id).bind(company_id)
        .execute(db.pool()).await.expect("agent");
    id
}

async fn insert_heartbeat_run(db: &Db, company_id: Uuid, agent_id: Uuid, issue_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    let ctx = serde_json::json!({"issueId": issue_id.to_string(), "source": "test"});
    sqlx::query("INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, context_snapshot) VALUES ($1, $2, $3, 'on_demand', $4, $5)")
        .bind(id).bind(company_id).bind(agent_id).bind(status).bind(&ctx)
        .execute(db.pool()).await.expect("run");
    id
}

/// 5. list_runs_by_issue — 空 issue 返回空集合。
#[tokio::test(flavor = "current_thread")]
async fn list_runs_by_issue_empty() {
    let db = db().await;
    let list = HeartbeatRepo::new(&db).list_runs_by_issue(Uuid::new_v4(), 100).await.expect("list");
    assert!(list.is_empty());
}

/// 6. list_runs_by_issue — 通过 context_snapshot->>'issueId' 过滤。
#[tokio::test(flavor = "current_thread")]
async fn list_runs_by_issue_filters_by_context() {
    let db = db().await;
    let cid = insert_company(&db, "ctx").await;
    let aid = insert_agent(&db, cid).await;
    let i1 = insert_issue(&db, cid).await;
    let i2 = insert_issue(&db, cid).await;
    insert_heartbeat_run(&db, cid, aid, i1, "queued").await;
    insert_heartbeat_run(&db, cid, aid, i1, "running").await;
    insert_heartbeat_run(&db, cid, aid, i2, "queued").await;
    assert_eq!(HeartbeatRepo::new(&db).list_runs_by_issue(i1, 100).await.expect("i1").len(), 2);
    assert_eq!(HeartbeatRepo::new(&db).list_runs_by_issue(i2, 100).await.expect("i2").len(), 1);
}

/// 7. list_runs_by_issue — limit 生效。
#[tokio::test(flavor = "current_thread")]
async fn list_runs_by_issue_respects_limit() {
    let db = db().await;
    let cid = insert_company(&db, "lim").await;
    let aid = insert_agent(&db, cid).await;
    let iid = insert_issue(&db, cid).await;
    for _ in 0..5 {
        insert_heartbeat_run(&db, cid, aid, iid, "queued").await;
    }
    let list = HeartbeatRepo::new(&db).list_runs_by_issue(iid, 3).await.expect("list");
    assert_eq!(list.len(), 3);
}

/// 8. list_runs_by_issue — limit 自动 clamp 到 [1, 500]。
#[tokio::test(flavor = "current_thread")]
async fn list_runs_by_issue_clamps_limit() {
    let db = db().await;
    let repo = HeartbeatRepo::new(&db);
    // limit > 500 应被 clamp 到 500（SQL 仍能成功执行）
    let _ = repo.list_runs_by_issue(Uuid::new_v4(), 1000).await.expect("clamp high");
    // limit < 1 应被 clamp 到 1
    let _ = repo.list_runs_by_issue(Uuid::new_v4(), 0).await.expect("clamp low");
}

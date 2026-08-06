//! Round 107 集成测试：验证 `pc_repos::HeartbeatRepo::find_active_run_by_issue` /
//! `list_runs_by_issue` 和 `pc_repos::IssueRepo::list_assigned_active` 的纯仓储路径。
//!
//! 这些是新加的仓储方法，把原本散落在 agents.rs 内联 SQL 的 5 个端点提到 Repo 层。
//!
//! 影响路由：
//! - `GET /api/agents/me/issues/active-run`      -> get_issue_active_run
//! - `GET /api/issues/:id/heartbeat-runs`       -> list_issue_live_runs
//! - `GET /api/agents/me/inbox/lite`            -> get_self_inbox_lite (filter by company + agent)

use pc_db::Db;
use pc_repos::heartbeat::HeartbeatRepo;
use pc_repos::issue::IssueRepo;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r107-{tag}-{id}"))
        .bind(format!("R107{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    // 用最少必填字段
    sqlx::query(
        "INSERT INTO agents (company_id, name, adapter_kind, status, default_policy) \
         VALUES ($1, $2, 'process', 'active', 'allow')",
    )
    .bind(company_id)
    .bind(name)
    .execute(db.pool())
    .await
    .expect("insert agent");
    id
}

async fn insert_heartbeat_run_for_issue(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    issue_id: Uuid,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let snapshot = json!({"issueId": issue_id.to_string()});
    sqlx::query(
        "INSERT INTO heartbeat_runs             (company_id, agent_id, status, context_snapshot) \
         VALUES ($1, $2, $3::run_status, $4::jsonb)",
    )
    // run_status 用 enum，但简化为直接用 raw 字符串
    .bind(company_id)
    .bind(agent_id)
    .bind(status)
    .bind(snapshot)
    .execute(db.pool())
    .await
    .expect("insert");
    id
}

/// 1. find_active_run_by_issue：status in queued/claimed/running/paused 才返回
#[tokio::test(flavor = "current_thread")]
async fn heartbeat_repo_find_active_run_filters_by_status_set() {
    let db = db().await;
    let repo = HeartbeatRepo::new(&db);
    let cid = insert_company(&db, "find-active").await;
    let agent_id = insert_agent(&db, cid, "agent-1").await;
    let issue_id = Uuid::new_v4();

    // 不同 status 各一个
    insert_heartbeat_run_for_issue(&db, cid, agent_id, issue_id, "queued").await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let active = insert_heartbeat_run_for_issue(&db, cid, agent_id, issue_id, "running").await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    insert_heartbeat_run_for_issue(&db, cid, agent_id, issue_id, "completed").await;

    // 返回最近一个 status in (queued/claimed/running/paused) 的 id
    let found = repo
        .find_active_run_by_issue(issue_id)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found, active, "must return most recent running run");
}

/// 2. find_active_run_by_issue：没有匹配 status 时返回 None
#[tokio::test(flavor = "current_thread")]
async fn heartbeat_repo_find_active_returns_none_when_no_active_runs() {
    let db = db().await;
    let repo = HeartbeatRepo::new(&db);
    let cid = insert_company(&db, "none").await;
    let agent_id = insert_agent(&db, cid, "a").await;
    let issue_id = Uuid::new_v4();

    // 只插入已完成/失败的 run
    insert_heartbeat_run_for_issue(&db, cid, agent_id, issue_id, "completed").await;
    insert_heartbeat_run_for_issue(&db, cid, agent_id, issue_id, "failed").await;

    let found = repo.find_active_run_by_issue(issue_id).await.expect("find");
    assert!(found.is_none());
}

/// 3. list_runs_by_issue：按 started_at DESC 排序 + limit clamp
#[tokio::test(flavor = "current_thread")]
async fn heartbeat_repo_list_runs_by_issue_orders_recent_first() {
    let db = db().await;
    let repo = HeartbeatRepo::new(&db);
    let cid = insert_company(&db, "list-runs").await;
    let agent_id = insert_agent(&db, cid, "a").await;
    let issue_id = Uuid::new_v4();
    let other_issue = Uuid::new_v4();

    // 3 个 in this issue + 1 in other
    let r1 = insert_heartbeat_run_for_issue(&db, cid, agent_id, issue_id, "running").await;
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    let r2 = insert_heartbeat_run_for_issue(&db, cid, agent_id, issue_id, "queued").await;
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    let r3 = insert_heartbeat_run_for_issue(&db, cid, agent_id, issue_id, "completed").await;
    insert_heartbeat_run_for_issue(&db, cid, agent_id, other_issue, "queued").await;

    let rows = repo.list_runs_by_issue(issue_id, 50).await.expect("list");
    assert_eq!(rows.len(), 3, "only matches this issue");
    // 最近在前：r3 → r2 → r1
    assert_eq!(rows[0].id, r3);
    assert_eq!(rows[1].id, r2);
    assert_eq!(rows[2].id, r1);
}

/// 4. IssueRepo::list_assigned_active：todo/in_progress/blocked 不过滤 hidden
#[tokio::test(flavor = "current_thread")]
async fn issue_repo_list_assigned_active_filters_correctly() {
    let db = db().await;
    let cid = insert_company(&db, "iss-active").await;
    let agent_id = insert_agent(&db, cid, "a").await;
    let other_agent = insert_agent(&db, cid, "b").await;
    let repo = IssueRepo::new(&db);

    // 必须先有 project 才能 create issue
    let project_id: Uuid = Uuid::new_v4();
    sqlx::query("INSERT INTO projects (id, company_id, name, slug) VALUES ($1, $2, 'p', 'p')")
        .bind(project_id)
        .bind(cid)
        .execute(db.pool())
        .await
        .expect("insert project");

    // 构造几条 issue
    // 1. 我们的 agent + todo
    sqlx::query(
        "INSERT INTO issues (company_id, project_id, title, status, assignee_agent_id) \
         VALUES ($1, $2, 'i1', 'todo', $3)",
    )
    .bind(cid)
    .bind(project_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert issue 1");

    // 2. 我们的 agent + done (不在 active 集合)
    sqlx::query(
        "INSERT INTO issues (company_id, project_id, title, status, assignee_agent_id) \
         VALUES ($1, $2, 'i2', 'done', $3)",
    )
    .bind(cid)
    .bind(project_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert issue 2");

    // 3. 其他 agent + todo (不属于我们)
    sqlx::query(
        "INSERT INTO issues (company_id, project_id, title, status, assignee_agent_id) \
         VALUES ($1, $2, 'i3', 'todo', $3)",
    )
    .bind(cid)
    .bind(project_id)
    .bind(other_agent)
    .execute(db.pool())
    .await
    .expect("insert issue 3");

    // 4. 我们 agent + todo 但 hidden (应被过滤)
    sqlx::query(
        "INSERT INTO issues (company_id, project_id, title, status, assignee_agent_id, hidden_at) \
         VALUES ($1, $2, 'i4', 'todo', $3, now())",
    )
    .bind(cid)
    .bind(project_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert issue 4");

    let rows = repo
        .list_assigned_active(cid, agent_id, 100)
        .await
        .expect("list");
    // 只剩 i1（i2 done, i3 other agent, i4 hidden）
    assert_eq!(rows.len(), 1, "expected only i1; got {} rows", rows.len());
    assert_eq!(rows[0].title, "i1");
}

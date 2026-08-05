//! Round 108 集成测试：验证
//! - `IssueRepo::list_assigned_filtered` (status 列表 + responsible_user_id 过滤)
//! - `ExecutionRepo::find_operation_log_meta` (workspace_operations 元数据)

use pc_db::Db;
use pc_repos::execution::ExecutionRepo;
use pc_repos::issue::IssueRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r108-{tag}-{id}"))
        .bind(format!("R108{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_project(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO projects (id, company_id, name, slug) VALUES ($1, $2, 'p', 'p')")
        .bind(id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("insert project");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
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

/// 1. list_assigned_filtered：状态多值过滤 + user_id 过滤
#[tokio::test(flavor = "current_thread")]
async fn issue_repo_list_assigned_filtered_by_statuses() {
    let db = db().await;
    let cid = insert_company(&db, "filter-stat").await;
    let project_id = insert_project(&db, cid).await;
    let agent_id = insert_agent(&db, cid, "a").await;
    let repo = IssueRepo::new(&db);

    // 1: our agent + todo
    sqlx::query(
        "INSERT INTO issues (company_id, project_id, title, status, assignee_agent_id, responsible_user_id) \
         VALUES ($1, $2, 'todo_no_responsible', 'todo', $3, NULL)",
    )
    .bind(cid)
    .bind(project_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert 1");

    // 2: our agent + in_progress (按 user_id 过滤时只有这条中)
    sqlx::query(
        "INSERT INTO issues (company_id, project_id, title, status, assignee_agent_id, responsible_user_id) \
         VALUES ($1, $2, 'in_progress_with_alice', 'in_progress', $3, 'alice')",
    )
    .bind(cid)
    .bind(project_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert 2");

    // 3: our agent + done (不在 filter 状态集合内)
    sqlx::query(
        "INSERT INTO issues (company_id, project_id, title, status, assignee_agent_id, responsible_user_id) \
         VALUES ($1, $2, 'done', 'done', $3, NULL)",
    )
    .bind(cid)
    .bind(project_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert 3");

    // 默认 status = todo/in_progress/blocked, 不限 user_id
    let rows = repo
        .list_assigned_filtered(cid, agent_id, "todo,in_progress,blocked", None, 100)
        .await
        .expect("list default");
    assert_eq!(rows.len(), 2, "应排除 done");
    // 限制 responsible_user_id='alice' → 只剩 in_progress 那条
    let rows = repo
        .list_assigned_filtered(cid, agent_id, "todo,in_progress,blocked", Some("alice"), 100)
        .await
        .expect("list alice");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "in_progress_with_alice");
    // 限制 responsible_user_id='bob' → 0 条
    let rows = repo
        .list_assigned_filtered(cid, agent_id, "todo,in_progress,blocked", Some("bob"), 100)
        .await
        .expect("list bob");
    assert_eq!(rows.len(), 0);
}

/// 2. list_assigned_filtered：CSV 单元素也能正确过滤
#[tokio::test(flavor = "current_thread")]
async fn issue_repo_list_assigned_filtered_single_status() {
    let db = db().await;
    let cid = insert_company(&db, "single").await;
    let project_id = insert_project(&db, cid).await;
    let agent_id = insert_agent(&db, cid, "a").await;
    let repo = IssueRepo::new(&db);

    sqlx::query(
        "INSERT INTO issues (company_id, project_id, title, status, assignee_agent_id) \
         VALUES ($1, $2, 'todo1', 'todo', $3)",
    )
    .bind(cid)
    .bind(project_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert todo1");
    sqlx::query(
        "INSERT INTO issues (company_id, project_id, title, status, assignee_agent_id) \
         VALUES ($1, $2, 'prog1', 'in_progress', $3)",
    )
    .bind(cid)
    .bind(project_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert prog1");

    // 只要 todo
    let rows = repo
        .list_assigned_filtered(cid, agent_id, "todo", None, 100)
        .await
        .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "todo1");
}

/// 3. find_operation_log_meta：返回 5 个元数据列
#[tokio::test(flavor = "current_thread")]
async fn execution_repo_find_operation_log_meta_returns_5_cols() {
    let db = db().await;
    let cid = insert_company(&db, "op-meta").await;
    let run_id = Uuid::new_v4();
    let op_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace_operations             (company_id, heartbeat_run_id, phase, stdout_excerpt, stderr_excerpt, log_ref) \
         VALUES ($1, $2, 'run', 'stdout text', 'stderr text', 's3://op') RETURNING id",
    )
    .bind(cid)
    .bind(run_id)
    .fetch_one(db.pool())
    .await
    .expect("insert op");

    let meta = ExecutionRepo::new(&db)
        .find_operation_log_meta(op_id)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(meta.company_id, cid);
    assert_eq!(meta.heartbeat_run_id, Some(run_id));
    assert_eq!(meta.stdout_excerpt.as_deref(), Some("stdout text"));
    assert_eq!(meta.stderr_excerpt.as_deref(), Some("stderr text"));
    assert_eq!(meta.log_ref.as_deref(), Some("s3://op"));
}

/// 4. find_operation_log_meta：未知 id 返回 None
#[tokio::test(flavor = "current_thread")]
async fn execution_repo_find_operation_log_meta_returns_none_for_missing() {
    let db = db().await;
    let _ = insert_company(&db, "missing").await;
    let none = ExecutionRepo::new(&db)
        .find_operation_log_meta(Uuid::new_v4())
        .await
        .expect("find");
    assert!(none.is_none());
}

/// 5. schema 防漂移：workspace_operations 没有 phase='init' 等错误字段（仅验证存在真实列）
#[tokio::test(flavor = "current_thread")]
async fn workspace_operations_table_real_columns_audit() {
    let db = db().await;
    let real: Vec<(String,)> = sqlx::query_as(
        "SELECT column_name FROM information_schema.columns              WHERE table_name='workspace_operations'              AND column_name IN ('company_id','heartbeat_run_id','stdout_excerpt','stderr_excerpt','log_ref')",
    )
    .fetch_all(db.pool())
    .await
    .expect("query real");
    let names: std::collections::HashSet<String> = real.into_iter().map(|(s,)| s).collect();
    for must in ["company_id", "heartbeat_run_id", "stdout_excerpt", "stderr_excerpt", "log_ref"] {
        assert!(names.contains(must), "missing: {must}");
    }
}

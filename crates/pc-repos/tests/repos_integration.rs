//! 真实数据库集成测试：覆盖 25 个仓储的关键列表查询路径。
//!
//! 前置：DATABASE_URL 对应的 PostgreSQL 库已运行 cargo run -p pc-migrate-smoke 完成 196 条迁移。

use pc_db::Db;
use pc_repos::{
    activity::ActivityRepo,
    agent::AgentRepo,
    approval::ApprovalRepo,
    auth::AuthRepo,
    case::CaseRepo,
    company::CompanyRepo,
    decision::DecisionRepo,
    document::DocumentRepo,
    environment::EnvironmentRepo,
    execution::ExecutionRepo,
    folder::FolderRepo,
    goal::GoalRepo,
    heartbeat::HeartbeatRepo,
    inbox::InboxRepo,
    issue::IssueRepo,
    pipeline::PipelineRepo,
    plugin::PluginRepo,
    project::ProjectRepo,
    routine::RoutineRepo,
    settings::SettingsRepo,
    sidebar::SidebarRepo,
    skill::SkillRepo,
    smoke::SmokeRepo,
    summary::SummaryRepo,
    tool::ToolRepo,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static POOL: OnceLock<PgPool> = OnceLock::new();

fn shared_pool() -> PgPool {
    POOL.get_or_init(|| {
        let opts = PgConnectOptions::from_str(URL).expect("url");
        PgPoolOptions::new()
            .max_connections(16)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(20))
            .connect_lazy_with(opts)
    }).clone()
}

fn fresh_db() -> Db { Db::from_pool(shared_pool()) }

async fn truncate_all(db: &Db) {
    let tables: Vec<String> = sqlx::query_scalar(
        r#"SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename"#,
    )
    .fetch_all(db.pool())
    .await
    .expect("list tables");
    assert!(!tables.is_empty(), "no tables to truncate (migrations not applied?)");
    let quoted: Vec<String> = tables.into_iter().map(|t| format!("\"{t}\"")).collect();
    let joined = quoted.join(", ");
    let sql = format!("TRUNCATE TABLE {joined} RESTART IDENTITY CASCADE");
    sqlx::query(&sql).execute(db.pool()).await.expect("truncate");
}

#[tokio::test(flavor = "current_thread")]
async fn companies_list() {
    let db = fresh_db();
    truncate_all(&db).await;
    let cid = uuid::Uuid::new_v4();
    let _ = CompanyRepo::new(&db).list().await.expect("list companies");
    let _ = AgentRepo::new(&db).list_by_company(cid).await.expect("list agents");
    let _ = IssueRepo::new(&db).list_by_company(cid).await.expect("list issues");
}

#[tokio::test(flavor = "current_thread")]
async fn all_repos_queryable() {
    let db = fresh_db();
    truncate_all(&db).await;
    let cid = uuid::Uuid::new_v4();
    let _ = ActivityRepo::new(&db).list_recent(cid).await.expect("list activity");
    let _ = ApprovalRepo::new(&db).list_by_company(cid).await.expect("list approvals");
    let _ = CaseRepo::new(&db).list_by_company(cid).await.expect("list cases");
    let _ = DecisionRepo::new(&db).list_by_company(cid).await.expect("list decisions");
    let _ = DocumentRepo::new(&db).list_by_company(cid).await.expect("list documents");
    let _ = EnvironmentRepo::new(&db).list().await.expect("list environments");
    let _ = ExecutionRepo::new(&db).list_by_company(cid).await.expect("list executions");
    let _ = FolderRepo::new(&db).list_by_company(cid).await.expect("list folders");
    let _ = GoalRepo::new(&db).list_by_company(cid).await.expect("list goals");
    let _ = InboxRepo::new(&db).list_for_user(cid, "u1").await.expect("list inbox");
    let _ = PipelineRepo::new(&db).list_by_company(cid).await.expect("list pipelines");
    let _ = PluginRepo::new(&db).list().await.expect("list plugins");
    let _ = ProjectRepo::new(&db).list_by_company(cid).await.expect("list projects");
    let _ = RoutineRepo::new(&db).list_by_company(cid).await.expect("list routines");
    let _ = SettingsRepo::new(&db).get("theme").await.expect("get setting");
    let _ = SidebarRepo::new(&db).get("u1").await.expect("get sidebar");
    let _ = SkillRepo::new(&db).list_by_company(cid).await.expect("list skills");
    let _ = SmokeRepo::new(&db).list(cid).await.expect("list smoke");
    let _ = SummaryRepo::new(&db).list(cid).await.expect("list summary");
    let _ = ToolRepo::new(&db).list_by_company(cid).await.expect("list tools");
    let _ = HeartbeatRepo::new(&db).list_for_agent(cid).await.expect("list heartbeats");
    let _ = AuthRepo::new(&db).find_by_email("nobody@example.com").await.expect("find user");
}

//! 真实数据库集成测试：覆盖 25 个仓储的关键读写路径。
//!
//! 运行：DATABASE_URL=postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos cargo test -p pc-repos --test repos_integration -- --nocapture

use pc_db::{Db, Migrator};
use pc_repos::{
    activity::ActivityRepo,
    agent::AgentRepo,
    approval::ApprovalRepo,
    auth::AuthRepo,
    case::CaseRepo,
    company::{CompanyRepo, NewCompany},
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
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn setup() -> Db {
    let db = Db::connect(URL, 4, 1).await.expect("connect db");
    Migrator::run(&db).await.expect("migrate db");
    db
}

async fn truncate_all(db: &Db) {
    sqlx::query(
        "TRUNCATE TABLE          activity_log, agents, agent_memberships, agent_config_revisions, agent_api_keys,          approvals, approval_comments, cases, companies, company_memberships, company_logos,          company_user_sidebar_preferences, decisions, decision_training_examples,          document_annotation_anchor_snapshots, document_annotation_comments, document_annotation_threads,          document_memberships, document_revisions, documents,          environment_custom_image_setup_sessions, environment_custom_image_templates, environment_leases,          environments, execution_workspaces, folders, goals, heartbeat_run_events, heartbeat_runs,          agent_runtime_state, agent_task_sessions, agent_wakeup_requests,          inbox_dismissals, issues, issue_comments, pipelines, plugins, projects,          company_secrets, company_secret_versions, company_secret_provider_configs, company_secret_bindings,          company_skills, company_skill_policies, routines, summary_slots, tool_access, smoke_lab,          instance_settings, assets, cost_events, external_object_mentions, external_objects,          board_api_keys, budget_incidents, budget_policies, built_in_managed_resources, cli_auth_challenges,          feedback_votes, feedback_exports, finance_events, board_claim, status_cards, user_profiles,          session, auth, account, verification, organization, member, invitation, jwks, oauth_account,          two_factor RESTART IDENTITY CASCADE",
    )
    .execute(db.pool())
    .await
    .expect("truncate");
}

#[tokio::test(flavor = "current_thread")]
async fn company_round_trip() {
    let db = setup().await;
    truncate_all(&db).await;
    let repo = CompanyRepo::new(&db);
    let c = repo
        .create(NewCompany {
            name: "Acme".into(),
            description: None,
            issue_prefix: "AC".into(),
            budget_monthly_cents: 100_000,
            attachment_max_bytes: 10 * 1024 * 1024,
        })
        .await
        .unwrap();
    assert_eq!(c.name, "Acme");
    assert_eq!(c.issue_prefix, "AC");
    let found = repo.find(c.id).await.unwrap().unwrap();
    assert_eq!(found.id, c.id);
    let paused = repo.pause(c.id, "audit").await.unwrap();
    assert_eq!(paused.status, "paused");
}

#[tokio::test(flavor = "current_thread")]
async fn agent_issue_flow() {
    let db = setup().await;
    truncate_all(&db).await;
    let company = CompanyRepo::new(&db)
        .create(NewCompany {
            name: "Acme2".into(),
            description: None,
            issue_prefix: "A2".into(),
            budget_monthly_cents: 0,
            attachment_max_bytes: 1024,
        })
        .await
        .unwrap();
    let agent = AgentRepo::new(&db)
        .create(company.id, "codex-bot", "engineer", "codex-local", serde_json::json!({}))
        .await
        .unwrap();
    let issue = IssueRepo::new(&db)
        .create(company.id, "A2-1", "Test", Some("body"), "p1")
        .await
        .unwrap();
    let assigned = IssueRepo::new(&db).assign_agent(issue.id, agent.id).await.unwrap();
    assert_eq!(assigned.assignee_agent_id, Some(agent.id));
    let agents = AgentRepo::new(&db).list_by_company(company.id).await.unwrap();
    assert_eq!(agents.len(), 1);
    let issues = IssueRepo::new(&db).list_by_company(company.id).await.unwrap();
    assert_eq!(issues.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn other_repos_are_queryable() {
    let db = setup().await;
    truncate_all(&db).await;
    let company = CompanyRepo::new(&db)
        .create(NewCompany {
            name: "Acme3".into(),
            description: None,
            issue_prefix: "A3".into(),
            budget_monthly_cents: 0,
            attachment_max_bytes: 1024,
        })
        .await
        .unwrap();
    let id = company.id;
    let cid = company.id;
    let _ = ActivityRepo::new(&db).list_recent(cid).await.unwrap();
    let _ = ApprovalRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = CaseRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = DecisionRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = DocumentRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = EnvironmentRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = ExecutionRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = FolderRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = GoalRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = InboxRepo::new(&db).list_for_user(cid, "u1").await.unwrap();
    let _ = PipelineRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = PluginRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = ProjectRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = RoutineRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = SettingsRepo::new(&db).get("theme").await.unwrap();
    let _ = SidebarRepo::new(&db).get("u1").await.unwrap();
    let _ = SkillRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = SmokeRepo::new(&db).list(cid).await.unwrap();
    let _ = SummaryRepo::new(&db).list(cid).await.unwrap();
    let _ = ToolRepo::new(&db).list_by_company(cid).await.unwrap();
    let _ = HeartbeatRepo::new(&db).list_for_agent(id).await.unwrap();
    let _ = AuthRepo::new(&db).find_by_email("nobody@example.com").await.unwrap();
}

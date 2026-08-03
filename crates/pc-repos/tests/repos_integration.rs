//! 真实数据库集成测试：覆盖 25 个仓储的关键列表查询路径。
//!
//! 前置：`DATABASE_URL` 对应的 `PostgreSQL` 库已运行 cargo run -p pc-migrate-smoke 完成 196 条迁移。

use pc_db::Db;
use pc_repos::{
    activity::ActivityRepo,
    agent::{
        AgentRepo, HeartbeatInvocationSource, NewAgentWakeupRequest, WakeupActorType,
        WakeupRequestStatus, WakeupTriggerDetail,
    },
    approval::ApprovalRepo,
    auth::AuthRepo,
    case::{CaseActor, CaseFilter, CasePatch, CaseRepo, CaseStatus, NewCaseRecord},
    decision::DecisionRepo, document::DocumentRepo, environment::EnvironmentRepo,
    execution::ExecutionRepo, folder::FolderRepo, goal::GoalRepo, heartbeat::HeartbeatRepo,
    inbox::InboxRepo, pipeline::PipelineRepo, plugin::PluginRepo, project::ProjectRepo,
    routine::RoutineRepo, settings::SettingsRepo, sidebar::SidebarRepo, skill::SkillRepo,
    smoke::SmokeRepo, summary::SummaryRepo, tool::ToolRepo,
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
    })
    .clone()
}

fn fresh_db() -> Db {
    Db::from_pool(shared_pool())
}

async fn truncate_all(db: &Db) {
    let tables: Vec<String> = sqlx::query_scalar(
        r"SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
    )
    .fetch_all(db.pool())
    .await
    .expect("list tables");
    assert!(
        !tables.is_empty(),
        "no tables to truncate (migrations not applied?)"
    );
    let quoted: Vec<String> = tables.into_iter().map(|t| format!("\"{t}\"")).collect();
    let joined = quoted.join(", ");
    let sql = format!("TRUNCATE TABLE {joined} RESTART IDENTITY CASCADE");
    sqlx::query(&sql)
        .execute(db.pool())
        .await
        .expect("truncate");
}

#[tokio::test(flavor = "current_thread")]
async fn all_repos_queryable() {
    let db = fresh_db();
    truncate_all(&db).await;
    let cid = uuid::Uuid::new_v4();
    let _ = ActivityRepo::new(&db)
        .list_for_company(cid, &Default::default()).await.expect("list activity");
    let _ = ApprovalRepo::new(&db)
        .list_by_company_simple(cid).await.expect("list approvals");
    let _ = CaseRepo::new(&db).list_by_company(cid).await.expect("list cases");
    let _ = DecisionRepo::new(&db).list_by_company(cid).await.expect("list decisions");
    let _ = DocumentRepo::new(&db).list_by_company(cid).await.expect("list documents");
    let _ = EnvironmentRepo::new(&db).list_all().await.expect("list environments");
    let _ = ExecutionRepo::new(&db).list_by_company(cid).await.expect("list executions");
    let _ = FolderRepo::new(&db).list_by_company(cid).await.expect("list folders");
    let _ = GoalRepo::new(&db).list_by_company(cid).await.expect("list goals");
    let _ = InboxRepo::new(&db).list_for_user(cid, "u1").await.expect("list inbox");
    let _ = PipelineRepo::new(&db).list_by_company(cid).await.expect("list pipelines");
    let _ = PluginRepo::new(&db).list().await.expect("list plugins");
    let _ = ProjectRepo::new(&db).list_by_company(cid, true).await.expect("list projects");
    let _ = RoutineRepo::new(&db).list_by_company(cid).await.expect("list routines");
    let _ = SettingsRepo::new(&db).get().await.expect("get setting");
    let _ = SidebarRepo::new(&db).get("u1").await; // OK return Option
    let _ = SkillRepo::new(&db).list_for_company(cid).await.expect("list skills");
    let _ = SmokeRepo::new(&db).list_by_company(cid, None).await.expect("list smoke");
    let _ = SummaryRepo::new(&db).list_by_company(cid).await.expect("list summary");
    let _ = ToolRepo::new(&db).list_by_company(cid).await.expect("list tools");
    let _ = HeartbeatRepo::new(&db).list_for_agent(cid).await.expect("list heartbeats");
    let _ = AuthRepo::new(&db).find_by_email("nobody@example.com").await.expect("find user");
}

#[tokio::test(flavor = "current_thread")]
async fn agent_wakeup_lifecycle_is_atomic_and_company_scoped() {
    let db = fresh_db();
    truncate_all(&db).await;
    let company_id = uuid::Uuid::new_v4();
    let other_company_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();
    for (id, name, issue_prefix) in [
        (company_id, "Wakeup Corp", "WAK"),
        (other_company_id, "Other Corp", "OTH"),
    ] {
        sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(name)
            .bind(issue_prefix)
            .execute(db.pool())
            .await
            .expect("insert company");
    }
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type) \
         VALUES ($1, $2, 'Worker', 'general', 'process')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert agent");

    let repo = AgentRepo::new(&db);
    let queued = repo
        .create_wakeup_request(NewAgentWakeupRequest {
            company_id,
            agent_id,
            source: HeartbeatInvocationSource::OnDemand,
            trigger_detail: Some(WakeupTriggerDetail::Manual),
            reason: Some("integration-test".into()),
            payload: Some(serde_json::json!({"issueId": uuid::Uuid::new_v4()})),
            status: WakeupRequestStatus::Queued,
            coalesced_count: 0,
            requested_by_actor_type: Some(WakeupActorType::User),
            requested_by_actor_id: Some("user-1".into()),
            idempotency_key: Some("wake-integration-1".into()),
            run_id: None,
            error: None,
        })
        .await
        .expect("create wakeup");
    assert_eq!(queued.wakeup_status(), Some(WakeupRequestStatus::Queued));
    assert!(queued.claimed_at.is_none());
    assert!(queued.finished_at.is_none());
    assert!(repo
        .get_wakeup_request(other_company_id, queued.id)
        .await
        .expect("other company lookup")
        .is_none());

    let run_id = uuid::Uuid::new_v4();
    let claimed = repo
        .transition_wakeup_request(
            company_id,
            queued.id,
            WakeupRequestStatus::Claimed,
            Some(run_id),
            None,
        )
        .await
        .expect("claim wakeup")
        .expect("claimed row");
    assert_eq!(claimed.wakeup_status(), Some(WakeupRequestStatus::Claimed));
    assert_eq!(claimed.run_id, Some(run_id));
    assert!(claimed.claimed_at.is_some());
    assert!(claimed.finished_at.is_none());

    let completed = repo
        .transition_wakeup_request(
            company_id,
            queued.id,
            WakeupRequestStatus::Completed,
            None,
            Some("must be cleared for successful completion"),
        )
        .await
        .expect("complete wakeup")
        .expect("completed row");
    assert_eq!(
        completed.wakeup_status(),
        Some(WakeupRequestStatus::Completed)
    );
    assert!(completed.finished_at.is_some());
    assert!(completed.error.is_none());

    assert!(repo
        .transition_wakeup_request(
            company_id,
            queued.id,
            WakeupRequestStatus::Queued,
            None,
            None,
        )
        .await
        .expect("terminal transition")
        .is_none());
    assert!(repo
        .increment_wakeup_coalesced_count(company_id, queued.id)
        .await
        .expect("terminal coalesce")
        .is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn case_upsert_records_events_and_preserves_terminal_invariants() {
    let db = fresh_db();
    truncate_all(&db).await;
    let company_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, issue_prefix) VALUES ($1, 'Case Corp', 'CAS')",
    )
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert company");

    let repo = CaseRepo::new(&db);
    let created = repo
        .create_or_update(NewCaseRecord {
            company_id,
            project_id: None,
            case_type: "support".into(),
            key: Some("customer-42".into()),
            title: "Original".into(),
            summary: Some("Initial summary".into()),
            status: CaseStatus::Draft,
            fields: serde_json::json!({"severity": "medium"}),
            parent_case_id: None,
            actor: CaseActor::system(),
        })
        .await
        .expect("create case");
    assert!(created.created);
    assert_eq!(created.row.identifier, "CAS-C1");
    assert_eq!(created.row.case_number, 1);

    let updated = repo
        .create_or_update(NewCaseRecord {
            company_id,
            project_id: None,
            case_type: "support".into(),
            key: Some("customer-42".into()),
            title: "Resolved".into(),
            summary: None,
            status: CaseStatus::Done,
            fields: serde_json::json!({"severity": "low"}),
            parent_case_id: None,
            actor: CaseActor::system(),
        })
        .await
        .expect("upsert case");
    assert!(!updated.created);
    assert_eq!(updated.row.id, created.row.id);
    assert_eq!(updated.row.case_number, 1);
    assert_eq!(updated.row.case_status(), Some(CaseStatus::Done));
    assert!(updated.row.completed_at.is_some());

    let reopened = repo
        .update_full(
            company_id,
            created.row.id,
            CasePatch {
                status: Some(CaseStatus::InReview),
                ..Default::default()
            },
        )
        .await
        .expect("reopen case")
        .expect("reopened row");
    assert_eq!(reopened.case_status(), Some(CaseStatus::InReview));
    assert!(reopened.completed_at.is_none());

    let events = repo
        .list_events(company_id, created.row.id, 100)
        .await
        .expect("list events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, "updated");
    assert_eq!(events[1].kind, "created");

    let rows = repo
        .list_by_company_filtered(
            company_id,
            &CaseFilter {
                statuses: vec![CaseStatus::InReview],
                search: Some("resolved".into()),
                ..Default::default()
            },
        )
        .await
        .expect("filter cases");
    assert_eq!(rows.len(), 1);
    assert!(repo
        .get_for_company(uuid::Uuid::new_v4(), created.row.id)
        .await
        .expect("other company lookup")
        .is_none());
}

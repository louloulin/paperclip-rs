//! 真实数据库集成测试：覆盖 25 个仓储的关键列表查询路径。
//!
//! 前置：`DATABASE_URL` 对应的 `PostgreSQL` 库已运行 cargo run -p pc-migrate-smoke 完成 196 条迁移。

use pc_db::Db;
use pc_repos::{
    activity::ActivityRepo,
    cost::{AgentCostWindow, CostRepo},
    agent::{
        AgentRepo, HeartbeatInvocationSource, NewAgentWakeupRequest, WakeupActorType,
        WakeupRequestStatus, WakeupTriggerDetail,
    },
    approval::ApprovalRepo,
    auth::AuthRepo,
    case::{CaseActor, CaseFilter, CasePatch, CaseRepo, CaseStatus, NewCaseRecord},
    decision::DecisionRepo, document::DocumentRepo, environment::EnvironmentRepo,
    execution::ExecutionRepo, folder::FolderRepo, goal::GoalRepo,
    heartbeat::{
        CreateHeartbeat, HeartbeatEventStream, HeartbeatRepo, HeartbeatRunStatus,
        NewHeartbeatEvent, NewWatchdogDecision, WatchdogDecision,
    },
    inbox::InboxRepo, issue::IssueRepo, pipeline::PipelineRepo, plugin::PluginRepo, project::ProjectRepo,
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

#[tokio::test(flavor = "current_thread")]
async fn heartbeat_events_are_serialized_and_watchdog_decisions_are_scoped() {
    let db = fresh_db();
    truncate_all(&db).await;
    let company_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, issue_prefix) VALUES ($1, 'Heartbeat Corp', 'HBT')",
    )
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert company");
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type) \
         VALUES ($1, $2, 'Runner', 'general', 'process')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert agent");

    let repo = HeartbeatRepo::new(&db);
    let queued = repo
        .create(CreateHeartbeat {
            company_id,
            agent_id,
            invocation_source: "on_demand",
            trigger_detail: Some("manual"),
            responsible_user_id: Some("user-1"),
            wakeup_request_id: None,
            context_snapshot: Some(serde_json::json!({"issueId": null})),
        })
        .await
        .expect("create run");
    assert_eq!(queued.run_status(), Some(HeartbeatRunStatus::Queued));
    assert_eq!(queued.issue_comment_status, "not_applicable");
    assert!(!queued.log_compressed);

    let first = repo.append_event_full(
        &queued,
        NewHeartbeatEvent {
            event_type: "adapter.output".into(),
            stream: Some(HeartbeatEventStream::Stdout),
            level: None,
            color: None,
            message: Some("first".into()),
            payload: None,
        },
        true,
    );
    let second = repo.append_event_full(
        &queued,
        NewHeartbeatEvent {
            event_type: "adapter.output".into(),
            stream: Some(HeartbeatEventStream::Stderr),
            level: None,
            color: None,
            message: Some("second".into()),
            payload: None,
        },
        true,
    );
    let (first, second) = tokio::join!(first, second);
    let mut sequences = [
        first.expect("first concurrent event").seq,
        second.expect("second concurrent event").seq,
    ];
    sequences.sort_unstable();
    assert_eq!(sequences, [1, 2]);

    let running = repo
        .claim_for_company(company_id, queued.id, Some("user-1"), Some(1234), Some(1234))
        .await
        .expect("claim run")
        .expect("running row");
    assert_eq!(running.run_status(), Some(HeartbeatRunStatus::Running));
    assert!(running.started_at.is_some());
    assert!(running.process_started_at.is_some());

    let decision = repo
        .record_watchdog_decision(NewWatchdogDecision {
            company_id,
            run_id: queued.id,
            evaluation_issue_id: None,
            decision: WatchdogDecision::Continue,
            snoozed_until: None,
            reason: Some("output is expected".into()),
            created_by_agent_id: None,
            created_by_user_id: Some("user-1".into()),
            created_by_run_id: None,
        })
        .await
        .expect("record watchdog decision");
    assert_eq!(decision.decision, "continue");
    assert!(decision.snoozed_until.is_some());
    assert!(repo
        .active_watchdog_snooze(company_id, queued.id)
        .await
        .expect("active watchdog snooze")
        .is_some());
    assert!(repo
        .active_watchdog_snooze(uuid::Uuid::new_v4(), queued.id)
        .await
        .expect("other company watchdog lookup")
        .is_none());

    let succeeded = repo
        .transition_status(
            company_id,
            queued.id,
            HeartbeatRunStatus::Succeeded,
            None,
            None,
        )
        .await
        .expect("finish run")
        .expect("succeeded row");
    assert_eq!(succeeded.run_status(), Some(HeartbeatRunStatus::Succeeded));
    assert!(succeeded.finished_at.is_some());
    assert!(repo
        .transition_status(
            company_id,
            queued.id,
            HeartbeatRunStatus::Running,
            None,
            None,
        )
        .await
        .expect("terminal restart")
        .is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn cost_repo_sum_agent_window_cost_cents_matches_inserted_rows() {
    let db = fresh_db();
    truncate_all(&db).await;
    let company_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind("Cost Cap Corp")
        .bind("CRC")
        .execute(db.pool())
        .await
        .expect("insert company");
    let agent_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, adapter_config)          VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind("Cost Cap Bot")
    .bind("tester")
    .bind("process")
    .bind(serde_json::json!({}))
    .execute(db.pool())
    .await
    .expect("insert agent");

    let now = chrono::Utc::now();
    let start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight")
        .and_utc();
    let end = start + chrono::Duration::days(1);
    let yesterday = start - chrono::Duration::hours(1);

    // Insert two events in today's window and one in the previous day
    let repo = CostRepo::new(&db);
    repo.create_event(
        company_id,
        &pc_repos::cost::CreateCostEvent {
            agent_id,
            issue_id: None,
            project_id: None,
            goal_id: None,
            heartbeat_run_id: None,
            billing_code: None,
            provider: "openai".to_owned(),
            biller: "openai".to_owned(),
            billing_type: "api".to_owned(),
            model: "gpt-4".to_owned(),
            input_tokens: 100,
            cached_input_tokens: 0,
            output_tokens: 50,
            cost_cents: 250,
            occurred_at: now,
        },
    )
    .await
    .expect("insert today event 1");
    repo.create_event(
        company_id,
        &pc_repos::cost::CreateCostEvent {
            agent_id,
            issue_id: None,
            project_id: None,
            goal_id: None,
            heartbeat_run_id: None,
            billing_code: None,
            provider: "openai".to_owned(),
            biller: "openai".to_owned(),
            billing_type: "api".to_owned(),
            model: "gpt-4".to_owned(),
            input_tokens: 200,
            cached_input_tokens: 0,
            output_tokens: 150,
            cost_cents: 750,
            occurred_at: now,
        },
    )
    .await
    .expect("insert today event 2");
    repo.create_event(
        company_id,
        &pc_repos::cost::CreateCostEvent {
            agent_id,
            issue_id: None,
            project_id: None,
            goal_id: None,
            heartbeat_run_id: None,
            billing_code: None,
            provider: "openai".to_owned(),
            biller: "openai".to_owned(),
            billing_type: "api".to_owned(),
            model: "gpt-4".to_owned(),
            input_tokens: 999,
            cached_input_tokens: 0,
            output_tokens: 999,
            cost_cents: 9999,
            occurred_at: yesterday,
        },
    )
    .await
    .expect("insert yesterday event");

    let sum = repo
        .sum_agent_window_cost_cents(AgentCostWindow {
            company_id,
            agent_id,
            window_start: start,
            window_end: end,
        })
        .await
        .expect("sum");
    assert_eq!(sum, 1000, "only today's events contribute to the daily window");

    // Different agent should see zero
    let other_id = uuid::Uuid::new_v4();
    let sum_other = repo
        .sum_agent_window_cost_cents(AgentCostWindow {
            company_id,
            agent_id: other_id,
            window_start: start,
            window_end: end,
        })
        .await
        .expect("sum other");
    assert_eq!(sum_other, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn heartbeat_daily_cap_blocks_when_run_count_equals_limit() {
    use pc_heartbeat::evaluate_daily_cap;
    use pc_heartbeat::HeartbeatPolicy;

    let policy = HeartbeatPolicy {
        enabled: true,
        interval_sec: 60,
        wake_on_demand: true,
        max_concurrent_runs: 1,
        skip_timer_when_no_actionable_work: false,
        max_daily_runs: Some(3),
        max_daily_cost_cents: None,
    };
    // At-limit: block
    assert!(evaluate_daily_cap(&policy, 3, 0).is_some());
    // Just-under: allow
    assert!(evaluate_daily_cap(&policy, 2, 0).is_none());
    // Above-limit: block
    assert!(evaluate_daily_cap(&policy, 4, 0).is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn heartbeat_daily_cap_blocks_when_cost_equals_limit() {
    use pc_heartbeat::evaluate_daily_cap;
    use pc_heartbeat::HeartbeatPolicy;

    let policy = HeartbeatPolicy {
        enabled: true,
        interval_sec: 60,
        wake_on_demand: true,
        max_concurrent_runs: 1,
        skip_timer_when_no_actionable_work: false,
        max_daily_runs: None,
        max_daily_cost_cents: Some(500),
    };
    // At-limit: block
    assert!(evaluate_daily_cap(&policy, 0, 500).is_some());
    // Just-under: allow
    assert!(evaluate_daily_cap(&policy, 0, 499).is_none());
    // Above-limit: block
    assert!(evaluate_daily_cap(&policy, 0, 501).is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn issue_unresolved_blockers_returns_open_blockers_only() {
    let db = fresh_db();
    truncate_all(&db).await;
    let company_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind("Blocker Corp")
        .bind("BLK")
        .execute(db.pool())
        .await
        .expect("insert company");

    let issue_id = uuid::Uuid::new_v4();
    let blocker_open_id = uuid::Uuid::new_v4();
    let blocker_done_id = uuid::Uuid::new_v4();
    let blocker_cancelled_id = uuid::Uuid::new_v4();
    let blocker_hidden_id = uuid::Uuid::new_v4();

    for (id, status, hidden) in [
        (issue_id, "todo", None),
        (blocker_open_id, "todo", None),
        (blocker_done_id, "done", None),
        (blocker_cancelled_id, "cancelled", None),
        (blocker_hidden_id, "todo", Some(chrono::Utc::now())),
    ] {
        sqlx::query(
            "INSERT INTO issues (id, company_id, title, status, hidden_at)              VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(company_id)
        .bind(format!("Issue {id}"))
        .bind(status)
        .bind(hidden)
        .execute(db.pool())
        .await
        .expect("insert issue");
        // Self-row blocks are needed by the inner join on company_id
        sqlx::query(
            "INSERT INTO issue_relations (company_id, issue_id, related_issue_id, type)              VALUES ($1, $2, $3, 'blocks')",
        )
        .bind(company_id)
        .bind(blocker_id_or_issue(blocker_open_id, issue_id, id))
        .bind(issue_id)
        .execute(db.pool())
        .await
        .ok();
    }

    let repo = IssueRepo::new(&db);
    let blockers = repo
        .unresolved_blocker_ids(company_id, issue_id)
        .await
        .expect("query blockers");
    // Only the open + non-cancelled + non-hidden blocker should be returned
    assert!(blockers.contains(&blocker_open_id));
    assert!(!blockers.contains(&blocker_done_id));
    assert!(!blockers.contains(&blocker_cancelled_id));
    assert!(!blockers.contains(&blocker_hidden_id));
}

fn blocker_id_or_issue(open_id: uuid::Uuid, issue_id: uuid::Uuid, current: uuid::Uuid) -> uuid::Uuid {
    if current == issue_id { open_id } else { current }
}

#[tokio::test(flavor = "current_thread")]
async fn issue_unresolved_blockers_for_missing_issue_returns_empty() {
    let db = fresh_db();
    truncate_all(&db).await;
    let repo = IssueRepo::new(&db);
    let blockers = repo
        .unresolved_blockers_for(uuid::Uuid::new_v4())
        .await
        .expect("query");
    assert!(blockers.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn agent_recover_stale_wakeup_claims_resets_only_old_claims() {
    let db = fresh_db();
    truncate_all(&db).await;
    let company_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind("Wakeup Reset Corp")
        .bind("WRP")
        .execute(db.pool())
        .await
        .expect("insert company");
    let agent_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, adapter_config)          VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind("Reset Bot")
    .bind("tester")
    .bind("process")
    .bind(serde_json::json!({}))
    .execute(db.pool())
    .await
    .expect("insert agent");

    let stale_id = uuid::Uuid::new_v4();
    let fresh_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agent_wakeup_requests             (id, company_id, agent_id, source, status, claimed_at, requested_at)          VALUES ($1, $2, $3, 'on_demand', 'claimed', now() - interval '10 minutes', now())",
    )
    .bind(stale_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert stale");
    sqlx::query(
        "INSERT INTO agent_wakeup_requests             (id, company_id, agent_id, source, status, claimed_at, requested_at)          VALUES ($1, $2, $3, 'on_demand', 'claimed', now() - interval '10 seconds', now())",
    )
    .bind(fresh_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert fresh");

    let repo = AgentRepo::new(&db);
    let recovered = repo.recover_stale_wakeup_claims(300).await.expect("recover");
    assert_eq!(recovered, 1);

    let stale = repo
        .get_wakeup_request(company_id, stale_id)
        .await
        .expect("get stale")
        .unwrap();
    let fresh = repo
        .get_wakeup_request(company_id, fresh_id)
        .await
        .expect("get fresh")
        .unwrap();
    assert_eq!(stale.status, "requested");
    assert!(stale.claimed_at.is_none());
    assert_eq!(fresh.status, "claimed");
}

#[tokio::test(flavor = "current_thread")]
async fn agent_find_wakeup_by_idempotency_key_returns_most_recent() {
    let db = fresh_db();
    truncate_all(&db).await;
    let company_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind("Idempotency Corp")
        .bind("IDM")
        .execute(db.pool())
        .await
        .expect("insert company");
    let agent_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, adapter_config)          VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind("Idem Bot")
    .bind("tester")
    .bind("process")
    .bind(serde_json::json!({}))
    .execute(db.pool())
    .await
    .expect("insert agent");

    let fresh_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agent_wakeup_requests             (id, company_id, agent_id, source, status, idempotency_key, requested_at)          VALUES ($1, $2, $3, 'on_demand', 'requested', 'wake-key-1', now())",
    )
    .bind(fresh_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert wakeup");

    let repo = AgentRepo::new(&db);
    let found = repo
        .find_wakeup_by_idempotency_key(company_id, agent_id, "wake-key-1")
        .await
        .expect("find")
        .unwrap();
    assert_eq!(found.id, fresh_id);
    let missing = repo
        .find_wakeup_by_idempotency_key(company_id, agent_id, "missing")
        .await
        .expect("find missing");
    assert!(missing.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn agent_find_active_wakeup_request_returns_pending_only() {
    let db = fresh_db();
    truncate_all(&db).await;
    let company_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind("Active Wakeup Corp")
        .bind("AWK")
        .execute(db.pool())
        .await
        .expect("insert company");
    let agent_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, adapter_config)          VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind("Active Bot")
    .bind("tester")
    .bind("process")
    .bind(serde_json::json!({}))
    .execute(db.pool())
    .await
    .expect("insert agent");

    let active_id = uuid::Uuid::new_v4();
    let completed_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agent_wakeup_requests             (id, company_id, agent_id, source, status, requested_at)          VALUES ($1, $2, $3, 'on_demand', 'requested', now())",
    )
    .bind(active_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert active");
    sqlx::query(
        "INSERT INTO agent_wakeup_requests             (id, company_id, agent_id, source, status, requested_at, finished_at)          VALUES ($1, $2, $3, 'on_demand', 'completed', now() - interval '1 minute', now())",
    )
    .bind(completed_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert completed");

    let repo = AgentRepo::new(&db);
    let active = repo
        .find_active_wakeup_request(company_id, agent_id)
        .await
        .expect("find active")
        .unwrap();
    assert_eq!(active.id, active_id);
}

//! Round 342：`append_recovery_run_event` + `finalize_agent_after_source_resolved_run` 接入 fold path。
//!
//! 对齐 Node：
//! - `services/recovery/service.ts:1568` (`appendRecoveryRunEvent` + `nextRunEventSeq`)
//! - `services/recovery/service.ts:1648` (`finalizeAgentAfterSourceResolvedRun`)
//!
//! 测试场景：
//! 1. append_recovery_run_event: 写入 heartbeat_run_events，seq 单调递增
//! 2. finalize_agent: agent status='running' → fold → 'idle'
//! 3. finalize_agent: agent 有其他 running run → fold → 'running' 保持
//! 4. finalize_agent: agent status='paused'/'terminated' → 不被覆盖
//! 5. fold path 集成: 完整 fold 路径会触发 append_event + finalize_agent

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::append_recovery_run_event::{
    append_recovery_run_event, AppendRecoveryRunEventInput,
};
use pc_heartbeat::recovery::create_or_update_stale_run_evaluation_full::{
    create_or_update_stale_run_evaluation_full, CreateOrUpdateStaleRunEvaluationInput,
};
use pc_heartbeat::recovery::finalize_agent_after_source_resolved_run::finalize_agent_after_source_resolved_run;
use pc_heartbeat::recovery::scan_silent_active_runs_db::{
    SilentRunCandidate, StaleRunEvaluationOutcome,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM heartbeat_run_events WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_comments WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_run_watchdog_decisions WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

async fn fixture(db: &Db) -> (Uuid, String) {
    let company_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r342-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, prefix)
}

async fn insert_agent(db: &Db, company_id: Uuid, name: &str, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, $3, 'engineer', 'process', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .bind(status)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_run(db: &Db, company_id: Uuid, agent_id: Uuid, last_output_min_ago: i64) -> Uuid {
    let id = Uuid::new_v4();
    let last_output_at = Utc::now() - Duration::minutes(last_output_min_ago);
    let started_at = Utc::now() - Duration::minutes(last_output_min_ago + 5);
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, \
                                    started_at, process_started_at, last_output_at) \
         VALUES ($1, $2, $3, 'manual', 'running', $4, $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(started_at)
    .bind(last_output_at)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn fetch_event_count(db: &Db, run_id: Uuid, event_type: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM heartbeat_run_events \
         WHERE run_id = $1 AND event_type = $2",
    )
    .bind(run_id)
    .bind(event_type)
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

async fn fetch_max_seq(db: &Db, run_id: Uuid) -> i32 {
    let (n,): (i32,) = sqlx::query_as(
        "SELECT COALESCE(MAX(seq),0)::int FROM heartbeat_run_events WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

async fn fetch_agent_status(db: &Db, agent_id: Uuid) -> String {
    let (s,): (String,) = sqlx::query_as("SELECT status::text FROM agents WHERE id = $1")
        .bind(agent_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    s
}

// ========================================================================
// append_recovery_run_event tests
// ========================================================================

#[tokio::test]
async fn append_recovery_run_event_writes_event() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1", "running").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;

    append_recovery_run_event(
        &db,
        AppendRecoveryRunEventInput {
            company_id,
            run_id,
            agent_id,
            level: "info",
            message: "Source-resolved watchdog fold finalized stale active run".to_owned(),
            payload: Some(json!({"test": "data"})),
        },
    )
    .await
    .unwrap();

    // 1 个 event 写入
    assert_eq!(fetch_event_count(&db, run_id, "lifecycle").await, 1);

    // seq 从 1 开始
    assert_eq!(fetch_max_seq(&db, run_id).await, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test]
async fn append_recovery_run_event_seq_monotonic() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1", "running").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;

    // 连续写 3 个 event
    for i in 1..=3 {
        append_recovery_run_event(
            &db,
            AppendRecoveryRunEventInput {
                company_id,
                run_id,
                agent_id,
                level: "info",
                message: format!("event {i}"),
                payload: None,
            },
        )
        .await
        .unwrap();
    }

    // 3 个 event，seq 1,2,3
    assert_eq!(fetch_event_count(&db, run_id, "lifecycle").await, 3);
    assert_eq!(fetch_max_seq(&db, run_id).await, 3);

    cleanup(&db, company_id).await;
}

// ========================================================================
// finalize_agent_after_source_resolved_run tests
// ========================================================================

#[tokio::test]
async fn finalize_agent_running_to_idle() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1", "running").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;

    finalize_agent_after_source_resolved_run(&db, run_id, company_id, agent_id, "succeeded")
        .await
        .unwrap();

    // agent.status → idle（因为没其他 running run）
    assert_eq!(fetch_agent_status(&db, agent_id).await, "idle");

    cleanup(&db, company_id).await;
}

#[tokio::test]
async fn finalize_agent_keeps_running_when_other_runs_exist() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1", "running").await;
    // 2 个 runs
    let _run_id_1 = insert_run(&db, company_id, agent_id, 250).await;
    let run_id_2 = insert_run(&db, company_id, agent_id, 200).await;

    finalize_agent_after_source_resolved_run(&db, run_id_2, company_id, agent_id, "succeeded")
        .await
        .unwrap();

    // 还有 run_id_1 在 running → agent.status 保持 'running'
    assert_eq!(fetch_agent_status(&db, agent_id).await, "running");

    cleanup(&db, company_id).await;
}

#[tokio::test]
async fn finalize_agent_skips_paused_or_terminated() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id_paused = insert_agent(&db, company_id, "agent-paused", "paused").await;
    let agent_id_terminated = insert_agent(&db, company_id, "agent-terminated", "terminated").await;
    let _run1 = insert_run(&db, company_id, agent_id_paused, 250).await;
    let _run2 = insert_run(&db, company_id, agent_id_terminated, 250).await;

    finalize_agent_after_source_resolved_run(&db, _run1, company_id, agent_id_paused, "succeeded")
        .await
        .unwrap();
    finalize_agent_after_source_resolved_run(
        &db,
        _run2,
        company_id,
        agent_id_terminated,
        "succeeded",
    )
    .await
    .unwrap();

    // paused/terminated 不被覆盖
    assert_eq!(fetch_agent_status(&db, agent_id_paused).await, "paused");
    assert_eq!(
        fetch_agent_status(&db, agent_id_terminated).await,
        "terminated"
    );

    cleanup(&db, company_id).await;
}

#[tokio::test]
async fn finalize_agent_cancelled_status() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1", "running").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;

    finalize_agent_after_source_resolved_run(&db, run_id, company_id, agent_id, "cancelled")
        .await
        .unwrap();

    // cancelled → idle
    assert_eq!(fetch_agent_status(&db, agent_id).await, "idle");

    cleanup(&db, company_id).await;
}

// ========================================================================
// Fold path integration test
// ========================================================================

#[tokio::test]
async fn fold_path_writes_event_and_finalizes_agent() {
    use pc_heartbeat::recovery::build_stale_run_evaluation_description::StaleAgentView;
    use pc_heartbeat::recovery::build_stale_run_evaluation_description::StaleSourceIssueView;
    use pc_heartbeat::recovery::ensure_source_issue_commented_for_stale_evaluation::SourceIssueView;

    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1", "running").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind, execution_run_id) \
         VALUES (gen_random_uuid(), $1, $2, 'r342-src', 'done', 'todo', $3) RETURNING id",
    )
    .bind(company_id)
    .bind(format!("{prefix}-1"))
    .bind(run_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    // 同 run evidence
    sqlx::query(
        "INSERT INTO activity_log (id, company_id, actor_type, actor_id, action, entity_type, \
                                    entity_id, agent_id, run_id, details, created_at) \
         VALUES (gen_random_uuid(), $1, 'system', 'system', 'issue.updated', 'issue', $2, $3, $4, $5, now())",
    )
    .bind(company_id)
    .bind(source_id.to_string())
    .bind(agent_id)
    .bind(run_id)
    .bind(json!({"status": "done"}))
    .execute(db.pool())
    .await
    .unwrap();

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: SilentRunCandidate {
            id: run_id,
            company_id,
            agent_id,
            status: "running".to_owned(),
            last_output_at: Some(Utc::now() - Duration::minutes(250)),
            started_at: Some(Utc::now() - Duration::hours(5)),
            process_started_at: Some(Utc::now() - Duration::hours(5)),
            created_at: Utc::now() - Duration::hours(5),
            context_snapshot: None,
        },
        running_agent: StaleAgentView {
            id: agent_id,
            name: "engineer-1".to_owned(),
            adapter_type: "process".to_owned(),
        },
        source_issue: Some(StaleSourceIssueView {
            id: source_id,
            identifier: Some("ROOT-1".to_owned()),
        }),
        source_issue_row: Some(SourceIssueView {
            id: source_id,
            company_id,
            status: "done".to_owned(),
        }),
        source_issue_origin_kind: Some("todo".to_owned()),
        evaluation_owner_agent_id: None,
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(outcome, StaleRunEvaluationOutcome::Folded);

    // fold path 触发 lifecycle event
    assert!(fetch_event_count(&db, run_id, "lifecycle").await >= 1);

    // agent 已被 finalize 为 idle
    assert_eq!(fetch_agent_status(&db, agent_id).await, "idle");

    cleanup(&db, company_id).await;
}

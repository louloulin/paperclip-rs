//! Round 339：`is_terminal_issue_status` + `latestSameRunSourceTerminalEvidence` +
//! `fold_source_resolved_stale_run` 接入主循环。
//!
//! 对齐 Node `services/recovery/service.ts:2077`：
//! - source_issue status == 'done'/'cancelled' (terminal)
//! - AND 有 same-run evidence (activity_log action='issue.updated' entity_type='issue' entity_id=source_issue_id,
//!   details->>'status' = terminal status, run_id = run.id, created_at >= silence_started_at)
//! - THEN fold (finalize run as succeeded/cancelled, mark existing evaluation done, insert watchdog_decision)
//
// 测试场景：
// 1. is_terminal_issue_status 单元测试（pure）
// 2. source_issue=done + 同run evidence → fold → run status='succeeded'
// 3. source_issue=cancelled + 同run evidence → fold → run status='cancelled'
// 4. source_issue=done + 无 evidence → 不 fold
// 5. source_issue=in_progress → 不 fold
// 6. fold 关闭现有 evaluation issue + 写 comment

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::build_stale_run_evaluation_description::{
    StaleAgentView, StaleSourceIssueView,
};
use pc_heartbeat::recovery::create_or_update_stale_run_evaluation_full::{
    create_or_update_stale_run_evaluation_full, CreateOrUpdateStaleRunEvaluationInput,
};
use pc_heartbeat::recovery::ensure_source_issue_commented_for_stale_evaluation::SourceIssueView;
use pc_heartbeat::recovery::is_terminal_issue_status::is_terminal_issue_status_str;
use pc_heartbeat::recovery::latest_same_run_source_terminal_evidence::{
    latest_same_run_source_terminal_evidence, LatestSameRunSourceTerminalEvidence,
};
use pc_heartbeat::recovery::scan_silent_active_runs_db::StaleRunEvaluationOutcome;
use pc_heartbeat::recovery::watchdog_decision_recording::STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_recovery_actions WHERE company_id = $1")
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
        .bind(format!("r339-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, prefix)
}

async fn insert_agent(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, $3, 'engineer', 'process', 'active')",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_run_with_silence(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    last_output_min_ago: i64,
) -> Uuid {
    let id = Uuid::new_v4();
    let now = Utc::now();
    let started_at = now - Duration::hours(5);
    let last_output_at = now - Duration::minutes(last_output_min_ago);
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

async fn insert_source_issue(
    db: &Db,
    company_id: Uuid,
    prefix: &str,
    status: &str,
    execution_run_id: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind, \
                              execution_run_id, execution_agent_name_key) \
         VALUES ($1, $2, $3, 'r339-src', $4, 'todo', $5, $6)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("{prefix}-1"))
    .bind(status)
    .bind(execution_run_id)
    .bind(execution_run_id.map(|_| "r339-agent"))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_active_watchdog_action(db: &Db, company_id: Uuid, source_issue_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_recovery_actions \
            (id, company_id, source_issue_id, kind, status, owner_type, cause, fingerprint, \
             evidence, next_action) \
         VALUES ($1, $2, $3, 'active_run_watchdog', 'active', 'agent', 'stale_output', \
                 $4, '{}'::jsonb, 'source resolved')",
    )
    .bind(id)
    .bind(company_id)
    .bind(source_issue_id)
    .bind(format!("r339-watchdog-{id}"))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_terminal_evidence(
    db: &Db,
    company_id: Uuid,
    run_id: Uuid,
    source_issue_id: Uuid,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO activity_log (id, company_id, actor_type, actor_id, action, entity_type, \
                                    entity_id, agent_id, run_id, details, created_at) \
         VALUES ($1, $2, 'system', 'system', 'issue.updated', 'issue', $3, \
                 (SELECT agent_id FROM heartbeat_runs WHERE id = $4), $4, $5, now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(source_issue_id.to_string())
    .bind(run_id)
    .bind(json!({"status": status}))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn fetch_run_status(db: &Db, run_id: Uuid) -> String {
    let (status,): (String,) =
        sqlx::query_as("SELECT status::text FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    status
}

async fn fetch_dismissed_count(db: &Db, company_id: Uuid, run_id: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM heartbeat_run_watchdog_decisions \
         WHERE company_id = $1 AND run_id = $2 AND decision = 'dismissed_false_positive'",
    )
    .bind(company_id)
    .bind(run_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

async fn fetch_fold_activity_count(db: &Db, company_id: Uuid, run_id: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM activity_log \
         WHERE company_id = $1 AND run_id = $2 AND action = 'heartbeat.output_stale_source_resolved'",
    )
    .bind(company_id)
    .bind(run_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

async fn insert_existing_evaluation(db: &Db, company_id: Uuid, run_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_id, origin_run_id, origin_fingerprint) \
         VALUES ($1, $2, 'existing-eval', 'todo', 'medium', $3, $4, $4, $5) RETURNING id",
    )
    .bind(id)
    .bind(company_id)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run_id.to_string())
    .bind(format!("existing-{run_id}"))
    .fetch_one(db.pool())
    .await
    .unwrap();
    id
}

// ========================================================================
// is_terminal_issue_status unit tests
// ========================================================================

#[test]
fn is_terminal_issue_status_str_matches_node_behavior() {
    assert!(is_terminal_issue_status_str("done"));
    assert!(is_terminal_issue_status_str("cancelled"));
    assert!(!is_terminal_issue_status_str("todo"));
    assert!(!is_terminal_issue_status_str("in_progress"));
    assert!(!is_terminal_issue_status_str("blocked"));
    assert!(!is_terminal_issue_status_str("in_review"));
    assert!(!is_terminal_issue_status_str(""));
}

// ========================================================================
// latest_same_run_source_terminal_evidence
// ========================================================================

#[tokio::test]
async fn latest_terminal_evidence_returns_recent_activity() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run_with_silence(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "done", Some(run_id)).await;
    let evidence_id = insert_terminal_evidence(&db, company_id, run_id, source_id, "done").await;

    let run_started_at: chrono::DateTime<Utc> = sqlx::query_as::<_, (chrono::DateTime<Utc>,)>(
        "SELECT started_at FROM heartbeat_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_one(db.pool())
    .await
    .unwrap()
    .0;

    let evidence = latest_same_run_source_terminal_evidence(
        &db,
        run_id,
        company_id,
        source_id,
        "done",
        Some(run_started_at),
    )
    .await
    .unwrap();

    assert!(matches!(
        evidence,
        Some(LatestSameRunSourceTerminalEvidence { .. })
    ));
    let e = evidence.unwrap();
    assert_eq!(e.id, evidence_id);
    assert_eq!(e.kind, "activity");

    cleanup(&db, company_id).await;
}

#[tokio::test]
async fn latest_terminal_evidence_returns_none_when_no_activity() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run_with_silence(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "done", Some(run_id)).await;

    let evidence =
        latest_same_run_source_terminal_evidence(&db, run_id, company_id, source_id, "done", None)
            .await
            .unwrap();

    assert!(evidence.is_none());

    cleanup(&db, company_id).await;
}

// ========================================================================
// 主循环 fold path: source_issue=done + 同run evidence → fold
// ========================================================================

#[tokio::test]
async fn source_issue_done_with_evidence_folds_run() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    sqlx::query("UPDATE agents SET adapter_type = 'codex_local' WHERE id = $1")
        .bind(agent_id)
        .execute(db.pool())
        .await
        .unwrap();
    let run_id = insert_run_with_silence(&db, company_id, agent_id, 250).await;
    let mut child = tokio::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    sqlx::query("UPDATE heartbeat_runs SET process_pid = $1 WHERE id = $2")
        .bind(child.id().unwrap() as i32)
        .bind(run_id)
        .execute(db.pool())
        .await
        .unwrap();
    let source_id = insert_source_issue(&db, company_id, &prefix, "done", Some(run_id)).await;
    let action_id = insert_active_watchdog_action(&db, company_id, source_id).await;
    insert_terminal_evidence(&db, company_id, run_id, source_id, "done").await;

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: silent_run_candidate(&db, company_id, agent_id, run_id).await,
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

    // run 已 finalized
    assert_eq!(fetch_run_status(&db, run_id).await, "succeeded");
    assert!(child.try_wait().unwrap().is_some());
    let result_json: serde_json::Value =
        sqlx::query_scalar("SELECT result_json FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(matches!(
        result_json["sourceResolvedWatchdogFold"]["cleanup"]["outcome"].as_str(),
        Some("terminated" | "termination_sent_still_running")
    ));

    // dismissed_false_positive decision 写入
    assert_eq!(fetch_dismissed_count(&db, company_id, run_id).await, 1);

    // activity log: heartbeat.output_stale_source_resolved 写入
    assert_eq!(fetch_fold_activity_count(&db, company_id, run_id).await, 1);

    let (status, outcome): (String, String) =
        sqlx::query_as("SELECT status, outcome FROM issue_recovery_actions WHERE id = $1")
            .bind(action_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "resolved");
    assert_eq!(outcome, "false_positive");

    // 不创建 evaluation issue
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = $2 AND origin_id = $3",
    )
    .bind(company_id)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(n, 0);

    cleanup(&db, company_id).await;
}

/// source_issue=cancelled + evidence → run finalized as 'cancelled'
#[tokio::test]
async fn source_issue_cancelled_with_evidence_folds_run() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run_with_silence(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "cancelled", Some(run_id)).await;
    insert_terminal_evidence(&db, company_id, run_id, source_id, "cancelled").await;

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: silent_run_candidate(&db, company_id, agent_id, run_id).await,
        running_agent: StaleAgentView {
            id: agent_id,
            name: "engineer-1".to_owned(),
            adapter_type: "process".to_owned(),
        },
        source_issue: Some(StaleSourceIssueView {
            id: source_id,
            identifier: Some("ROOT-2".to_owned()),
        }),
        source_issue_row: Some(SourceIssueView {
            id: source_id,
            company_id,
            status: "cancelled".to_owned(),
        }),
        source_issue_origin_kind: Some("todo".to_owned()),
        evaluation_owner_agent_id: None,
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(outcome, StaleRunEvaluationOutcome::Folded);

    assert_eq!(fetch_run_status(&db, run_id).await, "cancelled");
    assert_eq!(fetch_dismissed_count(&db, company_id, run_id).await, 1);

    cleanup(&db, company_id).await;
}

/// source_issue=in_progress + 同run evidence (status=in_progress) → 不 fold (status 不是 terminal)
#[tokio::test]
async fn source_issue_in_progress_does_not_fold() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run_with_silence(&db, company_id, agent_id, 250).await;
    let source_id =
        insert_source_issue(&db, company_id, &prefix, "in_progress", Some(run_id)).await;
    insert_terminal_evidence(&db, company_id, run_id, source_id, "in_progress").await;

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: silent_run_candidate(&db, company_id, agent_id, run_id).await,
        running_agent: StaleAgentView {
            id: agent_id,
            name: "engineer-1".to_owned(),
            adapter_type: "process".to_owned(),
        },
        source_issue: Some(StaleSourceIssueView {
            id: source_id,
            identifier: Some("ROOT-3".to_owned()),
        }),
        source_issue_row: Some(SourceIssueView {
            id: source_id,
            company_id,
            status: "in_progress".to_owned(),
        }),
        source_issue_origin_kind: Some("todo".to_owned()),
        evaluation_owner_agent_id: None,
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    // in_progress + silence >= 4h → critical → Created
    assert!(matches!(outcome, StaleRunEvaluationOutcome::Created(_)));

    cleanup(&db, company_id).await;
}

/// source_issue=done 但无同run evidence → 不 fold (latestSameRunSourceTerminalEvidence returns null)
#[tokio::test]
async fn source_issue_done_without_evidence_does_not_fold() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run_with_silence(&db, company_id, agent_id, 250).await;
    // source_issue=done but NO terminal evidence activity_log row
    let source_id = insert_source_issue(&db, company_id, &prefix, "done", Some(run_id)).await;

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: silent_run_candidate(&db, company_id, agent_id, run_id).await,
        running_agent: StaleAgentView {
            id: agent_id,
            name: "engineer-1".to_owned(),
            adapter_type: "process".to_owned(),
        },
        source_issue: Some(StaleSourceIssueView {
            id: source_id,
            identifier: Some("ROOT-4".to_owned()),
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
    // No evidence → don't fold → critical → Created
    assert!(matches!(outcome, StaleRunEvaluationOutcome::Created(_)));

    cleanup(&db, company_id).await;
}

/// fold 关闭 existing evaluation + 写 comment
#[tokio::test]
async fn fold_closes_existing_evaluation_with_comment() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run_with_silence(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "done", Some(run_id)).await;
    insert_terminal_evidence(&db, company_id, run_id, source_id, "done").await;
    let eval_id = insert_existing_evaluation(&db, company_id, run_id).await;

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: silent_run_candidate(&db, company_id, agent_id, run_id).await,
        running_agent: StaleAgentView {
            id: agent_id,
            name: "engineer-1".to_owned(),
            adapter_type: "process".to_owned(),
        },
        source_issue: Some(StaleSourceIssueView {
            id: source_id,
            identifier: Some("ROOT-5".to_owned()),
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

    // evaluation 已标记 done
    let (status,): (String,) = sqlx::query_as("SELECT status::text FROM issues WHERE id = $1")
        .bind(eval_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(status, "done");

    // evaluation 写 comment
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_comments \
         WHERE issue_id = $1 AND deleted_at IS NULL",
    )
    .bind(eval_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(n >= 1);

    cleanup(&db, company_id).await;
}

// Helper for building SilentRunCandidate from DB row
async fn silent_run_candidate(
    _db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    run_id: Uuid,
) -> pc_heartbeat::recovery::scan_silent_active_runs_db::SilentRunCandidate {
    pc_heartbeat::recovery::scan_silent_active_runs_db::SilentRunCandidate {
        id: run_id,
        company_id,
        agent_id,
        status: "running".to_owned(),
        last_output_at: Some(Utc::now() - Duration::minutes(250)),
        started_at: Some(Utc::now() - Duration::hours(5)),
        process_started_at: Some(Utc::now() - Duration::hours(5)),
        created_at: Utc::now() - Duration::hours(5),
        context_snapshot: None,
    }
}

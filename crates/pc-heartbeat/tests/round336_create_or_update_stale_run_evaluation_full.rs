//! Round 336：`create_or_update_stale_run_evaluation_full` 高阶编排的 PostgreSQL round-trip 验证。
//!
//! 与 Node `services/recovery/service.ts:2052` 对齐：
//! - 输入：SilentRunCandidate + running_agent + source_issue (view + row) + owner_agent_id + now
//! - 输出：StaleRunEvaluationOutcome
//!
//! 关键 invariants：
//! - dismissed_false_positive → Skipped
//! - 已有 evaluation + critical → Escalated（升级 priority + source comment）
//! - 已有 evaluation + not critical → Existing
//! - 无 existing → Created（description 用 build_stale_run_evaluation_description + activity log）
//! - Created + critical + owner → 唤醒 reviewer (agent_wakeup_requests)

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::build_stale_run_evaluation_description::{
    StaleAgentView, StaleSourceIssueView,
};
use pc_heartbeat::recovery::create_or_update_stale_run_evaluation_full::{
    create_or_update_stale_run_evaluation_full, CreateOrUpdateStaleRunEvaluationInput,
};
use pc_heartbeat::recovery::ensure_source_issue_commented_for_stale_evaluation::SourceIssueView;
use pc_heartbeat::recovery::scan_silent_active_runs_db::{
    SilentRunCandidate, StaleRunEvaluationOutcome,
};
use pc_heartbeat::recovery::watchdog_decision_recording::STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND;
use pc_repos::Db;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id = $1")
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

async fn fixture(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r336-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status)          VALUES ($1, $2, $3, 'engineer', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("r336-agent-{agent_id}"))
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_run(db: &Db, company_id: Uuid, agent_id: Uuid, last_output_min_ago: i64) -> Uuid {
    let id = Uuid::new_v4();
    let last_output_at = Utc::now() - Duration::minutes(last_output_min_ago);
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status,                                     started_at, process_started_at, last_output_at)          VALUES ($1, $2, $3, 'manual', 'running', now(), now(), $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(last_output_at)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_source_issue(db: &Db, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind)          VALUES ($1, $2, $3, 'r336-src', $4, 'todo')",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("{prefix}-1"))
    .bind(status)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_dismissed_false_positive(db: &Db, company_id: Uuid, run_id: Uuid) {
    sqlx::query(
        "INSERT INTO heartbeat_run_watchdog_decisions          (company_id, run_id, decision, reason) VALUES ($1, $2, 'dismissed_false_positive', 'r336')",
    )
    .bind(company_id)
    .bind(run_id)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn fetch_issue(db: &Db, id: Uuid) -> (String, String, Option<Uuid>) {
    let row: (String, String, Option<Uuid>) = sqlx::query_as(
        "SELECT status::text, priority::text, assignee_agent_id FROM issues WHERE id=$1",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    row
}

async fn fetch_comment_count(db: &Db, issue_id: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_comments WHERE issue_id=$1 AND deleted_at IS NULL",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

async fn fetch_wake_count(db: &Db, company_id: Uuid, evaluation_id: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM agent_wakeup_requests WHERE company_id=$1 AND payload->>'issueId' = $2",
    )
    .bind(company_id)
    .bind(evaluation_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

/// 完整路径：critical level + 有 owner + 有 source issue → Created + activity + source comment + wake
#[tokio::test]
async fn critical_with_owner_and_source_creates_full() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    // silence >= 4h → critical
    let run_id = insert_run(&db, company_id, agent_id, 4 * 60 + 10).await;
    let source_id = insert_source_issue(&db, company_id, "in_progress").await;

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
            status: "in_progress".to_owned(),
        }),
        source_issue_origin_kind: None,
        evaluation_owner_agent_id: Some(agent_id),
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    let id = match outcome {
        StaleRunEvaluationOutcome::Created(id) => id,
        _ => panic!("expected Created, got {outcome:?}"),
    };

    // 1. priority = high
    let (_status, priority, assignee) = fetch_issue(&db, id).await;
    assert_eq!(priority, "high");
    assert_eq!(assignee, Some(agent_id));

    // 2. description 是 markdown text（包含 Run / Last Output Excerpt）
    let (description,): (Option<String>,) =
        sqlx::query_as("SELECT description FROM issues WHERE id=$1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let desc = description.unwrap_or_default();
    assert!(
        desc.starts_with("Paperclip detected critical output silence on an active heartbeat run.")
    );
    assert!(desc.contains("## Run"));
    assert!(desc.contains("## Decision Checklist"));
    assert!(desc.contains(&format!("- Run: [{run_id}]")));

    // 3. source issue 被写 comment
    assert!(fetch_comment_count(&db, source_id).await >= 1);

    // 4. reviewer wake 被 enqueue
    assert_eq!(fetch_wake_count(&db, company_id, id).await, 1);

    cleanup(&db, company_id).await;
}

/// dismissed_false_positive → Skipped
#[tokio::test]
async fn dismissed_false_positive_returns_skipped() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_run(&db, company_id, agent_id, 5).await;
    insert_dismissed_false_positive(&db, company_id, run_id).await;

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: SilentRunCandidate {
            id: run_id,
            company_id,
            agent_id,
            status: "running".to_owned(),
            last_output_at: Some(Utc::now() - Duration::minutes(5)),
            started_at: Some(Utc::now()),
            process_started_at: Some(Utc::now()),
            created_at: Utc::now(),
            context_snapshot: None,
        },
        running_agent: StaleAgentView {
            id: agent_id,
            name: "engineer-1".to_owned(),
            adapter_type: "process".to_owned(),
        },
        source_issue: None,
        source_issue_row: None,
        source_issue_origin_kind: None,
        evaluation_owner_agent_id: None,
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(outcome, StaleRunEvaluationOutcome::Skipped);

    cleanup(&db, company_id).await;
}

/// 已有 evaluation + critical → Escalated（升级 priority + source comment）
#[tokio::test]
async fn existing_with_critical_escalates() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_run(&db, company_id, agent_id, 5).await;
    let source_id = insert_source_issue(&db, company_id, "in_progress").await;

    // 先创建一个 low-priority existing evaluation
    let existing_id: (Uuid,) = sqlx::query_as(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_id, origin_run_id, origin_fingerprint)          VALUES (gen_random_uuid(), $1, 'existing', 'todo', 'medium', $2, $3, $3, $4)          RETURNING id",
    )
    .bind(company_id)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run_id.to_string())
    .bind(format!("stale_active_run:{company_id}:{run_id}"))
    .fetch_one(db.pool())
    .await
    .unwrap();
    let existing_id = existing_id.0;

    // 触发 critical level（4h+ silence）
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
            status: "in_progress".to_owned(),
        }),
        source_issue_origin_kind: None,
        evaluation_owner_agent_id: Some(agent_id),
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(outcome, StaleRunEvaluationOutcome::Escalated(existing_id));

    // priority 升级为 high
    let (_s, priority, _a) = fetch_issue(&db, existing_id).await;
    assert_eq!(priority, "high");

    // evaluation issue 上写了 "Critical threshold crossed" comment
    assert!(fetch_comment_count(&db, existing_id).await >= 1);

    // source issue 被写 escalation comment
    assert!(fetch_comment_count(&db, source_id).await >= 1);

    cleanup(&db, company_id).await;
}

/// 已有 evaluation + suspicious → Existing (no change)
#[tokio::test]
async fn existing_with_suspicious_returns_existing() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_run(&db, company_id, agent_id, 5).await;

    let existing_id: (Uuid,) = sqlx::query_as(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_id, origin_run_id, origin_fingerprint)          VALUES (gen_random_uuid(), $1, 'existing', 'todo', 'medium', $2, $3, $3, $4)          RETURNING id",
    )
    .bind(company_id)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run_id.to_string())
    .bind(format!("stale_active_run:{company_id}:{run_id}"))
    .fetch_one(db.pool())
    .await
    .unwrap();

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: SilentRunCandidate {
            id: run_id,
            company_id,
            agent_id,
            status: "running".to_owned(),
            last_output_at: Some(Utc::now() - Duration::minutes(5)),
            started_at: Some(Utc::now()),
            process_started_at: Some(Utc::now()),
            created_at: Utc::now(),
            context_snapshot: None,
        },
        running_agent: StaleAgentView {
            id: agent_id,
            name: "engineer-1".to_owned(),
            adapter_type: "process".to_owned(),
        },
        source_issue: None,
        source_issue_row: None,
        source_issue_origin_kind: None,
        evaluation_owner_agent_id: None,
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(outcome, StaleRunEvaluationOutcome::Existing(existing_id.0));

    cleanup(&db, company_id).await;
}

/// suspicious + 无 owner + 无 source → Created (suspicious level, no wake, no source comment)
#[tokio::test]
async fn suspicious_no_owner_no_source_creates_minimal() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_run(&db, company_id, agent_id, 5).await;

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: SilentRunCandidate {
            id: run_id,
            company_id,
            agent_id,
            status: "running".to_owned(),
            last_output_at: Some(Utc::now() - Duration::minutes(5)),
            started_at: Some(Utc::now()),
            process_started_at: Some(Utc::now()),
            created_at: Utc::now(),
            context_snapshot: None,
        },
        running_agent: StaleAgentView {
            id: agent_id,
            name: "engineer-1".to_owned(),
            adapter_type: "process".to_owned(),
        },
        source_issue: None,
        source_issue_row: None,
        source_issue_origin_kind: None,
        evaluation_owner_agent_id: None,
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    let id = match outcome {
        StaleRunEvaluationOutcome::Created(id) => id,
        _ => panic!("expected Created"),
    };

    // priority = medium
    let (_s, priority, assignee) = fetch_issue(&db, id).await;
    assert_eq!(priority, "medium");
    assert_eq!(assignee, None);

    // 无 reviewer wake
    assert_eq!(fetch_wake_count(&db, company_id, id).await, 0);

    cleanup(&db, company_id).await;
}

/// source_issue status = done → 跳过 source comment 写入（ensure_source_issue_commented 内部短路）
#[tokio::test]
async fn source_issue_done_skips_source_comment() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_run(&db, company_id, agent_id, 4 * 60 + 10).await;
    let source_id = insert_source_issue(&db, company_id, "done").await;

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
        source_issue_origin_kind: None,
        evaluation_owner_agent_id: Some(agent_id),
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert!(matches!(outcome, StaleRunEvaluationOutcome::Created(_)));

    // source done → 不写 comment
    assert_eq!(fetch_comment_count(&db, source_id).await, 0);

    cleanup(&db, company_id).await;
}

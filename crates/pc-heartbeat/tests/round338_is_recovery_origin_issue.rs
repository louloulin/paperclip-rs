//! Round 338：`is_recovery_origin_issue` 递归短路 + `output_stale_recovery_recursion_refused` activity log。
//!
//! 对齐 Node `services/recovery/service.ts:2073` (Node 在 createOrUpdateStaleRunEvaluation 入口检查)：
//! - 若 source_issue 是 recovery issue (origin_kind ∈ RECOVERY_ORIGIN_KINDS)：
//!   1. 写 `heartbeat.output_stale_recovery_recursion_refused` activity_log
//!   2. 返回 Skipped
//!
//! RECOVERY_ORIGIN_KINDS（Node `recovery/origins.ts:1`）：
//! - `harness_liveness_escalation`
//! - `issue_productivity_review`
//! - `stranded_issue_recovery`
//! - `stale_active_run_evaluation`
//!
//! 测试场景：
//! 1. source_issue 是 stranded_issue_recovery → Skipped + activity log
//! 2. source_issue 是 stale_active_run_evaluation → Skipped + activity log
//! 3. source_issue 是 harness_liveness_escalation → Skipped + activity log
//! 4. source_issue 是 issue_productivity_review → Skipped + activity log
//! 5. source_issue 是普通 todo → 正常流程（Created）
//! 6. pure `is_recovery_origin_issue_str` 单元测试

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::build_stale_run_evaluation_description::{
    StaleAgentView, StaleSourceIssueView,
};
use pc_heartbeat::recovery::create_or_update_stale_run_evaluation_full::{
    create_or_update_stale_run_evaluation_full, CreateOrUpdateStaleRunEvaluationInput,
};
use pc_heartbeat::recovery::ensure_source_issue_commented_for_stale_evaluation::SourceIssueView;
use pc_heartbeat::recovery::is_recovery_origin_issue::{
    is_recovery_origin_issue_str, RECOVERY_ORIGIN_KINDS,
};
use pc_heartbeat::recovery::scan_silent_active_runs_db::StaleRunEvaluationOutcome;
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

async fn fixture(db: &Db) -> (Uuid, String) {
    let company_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r338-{company_id}"))
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

async fn insert_run(db: &Db, company_id: Uuid, agent_id: Uuid, silence_min: i64) -> Uuid {
    let id = Uuid::new_v4();
    let last_output_at = Utc::now() - Duration::minutes(silence_min);
    let started_at = Utc::now() - Duration::minutes(silence_min + 5);
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

async fn insert_source_issue_with_origin(
    db: &Db,
    company_id: Uuid,
    prefix: &str,
    status: &str,
    origin_kind: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind) \
         VALUES ($1, $2, $3, 'r338-src', $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("{prefix}-1"))
    .bind(status)
    .bind(origin_kind)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn fetch_recursion_refused_count(db: &Db, company_id: Uuid, run_id: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM activity_log \
         WHERE company_id = $1 AND run_id = $2 \
           AND action = 'heartbeat.output_stale_recovery_recursion_refused'",
    )
    .bind(company_id)
    .bind(run_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

async fn fetch_evaluation_count(db: &Db, company_id: Uuid, run_id: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = 'stale_active_run_evaluation' \
           AND origin_id = $2",
    )
    .bind(company_id)
    .bind(run_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

/// pure function tests
#[test]
fn is_recovery_origin_issue_str_matches_all_kinds() {
    for kind in RECOVERY_ORIGIN_KINDS {
        assert!(
            is_recovery_origin_issue_str(kind),
            "expected true for {kind}"
        );
    }
}

#[test]
fn is_recovery_origin_issue_str_rejects_non_recovery() {
    assert!(!is_recovery_origin_issue_str("todo"));
    assert!(!is_recovery_origin_issue_str("user_created"));
    assert!(!is_recovery_origin_issue_str(""));
    assert!(!is_recovery_origin_issue_str("stranded")); // partial match should fail
}

#[test]
fn recovery_origin_kinds_contains_expected_values() {
    assert!(RECOVERY_ORIGIN_KINDS.contains(&"harness_liveness_escalation"));
    assert!(RECOVERY_ORIGIN_KINDS.contains(&"issue_productivity_review"));
    assert!(RECOVERY_ORIGIN_KINDS.contains(&"stranded_issue_recovery"));
    assert!(RECOVERY_ORIGIN_KINDS.contains(&"stale_active_run_evaluation"));
    assert_eq!(RECOVERY_ORIGIN_KINDS.len(), 4);
}

/// source_issue origin_kind = stranded_issue_recovery → Skipped + activity log
#[tokio::test]
async fn stranded_recovery_source_refuses_recursion() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue_with_origin(
        &db,
        company_id,
        &prefix,
        "in_progress",
        "stranded_issue_recovery",
    )
    .await;

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: silent_run_candidate_for_input::build(&db, company_id, agent_id, run_id).await,
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
        evaluation_owner_agent_id: None,
        source_issue_origin_kind: Some("stranded_issue_recovery".to_owned()),
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(outcome, StaleRunEvaluationOutcome::Skipped);

    // activity log 写入
    assert_eq!(
        fetch_recursion_refused_count(&db, company_id, run_id).await,
        1
    );

    // 不创建 evaluation issue
    assert_eq!(fetch_evaluation_count(&db, company_id, run_id).await, 0);

    cleanup(&db, company_id).await;
}

/// source_issue origin_kind = stale_active_run_evaluation → Skipped + activity log
#[tokio::test]
async fn stale_eval_source_refuses_recursion() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue_with_origin(
        &db,
        company_id,
        &prefix,
        "in_progress",
        "stale_active_run_evaluation",
    )
    .await;

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: silent_run_candidate_for_input::build(&db, company_id, agent_id, run_id).await,
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
            status: "in_progress".to_owned(),
        }),
        evaluation_owner_agent_id: None,
        source_issue_origin_kind: Some("stale_active_run_evaluation".to_owned()),
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(outcome, StaleRunEvaluationOutcome::Skipped);

    assert_eq!(
        fetch_recursion_refused_count(&db, company_id, run_id).await,
        1
    );
    assert_eq!(fetch_evaluation_count(&db, company_id, run_id).await, 0);

    cleanup(&db, company_id).await;
}

/// source_issue origin_kind = todo (普通 issue) → 正常流程 Created
#[tokio::test]
async fn normal_source_continues_normal_flow() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id =
        insert_source_issue_with_origin(&db, company_id, &prefix, "in_progress", "todo").await;

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: silent_run_candidate_for_input::build(&db, company_id, agent_id, run_id).await,
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
        evaluation_owner_agent_id: None,
        source_issue_origin_kind: Some("todo".to_owned()),
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert!(matches!(outcome, StaleRunEvaluationOutcome::Created(_)));

    // activity log 不写 recursion_refused
    assert_eq!(
        fetch_recursion_refused_count(&db, company_id, run_id).await,
        0
    );

    // evaluation 创建
    assert_eq!(fetch_evaluation_count(&db, company_id, run_id).await, 1);

    cleanup(&db, company_id).await;
}

/// 递归短路优先级：先于 dismissed_false_positive（dismissed 但 source 是 recovery → 也走 recursion refused）
#[tokio::test]
async fn recursion_check_takes_priority_over_dismissed() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue_with_origin(
        &db,
        company_id,
        &prefix,
        "in_progress",
        "stranded_issue_recovery",
    )
    .await;
    // 也设置 dismissed_false_positive
    sqlx::query(
        "INSERT INTO heartbeat_run_watchdog_decisions \
         (company_id, run_id, decision, reason) VALUES ($1, $2, 'dismissed_false_positive', 'r338')",
    )
    .bind(company_id)
    .bind(run_id)
    .execute(db.pool())
    .await
    .unwrap();

    let input = CreateOrUpdateStaleRunEvaluationInput {
        run: silent_run_candidate_for_input::build(&db, company_id, agent_id, run_id).await,
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
            status: "in_progress".to_owned(),
        }),
        evaluation_owner_agent_id: None,
        source_issue_origin_kind: Some("stranded_issue_recovery".to_owned()),
        now: Utc::now(),
    };

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(outcome, StaleRunEvaluationOutcome::Skipped);

    // recursion_refused 写入（即使也有 dismissed）
    assert_eq!(
        fetch_recursion_refused_count(&db, company_id, run_id).await,
        1
    );

    cleanup(&db, company_id).await;
}

// Test helper for building SilentRunCandidate from a fixture row
mod silent_run_candidate_for_input {
    use pc_heartbeat::recovery::scan_silent_active_runs_db::SilentRunCandidate;
    use pc_repos::Db;
    use uuid::Uuid;

    pub async fn build(
        _db: &Db,
        company_id: Uuid,
        agent_id: Uuid,
        run_id: Uuid,
    ) -> SilentRunCandidate {
        SilentRunCandidate {
            id: run_id,
            company_id,
            agent_id,
            status: "running".to_owned(),
            last_output_at: Some(chrono::Utc::now() - chrono::Duration::minutes(250)),
            started_at: Some(chrono::Utc::now() - chrono::Duration::hours(5)),
            process_started_at: Some(chrono::Utc::now() - chrono::Duration::hours(5)),
            created_at: chrono::Utc::now() - chrono::Duration::hours(5),
            context_snapshot: None,
        }
    }
}

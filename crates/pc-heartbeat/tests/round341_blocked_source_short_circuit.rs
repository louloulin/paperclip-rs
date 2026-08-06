//! Round 341：`source_issue.status === 'blocked'` short-circuit（Node 第 2099 行）。
//!
//! 对齐 Node `services/recovery/service.ts:2099`：
//! - "Idle output is expected when the source issue is blocked — skip ticket creation entirely."
//! - 当 source_issue.status == 'blocked' → 返回 Skipped，不创建 evaluation issue
//!
//! 测试场景：
//! 1. source_issue=blocked → Skipped（不创建 eval）
//! 2. source_issue=in_progress → 正常流程（Created）
//! 3. source_issue=todo → 正常流程（Created）
//! 4. source_issue=blocked 但有 dismissed_false_positive → 已早 short-circuit，blocked 不重复触发
//! 5. source_issue=blocked + 已有 open evaluation → blocked 仍 Skipped（避免重复升级）

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::create_or_update_stale_run_evaluation_full::{
    create_or_update_stale_run_evaluation_full, CreateOrUpdateStaleRunEvaluationInput,
};
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
        .bind(format!("r341-{company_id}"))
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

async fn insert_source_issue(db: &Db, company_id: Uuid, prefix: &str, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind) \
         VALUES ($1, $2, $3, 'r341-src', $4, 'todo')",
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

async fn fetch_evaluation_count(db: &Db, company_id: Uuid, run_id: Uuid) -> i64 {
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
    n
}

fn make_input(
    company_id: Uuid,
    agent_id: Uuid,
    run_id: Uuid,
) -> CreateOrUpdateStaleRunEvaluationInput {
    use pc_heartbeat::recovery::build_stale_run_evaluation_description::StaleAgentView;
    use pc_heartbeat::recovery::build_stale_run_evaluation_description::StaleSourceIssueView;
    use pc_heartbeat::recovery::ensure_source_issue_commented_for_stale_evaluation::SourceIssueView;

    CreateOrUpdateStaleRunEvaluationInput {
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
            id: Uuid::nil(), // dummy
            identifier: Some("ROOT-1".to_owned()),
        }),
        source_issue_row: Some(SourceIssueView {
            id: Uuid::nil(), // will be overridden
            company_id,
            status: "blocked".to_owned(),
        }),
        source_issue_origin_kind: Some("todo".to_owned()),
        evaluation_owner_agent_id: None,
        now: Utc::now(),
    }
}

/// 主路径：source_issue=blocked → Skipped（不创建 eval）
#[tokio::test]
async fn blocked_source_skips_evaluation() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "blocked").await;

    let mut input = make_input(company_id, agent_id, run_id);
    input.source_issue.as_mut().unwrap().id = source_id;
    input.source_issue_row.as_mut().unwrap().id = source_id;
    input.source_issue_row.as_mut().unwrap().status = "blocked".to_owned();

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(outcome, StaleRunEvaluationOutcome::Skipped);

    // 不创建 evaluation issue
    assert_eq!(fetch_evaluation_count(&db, company_id, run_id).await, 0);

    cleanup(&db, company_id).await;
}

/// source_issue=in_progress → 正常流程（Created）
#[tokio::test]
async fn in_progress_source_continues_normal_flow() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "in_progress").await;

    let mut input = make_input(company_id, agent_id, run_id);
    input.source_issue.as_mut().unwrap().id = source_id;
    input.source_issue_row.as_mut().unwrap().id = source_id;
    input.source_issue_row.as_mut().unwrap().status = "in_progress".to_owned();

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    // critical + 无 owner → Created
    assert!(matches!(outcome, StaleRunEvaluationOutcome::Created(_)));

    cleanup(&db, company_id).await;
}

/// source_issue=todo → 正常流程（Created）
#[tokio::test]
async fn todo_source_continues_normal_flow() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "todo").await;

    let mut input = make_input(company_id, agent_id, run_id);
    input.source_issue.as_mut().unwrap().id = source_id;
    input.source_issue_row.as_mut().unwrap().id = source_id;
    input.source_issue_row.as_mut().unwrap().status = "todo".to_owned();

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert!(matches!(outcome, StaleRunEvaluationOutcome::Created(_)));

    cleanup(&db, company_id).await;
}

/// source_issue=blocked + 已有 dismissed_false_positive → 仍 Skipped（但走的可能是 dismissed 路径）
#[tokio::test]
async fn blocked_source_with_dismissed_still_skipped() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "blocked").await;
    // 设置 dismissed_false_positive
    sqlx::query(
        "INSERT INTO heartbeat_run_watchdog_decisions \
         (company_id, run_id, decision, reason) VALUES ($1, $2, 'dismissed_false_positive', 'r341')",
    )
    .bind(company_id)
    .bind(run_id)
    .execute(db.pool())
    .await
    .unwrap();

    let mut input = make_input(company_id, agent_id, run_id);
    input.source_issue.as_mut().unwrap().id = source_id;
    input.source_issue_row.as_mut().unwrap().id = source_id;
    input.source_issue_row.as_mut().unwrap().status = "blocked".to_owned();

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    // dismissed 优先级高，但结果都是 Skipped
    assert_eq!(outcome, StaleRunEvaluationOutcome::Skipped);

    cleanup(&db, company_id).await;
}

/// source_issue=blocked + 已有 open evaluation → blocked 仍 Skipped（避免重复升级）
#[tokio::test]
async fn blocked_source_with_existing_eval_skipped() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let source_id = insert_source_issue(&db, company_id, &prefix, "blocked").await;
    // 预先创建一个 open evaluation
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_id, origin_run_id, origin_fingerprint) \
         VALUES ($1, $2, 'pre-existing', 'todo', 'medium', $3, $4, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run_id.to_string())
    .bind(format!("existing-{run_id}"))
    .execute(db.pool())
    .await
    .unwrap();

    let mut input = make_input(company_id, agent_id, run_id);
    input.source_issue.as_mut().unwrap().id = source_id;
    input.source_issue_row.as_mut().unwrap().id = source_id;
    input.source_issue_row.as_mut().unwrap().status = "blocked".to_owned();

    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    // blocked 短路 → Skipped（不升级 priority，不写 critical comment）
    assert_eq!(outcome, StaleRunEvaluationOutcome::Skipped);

    // evaluation 仍 medium（未升级）
    let (priority,): (String,) = sqlx::query_as(
        "SELECT priority::text FROM issues \
         WHERE company_id = $1 AND origin_kind = $2 AND origin_id = $3",
    )
    .bind(company_id)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(priority, "medium");

    cleanup(&db, company_id).await;
}

//! Round 340：`auto_dismiss_closed_evaluation` 主循环接入。
//!
//! 对齐 Node `services/recovery/service.ts:2103`：
//! - 现有 evaluation 已 done（closed）但没有 watchdog decision 时，
//!   自动记录 `dismissed_false_positive`，避免 watchdog 下一轮再次触发
//! - 用 pg_advisory_xact_lock 序列化并发（hashtextextended key）
//!
//! 测试场景：
//! 1. closed evaluation + 无 watchdog decision → auto_dismiss + Skipped
//! 2. closed evaluation + 有 watchdog decision (snooze) → 不 auto_dismiss + 继续正常流程
//! 3. auto_dismiss 后下一轮 cycle → Skipped（hasDismissedFalsePositiveDecision 命中）
//! 4. 无 closed evaluation → 不触发 auto_dismiss
//! 5. 并发两个 cycle 调用 → 仅一个 auto_dismiss 成功（advisory lock 保护）

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::build_stale_run_evaluation_description::StaleAgentView;
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
        .bind(format!("r340-{company_id}"))
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

async fn insert_closed_evaluation(db: &Db, company_id: Uuid, run_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_id, origin_run_id, origin_fingerprint, updated_at) \
         VALUES ($1, $2, 'closed-eval', 'done', 'medium', $3, $4, $4, $5, now() - interval '1 day') RETURNING id",
    )
    .bind(id)
    .bind(company_id)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run_id.to_string())
    .bind(format!("closed-{run_id}"))
    .fetch_one(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_existing_snooze_decision(db: &Db, company_id: Uuid, run_id: Uuid) {
    sqlx::query(
        "INSERT INTO heartbeat_run_watchdog_decisions \
         (company_id, run_id, decision, snoozed_until, reason) \
         VALUES ($1, $2, 'snooze', now() + interval '1 hour', 'r340')",
    )
    .bind(company_id)
    .bind(run_id)
    .execute(db.pool())
    .await
    .unwrap();
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

async fn fetch_new_evaluation_count(db: &Db, company_id: Uuid, run_id: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = $2 AND origin_id = $3 AND status != 'done'",
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
    CreateOrUpdateStaleRunEvaluationInput {
        run: pc_heartbeat::recovery::scan_silent_active_runs_db::SilentRunCandidate {
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
        source_issue: None,
        source_issue_row: None,
        source_issue_origin_kind: None,
        evaluation_owner_agent_id: None,
        now: Utc::now(),
    }
}

/// 主路径：closed evaluation + 无 watchdog decision → auto_dismiss + Skipped
#[tokio::test]
async fn closed_evaluation_triggers_auto_dismiss_and_skips() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let _closed_id = insert_closed_evaluation(&db, company_id, run_id).await;

    let input = make_input(company_id, agent_id, run_id);
    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(outcome, StaleRunEvaluationOutcome::Skipped);

    // dismissed_false_positive decision 写入
    assert_eq!(fetch_dismissed_count(&db, company_id, run_id).await, 1);

    // 没有新建 evaluation issue（除已 closed 的）
    assert_eq!(fetch_new_evaluation_count(&db, company_id, run_id).await, 0);

    cleanup(&db, company_id).await;
}

/// closed evaluation + 已 snooze decision → 不 auto_dismiss（hasAnyDecision=true），继续正常流程
#[tokio::test]
async fn existing_snooze_decision_prevents_auto_dismiss() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let _closed_id = insert_closed_evaluation(&db, company_id, run_id).await;
    insert_existing_snooze_decision(&db, company_id, run_id).await;

    let input = make_input(company_id, agent_id, run_id);
    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    // 有 snooze decision → 不 auto_dismiss → critical + 无 source → Created
    assert!(matches!(outcome, StaleRunEvaluationOutcome::Created(_)));

    // dismissed_false_positive 没新增
    assert_eq!(fetch_dismissed_count(&db, company_id, run_id).await, 0);

    cleanup(&db, company_id).await;
}

/// auto_dismiss 之后下一轮 cycle → Skipped（hasDismissedFalsePositiveDecision 命中）
#[tokio::test]
async fn auto_dismissed_run_skipped_on_next_cycle() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let _closed_id = insert_closed_evaluation(&db, company_id, run_id).await;

    // 第一轮：auto_dismiss
    let input = make_input(company_id, agent_id, run_id);
    let out1 = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(out1, StaleRunEvaluationOutcome::Skipped);

    // 第二轮：hasDismissedFalsePositiveDecision 命中 → Skipped
    let out2 = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    assert_eq!(out2, StaleRunEvaluationOutcome::Skipped);

    // 仅一个 dismissed decision（第二轮没新增）
    assert_eq!(fetch_dismissed_count(&db, company_id, run_id).await, 1);

    cleanup(&db, company_id).await;
}

/// 无 closed evaluation → 不触发 auto_dismiss，正常流程
#[tokio::test]
async fn no_closed_evaluation_skips_auto_dismiss() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    // 无 closed evaluation

    let input = make_input(company_id, agent_id, run_id);
    let outcome = create_or_update_stale_run_evaluation_full(&db, &input)
        .await
        .unwrap();
    // critical + 无 source → Created
    assert!(matches!(outcome, StaleRunEvaluationOutcome::Created(_)));

    // 无 dismissed decision
    assert_eq!(fetch_dismissed_count(&db, company_id, run_id).await, 0);

    cleanup(&db, company_id).await;
}

/// 并发：两个并发调用 → advisory lock 保证仅一个 auto_dismiss 成功
#[tokio::test]
async fn concurrent_auto_dismiss_only_one_succeeds() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "engineer-1").await;
    let run_id = insert_run(&db, company_id, agent_id, 250).await;
    let _closed_id = insert_closed_evaluation(&db, company_id, run_id).await;

    // 并发两个调用（不 await join，全部 spawn）
    let db1 = db.clone();
    let db2 = db.clone();
    let input1 = make_input(company_id, agent_id, run_id);
    let input2 = make_input(company_id, agent_id, run_id);
    let h1 =
        tokio::spawn(
            async move { create_or_update_stale_run_evaluation_full(&db1, &input1).await },
        );
    let h2 =
        tokio::spawn(
            async move { create_or_update_stale_run_evaluation_full(&db2, &input2).await },
        );
    let out1 = h1.await.unwrap().unwrap();
    let out2 = h2.await.unwrap().unwrap();

    // 至少一个是 Skipped（auto_dismiss 成功）
    // 第二个可能也是 Skipped（hasDismissed 命中）或别的状态
    let skipped_count = [&out1, &out2]
        .iter()
        .filter(|o| matches!(o, StaleRunEvaluationOutcome::Skipped))
        .count();
    assert!(
        skipped_count >= 1,
        "expected at least one Skipped, got {:?} {:?}",
        out1,
        out2
    );

    // 最多一个 dismissed decision（advisory lock 防止重复 insert）
    let dismissed_count = fetch_dismissed_count(&db, company_id, run_id).await;
    assert_eq!(dismissed_count, 1, "expected exactly 1 dismissed decision");

    cleanup(&db, company_id).await;
}

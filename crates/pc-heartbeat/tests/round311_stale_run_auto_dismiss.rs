//! `stale_run_auto_dismiss` 模块的真实 PostgreSQL 集成测试。
//!
//! 验证两个核心子流程在真实 DB 上的端到端行为：
//!
//! auto_dismiss_closed_evaluation：
//! - 没有 closed evaluation → Skipped(NoClosedEvaluation)
//! - 已有 watchdog decision → Skipped(HasExistingDecision)
//! - 存在 closed evaluation + 无 decision → Dismissed + 新 decision 行
//! - idempotency：第二次调用 → Skipped(HasExistingDecision)
//!
//! fold_source_resolved_stale_run：
//! - run 不存在 → Skipped(RunNotRunning)
//! - source issue 状态为 done → run status='succeeded'
//! - source issue 状态为 cancelled → run status='cancelled'
//! - run 有 wakeup_request_id → wakeup 同步 finalize
//! - source issue.execution_run_id 清空
//! - watchdog_decision 记录插入
//! - 已有 watchdog decision → 不应阻止 fold（不同表）
//! - existing_evaluation_id 非空 → evaluation issue 标记 done
use pc_heartbeat::recovery::{
    auto_dismiss_closed_evaluation, fold_source_resolved_stale_run,
    AutoDismissClosedEvaluationInput, AutoDismissClosedEvaluationOutcome, AutoDismissSkipReason,
    FoldSourceResolvedInput, FoldSourceResolvedOutcome, FoldSourceResolvedSkipReason,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
const STALE_EVAL_ORIGIN_KIND: &str = "stale_active_run_evaluation";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn fixture(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r311-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r311-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_issue(
    db: &Db,
    company_id: Uuid,
    status: &str,
    origin_kind: &str,
    origin_id: Option<&str>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_fingerprint, origin_id) \
         VALUES ($1, $2, $3, $4, 'normal', $5, $6, $7)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r311-iss-{id}"))
    .bind(status)
    .bind(origin_kind)
    .bind(format!("r311-fp-{id}"))
    .bind(origin_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_running_run(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    wakeup_id: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source, \
                                     context_snapshot, started_at, created_at, wakeup_request_id) \
         VALUES ($1, $2, $3, 'running', 'on_demand', '{}'::jsonb, now(), now(), $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(wakeup_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_wakeup_request(db: &Db, company_id: Uuid, agent_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agent_wakeup_requests (id, company_id, agent_id, source, status) \
         VALUES ($1, $2, $3, 'on_demand', 'queued')",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM heartbeat_run_watchdog_decisions WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
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

async fn global_cleanup_stale_evaluations(db: &Db) {
    let _ = sqlx::query(
        "DELETE FROM heartbeat_run_watchdog_decisions WHERE run_id IN \
         (SELECT id FROM heartbeat_runs WHERE company_id IN \
          (SELECT id FROM companies WHERE name LIKE 'r311-%'))",
    )
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM issues WHERE origin_kind = $1 AND company_id IN \
         (SELECT id FROM companies WHERE name LIKE 'r311-%')",
    )
    .bind(STALE_EVAL_ORIGIN_KIND)
    .execute(db.pool())
    .await;
}

// ============================================================================
// auto_dismiss_closed_evaluation tests
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn auto_dismiss_skipped_when_no_closed_evaluation() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_running_run(&db, company_id, agent_id, None).await;

    let outcome = auto_dismiss_closed_evaluation(
        &db,
        AutoDismissClosedEvaluationInput {
            company_id,
            run_id,
            now: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        AutoDismissClosedEvaluationOutcome::Skipped {
            reason: AutoDismissSkipReason::NoClosedEvaluation,
        }
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_dismiss_skipped_when_has_existing_decision() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_running_run(&db, company_id, agent_id, None).await;
    let _eval = insert_issue(
        &db,
        company_id,
        "done",
        STALE_EVAL_ORIGIN_KIND,
        Some(&run_id.to_string()),
    )
    .await;

    // 先插入一条 watchdog decision
    sqlx::query(
        "INSERT INTO heartbeat_run_watchdog_decisions \
            (company_id, run_id, decision, reason) \
         VALUES ($1, $2, 'snooze', 'pre-existing snooze')",
    )
    .bind(company_id)
    .bind(run_id)
    .execute(db.pool())
    .await
    .unwrap();

    let outcome = auto_dismiss_closed_evaluation(
        &db,
        AutoDismissClosedEvaluationInput {
            company_id,
            run_id,
            now: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        AutoDismissClosedEvaluationOutcome::Skipped {
            reason: AutoDismissSkipReason::HasExistingDecision,
        }
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_dismiss_inserts_decision_when_closed_evaluation_exists() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_running_run(&db, company_id, agent_id, None).await;
    let _eval = insert_issue(
        &db,
        company_id,
        "done",
        STALE_EVAL_ORIGIN_KIND,
        Some(&run_id.to_string()),
    )
    .await;

    let outcome = auto_dismiss_closed_evaluation(
        &db,
        AutoDismissClosedEvaluationInput {
            company_id,
            run_id,
            now: None,
        },
    )
    .await
    .unwrap();

    let decision_id = match outcome {
        AutoDismissClosedEvaluationOutcome::Dismissed { decision_id } => decision_id,
        other => panic!("expected Dismissed, got {:?}", other),
    };

    // 验证 DB 中存在该 decision
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM heartbeat_run_watchdog_decisions \
         WHERE id = $1 AND company_id = $2 AND run_id = $3 \
           AND decision = 'dismissed_false_positive'",
    )
    .bind(decision_id)
    .bind(company_id)
    .bind(run_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_dismiss_is_idempotent() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_running_run(&db, company_id, agent_id, None).await;
    let _eval = insert_issue(
        &db,
        company_id,
        "done",
        STALE_EVAL_ORIGIN_KIND,
        Some(&run_id.to_string()),
    )
    .await;

    let input = AutoDismissClosedEvaluationInput {
        company_id,
        run_id,
        now: None,
    };

    let outcome1 = auto_dismiss_closed_evaluation(&db, input.clone())
        .await
        .unwrap();
    assert!(matches!(
        outcome1,
        AutoDismissClosedEvaluationOutcome::Dismissed { .. }
    ));

    let outcome2 = auto_dismiss_closed_evaluation(&db, input).await.unwrap();
    assert_eq!(
        outcome2,
        AutoDismissClosedEvaluationOutcome::Skipped {
            reason: AutoDismissSkipReason::HasExistingDecision,
        }
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_dismiss_skipped_when_evaluation_is_cancelled_not_done() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_running_run(&db, company_id, agent_id, None).await;
    // status=cancelled 的 evaluation 不应触发 auto_dismiss
    let _eval = insert_issue(
        &db,
        company_id,
        "cancelled",
        STALE_EVAL_ORIGIN_KIND,
        Some(&run_id.to_string()),
    )
    .await;

    let outcome = auto_dismiss_closed_evaluation(
        &db,
        AutoDismissClosedEvaluationInput {
            company_id,
            run_id,
            now: None,
        },
    )
    .await
    .unwrap();

    // Node 的 findClosedStaleRunEvaluation 仅查 status='done' → Skipped(NoClosedEvaluation)
    assert_eq!(
        outcome,
        AutoDismissClosedEvaluationOutcome::Skipped {
            reason: AutoDismissSkipReason::NoClosedEvaluation,
        }
    );

    cleanup(&db, company_id).await;
}

// ============================================================================
// fold_source_resolved_stale_run tests
// ============================================================================

#[tokio::test(flavor = "current_thread")]
async fn fold_skipped_when_run_not_exists() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, _agent_id) = fixture(&db).await;

    let fake_run_id = Uuid::new_v4();
    let outcome = fold_source_resolved_stale_run(
        &db,
        FoldSourceResolvedInput {
            run_id: fake_run_id,
            source_issue_id: Uuid::new_v4(),
            source_issue_status: "done".to_string(),
            source_issue_identifier: None,
            evidence_kind: "activity_log".to_string(),
            evidence_id: Uuid::new_v4(),
            evidence_at: chrono::Utc::now(),
            existing_evaluation_id: None,
            existing_evaluation_identifier: None,
            silence_started_at: None,
            silence_age_ms: None,
            wakeup_request_id: None,
            now: chrono::Utc::now(),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        FoldSourceResolvedOutcome::Skipped {
            reason: FoldSourceResolvedSkipReason::RunNotRunning,
        }
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn fold_finalizes_run_with_status_succeeded() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_running_run(&db, company_id, agent_id, None).await;
    let source_id = insert_issue(&db, company_id, "done", "system", None).await;
    // 把 source.execution_run_id 设为当前 run
    sqlx::query("UPDATE issues SET execution_run_id = $1 WHERE id = $2")
        .bind(run_id)
        .bind(source_id)
        .execute(db.pool())
        .await
        .unwrap();

    let outcome = fold_source_resolved_stale_run(
        &db,
        FoldSourceResolvedInput {
            run_id,
            source_issue_id: source_id,
            source_issue_status: "done".to_string(),
            source_issue_identifier: Some("ISS-1".to_string()),
            evidence_kind: "activity_log".to_string(),
            evidence_id: Uuid::new_v4(),
            evidence_at: chrono::Utc::now(),
            existing_evaluation_id: None,
            existing_evaluation_identifier: None,
            silence_started_at: None,
            silence_age_ms: Some(12345),
            wakeup_request_id: None,
            now: chrono::Utc::now(),
        },
    )
    .await
    .unwrap();

    let decision_id = match outcome {
        FoldSourceResolvedOutcome::Folded {
            run_status,
            decision_id,
        } => {
            assert_eq!(run_status, "succeeded");
            decision_id
        }
        other => panic!("expected Folded, got {:?}", other),
    };

    // 验证 run 状态
    let run_status: String =
        sqlx::query_scalar("SELECT status::text FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(run_status, "succeeded");

    // 验证 source.execution_run_id 已清空
    let exec_run_id: Option<Uuid> =
        sqlx::query_scalar("SELECT execution_run_id FROM issues WHERE id = $1")
            .bind(source_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(exec_run_id, None);

    // 验证 decision 已插入
    let decision_kind: String = sqlx::query_scalar(
        "SELECT decision::text FROM heartbeat_run_watchdog_decisions WHERE id = $1",
    )
    .bind(decision_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(decision_kind, "dismissed_false_positive");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn fold_finalizes_run_with_status_cancelled() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_running_run(&db, company_id, agent_id, None).await;
    let source_id = insert_issue(&db, company_id, "cancelled", "system", None).await;
    sqlx::query("UPDATE issues SET execution_run_id = $1 WHERE id = $2")
        .bind(run_id)
        .bind(source_id)
        .execute(db.pool())
        .await
        .unwrap();

    let outcome = fold_source_resolved_stale_run(
        &db,
        FoldSourceResolvedInput {
            run_id,
            source_issue_id: source_id,
            source_issue_status: "cancelled".to_string(),
            source_issue_identifier: None,
            evidence_kind: "activity_log".to_string(),
            evidence_id: Uuid::new_v4(),
            evidence_at: chrono::Utc::now(),
            existing_evaluation_id: None,
            existing_evaluation_identifier: None,
            silence_started_at: None,
            silence_age_ms: None,
            wakeup_request_id: None,
            now: chrono::Utc::now(),
        },
    )
    .await
    .unwrap();

    match outcome {
        FoldSourceResolvedOutcome::Folded { run_status, .. } => {
            assert_eq!(run_status, "cancelled");
        }
        other => panic!("expected Folded, got {:?}", other),
    }

    let run_status: String =
        sqlx::query_scalar("SELECT status::text FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(run_status, "cancelled");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn fold_finalizes_wakeup_when_wakeup_id_provided() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, agent_id) = fixture(&db).await;
    let wakeup_id = insert_wakeup_request(&db, company_id, agent_id).await;
    let run_id = insert_running_run(&db, company_id, agent_id, Some(wakeup_id)).await;
    let source_id = insert_issue(&db, company_id, "done", "system", None).await;

    let _ = fold_source_resolved_stale_run(
        &db,
        FoldSourceResolvedInput {
            run_id,
            source_issue_id: source_id,
            source_issue_status: "done".to_string(),
            source_issue_identifier: None,
            evidence_kind: "activity_log".to_string(),
            evidence_id: Uuid::new_v4(),
            evidence_at: chrono::Utc::now(),
            existing_evaluation_id: None,
            existing_evaluation_identifier: None,
            silence_started_at: None,
            silence_age_ms: None,
            wakeup_request_id: Some(wakeup_id),
            now: chrono::Utc::now(),
        },
    )
    .await
    .unwrap();

    let wakeup_status: String =
        sqlx::query_scalar("SELECT status::text FROM agent_wakeup_requests WHERE id = $1")
            .bind(wakeup_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(wakeup_status, "completed");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn fold_marks_existing_evaluation_done() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_running_run(&db, company_id, agent_id, None).await;
    let source_id = insert_issue(&db, company_id, "done", "system", None).await;
    let eval_id = insert_issue(
        &db,
        company_id,
        "todo",
        STALE_EVAL_ORIGIN_KIND,
        Some(&run_id.to_string()),
    )
    .await;

    let _ = fold_source_resolved_stale_run(
        &db,
        FoldSourceResolvedInput {
            run_id,
            source_issue_id: source_id,
            source_issue_status: "done".to_string(),
            source_issue_identifier: None,
            evidence_kind: "activity_log".to_string(),
            evidence_id: Uuid::new_v4(),
            evidence_at: chrono::Utc::now(),
            existing_evaluation_id: Some(eval_id),
            existing_evaluation_identifier: Some("EVAL-1".to_string()),
            silence_started_at: Some(chrono::Utc::now()),
            silence_age_ms: Some(60000),
            wakeup_request_id: None,
            now: chrono::Utc::now(),
        },
    )
    .await
    .unwrap();

    let eval_status: String = sqlx::query_scalar("SELECT status::text FROM issues WHERE id = $1")
        .bind(eval_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(eval_status, "done");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn fold_appends_source_resolved_fold_to_result_json() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_running_run(&db, company_id, agent_id, None).await;
    // 预先设置一些 result_json
    sqlx::query("UPDATE heartbeat_runs SET result_json = $1 WHERE id = $2")
        .bind(json!({"previousKey": "previousValue"}))
        .bind(run_id)
        .execute(db.pool())
        .await
        .unwrap();

    let source_id = insert_issue(&db, company_id, "done", "system", None).await;
    let evidence_id = Uuid::new_v4();
    let evidence_at = chrono::Utc::now();

    let _ = fold_source_resolved_stale_run(
        &db,
        FoldSourceResolvedInput {
            run_id,
            source_issue_id: source_id,
            source_issue_status: "done".to_string(),
            source_issue_identifier: Some("SRC-42".to_string()),
            evidence_kind: "issue_comment".to_string(),
            evidence_id,
            evidence_at,
            existing_evaluation_id: None,
            existing_evaluation_identifier: None,
            silence_started_at: None,
            silence_age_ms: Some(5000),
            wakeup_request_id: None,
            now: chrono::Utc::now(),
        },
    )
    .await
    .unwrap();

    let result_json: serde_json::Value =
        sqlx::query_scalar("SELECT result_json FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();

    let obj = result_json.as_object().unwrap();
    assert_eq!(
        obj.get("previousKey").and_then(|v| v.as_str()),
        Some("previousValue")
    );
    let fold_obj = obj
        .get("sourceResolvedWatchdogFold")
        .unwrap()
        .as_object()
        .unwrap();
    assert_eq!(
        fold_obj.get("sourceIssueId").and_then(|v| v.as_str()),
        Some(source_id.to_string().as_str())
    );
    assert_eq!(
        fold_obj.get("sameRunEvidenceKind").and_then(|v| v.as_str()),
        Some("issue_comment")
    );
    assert_eq!(
        fold_obj.get("sameRunEvidenceId").and_then(|v| v.as_str()),
        Some(evidence_id.to_string().as_str())
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn fold_skipped_when_run_already_not_running() {
    let db = connect().await;
    global_cleanup_stale_evaluations(&db).await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_running_run(&db, company_id, agent_id, None).await;
    // 把 run 改成 succeeded
    sqlx::query("UPDATE heartbeat_runs SET status = 'succeeded' WHERE id = $1")
        .bind(run_id)
        .execute(db.pool())
        .await
        .unwrap();

    let source_id = insert_issue(&db, company_id, "done", "system", None).await;
    let outcome = fold_source_resolved_stale_run(
        &db,
        FoldSourceResolvedInput {
            run_id,
            source_issue_id: source_id,
            source_issue_status: "done".to_string(),
            source_issue_identifier: None,
            evidence_kind: "activity_log".to_string(),
            evidence_id: Uuid::new_v4(),
            evidence_at: chrono::Utc::now(),
            existing_evaluation_id: None,
            existing_evaluation_identifier: None,
            silence_started_at: None,
            silence_age_ms: None,
            wakeup_request_id: None,
            now: chrono::Utc::now(),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        FoldSourceResolvedOutcome::Skipped {
            reason: FoldSourceResolvedSkipReason::RunNotRunning,
        }
    );

    cleanup(&db, company_id).await;
}

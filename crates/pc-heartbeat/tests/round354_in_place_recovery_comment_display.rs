//! Round 354：in-place recovery comment 的 presentation/metadata 写入与 metadata-aware dedup 真实 PG 验证。
//!
//! 对齐 Node：
//! - `compactRecoveryPresentation("Recovery: recovery attempt failed — remains blocked")` + notice metadata
//! - `noticeMetadataReferencesRecoveryAction` sections/rows 判定
//! - source escalation `escalateStrandedAssignedIssue` 写入 marker / metadata `Recovery action` 行
//!
//! 与已有轮次的区别：
//! - R329 验证 body 内容（markdown 占位符）
//! - R353 验证 source escalation 的 presentation/metadata（含 Recovery action 行）
//! - R354 验证 in-place escalation 的 presentation/metadata（**不含** Recovery action 行）
//! - R354 还验证 metadata-aware dedup：source escalation 第二次调用被 metadata 引用阻断

use pc_heartbeat::recovery::build_execution_review_participant_recovery_comment::build_execution_review_participant_recovery_comment;
use pc_heartbeat::recovery::build_recovery_comment_display::metadata_references_recovery_action;
use pc_heartbeat::recovery::build_recovery_issue_in_place_escalation_comment::EscalationRunView;
use pc_heartbeat::recovery::escalate_db::{
    escalate_stranded_assigned_issue_with_comment, escalate_stranded_recovery_issue_in_place,
    EscalateDbInput, EscalateOutcome,
};
use pc_repos::agent::{
    HeartbeatInvocationSource, NewAgentWakeupRequest, WakeupActorType, WakeupRequestStatus,
    WakeupTriggerDetail,
};
use pc_repos::Db;
use serde_json::{json, Value};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

fn wake_template(company_id: Uuid, agent_id: Uuid) -> NewAgentWakeupRequest {
    NewAgentWakeupRequest {
        company_id,
        agent_id,
        source: HeartbeatInvocationSource::OnDemand,
        trigger_detail: Some(WakeupTriggerDetail::Manual),
        reason: None,
        payload: None,
        status: WakeupRequestStatus::Queued,
        coalesced_count: 0,
        requested_by_actor_type: Some(WakeupActorType::System),
        requested_by_actor_id: None,
        idempotency_key: None,
        run_id: None,
        error: None,
    }
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let statements = [
        "DELETE FROM agent_wakeup_requests WHERE company_id = $1",
        "DELETE FROM issue_comments WHERE company_id = $1",
        "DELETE FROM issue_recovery_actions WHERE company_id = $1",
        "DELETE FROM heartbeat_runs WHERE company_id = $1",
        "DELETE FROM issues WHERE company_id = $1",
        "DELETE FROM agents WHERE company_id = $1",
        "DELETE FROM companies WHERE id = $1",
    ];
    for statement in statements {
        let _ = sqlx::query(statement)
            .bind(company_id)
            .execute(db.pool())
            .await;
    }
}

async fn insert_company_with_prefix(db: &Db, company_id: Uuid, tag: &str) -> String {
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("{tag}-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    prefix
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

async fn insert_recovery_issue(
    db: &Db,
    company_id: Uuid,
    assignee_agent_id: Uuid,
    identifier: &str,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind, assignee_agent_id) \
         VALUES ($1, $2, $3, 'recovery', $4, 'stranded_issue_recovery', $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(identifier)
    .bind(status)
    .bind(assignee_agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_terminated_run(db: &Db, company_id: Uuid, agent_id: Uuid, issue_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs \
         (id, company_id, agent_id, invocation_source, status, error, context_snapshot, started_at) \
         VALUES ($1, $2, $3, 'manual', 'failed', 'crashed', $4, now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(json!({"issueId": issue_id.to_string()}))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn fetch_latest_comment(db: &Db, issue_id: Uuid) -> (String, Option<Value>, Option<Value>) {
    let row: (String, Option<Value>, Option<Value>) = sqlx::query_as(
        "SELECT body, presentation, metadata FROM issue_comments \
         WHERE issue_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    row
}

async fn count_system_comments(db: &Db, issue_id: Uuid) -> i64 {
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_comments \
         WHERE issue_id = $1 AND author_user_id = 'system' AND deleted_at IS NULL",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    count.0
}

fn assert_recovery_action_row_absent(metadata: &Value) {
    let rows = metadata["sections"][0]["rows"]
        .as_array()
        .expect("sections[0].rows should be array");
    assert!(
        rows.iter()
            .all(|row| row.get("label").and_then(Value::as_str) != Some("Recovery action")),
        "in-place metadata must NOT contain a `Recovery action` row, got {rows:?}"
    );
}

fn assert_run_link_row_present(metadata: &Value, expected_run_id: Uuid) {
    let rows = metadata["sections"][0]["rows"]
        .as_array()
        .expect("sections[0].rows should be array");
    let run_link = rows
        .iter()
        .find(|row| row.get("type").and_then(Value::as_str) == Some("run_link"))
        .unwrap_or_else(|| panic!("expected a run_link row, got {rows:?}"));
    assert_eq!(
        run_link["label"].as_str(),
        Some("Latest run"),
        "run_link row label mismatch"
    );
    assert_eq!(
        run_link["runId"].as_str(),
        Some(expected_run_id.to_string().as_str())
    );
    assert_eq!(run_link["title"].as_str(), Some("failed"));
}

fn assert_cause_row(metadata: &Value, expected_cause: &str) {
    let rows = metadata["sections"][0]["rows"].as_array().unwrap();
    let cause_row = rows
        .iter()
        .find(|row| row.get("label").and_then(Value::as_str) == Some("Cause"))
        .expect("expected Cause row");
    assert_eq!(cause_row["value"].as_str(), Some(expected_cause));
}

fn assert_previous_status_row(metadata: &Value, expected: &str) {
    let rows = metadata["sections"][0]["rows"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row.get("label").and_then(Value::as_str) == Some("Previous status"))
        .expect("expected Previous status row");
    assert_eq!(row["value"].as_str(), Some(expected));
}

fn assert_recovery_owner_board(metadata: &Value) {
    let rows = metadata["sections"][0]["rows"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row.get("label").and_then(Value::as_str) == Some("Recovery owner"))
        .expect("expected Recovery owner row");
    assert_eq!(row["value"].as_str(), Some("board"));
}

/// in-place 升级有 latest run：metadata 4 行，无 Recovery action 行。
#[tokio::test]
async fn in_place_with_run_writes_presentation_and_metadata_without_action_row() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let prefix = insert_company_with_prefix(&db, company_id, "r354a").await;
    let agent_id = insert_agent(&db, company_id, "recovery-agent").await;
    let identifier = format!("{prefix}-91");
    let issue_id =
        insert_recovery_issue(&db, company_id, agent_id, &identifier, "in_progress").await;
    let run_id = insert_terminated_run(&db, company_id, agent_id, issue_id).await;

    let result = escalate_stranded_recovery_issue_in_place(&db, issue_id, "in_progress".to_owned())
        .await
        .unwrap()
        .expect("result Some");
    assert_eq!(result.outcome, EscalateOutcome::RecoveryInPlace);
    assert!(result.comment_id.is_some());

    let (body, presentation, metadata) = fetch_latest_comment(&db, issue_id).await;
    assert!(body.contains("Paperclip stopped automatic stranded-work recovery"));
    let presentation = presentation.expect("presentation missing");
    assert_eq!(
        presentation.get("kind").and_then(Value::as_str),
        Some("system_notice")
    );
    assert_eq!(
        presentation.get("tone").and_then(Value::as_str),
        Some("warning")
    );
    let metadata = metadata.expect("metadata missing");
    assert_eq!(metadata.get("version").and_then(Value::as_i64), Some(1));
    assert_recovery_action_row_absent(&metadata);
    assert_cause_row(&metadata, "recovery_issue_failed");
    assert_previous_status_row(&metadata, "in_progress");
    assert_recovery_owner_board(&metadata);
    assert_run_link_row_present(&metadata, run_id);

    cleanup(&db, company_id).await;
}

/// in-place 升级没有 latest run：metadata 仅有 Cause + Previous status + Recovery owner 三行。
#[tokio::test]
async fn in_place_without_run_omits_run_link_row() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let prefix = insert_company_with_prefix(&db, company_id, "r354b").await;
    let agent_id = insert_agent(&db, company_id, "no-run-agent").await;
    let identifier = format!("{prefix}-3");
    let issue_id = insert_recovery_issue(&db, company_id, agent_id, &identifier, "todo").await;
    // 无 heartbeat_run

    let result = escalate_stranded_recovery_issue_in_place(&db, issue_id, "todo".to_owned())
        .await
        .unwrap()
        .expect("result Some");
    assert_eq!(result.outcome, EscalateOutcome::RecoveryInPlace);

    let (_, presentation, metadata) = fetch_latest_comment(&db, issue_id).await;
    assert_eq!(
        presentation
            .expect("presentation")
            .get("title")
            .and_then(Value::as_str),
        Some("Recovery: recovery attempt failed — remains blocked")
    );
    let metadata = metadata.expect("metadata");
    assert_recovery_action_row_absent(&metadata);
    assert_cause_row(&metadata, "recovery_issue_failed");
    assert_previous_status_row(&metadata, "todo");
    assert_recovery_owner_board(&metadata);

    let rows = metadata["sections"][0]["rows"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        3,
        "in-place metadata without run should have exactly 3 rows (cause, prev status, owner=board), got {rows:?}"
    );

    cleanup(&db, company_id).await;
}

/// in-place 升级重复调用：因为 issue 已被切到 blocked，第二次 decide_escalation 返回 Skip，
/// 且即使强行走完路径也不会新建第二个 system comment。
#[tokio::test]
async fn in_place_repeat_does_not_double_write() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let prefix = insert_company_with_prefix(&db, company_id, "r354c").await;
    let agent_id = insert_agent(&db, company_id, "repeat-agent").await;
    let identifier = format!("{prefix}-1");
    let issue_id =
        insert_recovery_issue(&db, company_id, agent_id, &identifier, "in_progress").await;
    insert_terminated_run(&db, company_id, agent_id, issue_id).await;

    let first = escalate_stranded_recovery_issue_in_place(&db, issue_id, "in_progress".to_owned())
        .await
        .unwrap()
        .expect("first result Some");
    assert!(first.comment_id.is_some());
    let count_after_first = count_system_comments(&db, issue_id).await;
    assert_eq!(count_after_first, 1);

    // 第二次会再走 RecoveryInPlace 路径（origin_kind 短路先于 status 检查），
    // 但 R354 的 in-place dedup 通过 body marker 拦截第二次 comment 写入。
    let second = escalate_stranded_recovery_issue_in_place(&db, issue_id, "in_progress".to_owned())
        .await
        .unwrap()
        .expect("dedup path still returns Some");
    assert_eq!(second.outcome, EscalateOutcome::RecoveryInPlace);
    assert!(
        second.comment_id.is_none(),
        "dedup should skip the second comment"
    );
    let count_after_second = count_system_comments(&db, issue_id).await;
    assert_eq!(
        count_after_second, 1,
        "in-place repeat must NOT write a second system comment"
    );

    cleanup(&db, company_id).await;
}

/// source escalation 重复调用：第二次走 Skip（issue 已 blocked）或不写新 comment，
/// 关键是 system comment 总数保持 1。这一断言体现 R354 metadata-aware dedup 的不变量：
/// 同 recovery action 的 marker 在元数据中可见，但重复调用不会引发第二次写入。
#[tokio::test]
async fn source_escalation_repeat_does_not_double_write() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let prefix = insert_company_with_prefix(&db, company_id, "r354d").await;
    let agent_id = insert_agent(&db, company_id, "reviewer-r354d").await;
    let issue_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, priority, origin_kind, origin_fingerprint, assignee_agent_id, execution_state) \
         VALUES ($1, $2, $3, 'review target', 'in_review', 'normal', 'system', $4, $5, $6)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(format!("{prefix}-71"))
    .bind(format!("r354d-{issue_id}"))
    .bind(agent_id)
    .bind(json!({
        "status": "pending",
        "currentStageId": "review-stage",
        "currentStageType": "execution_review",
        "currentParticipant": {"type": "agent", "agentId": agent_id}
    }))
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO heartbeat_runs \
         (id, company_id, agent_id, invocation_source, status, error, error_code, context_snapshot, started_at, created_at) \
         VALUES ($1, $2, $3, 'manual', 'failed', 'review failed', 'adapter_failed', $4, now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(json!({"issueId": issue_id.to_string()}))
    .execute(db.pool())
    .await
    .unwrap();

    let comment = build_execution_review_participant_recovery_comment(&EscalationRunView {
        id: run_id,
        agent_id: Some(agent_id),
        status: "failed".to_owned(),
        error: Some("review failed".to_owned()),
        error_code: Some("adapter_failed".to_owned()),
        context_snapshot: Some(json!({})),
    });
    let first = escalate_stranded_assigned_issue_with_comment(
        &db,
        EscalateDbInput {
            issue_id,
            previous_status: "in_review".to_owned(),
            recovery_cause_override: Some(
                pc_heartbeat::recovery::source_scoped_recovery_action::StrandedRecoveryCause::ExecutionReviewParticipantRecovery,
            ),
            recovery_owner_agent_id: Some(agent_id),
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        Some(comment),
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap()
    .expect("first result Some");
    assert_eq!(first.outcome, EscalateOutcome::SourceEscalated);
    let count_after_first = count_system_comments(&db, issue_id).await;
    assert_eq!(count_after_first, 1);
    let action_id = first.recovery_action_id.expect("recovery action id");

    // 第二次调用：不论走 Skip 还是 dedup，system comment 总数都不应增加
    let second = escalate_stranded_assigned_issue_with_comment(
        &db,
        EscalateDbInput {
            issue_id,
            previous_status: "blocked".to_owned(),
            recovery_cause_override: Some(
                pc_heartbeat::recovery::source_scoped_recovery_action::StrandedRecoveryCause::ExecutionReviewParticipantRecovery,
            ),
            recovery_owner_agent_id: Some(agent_id),
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        Some("Paperclip exhausted automatic recovery for the assigned issue.".to_owned()),
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap();
    if let Some(r) = second {
        assert!(
            matches!(
                r.outcome,
                EscalateOutcome::Skipped | EscalateOutcome::SourceEscalated
            ),
            "second call should skip or de-dup, got {:?}",
            r.outcome
        );
    }
    let count_after_second = count_system_comments(&db, issue_id).await;
    assert_eq!(
        count_after_second, 1,
        "source escalation repeat must NOT write a second system comment"
    );

    // 现有 comment 的 metadata 引用了 action_id（这是 R354 新增 dedup 通道的载体）
    let (_, _, metadata) = fetch_latest_comment(&db, issue_id).await;
    let metadata = metadata.expect("metadata of the first comment");
    assert!(
        metadata_references_recovery_action(Some(&metadata), action_id),
        "first system comment's metadata should reference the recovery action for future dedup"
    );

    cleanup(&db, company_id).await;
}

//! Round 359：source escalation / in-place escalation 路径写入
//! `activity_log` 行（actor=system, action=heartbeat.source_escalated / heartbeat.recovery_in_place）。
//!
//! 业务背景：
//! - watchdog_decision_recording 路径已写 activity_log（actor 端到端 OK）
//! - stale_evaluation 路径已写 activity_log（actor 端到端 OK）
//! - source escalation 路径（R350-R355）**从未写** activity_log → 系统视角看不到
//!   "是谁把 issue 移到 blocked" 的审计线索
//! - in-place escalation 路径（R354）同样**从未写** activity_log
//!
//! Node 参考：`packages/server/src/services/recovery/service.ts` 在
//! `escalateStrandedAssignedIssue` / `escalateStrandedRecoveryIssueInPlace` 后
//! 都调用 `logActivity({action: "heartbeat.source_escalated"|"heartbeat.recovery_in_place",
//!                       actor: "system", ...})`
//!
//! 本轮闭合：在 `apply_source_escalation` / `apply_in_place_escalation` 后写入
//! activity_log，并提供端到端 round-trip 测试。

use pc_heartbeat::recovery::{
    escalate_stranded_assigned_issue, escalate_stranded_recovery_issue_in_place, EscalateDbInput,
    EscalateOutcome,
};
use pc_repos::activity::{ActivityRepo, ActorType, NewActivity};
use pc_repos::agent::{
    NewAgentWakeupRequest, WakeupActorType, WakeupRequestStatus, WakeupTriggerDetail,
};
use pc_repos::Db;
use serde_json::{json, Value};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn fixture_with_company(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r359-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r359-agent','general','process','active')")
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
    agent_id: Uuid,
    title: &str,
    status: &str,
    origin_kind: &str,
) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id) VALUES ($1,$2,$3,$4,'normal',$5,$6,$7)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(title)
    .bind(status)
    .bind(origin_kind)
    .bind(format!("r359-fp-{issue_id}"))
    .bind(agent_id)
    .execute(db.pool())
        .await
        .unwrap();
    issue_id
}

async fn insert_run(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    agent_id: Uuid,
    error_code: &str,
    error: &str,
) -> Uuid {
    let run_id = Uuid::new_v4();
    let context_snapshot = json!({ "issueId": issue_id.to_string() });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error_code, error, context_snapshot, started_at, created_at) VALUES ($1, $2, $3, 'failed', $4, $5, $6, now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(error_code)
    .bind(error)
    .bind(context_snapshot)
    .execute(db.pool())
        .await
        .unwrap();
    run_id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query(
        "DELETE FROM issue_comments WHERE issue_id IN (SELECT id FROM issues WHERE company_id=$1)",
    )
    .bind(company_id)
    .execute(db.pool())
    .await;
    let _ = sqlx::query("DELETE FROM issue_recovery_actions WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

fn wake_template(company_id: Uuid, agent_id: Uuid) -> NewAgentWakeupRequest {
    NewAgentWakeupRequest {
        company_id,
        agent_id,
        source: pc_repos::agent::HeartbeatInvocationSource::OnDemand,
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

/// SourceEscalate 路径：escalate 后该 issue 的 activity_log 必须出现
/// `heartbeat.source_escalated` 行（actor=system, entity_type=issue）。
#[tokio::test(flavor = "current_thread")]
async fn source_escalation_writes_heartbeat_source_escalated_activity_log() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        agent_id,
        "r359-source-escalate",
        "in_progress",
        "system",
    )
    .await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        "r359-source-escalate-err",
    )
    .await;

    let result = escalate_stranded_assigned_issue(
        &db,
        EscalateDbInput {
            issue_id,
            previous_status: "in_progress".to_string(),
            recovery_cause_override: Some(
                pc_heartbeat::recovery::source_scoped_recovery_action::StrandedRecoveryCause::ProcessLost,
            ),
            recovery_owner_agent_id: Some(agent_id),
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .expect("escalate")
    .expect("some result");
    assert_eq!(result.outcome, EscalateOutcome::SourceEscalated);

    // 端到端：从 activity_log 拉 issue 实体的所有行
    let rows = ActivityRepo::new(&db)
        .list_for_entity(company_id, "issue", &issue_id.to_string(), 100)
        .await
        .expect("list activity");
    let source_escalated: Vec<_> = rows
        .iter()
        .filter(|r| r.action == "heartbeat.source_escalated")
        .collect();
    assert_eq!(
        source_escalated.len(),
        1,
        "expected 1 heartbeat.source_escalated activity row, got {:?}",
        rows.iter().map(|r| &r.action).collect::<Vec<_>>()
    );

    let row = &source_escalated[0];
    // actor 端到端：source escalation 由 system 驱动
    assert_eq!(row.actor_type, "system");
    assert_eq!(row.actor_id, "system");
    assert_eq!(row.entity_type, "issue");
    assert_eq!(row.entity_id, issue_id.to_string());
    assert_eq!(row.company_id, company_id);
    // details 含 cause + recovery_action_id + previous_status
    let details: Value = row
        .details
        .clone()
        .expect("source escalation must carry details");
    assert_eq!(
        details.get("cause").and_then(|v| v.as_str()),
        Some("process_lost")
    );
    assert_eq!(
        details.get("previous_status").and_then(|v| v.as_str()),
        Some("in_progress")
    );
    assert!(
        details.get("recovery_action_id").is_some(),
        "details.recovery_action_id missing: {details}"
    );
    let action_id = details
        .get("recovery_action_id")
        .and_then(|v| v.as_str())
        .expect("action id");
    let parsed = Uuid::parse_str(action_id).expect("uuid");
    assert_eq!(parsed, result.recovery_action_id.expect("action id"));

    cleanup(&db, company_id).await;
}

/// RecoveryInPlace 路径：escalate 后该 issue 的 activity_log 必须出现
/// `heartbeat.recovery_in_place` 行（actor=system）。
#[tokio::test(flavor = "current_thread")]
async fn in_place_escalation_writes_heartbeat_recovery_in_place_activity_log() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        agent_id,
        "r359-recovery-in-place",
        "in_progress",
        "stranded_issue_recovery",
    )
    .await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        "r359-in-place-err",
    )
    .await;

    let result =
        escalate_stranded_recovery_issue_in_place(&db, issue_id, "in_progress".to_string())
            .await
            .expect("escalate in place")
            .expect("some result");
    assert_eq!(result.outcome, EscalateOutcome::RecoveryInPlace);

    let rows = ActivityRepo::new(&db)
        .list_for_entity(company_id, "issue", &issue_id.to_string(), 100)
        .await
        .expect("list activity");
    let in_place: Vec<_> = rows
        .iter()
        .filter(|r| r.action == "heartbeat.recovery_in_place")
        .collect();
    assert_eq!(
        in_place.len(),
        1,
        "expected 1 heartbeat.recovery_in_place activity row, got {:?}",
        rows.iter().map(|r| &r.action).collect::<Vec<_>>()
    );

    let row = &in_place[0];
    assert_eq!(row.actor_type, "system");
    assert_eq!(row.actor_id, "system");
    assert_eq!(row.entity_type, "issue");
    assert_eq!(row.entity_id, issue_id.to_string());
    let details: Value = row
        .details
        .clone()
        .expect("in_place escalation must carry details");
    assert_eq!(
        details.get("source").and_then(|v| v.as_str()),
        Some("recovery.reconcile_stranded_recovery_issue"),
        "in_place details.source mismatch: {details}"
    );
    assert_eq!(
        details.get("previous_status").and_then(|v| v.as_str()),
        Some("in_progress")
    );
    assert!(
        details.get("comment_id").and_then(|v| v.as_str()).is_some(),
        "in_place details.comment_id missing: {details}"
    );

    cleanup(&db, company_id).await;
}

/// 幂等：同一 issue 重复 escalate source 不会重复写 activity_log
/// （dedup 由 issue_comments 路径控制，activity_log 只在首次 escalate 时写一次）。
#[tokio::test(flavor = "current_thread")]
async fn repeated_source_escalation_does_not_repeat_activity_log() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        agent_id,
        "r359-source-escalate-twice",
        "in_progress",
        "system",
    )
    .await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        "r359-twice-err",
    )
    .await;

    // 第一次 escalate
    let first = escalate_stranded_assigned_issue(
        &db,
        EscalateDbInput {
            issue_id,
            previous_status: "in_progress".to_string(),
            recovery_cause_override: Some(
                pc_heartbeat::recovery::source_scoped_recovery_action::StrandedRecoveryCause::ProcessLost,
            ),
            recovery_owner_agent_id: Some(agent_id),
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .expect("first escalate")
    .expect("first result");
    assert_eq!(first.outcome, EscalateOutcome::SourceEscalated);

    // 第二次 escalate —— issue 已经 blocked；decision 路径会跳过 SourceEscalate，
    // 因此不应再写一条 source_escalated activity_log。
    let second = escalate_stranded_assigned_issue(
        &db,
        EscalateDbInput {
            issue_id,
            previous_status: "blocked".to_string(),
            recovery_cause_override: Some(
                pc_heartbeat::recovery::source_scoped_recovery_action::StrandedRecoveryCause::ProcessLost,
            ),
            recovery_owner_agent_id: Some(agent_id),
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .expect("second escalate")
    .expect("second result");
    // 二次 escalate 不再是 SourceEscalated（issue 已 blocked，dedup）。
    assert!(
        matches!(second.outcome, EscalateOutcome::Skipped),
        "second outcome must be Skipped, got {:?}",
        second.outcome
    );

    let rows = ActivityRepo::new(&db)
        .list_for_entity(company_id, "issue", &issue_id.to_string(), 100)
        .await
        .expect("list");
    let source_count = rows
        .iter()
        .filter(|r| r.action == "heartbeat.source_escalated")
        .count();
    assert_eq!(source_count, 1, "expected exactly 1 source_escalated log");

    cleanup(&db, company_id).await;
}

/// 防回归：`heartbeat.source_escalated` 的 details 字段不应被吞掉；
/// `previous_status` 必须保留原状态名（方便审计追溯）。
#[tokio::test(flavor = "current_thread")]
async fn source_escalation_activity_log_details_preserve_previous_status() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        agent_id,
        "r359-source-prev-status",
        "in_review",
        "user",
    )
    .await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        "r359-prev-status-err",
    )
    .await;

    let _ = escalate_stranded_assigned_issue(
        &db,
        EscalateDbInput {
            issue_id,
            previous_status: "in_review".to_string(),
            recovery_cause_override: Some(
                pc_heartbeat::recovery::source_scoped_recovery_action::StrandedRecoveryCause::ExecutionReviewParticipantRecovery,
            ),
            recovery_owner_agent_id: Some(agent_id),
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .expect("escalate");

    let rows = ActivityRepo::new(&db)
        .list_for_entity(company_id, "issue", &issue_id.to_string(), 100)
        .await
        .expect("list");
    let target = rows
        .iter()
        .find(|r| r.action == "heartbeat.source_escalated")
        .expect("source_escalated must exist");
    let details = target.details.clone().expect("details");
    assert_eq!(
        details.get("previous_status").and_then(|v| v.as_str()),
        Some("in_review"),
        "previous_status must be preserved: {details}"
    );
    assert_eq!(
        details.get("cause").and_then(|v| v.as_str()),
        Some("execution_review_participant_recovery"),
        "cause must be preserved: {details}"
    );

    cleanup(&db, company_id).await;
}

// 强制 NewActivity 类型在测试中至少被 reference 一次，避免 unused-import warning。
#[allow(dead_code)]
fn _force_link(_x: NewActivity) {}

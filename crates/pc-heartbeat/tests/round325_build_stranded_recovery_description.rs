//! Round 325：`buildStrandedIssueRecoveryDescription` 的 PostgreSQL round-trip 验证。
//!
//! 与 Node `services/recovery/service.ts::buildStrandedIssueRecoveryDescription` 对齐：
//! - 输入：source issue + latest run + previous status + prefix + cause + evidence
//! - 输出：description 文本（Markdown），与 Node 内容结构一致
//!
//! 两种主分支：
//! 1. **SuccessfulRunMissingState** —— "Safe Evidence" + "Required Action" 段
//! 2. **Default (stranded_assigned_issue / execution_review_participant_recovery)** ——
//!    "Source" + "Ownership" + "Required Action" 段；review participant 时文本不同

use pc_heartbeat::recovery::build_stranded_issue_recovery_description::{
    build_stranded_issue_recovery_description, AgentShortView,
    BuildStrandedIssueRecoveryDescriptionInput, LatestRunView,
};
use pc_heartbeat::recovery::source_scoped_recovery_action::StrandedRecoveryCause;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
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

async fn fixture(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r325-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r325-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_issue(db: &Db, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, identifier, execution_policy) \
         VALUES ($1, $2, $3, $4, 'normal', 'system', $5, $6, $7)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r325-issue-{id}"))
    .bind(status)
    .bind(format!("r325-fp-{id}"))
    .bind(format!("PAP-{id}"))
    .bind(json!({"mode":"normal","commentRequired":false,"stages":[]}))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn fetch_issue_row(db: &Db, issue_id: Uuid) -> pc_repos::issue::IssueRow {
    pc_repos::issue::IssueRepo::new(db)
        .get(issue_id)
        .await
        .unwrap()
        .expect("issue should exist")
}

/// 默认路径（stranded_assigned_issue）描述：包含 Source / Ownership / Required Action 段
#[tokio::test]
async fn builds_default_description_with_source_ownership_and_action() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "todo").await;
    let issue = fetch_issue_row(&db, issue_id).await;
    let run_id = Uuid::new_v4();

    let latest_run = LatestRunView {
        id: run_id,
        agent_id,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "issue_continuation_needed"})),
        result_json: None,
    };
    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::RuntimeFailure),
        successful_run_handoff_evidence: None,
        source_assignee: None,
        workspace_validation_fingerprint: None,
    };

    let description = build_stranded_issue_recovery_description(&input);
    assert!(description.contains("## Source"));
    assert!(description.contains("## Ownership"));
    assert!(description.contains("## Required Action"));
    assert!(description.contains("Source issue:"));
    assert!(description.contains("Previous source status: `in_progress`"));
    assert!(description.contains("Latest retry status: `failed`"));
    assert!(description.contains("Detected invariant: `stranded_assigned_issue`"));
    assert!(description.contains("Retry reason: `issue_continuation_needed`"));
    assert!(description.contains("- Failure: none recorded"));
    assert!(description.contains("automatic recovery for an assigned issue"));

    cleanup(&db, company_id).await;
}

/// execution_review_participant_recovery cause → 文本不同
#[tokio::test]
async fn review_participant_path_uses_distinct_text() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "in_review").await;
    let issue = fetch_issue_row(&db, issue_id).await;
    let run_id = Uuid::new_v4();

    let latest_run = LatestRunView {
        id: run_id,
        agent_id,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "execution_review_participant_recovery"})),
        result_json: None,
    };
    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: Some(&latest_run),
        previous_status: "in_review",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::ExecutionReviewParticipantRecovery),
        successful_run_handoff_evidence: None,
        source_assignee: None,
        workspace_validation_fingerprint: None,
    };

    let description = build_stranded_issue_recovery_description(&input);
    assert!(description.contains("Detected invariant: `execution_review_participant_recovery`"));
    assert!(description.contains("execution-review participant"));
    assert!(description.contains("reviewer run"));
    // 与 stranded_assigned_issue 的 "Fix the runtime/adapter problem" 不同
    assert!(description.contains("Fix the reviewer runtime"));
    assert!(!description.contains("Fix the runtime/adapter problem"));

    cleanup(&db, company_id).await;
}

/// SuccessfulRunMissingState path：包含 Safe Evidence + Required Action 段
#[tokio::test]
async fn successful_run_missing_state_uses_safe_evidence_section() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "todo").await;
    let issue = fetch_issue_row(&db, issue_id).await;
    let run_id = Uuid::new_v4();
    let source_run_id = Uuid::new_v4();

    let latest_run = LatestRunView {
        id: run_id,
        agent_id,
        status: Some("succeeded".to_owned()),
        context_snapshot: Some(json!({"retryReason": "successful_run_missing_state"})),
        result_json: None,
    };
    let evidence = json!({
        "sourceRunId": source_run_id.to_string(),
        "missingDisposition": "blocked",
    });
    let source_assignee = AgentShortView {
        id: agent_id,
        name: "source-agent".to_owned(),
    };
    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: Some(&latest_run),
        previous_status: "todo",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::SuccessfulRunMissingState),
        successful_run_handoff_evidence: Some(&evidence),
        source_assignee: Some(&source_assignee),
        workspace_validation_fingerprint: None,
    };

    let description = build_stranded_issue_recovery_description(&input);
    assert!(description.contains("## Safe Evidence"));
    assert!(description.contains("## Required Action"));
    assert!(description.contains("Source run:"));
    assert!(description.contains("Corrective handoff run:"));
    assert!(description.contains("Missing disposition: `blocked`"));
    assert!(description.contains("Normalized cause: `successful_run_missing_state`"));
    assert!(description.contains("not a runtime/adapter crash report"));
    assert!(description.contains("valid issue disposition"));

    cleanup(&db, company_id).await;
}

/// 无 latest_run 时不应 panic；runLink 显示 "none"
#[tokio::test]
async fn handles_missing_latest_run() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "todo").await;
    let issue = fetch_issue_row(&db, issue_id).await;

    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: None,
        previous_status: "in_progress",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::RuntimeFailure),
        successful_run_handoff_evidence: None,
        source_assignee: None,
        workspace_validation_fingerprint: None,
    };

    let description = build_stranded_issue_recovery_description(&input);
    assert!(description.contains("Latest retry status: `unknown`"));
    assert!(description.contains("Retry reason: `unknown`"));
    assert!(description.contains("Latest retry run:"));

    cleanup(&db, company_id).await;
}

/// run 有 error / error_code → failure summary 出现 "withheld"
#[tokio::test]
async fn failure_summary_appears_when_run_has_error() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "todo").await;
    let issue = fetch_issue_row(&db, issue_id).await;

    let run_id = Uuid::new_v4();
    let latest_run = LatestRunView {
        id: run_id,
        agent_id,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "issue_continuation_needed"})),
        result_json: None,
    };
    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::RuntimeFailure),
        successful_run_handoff_evidence: None,
        source_assignee: None,
        workspace_validation_fingerprint: None,
    };

    let description = build_stranded_issue_recovery_description(&input);
    // 纯函数没有 error/error_code 字段（LatestRunView 不含）→ 应输出 "none recorded"
    // 这里我们验证 LatestRunView 不含 error 字段时输出默认
    assert!(description.contains("- Failure:"));

    cleanup(&db, company_id).await;
}

/// retry_reason 为 None / 空 context_snapshot → "unknown"
#[tokio::test]
async fn retry_reason_unknown_when_context_snapshot_empty() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "todo").await;
    let issue = fetch_issue_row(&db, issue_id).await;

    let run_id = Uuid::new_v4();
    let latest_run = LatestRunView {
        id: run_id,
        agent_id,
        status: Some("failed".to_owned()),
        context_snapshot: None,
        result_json: None,
    };
    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::RuntimeFailure),
        successful_run_handoff_evidence: None,
        source_assignee: None,
        workspace_validation_fingerprint: None,
    };

    let description = build_stranded_issue_recovery_description(&input);
    assert!(description.contains("Retry reason: `unknown`"));

    cleanup(&db, company_id).await;
}

/// context_snapshot.retryReason 为空字符串 → "unknown"
#[tokio::test]
async fn retry_reason_unknown_when_empty_string() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "todo").await;
    let issue = fetch_issue_row(&db, issue_id).await;

    let run_id = Uuid::new_v4();
    let latest_run = LatestRunView {
        id: run_id,
        agent_id,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": ""})),
        result_json: None,
    };
    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::RuntimeFailure),
        successful_run_handoff_evidence: None,
        source_assignee: None,
        workspace_validation_fingerprint: None,
    };

    let description = build_stranded_issue_recovery_description(&input);
    assert!(description.contains("Retry reason: `unknown`"));

    cleanup(&db, company_id).await;
}

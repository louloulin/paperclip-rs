//! Round 357：`workspace_validation_failed` cause 的 description 注入
//! workspace validation fingerprint，让人工 readable。
//!
//! 业务语义（与 Node `services/recovery/service.ts` 对齐）：
//! - `cause == WorkspaceValidationFailed` 时，description 的 `## Source` 段
//!   必须额外包含 `- Workspace validation fingerprint: \`<value>\`` 行
//! - fingerprint 的来源优先级：
//!   1. caller 显式传入的 `workspace_validation_fingerprint` 字段（override）
//!   2. `latest_run.result_json.workspaceValidation.fingerprint`
//!   3. 都没有 → 输出 `- Workspace validation fingerprint: \`none reported\``（兜底）
//! - 非 WorkspaceValidationFailed cause → 不出现 fingerprint 行（避免噪音）

use pc_heartbeat::recovery::build_stranded_issue_recovery_description::{
    build_stranded_issue_recovery_description,
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
        .bind(format!("r357-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r357-agent', 'general', 'process', 'active')",
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
    .bind(format!("r357-issue-{id}"))
    .bind(status)
    .bind(format!("r357-fp-{id}"))
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

/// workspace_validation_failed + caller override fingerprint → 出现 fingerprint 行
#[tokio::test]
async fn workspace_validation_failed_emits_fingerprint_from_override() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "blocked").await;
    let issue = fetch_issue_row(&db, issue_id).await;
    let run_id = Uuid::new_v4();

    let latest_run = LatestRunView {
        id: run_id,
        agent_id,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "workspace_validation_failed"})),
        result_json: None,
    };
    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::WorkspaceValidationFailed),
        successful_run_handoff_evidence: None,
        source_assignee: None,
        workspace_validation_fingerprint: Some("branch:main"),
    };

    let description = build_stranded_issue_recovery_description(&input);
    assert!(
        description.contains("Workspace validation fingerprint: `branch:main`"),
        "description should embed override fingerprint: {description}"
    );
    // 必须仍然保留 Node 风格的 Retry reason 行
    assert!(description.contains("Retry reason: `workspace_validation_failed`"));
    // 必须仍然保留 ## Source / ## Required Action 段
    assert!(description.contains("## Source"));
    assert!(description.contains("## Required Action"));

    cleanup(&db, company_id).await;
}

/// workspace_validation_failed + 没有 override → 从 latest_run.result_json 读
#[tokio::test]
async fn workspace_validation_failed_emits_fingerprint_from_result_json() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "blocked").await;
    let issue = fetch_issue_row(&db, issue_id).await;
    let run_id = Uuid::new_v4();

    let result_json = json!({
        "workspaceValidation": {
            "reason": "git_worktree_branch_incoherence",
            "fingerprint": "branch:feature/r357"
        }
    });
    let latest_run = LatestRunView {
        id: run_id,
        agent_id,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "workspace_validation_failed"})),
        result_json: Some(result_json),
    };
    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::WorkspaceValidationFailed),
        successful_run_handoff_evidence: None,
        source_assignee: None,
        workspace_validation_fingerprint: None,
    };

    let description = build_stranded_issue_recovery_description(&input);
    assert!(
        description.contains("Workspace validation fingerprint: `branch:feature/r357`"),
        "description should embed result_json fingerprint: {description}"
    );

    cleanup(&db, company_id).await;
}

/// caller override 优先级高于 result_json
#[tokio::test]
async fn workspace_validation_fingerprint_override_wins_over_result_json() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "blocked").await;
    let issue = fetch_issue_row(&db, issue_id).await;
    let run_id = Uuid::new_v4();

    let result_json = json!({
        "workspaceValidation": {
            "reason": "git_worktree_branch_incoherence",
            "fingerprint": "from-result-json"
        }
    });
    let latest_run = LatestRunView {
        id: run_id,
        agent_id,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "workspace_validation_failed"})),
        result_json: Some(result_json),
    };
    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::WorkspaceValidationFailed),
        successful_run_handoff_evidence: None,
        source_assignee: None,
        workspace_validation_fingerprint: Some("from-override"),
    };

    let description = build_stranded_issue_recovery_description(&input);
    assert!(description.contains("Workspace validation fingerprint: `from-override`"));
    assert!(!description.contains("from-result-json"));

    cleanup(&db, company_id).await;
}

/// workspace_validation_failed + 没有 fingerprint 源 → fallback "none reported"
#[tokio::test]
async fn workspace_validation_failed_falls_back_to_none_reported() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "blocked").await;
    let issue = fetch_issue_row(&db, issue_id).await;
    let run_id = Uuid::new_v4();

    let latest_run = LatestRunView {
        id: run_id,
        agent_id,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "workspace_validation_failed"})),
        result_json: None,
    };
    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::WorkspaceValidationFailed),
        successful_run_handoff_evidence: None,
        source_assignee: None,
        workspace_validation_fingerprint: None,
    };

    let description = build_stranded_issue_recovery_description(&input);
    assert!(
        description.contains("Workspace validation fingerprint: `none reported`"),
        "description should fall back to none reported: {description}"
    );

    cleanup(&db, company_id).await;
}

/// 非 WorkspaceValidationFailed cause → 不应出现 fingerprint 行
#[tokio::test]
async fn non_workspace_validation_cause_omits_fingerprint_line() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, "in_progress").await;
    let issue = fetch_issue_row(&db, issue_id).await;
    let run_id = Uuid::new_v4();

    let result_json = json!({
        "workspaceValidation": {
            "reason": "git_worktree_branch_incoherence",
            "fingerprint": "should-be-ignored"
        }
    });
    let latest_run = LatestRunView {
        id: run_id,
        agent_id,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "issue_continuation_needed"})),
        result_json: Some(result_json),
    };
    let input = BuildStrandedIssueRecoveryDescriptionInput {
        issue: &issue,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        prefix: "PAP",
        recovery_cause: Some(StrandedRecoveryCause::RuntimeFailure),
        successful_run_handoff_evidence: None,
        source_assignee: None,
        workspace_validation_fingerprint: Some("ignored"),
    };

    let description = build_stranded_issue_recovery_description(&input);
    assert!(
        !description.contains("Workspace validation fingerprint"),
        "non-workspace_validation cause must not show fingerprint: {description}"
    );

    cleanup(&db, company_id).await;
}

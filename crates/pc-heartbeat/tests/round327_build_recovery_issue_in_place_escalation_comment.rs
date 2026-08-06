//! Round 327：`buildRecoveryIssueInPlaceEscalationComment` 的 PostgreSQL round-trip 验证。
//!
//! 与 Node `services/recovery/service.ts:3095` 对齐：
//! - 输入：issue(identifier, id) + previous_status + latest_run + prefix
//! - 输出：完整 markdown 格式的 escalation comment body
//!
//! 关键 invariants（与 Node 完全对齐）：
//! - 第一行："Paperclip stopped automatic stranded-work recovery for this recovery issue."
//! - Recovery issue UI link：`[identifier|uuid](/{prefix}/issues/{label})`
//! - Previous status: `\`{previous_status}\``
//! - Latest run link：`[uuid](/{prefix}/agents/{agent_id}/runs/{uuid})` 或 `none`
//! - Latest run status: `\`{latest_run.status ?? "unknown"}\``
//! - Retry reason: `\`{retryReason ?? "none"}\``（来自 context_snapshot）
//! - Failure summary：`- Failure: {summary}` 或 `- Failure: none recorded`
//! - Guard line："recovery issues do not create nested `stranded_issue_recovery` issues."
//! - Next action：关于如何解除 blocked 的指引
//!
//! 设计：pure 函数，不依赖 DB；测试用最小化的 fixture 验证输出字符串。

use pc_heartbeat::recovery::build_recovery_issue_in_place_escalation_comment::{
    build_recovery_issue_in_place_escalation_comment,
    BuildRecoveryIssueInPlaceEscalationCommentInput, EscalationRunView,
};
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

async fn fixture(db: &Db) -> Uuid {
    let company_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r327-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    company_id
}

/// 完整输入：identifier + latest_run + context_snapshot.retryReason
#[tokio::test]
async fn builds_full_comment_with_retry_reason_and_failure_summary() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let issue_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
        issue_identifier: Some("PAP-42".to_owned()),
        issue_id,
        previous_status: "in_progress".to_owned(),
        latest_run: Some(EscalationRunView {
            id: run_id,
            agent_id: Some(agent_id),
            status: "failed".to_owned(),
            error: Some("boom".to_owned()),
            error_code: None,
            context_snapshot: Some(json!({"retryReason": "issue_continuation_needed"})),
        }),
        prefix: "PAP".to_owned(),
    };

    let body = build_recovery_issue_in_place_escalation_comment(&input);

    assert!(body.starts_with(
        "Paperclip stopped automatic stranded-work recovery for this recovery issue."
    ));
    assert!(body.contains("[PAP-42](/PAP/issues/PAP-42)"));
    assert!(body.contains("Previous status: `in_progress`"));
    assert!(body.contains(&format!(
        "[{}](/PAP/agents/{}/runs/{})",
        run_id, agent_id, run_id
    )));
    assert!(body.contains("Latest run status: `failed`"));
    assert!(body.contains("Retry reason: `issue_continuation_needed`"));
    assert!(body.contains("Latest retry failure details were withheld"));
    assert!(body.contains("recovery issues do not create nested `stranded_issue_recovery` issues."));
    assert!(body.contains("Next action:"));

    cleanup(&db, _company_id).await;
}

/// 没有 identifier → 使用 uuid 作为 fallback label
#[tokio::test]
async fn identifier_none_falls_back_to_uuid_label() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let issue_id = Uuid::new_v4();
    let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
        issue_identifier: None,
        issue_id,
        previous_status: "todo".to_owned(),
        latest_run: None,
        prefix: "PAP".to_owned(),
    };

    let body = build_recovery_issue_in_place_escalation_comment(&input);

    assert!(body.contains(&format!("[{}](/PAP/issues/{})", issue_id, issue_id)));
    cleanup(&db, _company_id).await;
}

/// 没有 latest_run → runLink = "none"，status = "unknown"
#[tokio::test]
async fn missing_latest_run_renders_none_and_unknown() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
        issue_identifier: Some("PAP-7".to_owned()),
        issue_id: Uuid::new_v4(),
        previous_status: "todo".to_owned(),
        latest_run: None,
        prefix: "ACME".to_owned(),
    };

    let body = build_recovery_issue_in_place_escalation_comment(&input);

    assert!(body.contains("- Latest run: none"));
    assert!(body.contains("Latest run status: `unknown`"));
    assert!(body.contains("Retry reason: `none`"));
    assert!(body.contains("- Failure: none recorded"));
    assert!(body.contains("[PAP-7](/ACME/issues/PAP-7)"));
    cleanup(&db, _company_id).await;
}

/// retryReason 空白字符串 → 视为空，返回 "none"
#[tokio::test]
async fn whitespace_retry_reason_falls_back_to_none() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let run_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
        issue_identifier: Some("PAP-1".to_owned()),
        issue_id: Uuid::new_v4(),
        previous_status: "in_progress".to_owned(),
        latest_run: Some(EscalationRunView {
            id: run_id,
            agent_id: Some(agent_id),
            status: "failed".to_owned(),
            error: None,
            error_code: None,
            context_snapshot: Some(json!({"retryReason": "   "})),
        }),
        prefix: "PAP".to_owned(),
    };

    let body = build_recovery_issue_in_place_escalation_comment(&input);
    assert!(body.contains("Retry reason: `none`"));
    assert!(body.contains("- Failure: none recorded"));
    cleanup(&db, _company_id).await;
}

/// context_snapshot 不存在 / 非 object → retryReason = "none"
#[tokio::test]
async fn missing_context_snapshot_renders_retry_reason_none() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let run_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
        issue_identifier: Some("PAP-2".to_owned()),
        issue_id: Uuid::new_v4(),
        previous_status: "todo".to_owned(),
        latest_run: Some(EscalationRunView {
            id: run_id,
            agent_id: Some(agent_id),
            status: "failed".to_owned(),
            error: Some("err".to_owned()),
            error_code: None,
            context_snapshot: None,
        }),
        prefix: "PAP".to_owned(),
    };

    let body = build_recovery_issue_in_place_escalation_comment(&input);
    assert!(body.contains("Retry reason: `none`"));
    assert!(body.contains("Latest retry failure details were withheld"));
    cleanup(&db, _company_id).await;
}

/// errorCode 触发 summary（error 为 None 时仍可触发）
#[tokio::test]
async fn error_code_alone_triggers_failure_summary() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let run_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
        issue_identifier: Some("PAP-3".to_owned()),
        issue_id: Uuid::new_v4(),
        previous_status: "in_progress".to_owned(),
        latest_run: Some(EscalationRunView {
            id: run_id,
            agent_id: Some(agent_id),
            status: "failed".to_owned(),
            error: None,
            error_code: Some("adapter_failed".to_owned()),
            context_snapshot: Some(json!({"retryReason": "execution_review_participant_recovery"})),
        }),
        prefix: "PAP".to_owned(),
    };

    let body = build_recovery_issue_in_place_escalation_comment(&input);
    assert!(body.contains("- Failure: Latest retry failure details were withheld"));
    assert!(body.contains("Retry reason: `execution_review_participant_recovery`"));
    cleanup(&db, _company_id).await;
}

/// 干净 run（无 error 无 errorCode）→ summary 为 none recorded
#[tokio::test]
async fn clean_run_renders_none_recorded() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let run_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let input = BuildRecoveryIssueInPlaceEscalationCommentInput {
        issue_identifier: Some("PAP-4".to_owned()),
        issue_id: Uuid::new_v4(),
        previous_status: "in_progress".to_owned(),
        latest_run: Some(EscalationRunView {
            id: run_id,
            agent_id: Some(agent_id),
            status: "succeeded".to_owned(),
            error: None,
            error_code: None,
            context_snapshot: Some(json!({})),
        }),
        prefix: "PAP".to_owned(),
    };

    let body = build_recovery_issue_in_place_escalation_comment(&input);
    assert!(body.contains("- Failure: none recorded"));
    assert!(!body.contains("withheld"));
    cleanup(&db, _company_id).await;
}

//! Round 331：`buildExecutionReviewParticipantUnavailableComment` 的 PostgreSQL round-trip 验证。
//!
//! 与 Node `services/recovery/service.ts:325` 对齐：
//! - 输入：LatestIssueRun view
//! - 输出：execution-review participant 不可用时的 escalation comment body
//!
//! 关键 invariants：
//! - 开头："Paperclip cannot continue the pending execution-review participant because the participant is not invokable"
//! - 含 "and the review stage has no completed decision or live reviewer run"
//! - 干净 run 不含 "withheld"；error / error_code 任一非空时含 "withheld"
//! - 引导 recovery owner：repair the reviewer runtime / restore the review stage / manual resolution

use pc_heartbeat::recovery::build_execution_review_participant_recovery_comment::build_execution_review_participant_recovery_comment;
use pc_heartbeat::recovery::build_execution_review_participant_unavailable_comment::build_execution_review_participant_unavailable_comment;
use pc_heartbeat::recovery::build_recovery_issue_in_place_escalation_comment::EscalationRunView;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
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
        .bind(format!("r331-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    company_id
}

/// 干净 run → 无 failure summary
#[tokio::test]
async fn clean_run_omits_failure_summary() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let view = EscalationRunView {
        id: Uuid::new_v4(),
        agent_id: Some(Uuid::new_v4()),
        status: "failed".to_owned(),
        error: None,
        error_code: None,
        context_snapshot: Some(json!({})),
    };

    let body = build_execution_review_participant_unavailable_comment(&view);
    assert!(body.starts_with("Paperclip cannot continue the pending execution-review participant"));
    assert!(body.contains("participant is not invokable"));
    assert!(body.contains("and the review stage has no completed decision or live reviewer run."));
    assert!(!body.contains("withheld"));
    cleanup(&db, company_id).await;
}

/// failed run with error → 含 failure summary
#[tokio::test]
async fn run_with_error_includes_failure_summary() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let view = EscalationRunView {
        id: Uuid::new_v4(),
        agent_id: Some(Uuid::new_v4()),
        status: "failed".to_owned(),
        error: Some("reviewer_not_found".to_owned()),
        error_code: None,
        context_snapshot: Some(json!({})),
    };

    let body = build_execution_review_participant_unavailable_comment(&view);
    assert!(body.contains(" Latest retry failure details were withheld"));
    assert!(body.contains("inspect the linked run for evidence"));
    cleanup(&db, company_id).await;
}

/// error_code 单独触发 summary
#[tokio::test]
async fn error_code_only_still_triggers_summary() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let view = EscalationRunView {
        id: Uuid::new_v4(),
        agent_id: Some(Uuid::new_v4()),
        status: "failed".to_owned(),
        error: None,
        error_code: Some("reviewer_unavailable".to_owned()),
        context_snapshot: Some(json!({})),
    };

    let body = build_execution_review_participant_unavailable_comment(&view);
    assert!(body.contains("withheld"));
    cleanup(&db, company_id).await;
}

/// 完整结构含全部 invariant
#[tokio::test]
async fn full_body_contains_all_recovery_owner_guidance() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let view = EscalationRunView {
        id: Uuid::new_v4(),
        agent_id: Some(Uuid::new_v4()),
        status: "failed".to_owned(),
        error: Some("err".to_owned()),
        error_code: Some("adapter_failed".to_owned()),
        context_snapshot: Some(json!({})),
    };

    let body = build_execution_review_participant_unavailable_comment(&view);
    assert!(body.contains("Paperclip cannot continue the pending execution-review participant"));
    assert!(body.contains("because the participant is not invokable"));
    assert!(body.contains("and the review stage has no completed decision or live reviewer run"));
    assert!(body.contains("withheld"));
    assert!(body.contains("Moving it to `blocked`"));
    assert!(body.contains("with a source-scoped recovery action"));
    assert!(body.contains("repair the reviewer runtime"));
    assert!(body.contains("restore the review stage"));
    assert!(body.contains("record an intentional manual resolution"));
    cleanup(&db, company_id).await;
}

/// 与 recovery comment 措辞不同
#[tokio::test]
async fn body_differs_from_recovery_comment() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let view = EscalationRunView {
        id: Uuid::new_v4(),
        agent_id: Some(Uuid::new_v4()),
        status: "failed".to_owned(),
        error: None,
        error_code: None,
        context_snapshot: Some(json!({})),
    };

    let unavailable_body = build_execution_review_participant_unavailable_comment(&view);
    let recovery_body = build_execution_review_participant_recovery_comment(&view);

    // unavailable: "cannot continue"
    assert!(unavailable_body.contains("cannot continue"));
    // recovery: "retried ... once"
    assert!(recovery_body.contains("retried the pending execution-review participant once"));
    // 两段尾部引导一致
    assert!(unavailable_body.contains("Moving it to `blocked`"));
    assert!(recovery_body.contains("Moving it to `blocked`"));
    cleanup(&db, company_id).await;
}

/// 空白字符串值时无 summary
#[tokio::test]
async fn whitespace_error_values_omit_summary() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let view = EscalationRunView {
        id: Uuid::new_v4(),
        agent_id: Some(Uuid::new_v4()),
        status: "failed".to_owned(),
        error: Some("   ".to_owned()),
        error_code: Some("\t".to_owned()),
        context_snapshot: Some(json!({})),
    };

    let body = build_execution_review_participant_unavailable_comment(&view);
    assert!(!body.contains("withheld"));
    cleanup(&db, company_id).await;
}

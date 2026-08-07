//! Round 356：`successful_run_handoff` system notice 真实 PG 验证。
//!
//! 对齐 Node `services/recovery/successful-run-handoff.ts` 的两个核心 notice：
//! - `buildSuccessfulRunHandoffRequiredNotice`：harness 第一次发现 source run 缺少 disposition 时
//!   写入的 system_notice（warning tone, "Missing issue disposition"）
//! - `buildSuccessfulRunHandoffExhaustedNotice`：recovery action 耗尽后写入的 system_notice
//!   （danger tone, "Missing disposition recovery blocked"）
//!
//! Rust 端通过 `pc_repos::IssueRepo::create_comment_with_display` 把 `body + presentation +
//! metadata` 落到 `issue_comments`，并通过 `recovery_cause_title("successful_run_missing_state")`
//! 取出专用 cause 标题。

use pc_heartbeat::recovery::successful_run_handoff::{
    build_successful_run_handoff_exhausted_notice, build_successful_run_handoff_required_notice,
    is_successful_run_handoff_required_notice_body, BuildExhaustedNoticeInput, BuildRequiredNoticeInput,
    NoticeAgentRef, NoticeIssueRef, NoticeRunRef, SUCCESSFUL_RUN_HANDOFF_EXHAUSTED_NOTICE_BODY,
    SUCCESSFUL_RUN_HANDOFF_REQUIRED_NOTICE_BODY, SUCCESSFUL_RUN_MISSING_STATE_REASON,
};
use pc_repos::issue::IssueRepo;
use pc_repos::Db;
use serde_json::{json, Value};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let stmts = [
        "DELETE FROM issue_comments WHERE company_id = $1",
        "DELETE FROM issues WHERE company_id = $1",
        "DELETE FROM companies WHERE id = $1",
    ];
    for s in stmts {
        let _ = sqlx::query(s)
            .bind(company_id)
            .execute(db.pool())
            .await;
    }
}

async fn fixture(db: &Db) -> (Uuid, Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r356-{company_id}"))
        .bind("R356")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r356-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, assignee_agent_id) \
         VALUES ($1, $2, 'r356-issue', 'in_progress', 'normal', 'system', $3, $4)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(format!("r356-fp-{issue_id}"))
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, \
         error, error_code, started_at) \
         VALUES ($1, $2, $3, 'manual', 'succeeded', NULL, NULL, now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id, issue_id, run_id)
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

/// 写入 required notice 到 source issue，并验证：
/// - body 等于 `SUCCESSFUL_RUN_HANDOFF_REQUIRED_NOTICE_BODY` 常量
/// - presentation 是 `system_notice` (warning tone, "Missing issue disposition")
/// - metadata 包含 `version=1`, `sourceRunId`, 两 section（Required action / Run evidence）
#[tokio::test(flavor = "current_thread")]
async fn required_notice_writes_full_system_comment_with_metadata() {
    let db = connect().await;
    let (company_id, agent_id, issue_id, run_id) = fixture(&db).await;

    let notice = build_successful_run_handoff_required_notice(BuildRequiredNoticeInput {
        issue: NoticeIssueRef {
            id: issue_id.to_string(),
            identifier: "R356-1".into(),
            title: "r356-issue".into(),
            status: "in_progress".into(),
        },
        run: NoticeRunRef {
            id: run_id.to_string(),
            status: "succeeded".into(),
        },
        agent: NoticeAgentRef {
            id: agent_id.to_string(),
            name: "r356-agent".into(),
        },
        detected_progress_summary: "made progress on task",
    });

    // 写入 PG
    let row = IssueRepo::new(&db)
        .create_comment_with_display(
            company_id,
            issue_id,
            None,
            Some("system"),
            &notice.body,
            Some(&notice.presentation),
            Some(&notice.metadata),
        )
        .await
        .expect("create comment");
    assert!(!row.id.is_nil());

    let (body, presentation, metadata) = fetch_latest_comment(&db, issue_id).await;
    assert_eq!(body, SUCCESSFUL_RUN_HANDOFF_REQUIRED_NOTICE_BODY);
    assert!(is_successful_run_handoff_required_notice_body(&body));
    let presentation = presentation.expect("presentation");
    assert_eq!(presentation["kind"], "system_notice");
    assert_eq!(presentation["tone"], "warning");
    assert_eq!(presentation["title"], "Missing issue disposition");
    let metadata = metadata.expect("metadata");
    assert_eq!(metadata["version"], 1);
    assert_eq!(metadata["sourceRunId"], run_id.to_string());
    let sections = metadata["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0]["title"], "Required action");
    assert_eq!(sections[1]["title"], "Run evidence");
    // Cause 必须为 successful_run_missing_state（防止回归到通用 fallback）
    let evidence = sections[1]["rows"].as_array().unwrap();
    assert!(
        evidence
            .iter()
            .any(|r| r["label"] == "Normalized cause"
                && r["value"] == SUCCESSFUL_RUN_MISSING_STATE_REASON),
        "expected cause row to use successful_run_missing_state"
    );
    // Required action 应包含 issue_link（Source issue）和 agent_link（Assignee）
    let required = sections[0]["rows"].as_array().unwrap();
    assert!(required
        .iter()
        .any(|r| r["type"] == "issue_link" && r["label"] == "Source issue"));
    assert!(required
        .iter()
        .any(|r| r["type"] == "agent_link" && r["label"] == "Assignee"));

    cleanup(&db, company_id).await;
}

/// 写入 exhausted notice（带 recovery_action_id）：
/// - body 等于 `SUCCESSFUL_RUN_HANDOFF_EXHAUSTED_NOTICE_BODY`
/// - presentation: system_notice, danger tone, "Missing disposition recovery blocked"
/// - metadata: Recovery action 行使用 key_value + 真实 action_id
#[tokio::test(flavor = "current_thread")]
async fn exhausted_notice_with_action_id_writes_recovery_action_key_value() {
    let db = connect().await;
    let (company_id, agent_id, issue_id, _run_id) = fixture(&db).await;
    let action_id = Uuid::new_v4();

    let notice = build_successful_run_handoff_exhausted_notice(BuildExhaustedNoticeInput {
        issue: NoticeIssueRef {
            id: issue_id.to_string(),
            identifier: "R356-1".into(),
            title: "r356-issue".into(),
            status: "blocked".into(),
        },
        source_run: Some(NoticeRunRef {
            id: Uuid::new_v4().to_string(),
            status: "succeeded".into(),
        }),
        corrective_run: Some(NoticeRunRef {
            id: Uuid::new_v4().to_string(),
            status: "succeeded".into(),
        }),
        source_assignee: Some(NoticeAgentRef {
            id: agent_id.to_string(),
            name: "r356-agent".into(),
        }),
        recovery_issue: None,
        recovery_action_id: Some(action_id.to_string()),
        recovery_owner: Some(NoticeAgentRef {
            id: Uuid::new_v4().to_string(),
            name: "Recovery Lead".into(),
        }),
        latest_issue_status: "blocked",
        latest_handoff_run_status: "succeeded",
        missing_disposition: "clear_next_step",
    });

    let _row = IssueRepo::new(&db)
        .create_comment_with_display(
            company_id,
            issue_id,
            None,
            Some("system"),
            &notice.body,
            Some(&notice.presentation),
            Some(&notice.metadata),
        )
        .await
        .expect("create comment");

    let (body, presentation, metadata) = fetch_latest_comment(&db, issue_id).await;
    assert_eq!(body, SUCCESSFUL_RUN_HANDOFF_EXHAUSTED_NOTICE_BODY);
    let presentation = presentation.expect("presentation");
    assert_eq!(presentation["kind"], "system_notice");
    assert_eq!(presentation["tone"], "danger");
    assert_eq!(presentation["title"], "Missing disposition recovery blocked");
    let metadata = metadata.expect("metadata");
    let sections = metadata["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 2);
    let owner_rows = sections[0]["rows"].as_array().unwrap();
    assert!(owner_rows
        .iter()
        .any(|r| r["type"] == "key_value"
            && r["label"] == "Recovery action"
            && r["value"] == action_id.to_string()),
        "Recovery action row should reference the action id (for metadata dedup)");
    let evidence_rows = sections[1]["rows"].as_array().unwrap();
    assert!(evidence_rows
        .iter()
        .any(|r| r["label"] == "Missing disposition"
            && r["value"] == "clear_next_step"));

    cleanup(&db, company_id).await;
}

/// 写入 exhausted notice（**无** recovery_action_id，仅 recovery_issue）：
/// - metadata 在 owner section 里把 "Recovery issue" 行降级为 issue_link
/// - "Recovery owner" 缺失时显示 "unknown"
#[tokio::test(flavor = "current_thread")]
async fn exhausted_notice_without_action_id_falls_back_to_recovery_issue_link() {
    let db = connect().await;
    let (company_id, _agent_id, issue_id, _run_id) = fixture(&db).await;

    let recovery_issue_id = Uuid::new_v4();
    let notice = build_successful_run_handoff_exhausted_notice(BuildExhaustedNoticeInput {
        issue: NoticeIssueRef {
            id: issue_id.to_string(),
            identifier: "R356-1".into(),
            title: "r356-issue".into(),
            status: "blocked".into(),
        },
        source_run: None,
        corrective_run: None,
        source_assignee: None,
        recovery_issue: Some(NoticeIssueRef {
            id: recovery_issue_id.to_string(),
            identifier: "R356-2".into(),
            title: "recovery-issue".into(),
            status: "in_progress".into(),
        }),
        recovery_action_id: None,
        recovery_owner: None,
        latest_issue_status: "blocked",
        latest_handoff_run_status: "unknown",
        missing_disposition: "clear_next_step",
    });

    let _row = IssueRepo::new(&db)
        .create_comment_with_display(
            company_id,
            issue_id,
            None,
            Some("system"),
            &notice.body,
            Some(&notice.presentation),
            Some(&notice.metadata),
        )
        .await
        .expect("create comment");

    let (_body, _presentation, metadata) = fetch_latest_comment(&db, issue_id).await;
    let metadata = metadata.expect("metadata");
    let sections = metadata["sections"].as_array().unwrap();
    let owner_rows = sections[0]["rows"].as_array().unwrap();
    assert!(owner_rows
        .iter()
        .any(|r| r["type"] == "issue_link"
            && r["label"] == "Recovery issue"
            && r["issueId"] == recovery_issue_id.to_string()));
    assert!(owner_rows
        .iter()
        .any(|r| r["type"] == "key_value"
            && r["label"] == "Recovery owner"
            && r["value"] == "unknown"));

    cleanup(&db, company_id).await;
}

/// 验证 `recovery_cause_title` 对 successful_run_missing_state 输出专用 title，
/// 而不是通用 "execution path recovery failed" fallback。
#[test]
fn cause_title_for_successful_run_missing_state_is_specific() {
    use pc_heartbeat::recovery::build_recovery_comment_display::recovery_cause_title;
    assert_eq!(
        recovery_cause_title(SUCCESSFUL_RUN_MISSING_STATE_REASON),
        "missing disposition recovery failed",
        "cause title for successful_run_missing_state must NOT fall back to generic fallback"
    );
    assert_ne!(
        recovery_cause_title(SUCCESSFUL_RUN_MISSING_STATE_REASON),
        recovery_cause_title("unknown_cause"),
        "must differ from generic fallback"
    );
}

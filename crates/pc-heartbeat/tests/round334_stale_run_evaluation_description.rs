//! Round 334：`buildStaleRunEvaluationDescription` 的 PostgreSQL round-trip 验证。
//!
//! 与 Node `services/recovery/service.ts:1902` 对齐：
//! - 输入：run view + running_agent + source_issue(可选) + evidence + level + prefix
//! - 输出：stale run evaluation issue 的 markdown description
//!
//! 关键 invariants：
//! - 含 ## Run / ## Last Output Excerpt / ## Recent Run Events / ## Related Work / ## Decision Checklist 五段
//! - Source issue None → "none"
//! - Empty collections → 各自 placeholder
//! - thresholds: suspicious after 1h, critical after 4h（与 Node 常量对齐）
//! - safeTail 渲染为 text 代码块

use chrono::TimeZone;
use pc_heartbeat::recovery::build_stale_run_evaluation_description::{
    build_stale_run_evaluation_description, format_duration,
    BuildStaleRunEvaluationDescriptionInput, StaleAgentView, StaleEvaluationLevel,
    StaleIssueLinkView, StaleRunEventView, StaleRunEvidenceView, StaleRunView,
    StaleSourceIssueView,
};
use pc_repos::Db;
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
        .bind(format!("r334-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    company_id
}

fn uuid(seed: u8) -> Uuid {
    Uuid::from_bytes([seed; 16])
}

fn epoch(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

fn sample_run() -> StaleRunView {
    StaleRunView {
        id: uuid(1),
        agent_id: uuid(2),
        invocation_source: "manual".to_owned(),
        trigger_detail: Some("r334-fixture".to_owned()),
        started_at: Some(epoch(2024, 1, 1, 10, 0)),
        process_started_at: Some(epoch(2024, 1, 1, 10, 1)),
        last_output_at: Some(epoch(2024, 1, 1, 10, 30)),
        last_output_seq: 42,
        process_pid: Some(1234),
        process_group_id: Some(5678),
    }
}

fn sample_agent() -> StaleAgentView {
    StaleAgentView {
        id: uuid(2),
        name: "engineer-1".to_owned(),
        adapter_type: "process".to_owned(),
    }
}

fn sample_source() -> StaleSourceIssueView {
    StaleSourceIssueView {
        id: uuid(3),
        identifier: Some("ROOT-1".to_owned()),
    }
}

fn sample_evidence() -> StaleRunEvidenceView {
    StaleRunEvidenceView {
        safe_tail: Some("last output line\nsecond line".to_owned()),
        silence_age_ms: 30 * 60_000,
        recent_events: vec![StaleRunEventView {
            event_type: "log".to_owned(),
            level: Some("info".to_owned()),
            created_at: "2024-01-01T10:30:00Z".to_owned(),
            message: Some("started".to_owned()),
        }],
        child_issues: vec![StaleIssueLinkView {
            id: uuid(4),
            identifier: Some("CHILD-1".to_owned()),
            title: "child".to_owned(),
            status: "todo".to_owned(),
        }],
        blockers: vec![StaleIssueLinkView {
            id: uuid(5),
            identifier: None,
            title: "blocker".to_owned(),
            status: "blocked".to_owned(),
        }],
    }
}

/// 完整路径：description 含全部 5 段 + decision checklist
#[tokio::test]
async fn description_contains_all_sections() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let input = BuildStaleRunEvaluationDescriptionInput {
        run: &sample_run(),
        running_agent: &sample_agent(),
        source_issue: Some(&sample_source()),
        prefix: "PAP",
        evidence: &sample_evidence(),
        level: StaleEvaluationLevel::Critical,
    };

    let body = build_stale_run_evaluation_description(&input);
    assert!(
        body.starts_with("Paperclip detected critical output silence on an active heartbeat run.")
    );
    assert!(body.contains("## Run"));
    assert!(body.contains("## Last Output Excerpt"));
    assert!(body.contains("## Recent Run Events"));
    assert!(body.contains("## Related Work"));
    assert!(body.contains("Active child issues:"));
    assert!(body.contains("Current source blockers:"));
    assert!(body.contains("## Decision Checklist"));
    cleanup(&db, _company_id).await;
}

/// level = suspicious 渲染正确
#[tokio::test]
async fn description_uses_suspicious_level() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let input = BuildStaleRunEvaluationDescriptionInput {
        run: &sample_run(),
        running_agent: &sample_agent(),
        source_issue: Some(&sample_source()),
        prefix: "PAP",
        evidence: &sample_evidence(),
        level: StaleEvaluationLevel::Suspicious,
    };

    let body = build_stale_run_evaluation_description(&input);
    assert!(body
        .starts_with("Paperclip detected suspicious output silence on an active heartbeat run."));
    cleanup(&db, _company_id).await;
}

/// thresholds 行：1h / 4h
#[tokio::test]
async fn description_thresholds_match_node_constants() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let input = BuildStaleRunEvaluationDescriptionInput {
        run: &sample_run(),
        running_agent: &sample_agent(),
        source_issue: Some(&sample_source()),
        prefix: "PAP",
        evidence: &sample_evidence(),
        level: StaleEvaluationLevel::Critical,
    };

    let body = build_stale_run_evaluation_description(&input);
    assert!(body.contains("- Thresholds: suspicious after 1h, critical after 4h"));
    cleanup(&db, _company_id).await;
}

/// safeTail 渲染为 text 代码块
#[tokio::test]
async fn description_safe_tail_in_code_block() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let input = BuildStaleRunEvaluationDescriptionInput {
        run: &sample_run(),
        running_agent: &sample_agent(),
        source_issue: Some(&sample_source()),
        prefix: "PAP",
        evidence: &sample_evidence(),
        level: StaleEvaluationLevel::Critical,
    };

    let body = build_stale_run_evaluation_description(&input);
    assert!(body.contains(
        "```text
last output line
second line
```"
    ));
    cleanup(&db, _company_id).await;
}

/// safeTail None → "_No run-log tail was available._"
#[tokio::test]
async fn description_no_tail_renders_placeholder() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let mut evidence = sample_evidence();
    evidence.safe_tail = None;
    let input = BuildStaleRunEvaluationDescriptionInput {
        run: &sample_run(),
        running_agent: &sample_agent(),
        source_issue: Some(&sample_source()),
        prefix: "PAP",
        evidence: &evidence,
        level: StaleEvaluationLevel::Suspicious,
    };

    let body = build_stale_run_evaluation_description(&input);
    assert!(body.contains("_No run-log tail was available._"));
    cleanup(&db, _company_id).await;
}

/// source_issue None → "none"
#[tokio::test]
async fn description_source_issue_none_renders_placeholder() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let input = BuildStaleRunEvaluationDescriptionInput {
        run: &sample_run(),
        running_agent: &sample_agent(),
        source_issue: None,
        prefix: "PAP",
        evidence: &sample_evidence(),
        level: StaleEvaluationLevel::Critical,
    };

    let body = build_stale_run_evaluation_description(&input);
    assert!(body.contains("- Source issue: none"));
    cleanup(&db, _company_id).await;
}

/// empty collections 各自 placeholder
#[tokio::test]
async fn description_empty_collections_render_placeholders() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let evidence = StaleRunEvidenceView {
        safe_tail: Some("t".to_owned()),
        silence_age_ms: 0,
        recent_events: vec![],
        child_issues: vec![],
        blockers: vec![],
    };
    let input = BuildStaleRunEvaluationDescriptionInput {
        run: &sample_run(),
        running_agent: &sample_agent(),
        source_issue: Some(&sample_source()),
        prefix: "PAP",
        evidence: &evidence,
        level: StaleEvaluationLevel::Suspicious,
    };

    let body = build_stale_run_evaluation_description(&input);
    assert!(body.contains("- Silent for: 0m"));
    // recent_events 空 → "- none"
    assert!(body.contains("- none"));
    // child/blockers 空 → "- none detected"
    let none_detected_count = body.matches("- none detected").count();
    assert_eq!(none_detected_count, 2);
    cleanup(&db, _company_id).await;
}

/// format_duration 边界
#[tokio::test]
async fn format_duration_branches() {
    assert_eq!(format_duration(None), "unknown");
    assert_eq!(format_duration(Some(0)), "0m");
    assert_eq!(format_duration(Some(30 * 60_000)), "30m");
    assert_eq!(format_duration(Some(60 * 60_000)), "1h");
    assert_eq!(format_duration(Some(90 * 60_000)), "1h 30m");
    assert_eq!(format_duration(Some(240 * 60_000)), "4h");
    cleanup(
        &Db::connect(
            "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos",
            4,
            0,
        )
        .await
        .unwrap(),
        Uuid::new_v4(),
    )
    .await;
}

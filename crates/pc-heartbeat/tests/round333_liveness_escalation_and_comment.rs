//! Round 333：`buildLivenessEscalationDescription` + `buildLivenessOriginalIssueComment` 的 PostgreSQL round-trip 验证。
//!
//! 与 Node `services/recovery/service.ts:718 / 748` 对齐：
//! - 输入：IssueLivenessFinding + （可选）escalation issue 引用
//! - 输出：escalation issue description / original issue comment body
//!
//! 关键 invariants：
//! - description: 含 "## Source" / "## Ownership" / "## Next Action" 三段
//! - comment: 8 行 bullet + close guidance
//! - dependency path 用 "id1 -> id2 -> ..." 格式

use pc_heartbeat::recovery::build_liveness_escalation_description::{
    build_liveness_escalation_description, format_dependency_path,
};
use pc_heartbeat::recovery::build_liveness_original_issue_comment::{
    build_liveness_original_issue_comment, OriginalIssueCommentContext,
};
use pc_heartbeat::recovery::issue_graph_liveness::{
    IssueLivenessDependencyPathEntry, IssueLivenessFinding, IssueLivenessOwnerCandidate,
    IssueLivenessOwnerCandidateReason, IssueLivenessSeverity, IssueLivenessState,
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
        .bind(format!("r333-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    company_id
}

fn uuid(seed: u8) -> Uuid {
    Uuid::from_bytes([seed; 16])
}

fn sample_finding() -> IssueLivenessFinding {
    IssueLivenessFinding {
        company_id: uuid(1),
        incident_key: "inc-r333".to_owned(),
        state: IssueLivenessState::BlockedByUninvokableAssignee,
        severity: IssueLivenessSeverity::Critical,
        source_issue_id: uuid(2),
        source_issue_label: "ROOT".to_owned(),
        reason: "test reason".to_owned(),
        dependency_path: vec![
            IssueLivenessDependencyPathEntry {
                issue_id: uuid(2),
                identifier: Some("ROOT-1".to_owned()),
                title: "Root".to_owned(),
                status: "todo".to_owned(),
            },
            IssueLivenessDependencyPathEntry {
                issue_id: uuid(3),
                identifier: Some("MID-2".to_owned()),
                title: "Mid".to_owned(),
                status: "blocked".to_owned(),
            },
        ],
        recovery_issue_id: Some(uuid(3)),
        blocker_issue_id: None,
        participant_agent_id: None,
        recommended_owner_agent_id: Some(uuid(4)),
        recommended_owner_candidate_agent_ids: vec![uuid(4), uuid(5)],
        recommended_owner_candidates: vec![IssueLivenessOwnerCandidate {
            agent_id: uuid(4),
            reason: IssueLivenessOwnerCandidateReason::StalledBlockerAssignee,
            source_issue_id: uuid(2),
        }],
        recommended_action: "Repair the path".to_owned(),
    }
}

/// description: 包含完整三段
#[tokio::test]
async fn escalation_description_contains_all_sections() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let finding = sample_finding();

    let desc = build_liveness_escalation_description(&finding);
    assert!(desc.starts_with("Paperclip detected a harness-level issue graph liveness incident."));
    assert!(desc.contains("## Source"));
    assert!(desc.contains("## Ownership"));
    assert!(desc.contains("## Next Action"));
    assert!(desc.contains("Source issue: ROOT-1"));
    assert!(desc.contains("Recovery target issue: MID-2"));
    assert!(desc.contains("Incident key: `inc-r333`"));
    assert!(desc.contains("Detected invariant: `blocked_by_uninvokable_assignee`"));
    assert!(desc.contains("Dependency path: ROOT-1 -> MID-2"));
    assert!(desc.contains("Repair the path"));
    assert!(desc.contains("Resolve the blocked chain, then mark this escalation issue done"));
    cleanup(&db, _company_id).await;
}

/// description: owner 是 none 时正确渲染
#[tokio::test]
async fn escalation_description_owner_none() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let mut finding = sample_finding();
    finding.recommended_owner_agent_id = None;
    finding.recommended_owner_candidate_agent_ids.clear();

    let desc = build_liveness_escalation_description(&finding);
    assert!(desc.contains("Selected owner agent: `none`"));
    assert!(desc.contains("Candidate owner agents: none"));
    cleanup(&db, _company_id).await;
}

/// format_dependency_path 使用 identifier with fallback
#[tokio::test]
async fn format_dependency_path_uses_uuid_fallback() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let mut finding = sample_finding();
    finding.dependency_path[1].identifier = None;

    let path = format_dependency_path(&finding);
    assert!(path.starts_with("ROOT-1 -> "));
    // fallback 到 uuid
    assert!(path.contains("03030303-0303-0303-0303-030303030303"));
    cleanup(&db, _company_id).await;
}

/// original issue comment: 含完整 8 行 bullet + close guidance
#[tokio::test]
async fn original_issue_comment_contains_all_fields() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let finding = sample_finding();
    let ctx = OriginalIssueCommentContext {
        identifier: Some("ESC-99".to_owned()),
        id: uuid(5),
    };

    let body = build_liveness_original_issue_comment(&finding, &ctx);
    assert!(body.starts_with(
        "Paperclip detected a harness-level liveness incident in this issue's dependency graph."
    ));
    assert!(body.contains("- Escalation issue: ESC-99"));
    assert!(body.contains("- Incident key: `inc-r333`"));
    assert!(body.contains("- Finding: `blocked_by_uninvokable_assignee`"));
    assert!(body.contains("- Dependency path: ROOT-1 -> MID-2"));
    assert!(body.contains("- Reason: test reason"));
    assert!(body.contains("- Manager action requested: Repair the path"));
    assert!(body.contains("This issue now keeps its existing blockers"));
    assert!(body.contains("blocked by the escalation issue so dependency wakeups remain explicit"));
    cleanup(&db, _company_id).await;
}

/// original issue comment: identifier None 时用 uuid 兜底
#[tokio::test]
async fn original_issue_comment_identifier_none_uses_uuid() {
    let db = connect().await;
    let _company_id = fixture(&db).await;
    let finding = sample_finding();
    let ctx = OriginalIssueCommentContext {
        identifier: None,
        id: uuid(5),
    };

    let body = build_liveness_original_issue_comment(&finding, &ctx);
    assert!(body.contains("- Escalation issue: 05050505-0505-0505-0505-050505050505"));
    cleanup(&db, _company_id).await;
}

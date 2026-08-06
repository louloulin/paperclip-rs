//! `buildLivenessEscalationDescription` —— Node `services/recovery/service.ts:718`。
//!
//! 业务语义：
//! - 当 `IssueLivenessFinding` 被转换为 escalation issue 时，用此函数生成 escalation issue 的
//!   `description` 字段（markdown 格式）。
//! - 内容包含 Source / Ownership / Next Action 三段，便于人工 / 自动恢复决策。
//!
//! 设计意图：
//! - pure 函数：输入 `&IssueLivenessFinding` + 输出 String
//! - 内部 helper `format_dependency_path` 复用
//! - 与 Node 完全对齐：每行 bullet / section header 完全一致

use crate::recovery::issue_graph_liveness::IssueLivenessFinding;

/// 把 dependency path 渲染为 "id1 -> id2 -> id3" 字符串。
///
/// 与 Node `formatDependencyPath` 1:1 对齐：
/// - 每项取 `entry.identifier ?? entry.issue_id`
/// - 用 " -> " 连接
pub fn format_dependency_path(finding: &IssueLivenessFinding) -> String {
    finding
        .dependency_path
        .iter()
        .map(|entry| {
            entry
                .identifier
                .clone()
                .unwrap_or_else(|| entry.issue_id.to_string())
        })
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// Node `buildLivenessEscalationDescription` 的 Rust 等价。
///
/// 输入：`&IssueLivenessFinding`
/// 输出：markdown 格式的 escalation issue description
pub fn build_liveness_escalation_description(finding: &IssueLivenessFinding) -> String {
    // Source = path 第一个 entry
    let source = finding.dependency_path.first();
    // Recovery = path 中 issueId 等于 recoveryIssueId 的 entry
    let recovery = finding
        .dependency_path
        .iter()
        .find(|entry| Some(entry.issue_id) == finding.recovery_issue_id);
    let selected_owner = finding
        .recommended_owner_agent_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "none".to_owned());
    let candidates = if finding.recommended_owner_candidate_agent_ids.is_empty() {
        "none".to_owned()
    } else {
        finding
            .recommended_owner_candidate_agent_ids
            .iter()
            .map(|id| format!("`{id}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let source_label = source
        .and_then(|s| s.identifier.clone())
        .unwrap_or_else(|| {
            source
                .map(|s| s.issue_id.to_string())
                .unwrap_or_else(|| finding.source_issue_label.clone())
        });
    let recovery_label = recovery
        .and_then(|r| r.identifier.clone())
        .or_else(|| recovery.map(|r| r.issue_id.to_string()))
        .or_else(|| finding.recovery_issue_id.map(|id| id.to_string()))
        .unwrap_or_else(|| "none".to_owned());
    [
        "Paperclip detected a harness-level issue graph liveness incident.",
        "",
        "## Source",
        "",
        &format!("- Source issue: {source_label}"),
        &format!("- Recovery target issue: {recovery_label}"),
        &format!("- Incident key: `{}`", finding.incident_key),
        &format!("- Detected invariant: `{}`", finding.state.as_str()),
        &format!("- Dependency path: {}", format_dependency_path(finding)),
        &format!("- Reason: {}", finding.reason),
        "",
        "## Ownership",
        "",
        &format!("- Selected owner agent: `{selected_owner}`"),
        &format!("- Candidate owner agents: {candidates}"),
        "",
        "## Next Action",
        "",
        &finding.recommended_action,
        "",
        "Resolve the blocked chain, then mark this escalation issue done so the original issue can resume when all blockers are cleared.",
    ]
    .join("
")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::issue_graph_liveness::{
        IssueLivenessDependencyPathEntry, IssueLivenessFinding, IssueLivenessOwnerCandidate,
        IssueLivenessOwnerCandidateReason, IssueLivenessSeverity, IssueLivenessState,
    };
    use uuid::Uuid;

    fn uuid(seed: u8) -> Uuid {
        Uuid::from_bytes([seed; 16])
    }

    fn finding() -> IssueLivenessFinding {
        IssueLivenessFinding {
            company_id: uuid(1),
            incident_key: "inc-1".to_owned(),
            state: IssueLivenessState::BlockedByUnassignedIssue,
            severity: IssueLivenessSeverity::Critical,
            source_issue_id: uuid(2),
            source_issue_label: "ROOT".to_owned(),
            reason: "blocked chain".to_owned(),
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
            recommended_action: "Repair the dependency chain".to_owned(),
        }
    }

    #[test]
    fn format_dependency_path_uses_identifiers_with_fallback() {
        let mut f = finding();
        f.dependency_path[1].identifier = None;
        let formatted = format_dependency_path(&f);
        assert_eq!(formatted, "ROOT-1 -> 03030303-0303-0303-0303-030303030303");
    }

    #[test]
    fn description_includes_source_recovery_incident_key() {
        let f = finding();
        let desc = build_liveness_escalation_description(&f);
        assert!(
            desc.starts_with("Paperclip detected a harness-level issue graph liveness incident.")
        );
        assert!(desc.contains("## Source"));
        assert!(desc.contains("## Ownership"));
        assert!(desc.contains("## Next Action"));
        assert!(desc.contains("Source issue: ROOT-1"));
        assert!(desc.contains("Recovery target issue: MID-2"));
        assert!(desc.contains("Incident key: `inc-1`"));
        assert!(desc.contains("Detected invariant: `blocked_by_unassigned_issue`"));
    }

    #[test]
    fn description_includes_dependency_path_arrow_format() {
        let f = finding();
        let desc = build_liveness_escalation_description(&f);
        assert!(desc.contains("Dependency path: ROOT-1 -> MID-2"));
    }

    #[test]
    fn description_includes_owner_and_candidates() {
        let f = finding();
        let desc = build_liveness_escalation_description(&f);
        assert!(desc.contains("Selected owner agent: `04040404-0404-0404-0404-040404040404`"));
        assert!(desc.contains("Candidate owner agents: `04040404-0404-0404-0404-040404040404`, `05050505-0505-0505-0505-050505050505`"));
    }

    #[test]
    fn description_includes_recommended_action_and_close_guidance() {
        let f = finding();
        let desc = build_liveness_escalation_description(&f);
        assert!(desc.contains("Repair the dependency chain"));
        assert!(desc.contains("Resolve the blocked chain, then mark this escalation issue done"));
    }

    #[test]
    fn owner_none_falls_back_to_none_label() {
        let mut f = finding();
        f.recommended_owner_agent_id = None;
        f.recommended_owner_candidate_agent_ids.clear();
        let desc = build_liveness_escalation_description(&f);
        assert!(desc.contains("Selected owner agent: `none`"));
        assert!(desc.contains("Candidate owner agents: none"));
    }

    #[test]
    fn missing_dependency_path_falls_back_to_source_label() {
        let mut f = finding();
        f.dependency_path.clear();
        // recovery_issue_id 仍存在（独立于 path），所以应 fallback 到 uuid(3)
        let desc = build_liveness_escalation_description(&f);
        assert!(desc.contains("Source issue: ROOT"));
        assert!(desc.contains("Recovery target issue: 03030303-0303-0303-0303-030303030303"));
    }

    #[test]
    fn missing_recovery_issue_id_renders_none() {
        let mut f = finding();
        f.dependency_path.clear();
        f.recovery_issue_id = None;
        let desc = build_liveness_escalation_description(&f);
        assert!(desc.contains("Recovery target issue: none"));
    }

    #[test]
    fn recovery_falls_back_to_uuid_when_no_match() {
        let mut f = finding();
        // recoveryIssueId 不在 dependency_path
        f.dependency_path[1].issue_id = uuid(99);
        let desc = build_liveness_escalation_description(&f);
        // 应 fallback 到 f.recovery_issue_id (uuid(3))
        assert!(desc.contains("Recovery target issue: 03030303-0303-0303-0303-030303030303"));
    }
}

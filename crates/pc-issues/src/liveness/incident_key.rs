//! Incident key 构造与解析（对应 Node `services/recovery/origins.ts` 中的
//! `buildIssueGraphLivenessIncidentKey` / `parseIssueGraphLivenessIncidentKey`）。
//!
//! Key 格式：`<prefix>:<companyId>:<issueId>:<state>:<leafIssueId | "none">`
//! 其中 leafIssueId = blockerIssueId ?? participantAgentId ?? "none"

use uuid::Uuid;

/// Recovery key 前缀（与 Node `RECOVERY_KEY_PREFIXES.issueGraphLivenessIncident` 对齐）。
pub const ISSUE_GRAPH_LIVENESS_INCIDENT_PREFIX: &str = "harness_liveness";

/// 构造 liveness incident key。
pub fn build_issue_graph_liveness_incident_key(input: IncidentKeyInput<'_>) -> String {
    let leaf = input
        .blocker_issue_id
        .map(|id| id.to_string())
        .or_else(|| input.participant_agent_id.map(|id| id.to_string()))
        .unwrap_or_else(|| "none".to_string());

    format!(
        "{}:{}:{}:{}:{}",
        ISSUE_GRAPH_LIVENESS_INCIDENT_PREFIX, input.company_id, input.issue_id, input.state, leaf
    )
}

/// 解析 liveness incident key。
///
/// 返回 `Some((companyId, issueId, state, leafIssueId))` 当格式正确；
/// 否则 `None`。
pub fn parse_issue_graph_liveness_incident_key(incident_key: &str) -> Option<ParsedIncidentKey> {
    let parts: Vec<&str> = incident_key.split(':').collect();
    if parts.len() != 5 || parts[0] != ISSUE_GRAPH_LIVENESS_INCIDENT_PREFIX {
        return None;
    }
    let company_id = Uuid::parse_str(parts[1]).ok()?;
    let issue_id = Uuid::parse_str(parts[2]).ok()?;
    let state = parts[3].to_string();
    let leaf = parts[4];
    if state.is_empty() || leaf.is_empty() {
        return None;
    }
    let leaf_issue_id = if leaf == "none" {
        None
    } else {
        Some(Uuid::parse_str(leaf).ok()?)
    };
    Some(ParsedIncidentKey {
        company_id,
        issue_id,
        state,
        leaf_issue_id,
    })
}

#[derive(Debug, Clone)]
pub struct IncidentKeyInput<'a> {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub state: &'a str,
    pub blocker_issue_id: Option<Uuid>,
    pub participant_agent_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedIncidentKey {
    pub company_id: Uuid,
    pub issue_id: Uuid,
    pub state: String,
    pub leaf_issue_id: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// blocker_issue_id 优先于 participant_agent_id。
    #[test]
    fn r758_incident_key_blocker_priority() {
        let company_id = Uuid::new_v4();
        let issue_id = Uuid::new_v4();
        let blocker = Uuid::new_v4();
        let participant = Uuid::new_v4();
        let input = IncidentKeyInput {
            company_id,
            issue_id,
            state: "blocked",
            blocker_issue_id: Some(blocker),
            participant_agent_id: Some(participant),
        };
        let key = build_issue_graph_liveness_incident_key(input);
        let parsed = parse_issue_graph_liveness_incident_key(&key).expect("parse ok");
        assert_eq!(parsed.company_id, company_id);
        assert_eq!(parsed.issue_id, issue_id);
        assert_eq!(parsed.state, "blocked");
        assert_eq!(parsed.leaf_issue_id, Some(blocker));
        // participant 未生效（被 blocker 覆盖）
    }

    /// 两个都是 None 时用 "none"。
    #[test]
    fn r758_incident_key_none_fallback() {
        let input = IncidentKeyInput {
            company_id: Uuid::new_v4(),
            issue_id: Uuid::new_v4(),
            state: "todo",
            blocker_issue_id: None,
            participant_agent_id: None,
        };
        let key = build_issue_graph_liveness_incident_key(input);
        let parsed = parse_issue_graph_liveness_incident_key(&key).expect("parse ok");
        assert_eq!(parsed.leaf_issue_id, None);
        assert!(key.ends_with(":none"));
    }

    /// 构造 -> 解析 round-trip。
    #[test]
    fn r758_incident_key_round_trip() {
        let company_id = Uuid::new_v4();
        let issue_id = Uuid::new_v4();
        let participant = Uuid::new_v4();
        let input = IncidentKeyInput {
            company_id,
            issue_id,
            state: "in_progress",
            blocker_issue_id: None,
            participant_agent_id: Some(participant),
        };
        let key = build_issue_graph_liveness_incident_key(input);
        let parsed = parse_issue_graph_liveness_incident_key(&key).expect("parse ok");
        assert_eq!(parsed.company_id, company_id);
        assert_eq!(parsed.issue_id, issue_id);
        assert_eq!(parsed.state, "in_progress");
        assert_eq!(parsed.leaf_issue_id, Some(participant));
    }

    /// 错误前缀返回 None。
    #[test]
    fn r758_parse_invalid_prefix() {
        let key = "wrong_prefix:00000000-0000-0000-0000-000000000000:00000000-0000-0000-0000-000000000000:todo:none";
        assert!(parse_issue_graph_liveness_incident_key(key).is_none());
    }

    /// 字段数不对返回 None。
    #[test]
    fn r758_parse_wrong_field_count() {
        let key = "harness_liveness:00000000-0000-0000-0000-000000000000:00000000-0000-0000-0000-000000000000";
        assert!(parse_issue_graph_liveness_incident_key(key).is_none());
    }

    /// UUID 解析失败返回 None。
    #[test]
    fn r758_parse_invalid_uuid() {
        let key = "harness_liveness:not-a-uuid:00000000-0000-0000-0000-000000000000:todo:none";
        assert!(parse_issue_graph_liveness_incident_key(key).is_none());
    }

    /// 空 state/leaf 返回 None。
    #[test]
    fn r758_parse_empty_state() {
        let key = "harness_liveness:00000000-0000-0000-0000-000000000000:00000000-0000-0000-0000-000000000000::none";
        assert!(parse_issue_graph_liveness_incident_key(key).is_none());
    }
}

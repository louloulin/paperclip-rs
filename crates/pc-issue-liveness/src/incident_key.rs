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
        ISSUE_GRAPH_LIVENESS_INCIDENT_PREFIX,
        input.company_id,
        input.issue_id,
        input.state,
        leaf
    )
}

/// 解析 liveness incident key。
///
/// 返回 `Some((companyId, issueId, state, leafIssueId))` 当格式正确；
/// 否则 `None`。
pub fn parse_issue_graph_liveness_incident_key(
    incident_key: &str,
) -> Option<ParsedIncidentKey> {
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

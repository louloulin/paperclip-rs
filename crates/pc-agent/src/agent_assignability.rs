//! Agent assignment gate：调用方在分配 work / routine 之前，校验目标 agent 的
//! lifecycle 状态 + 上级链 (org chain) 健康度。
//!
//! 对齐 Node `services/agent-assignability.ts`：
//! - `assertAssignableAgent`: 如果 `assignable` 为 true 直接返回；否则根据
//!   `eligibility.assignabilityReason` 抛出对应类型的错误
//! - reason 与 Node 错误码 1:1 对齐：
//!   `pending_approval` / `assignee_terminated` / `assignee_unknown_status` /
//!   `ancestor_terminated` / `ancestor_missing` / `ancestor_cycle` /
//!   `ancestor_depth_exceeded`（暂不在 Rust 错误枚举中暴露，使用 Unknown）
//! - 错误 detail 携带 ancestor chain + firstInvalidAncestor + missingAncestorAgentId

use pc_core::agent_eligibility::{
    get_agent_work_eligibility, AgentEligibilityAgent, AgentOrgChainHealth, AgentWorkEligibility,
};
use pc_repos::Db;
use serde::Serialize;
use thiserror::Error;

/// 分配类型（work / routine），用于错误消息文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAssignmentKind {
    Work,
    Routine,
}

/// 分配冲突原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAssignmentConflictReason {
    PendingApproval,
    AssigneeTerminated,
    AssigneeUnknownStatus,
    AncestorTerminated,
    AncestorMissing,
    AncestorCycle,
    AncestorDepthExceeded,
}

/// Ancestor chain entry (用于错误 detail)。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AncestorChainEntry {
    pub id: String,
    pub company_id: String,
    pub name: String,
    pub status: String,
    pub reports_to: Option<String>,
}

/// Conflict detail（与 Node `conflictDetails` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentAssignmentConflictDetail {
    pub code: &'static str,
    pub reason: AgentAssignmentConflictReason,
    pub company_id: String,
    pub assignee_agent_id: String,
    pub invalid_ancestor_agent_id: Option<String>,
    pub missing_ancestor_agent_id: Option<String>,
    pub ancestor_chain: Vec<AncestorChainEntry>,
}

/// Assignability error。
#[derive(Debug, Clone, Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentAssignmentError {
    /// Assignee agent 不存在。
    #[error("Assignee agent not found")]
    NotFound,

    /// Assignee 与目标 company 不匹配。
    #[error("Assignee must belong to same company")]
    CrossCompany,

    /// 分配冲突（生命周期状态或 org chain 不健康）。
    #[error("{message}")]
    Conflict {
        message: String,
        #[serde(flatten)]
        detail: AgentAssignmentConflictDetail,
    },
}

/// `assertAssignableAgent` 的额外选项。
#[derive(Debug, Clone, Default)]
pub struct AssertAssignableOptions {
    pub kind: Option<AgentAssignmentKind>,
}

/// 把 DB 行转 `AgentEligibilityAgent`。
fn row_to_eligibility(row: AgentRow) -> AgentEligibilityAgent {
    AgentEligibilityAgent {
        id: row.id.to_string(),
        company_id: row.company_id.to_string(),
        name: row.name,
        status: row.status,
        reports_to: row.reports_to.map(|u| u.to_string()),
    }
}

/// DB 行最小化形状。
#[derive(Debug, Clone, sqlx::FromRow)]
struct AgentRow {
    id: uuid::Uuid,
    company_id: uuid::Uuid,
    name: String,
    status: String,
    reports_to: Option<uuid::Uuid>,
}

/// `getAgent` — 查单个 agent。
pub async fn get_agent(
    db: &Db,
    agent_id: uuid::Uuid,
) -> Result<Option<AgentEligibilityAgent>, sqlx::Error> {
    let row: Option<AgentRow> = sqlx::query_as::<_, AgentRow>(
        "SELECT id, company_id, name, status, reports_to FROM agents WHERE id = $1",
    )
    .bind(agent_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(row_to_eligibility))
}

/// `listCompanyAgents` — 列同 company 下所有 agent。
pub async fn list_company_agents(
    db: &Db,
    company_id: uuid::Uuid,
) -> Result<Vec<AgentEligibilityAgent>, sqlx::Error> {
    let rows: Vec<AgentRow> = sqlx::query_as::<_, AgentRow>(
        "SELECT id, company_id, name, status, reports_to FROM agents WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(row_to_eligibility).collect())
}

/// `assertAssignableAgent` — 校验 agent 可分配。
pub async fn assert_assignable_agent(
    db: &Db,
    company_id: uuid::Uuid,
    agent_id: Option<uuid::Uuid>,
    options: AssertAssignableOptions,
) -> Result<(), AgentAssignmentError> {
    let Some(agent_id) = agent_id else {
        return Ok(());
    };
    let kind = options.kind.unwrap_or(AgentAssignmentKind::Work);

    let assignee = get_agent(db, agent_id)
        .await
        .map_err(|e| AgentAssignmentError::Conflict {
            message: format!("db error: {e}"),
            detail: empty_detail(company_id, agent_id),
        })?
        .ok_or(AgentAssignmentError::NotFound)?;
    if assignee.company_id != company_id.to_string() {
        return Err(AgentAssignmentError::CrossCompany);
    }

    let company_agents =
        list_company_agents(db, company_id)
            .await
            .map_err(|e| AgentAssignmentError::Conflict {
                message: format!("db error: {e}"),
                detail: empty_detail(company_id, agent_id),
            })?;
    let eligibility: AgentWorkEligibility = get_agent_work_eligibility(&assignee, &company_agents);
    let chain: Vec<AncestorChainEntry> = eligibility
        .org_chain_health
        .full_chain
        .iter()
        .map(|entry| AncestorChainEntry {
            id: entry.id.clone(),
            company_id: entry.company_id.clone(),
            name: entry.name.clone(),
            status: entry.status.clone(),
            reports_to: entry.reports_to.clone(),
        })
        .collect();

    if eligibility.assignable {
        return Ok(());
    }

    let reason = match eligibility.assignability_reason {
        pc_core::agent_eligibility::AgentEligibilityLifecycleReason::PendingApproval => {
            AgentAssignmentConflictReason::PendingApproval
        }
        pc_core::agent_eligibility::AgentEligibilityLifecycleReason::Terminated => {
            AgentAssignmentConflictReason::AssigneeTerminated
        }
        pc_core::agent_eligibility::AgentEligibilityLifecycleReason::UnknownStatus => {
            AgentAssignmentConflictReason::AssigneeUnknownStatus
        }
        _ => conflict_reason_from_org_chain_health(&eligibility.org_chain_health),
    };

    let first_invalid = eligibility.org_chain_health.first_invalid_ancestor.as_ref();
    let (invalid_id, missing_id) = match first_invalid {
        Some(anc) if anc.status != "missing" => (Some(anc.id.clone()), None),
        Some(anc) => (None, Some(anc.id.clone())),
        None => (None, None),
    };

    Err(AgentAssignmentError::Conflict {
        message: assignment_message(kind, reason).to_string(),
        detail: AgentAssignmentConflictDetail {
            code: "agent_not_assignable",
            reason,
            company_id: company_id.to_string(),
            assignee_agent_id: agent_id.to_string(),
            invalid_ancestor_agent_id: invalid_id,
            missing_ancestor_agent_id: missing_id,
            ancestor_chain: chain,
        },
    })
}

fn conflict_reason_from_org_chain_health(
    health: &AgentOrgChainHealth,
) -> AgentAssignmentConflictReason {
    use pc_core::agent_eligibility::AgentOrgChainInvalidReason;
    match health.reason {
        AgentOrgChainInvalidReason::TerminatedAncestor => {
            AgentAssignmentConflictReason::AncestorTerminated
        }
        AgentOrgChainInvalidReason::Cycle => AgentAssignmentConflictReason::AncestorCycle,
        AgentOrgChainInvalidReason::MissingManager => {
            AgentAssignmentConflictReason::AncestorMissing
        }
        AgentOrgChainInvalidReason::Healthy => AgentAssignmentConflictReason::AncestorMissing,
    }
}

fn assignment_message(
    kind: AgentAssignmentKind,
    reason: AgentAssignmentConflictReason,
) -> &'static str {
    match (kind, reason) {
        (AgentAssignmentKind::Routine, AgentAssignmentConflictReason::PendingApproval) => {
            "Cannot assign routines to pending approval agents"
        }
        (AgentAssignmentKind::Work, AgentAssignmentConflictReason::PendingApproval) => {
            "Cannot assign work to pending approval agents"
        }
        (AgentAssignmentKind::Routine, AgentAssignmentConflictReason::AssigneeTerminated) => {
            "Cannot assign routines to terminated agents"
        }
        (AgentAssignmentKind::Work, AgentAssignmentConflictReason::AssigneeTerminated) => {
            "Cannot assign work to terminated agents"
        }
        (AgentAssignmentKind::Routine, AgentAssignmentConflictReason::AssigneeUnknownStatus) => {
            "Cannot assign routines to agents with an unsupported lifecycle status"
        }
        (AgentAssignmentKind::Work, AgentAssignmentConflictReason::AssigneeUnknownStatus) => {
            "Cannot assign work to agents with an unsupported lifecycle status"
        }
        (AgentAssignmentKind::Routine, _) => {
            "Cannot assign routines to agents with an invalid org chain"
        }
        (AgentAssignmentKind::Work, _) => "Cannot assign work to agents with an invalid org chain",
    }
}

fn empty_detail(company_id: uuid::Uuid, agent_id: uuid::Uuid) -> AgentAssignmentConflictDetail {
    AgentAssignmentConflictDetail {
        code: "agent_not_assignable",
        reason: AgentAssignmentConflictReason::AncestorMissing,
        company_id: company_id.to_string(),
        assignee_agent_id: agent_id.to_string(),
        invalid_ancestor_agent_id: None,
        missing_ancestor_agent_id: None,
        ancestor_chain: Vec::new(),
    }
}


#[cfg(test)]
mod internal_tests {
    //! R778 - pure data tests for assignability public types.
    //! Note: types derive only Serialize (not Deserialize).

    use super::*;

    #[test]
    fn r778_assignment_kind_serializes_work() {
        let work = serde_json::to_string(&AgentAssignmentKind::Work).unwrap();
        assert!(work.contains("work"), "work not in {}", work);
    }

    #[test]
    fn r778_assignment_kind_serializes_routine() {
        let routine = serde_json::to_string(&AgentAssignmentKind::Routine).unwrap();
        assert!(routine.contains("routine"), "routine not in {}", routine);
    }

    #[test]
    fn r778_assignment_kind_is_copy_and_eq() {
        let a = AgentAssignmentKind::Work;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(AgentAssignmentKind::Work, AgentAssignmentKind::Routine);
    }

    #[test]
    fn r778_conflict_reason_serializes_all_variants() {
        let pairs = [
            (AgentAssignmentConflictReason::PendingApproval, "pending_approval"),
            (AgentAssignmentConflictReason::AssigneeTerminated, "assignee_terminated"),
            (AgentAssignmentConflictReason::AssigneeUnknownStatus, "assignee_unknown_status"),
            (AgentAssignmentConflictReason::AncestorTerminated, "ancestor_terminated"),
            (AgentAssignmentConflictReason::AncestorMissing, "ancestor_missing"),
            (AgentAssignmentConflictReason::AncestorCycle, "ancestor_cycle"),
            (AgentAssignmentConflictReason::AncestorDepthExceeded, "ancestor_depth_exceeded"),
        ];
        for (reason, expected_substr) in pairs {
            let json = serde_json::to_string(&reason).unwrap();
            assert!(json.contains(expected_substr), "expected {} in {}", expected_substr, json);
        }
    }

    #[test]
    fn r778_conflict_reason_distinct_pairs() {
        assert_ne!(AgentAssignmentConflictReason::PendingApproval, AgentAssignmentConflictReason::AssigneeTerminated);
        assert_ne!(AgentAssignmentConflictReason::AncestorCycle, AgentAssignmentConflictReason::AncestorMissing);
        assert_ne!(AgentAssignmentConflictReason::AncestorDepthExceeded, AgentAssignmentConflictReason::AncestorTerminated);
    }

    #[test]
    fn r778_ancestor_chain_entry_serializes_camel_case() {
        let entry = AncestorChainEntry {
            id: "agent-1".to_string(),
            company_id: "co-1".to_string(),
            name: "Alice".to_string(),
            status: "active".to_string(),
            reports_to: Some("agent-0".to_string()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("companyId"));
        assert!(json.contains("co-1"));
        assert!(json.contains("reportsTo"));
        assert!(json.contains("agent-0"));
    }

    #[test]
    fn r778_ancestor_chain_entry_serializes_reports_to_null() {
        let entry = AncestorChainEntry {
            id: "root-1".to_string(),
            company_id: "co-1".to_string(),
            name: "Root".to_string(),
            status: "active".to_string(),
            reports_to: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("null"), "got: {}", json);
    }

    #[test]
    fn r778_conflict_detail_code_is_agent_not_assignable() {
        let detail = AgentAssignmentConflictDetail {
            code: "agent_not_assignable",
            reason: AgentAssignmentConflictReason::AncestorMissing,
            company_id: "co-1".to_string(),
            assignee_agent_id: "agent-1".to_string(),
            invalid_ancestor_agent_id: None,
            missing_ancestor_agent_id: Some("agent-99".to_string()),
            ancestor_chain: Vec::new(),
        };
        assert_eq!(detail.code, "agent_not_assignable");
        assert_eq!(detail.reason, AgentAssignmentConflictReason::AncestorMissing);
    }

    #[test]
    fn r778_conflict_detail_serializes_with_chain() {
        let detail = AgentAssignmentConflictDetail {
            code: "agent_not_assignable",
            reason: AgentAssignmentConflictReason::AncestorTerminated,
            company_id: "co-1".to_string(),
            assignee_agent_id: "agent-1".to_string(),
            invalid_ancestor_agent_id: Some("agent-2".to_string()),
            missing_ancestor_agent_id: None,
            ancestor_chain: vec![AncestorChainEntry {
                id: "agent-2".to_string(),
                company_id: "co-1".to_string(),
                name: "Bob".to_string(),
                status: "terminated".to_string(),
                reports_to: None,
            }],
        };
        let json = serde_json::to_string(&detail).unwrap();
        assert!(json.contains("invalidAncestorAgentId"));
        assert!(json.contains("agent-2"));
        assert!(json.contains("ancestorChain"));
        assert!(json.contains("Bob"));
    }

    #[test]
    fn r778_error_not_found_display() {
        let err = AgentAssignmentError::NotFound;
        assert_eq!(err.to_string(), "Assignee agent not found");
    }

    #[test]
    fn r778_error_cross_company_display() {
        let err = AgentAssignmentError::CrossCompany;
        assert_eq!(err.to_string(), "Assignee must belong to same company");
    }

    #[test]
    fn r778_error_conflict_serializes_with_detail_flatten() {
        let detail = AgentAssignmentConflictDetail {
            code: "agent_not_assignable",
            reason: AgentAssignmentConflictReason::PendingApproval,
            company_id: "co-1".to_string(),
            assignee_agent_id: "agent-1".to_string(),
            invalid_ancestor_agent_id: None,
            missing_ancestor_agent_id: None,
            ancestor_chain: Vec::new(),
        };
        let err = AgentAssignmentError::Conflict {
            message: "Cannot assign work to pending approval agents".to_string(),
            detail,
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("kind"));
        assert!(json.contains("conflict"));
        assert!(json.contains("companyId"));
        assert!(json.contains("co-1"));
        assert!(json.contains("assigneeAgentId"));
        assert!(json.contains("reason"));
        assert!(json.contains("pending_approval"));
        assert!(json.contains("code"));
        assert!(json.contains("agent_not_assignable"));
    }

    #[test]
    fn r778_error_conflict_display_includes_message() {
        let detail = AgentAssignmentConflictDetail {
            code: "agent_not_assignable",
            reason: AgentAssignmentConflictReason::AssigneeTerminated,
            company_id: "co-1".to_string(),
            assignee_agent_id: "agent-1".to_string(),
            invalid_ancestor_agent_id: None,
            missing_ancestor_agent_id: None,
            ancestor_chain: Vec::new(),
        };
        let err = AgentAssignmentError::Conflict {
            message: "Cannot assign routines to terminated agents".to_string(),
            detail,
        };
        let s = err.to_string();
        assert!(s.contains("Cannot assign routines to terminated agents"), "got: {}", s);
    }

    #[test]
    fn r778_options_default_has_no_kind() {
        let opts = AssertAssignableOptions::default();
        assert!(opts.kind.is_none());
    }

    #[test]
    fn r778_options_with_kind_clone() {
        let opts = AssertAssignableOptions {
            kind: Some(AgentAssignmentKind::Routine),
        };
        let cloned = opts.clone();
        assert_eq!(cloned.kind, Some(AgentAssignmentKind::Routine));
    }
}

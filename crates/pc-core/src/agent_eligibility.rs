//! Agent eligibility / org-chain health 纯规则层。
//!
//! 对齐 Node `packages/shared/src/agent-eligibility.ts`（245 行）。
//!
//! 单一职责：
//! - 判断单个 agent 在某个 company agents 集合中的可分配性（assignable / invokable）
//! - 计算 agent 的 org chain 健康状态（terminated ancestor / missing manager / cycle）
//! - 不持有任何 IO 状态，不依赖 pc-repos / pc-db。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Agent 生命周期原因（与 Node `AgentEligibilityLifecycleReason` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEligibilityLifecycleReason {
    Eligible,
    Terminated,
    PendingApproval,
    Paused,
    InvalidOrgChain,
    UnknownStatus,
}

impl AgentEligibilityLifecycleReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Terminated => "terminated",
            Self::PendingApproval => "pending_approval",
            Self::Paused => "paused",
            Self::InvalidOrgChain => "invalid_org_chain",
            Self::UnknownStatus => "unknown_status",
        }
    }
}

/// Org chain 无效原因（与 Node `AgentOrgChainInvalidReason` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOrgChainInvalidReason {
    Healthy,
    TerminatedAncestor,
    MissingManager,
    Cycle,
}

impl AgentOrgChainInvalidReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::TerminatedAncestor => "terminated_ancestor",
            Self::MissingManager => "missing_manager",
            Self::Cycle => "cycle",
        }
    }
}

/// Org chain 健康状态枚举（与 Node `AgentOrgChainHealth.status` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOrgChainHealthStatus {
    Healthy,
    InvalidOrgChain,
}

/// Org chain entry 关系（self / ancestor）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOrgChainRelation {
    Self_,
    Ancestor,
}

/// Agent 最小化形状（与 Node `AgentEligibilityAgent` 1:1 对齐）。
///
/// 字段顺序：id / companyId / name / status / reportsTo。
/// `status` 接受任意 `String`（与 Node `AgentStatus | string` 等价），便于共享层不绑定 constants 枚举。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEligibilityAgent {
    pub id: String,
    #[serde(rename = "companyId")]
    pub company_id: String,
    pub name: String,
    pub status: String,
    #[serde(rename = "reportsTo", skip_serializing_if = "Option::is_none")]
    pub reports_to: Option<String>,
}

impl AgentEligibilityAgent {
    /// 便利构造器，便于单测。
    pub fn new(
        id: impl Into<String>,
        company_id: impl Into<String>,
        name: impl Into<String>,
        status: impl Into<String>,
        reports_to: Option<impl Into<String>>,
    ) -> Self {
        Self {
            id: id.into(),
            company_id: company_id.into(),
            name: name.into(),
            status: status.into(),
            reports_to: reports_to.map(Into::into),
        }
    }
}

/// Org chain 节点（与 Node `AgentOrgChainEntry` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentOrgChainEntry {
    pub id: String,
    #[serde(rename = "companyId")]
    pub company_id: String,
    pub name: String,
    pub status: String,
    #[serde(rename = "reportsTo")]
    pub reports_to: Option<String>,
    pub depth: i32,
    pub relation: AgentOrgChainRelation,
}

/// 无效 ancestor（与 Node `AgentInvalidOrgChainAncestor` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInvalidOrgChainAncestor {
    pub id: String,
    pub name: String,
    pub status: String,
}

/// Org chain 健康结果（与 Node `AgentOrgChainHealth` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentOrgChainHealth {
    pub status: AgentOrgChainHealthStatus,
    pub reason: AgentOrgChainInvalidReason,
    #[serde(rename = "fullChain")]
    pub full_chain: Vec<AgentOrgChainEntry>,
    #[serde(rename = "firstInvalidAncestor")]
    pub first_invalid_ancestor: Option<AgentInvalidOrgChainAncestor>,
    #[serde(rename = "invalidAncestors")]
    pub invalid_ancestors: Vec<AgentInvalidOrgChainAncestor>,
    #[serde(rename = "repairGuidance")]
    pub repair_guidance: Option<String>,
}

/// Agent 工作资格评估（与 Node `AgentWorkEligibility` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkEligibility {
    pub assignable: bool,
    pub invokable: bool,
    #[serde(rename = "assignabilityReason")]
    pub assignability_reason: AgentEligibilityLifecycleReason,
    #[serde(rename = "invokabilityReason")]
    pub invokability_reason: AgentEligibilityLifecycleReason,
    #[serde(rename = "orgChainHealth")]
    pub org_chain_health: AgentOrgChainHealth,
}

// ---- 常量（与 Node `NON_ASSIGNABLE_AGENT_STATUSES` 等 1:1 对齐） ----

/// 不可被分配的 lifecycle 状态集合。
const NON_ASSIGNABLE_AGENT_STATUSES: &[&str] = &["terminated", "pending_approval"];

/// 不可被调用的 lifecycle 状态集合。
const NON_INVOKABLE_AGENT_STATUSES: &[&str] = &["terminated", "pending_approval", "paused"];

/// 可被分配的 lifecycle 状态集合。
const ASSIGNABLE_AGENT_STATUSES: &[&str] = &["active", "paused", "idle", "running", "error"];

/// 可被调用的 lifecycle 状态集合。
const INVOKABLE_AGENT_STATUSES: &[&str] = &["active", "idle", "running", "error"];

/// 判断 status 是否可被分配工作（与 Node `isAgentStatusAssignableToWork` 1:1 对齐）。
#[must_use]
pub fn is_agent_status_assignable_to_work(status: &str) -> bool {
    ASSIGNABLE_AGENT_STATUSES.contains(&status) && !NON_ASSIGNABLE_AGENT_STATUSES.contains(&status)
}

/// 判断 status 是否可被调用（与 Node `isAgentStatusInvokable` 1:1 对齐）。
#[must_use]
pub fn is_agent_status_invokable(status: &str) -> bool {
    INVOKABLE_AGENT_STATUSES.contains(&status) && !NON_INVOKABLE_AGENT_STATUSES.contains(&status)
}

/// 计算 agent 的 org chain 健康（与 Node `getAgentOrgChainHealth` 1:1 对齐）。
#[must_use]
pub fn get_agent_org_chain_health(
    agent: &AgentEligibilityAgent,
    agents: &[AgentEligibilityAgent],
) -> AgentOrgChainHealth {
    let by_id: std::collections::HashMap<&str, &AgentEligibilityAgent> =
        agents.iter().map(|a| (a.id.as_str(), a)).collect();

    let mut full_chain = vec![make_chain_entry(agent, 0, AgentOrgChainRelation::Self_)];
    let mut invalid_ancestors: Vec<AgentInvalidOrgChainAncestor> = Vec::new();
    let mut seen: HashSet<String> = HashSet::with_capacity(8);
    seen.insert(agent.id.clone());

    let mut current = agent.clone();
    let mut depth: i32 = 1;
    while let Some(ref reports_to) = current.reports_to.clone() {
        if seen.contains(reports_to) {
            let cycle_agent = by_id.get(reports_to.as_str());
            let invalid = AgentInvalidOrgChainAncestor {
                id: reports_to.clone(),
                name: cycle_agent
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| reports_to.clone()),
                status: "cycle".to_string(),
            };
            full_chain.push(AgentOrgChainEntry {
                id: invalid.id.clone(),
                company_id: agent.company_id.clone(),
                name: invalid.name.clone(),
                status: invalid.status.clone(),
                reports_to: cycle_agent.and_then(|a| a.reports_to.clone()),
                depth,
                relation: AgentOrgChainRelation::Ancestor,
            });
            invalid_ancestors.push(invalid);
            break;
        }
        seen.insert(reports_to.clone());

        let parent = match by_id.get(reports_to.as_str()) {
            Some(p) if p.company_id == agent.company_id => p,
            _ => {
                let invalid = AgentInvalidOrgChainAncestor {
                    id: reports_to.clone(),
                    name: reports_to.clone(),
                    status: "missing".to_string(),
                };
                full_chain.push(AgentOrgChainEntry {
                    id: invalid.id.clone(),
                    company_id: agent.company_id.clone(),
                    name: invalid.name.clone(),
                    status: invalid.status.clone(),
                    reports_to: None,
                    depth,
                    relation: AgentOrgChainRelation::Ancestor,
                });
                invalid_ancestors.push(invalid);
                break;
            }
        };

        full_chain.push(make_chain_entry(
            parent,
            depth,
            AgentOrgChainRelation::Ancestor,
        ));
        if parent.status == "terminated" {
            invalid_ancestors.push(make_invalid_ancestor(parent));
        }

        current = (*parent).clone();
        depth += 1;
    }

    let first_invalid_ancestor = invalid_ancestors.first().cloned();

    let reason = match first_invalid_ancestor.as_ref() {
        Some(inv) if inv.status == "missing" => AgentOrgChainInvalidReason::MissingManager,
        Some(inv) if inv.status == "cycle" => AgentOrgChainInvalidReason::Cycle,
        Some(_) => AgentOrgChainInvalidReason::TerminatedAncestor,
        None => AgentOrgChainInvalidReason::Healthy,
    };

    let status = if first_invalid_ancestor.is_some() {
        AgentOrgChainHealthStatus::InvalidOrgChain
    } else {
        AgentOrgChainHealthStatus::Healthy
    };

    let repair_guidance = first_invalid_ancestor
        .as_ref()
        .map(|first| build_repair_guidance(agent, first));

    AgentOrgChainHealth {
        status,
        reason,
        full_chain,
        first_invalid_ancestor,
        invalid_ancestors,
        repair_guidance,
    }
}

/// 计算 agent 工作资格（与 Node `getAgentWorkEligibility` 1:1 对齐）。
#[must_use]
pub fn get_agent_work_eligibility(
    agent: &AgentEligibilityAgent,
    agents: &[AgentEligibilityAgent],
) -> AgentWorkEligibility {
    let org_chain_health = get_agent_org_chain_health(agent, agents);
    let assignability_reason = compute_assignability_reason(agent, &org_chain_health);
    let invokability_reason = compute_invokability_reason(agent, &org_chain_health);
    AgentWorkEligibility {
        assignable: assignability_reason == AgentEligibilityLifecycleReason::Eligible,
        invokable: invokability_reason == AgentEligibilityLifecycleReason::Eligible,
        assignability_reason,
        invokability_reason,
        org_chain_health,
    }
}

/// 便捷包装：是否可分配工作（与 Node `isAgentAssignableToWork` 1:1 对齐）。
#[must_use]
pub fn is_agent_assignable_to_work(
    agent: &AgentEligibilityAgent,
    agents: &[AgentEligibilityAgent],
) -> bool {
    get_agent_work_eligibility(agent, agents).assignable
}

/// 便捷包装：是否可调用（与 Node `isAgentInvokable` 1:1 对齐）。
#[must_use]
pub fn is_agent_invokable(agent: &AgentEligibilityAgent, agents: &[AgentEligibilityAgent]) -> bool {
    get_agent_work_eligibility(agent, agents).invokable
}

// ---- private helpers ----

fn make_chain_entry(
    agent: &AgentEligibilityAgent,
    depth: i32,
    relation: AgentOrgChainRelation,
) -> AgentOrgChainEntry {
    AgentOrgChainEntry {
        id: agent.id.clone(),
        company_id: agent.company_id.clone(),
        name: agent.name.clone(),
        status: agent.status.clone(),
        reports_to: agent.reports_to.clone(),
        depth,
        relation,
    }
}

fn make_invalid_ancestor(agent: &AgentEligibilityAgent) -> AgentInvalidOrgChainAncestor {
    AgentInvalidOrgChainAncestor {
        id: agent.id.clone(),
        name: agent.name.clone(),
        status: agent.status.clone(),
    }
}

fn build_repair_guidance(
    agent: &AgentEligibilityAgent,
    first_invalid_ancestor: &AgentInvalidOrgChainAncestor,
) -> String {
    if first_invalid_ancestor.status == "missing" {
        format!(
            "{} reports to missing manager {}. Reassign {} or the nearest affected ancestor under an active manager/root, or explicitly pause or terminate the invalid subtree before assigning work or starting runs.",
            agent.name, first_invalid_ancestor.id, agent.name
        )
    } else if first_invalid_ancestor.status == "cycle" {
        format!(
            "{} has a cycle in its reporting chain at {}. Break the cycle by assigning one affected agent to an active manager/root, or explicitly pause or terminate the invalid subtree before assigning work or starting runs.",
            agent.name, first_invalid_ancestor.name
        )
    } else {
        format!(
            "{} reports through terminated ancestor {}. Reassign {} or the nearest affected ancestor under an active manager/root, or explicitly pause or terminate the invalid subtree before assigning work or starting runs.",
            agent.name, first_invalid_ancestor.name, agent.name
        )
    }
}

fn compute_assignability_reason(
    agent: &AgentEligibilityAgent,
    org_chain_health: &AgentOrgChainHealth,
) -> AgentEligibilityLifecycleReason {
    if is_agent_status_assignable_to_work(&agent.status) {
        if org_chain_health.status == AgentOrgChainHealthStatus::InvalidOrgChain {
            AgentEligibilityLifecycleReason::InvalidOrgChain
        } else {
            AgentEligibilityLifecycleReason::Eligible
        }
    } else if agent.status == "terminated" {
        AgentEligibilityLifecycleReason::Terminated
    } else if agent.status == "pending_approval" {
        AgentEligibilityLifecycleReason::PendingApproval
    } else {
        AgentEligibilityLifecycleReason::UnknownStatus
    }
}

fn compute_invokability_reason(
    agent: &AgentEligibilityAgent,
    org_chain_health: &AgentOrgChainHealth,
) -> AgentEligibilityLifecycleReason {
    if is_agent_status_invokable(&agent.status) {
        if org_chain_health.status == AgentOrgChainHealthStatus::InvalidOrgChain {
            AgentEligibilityLifecycleReason::InvalidOrgChain
        } else {
            AgentEligibilityLifecycleReason::Eligible
        }
    } else if agent.status == "terminated" {
        AgentEligibilityLifecycleReason::Terminated
    } else if agent.status == "pending_approval" {
        AgentEligibilityLifecycleReason::PendingApproval
    } else if agent.status == "paused" {
        AgentEligibilityLifecycleReason::Paused
    } else {
        AgentEligibilityLifecycleReason::UnknownStatus
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPANY: &str = "company-1";

    fn agent(
        id: &str,
        name: &str,
        status: &str,
        reports_to: Option<&str>,
    ) -> AgentEligibilityAgent {
        AgentEligibilityAgent::new(id, COMPANY, name, status, reports_to)
    }

    #[test]
    fn status_predicates_match_node_sets() {
        for s in ["active", "idle", "running", "error"] {
            assert!(
                is_agent_status_assignable_to_work(s),
                "{s} should be assignable"
            );
            assert!(is_agent_status_invokable(s), "{s} should be invokable");
        }
        for s in ["terminated", "pending_approval"] {
            assert!(
                !is_agent_status_assignable_to_work(s),
                "{s} should not be assignable"
            );
            assert!(!is_agent_status_invokable(s), "{s} should not be invokable");
        }
        assert!(is_agent_status_assignable_to_work("paused"));
        assert!(!is_agent_status_invokable("paused"));
        assert!(!is_agent_status_assignable_to_work("sabbatical"));
        assert!(!is_agent_status_invokable("sabbatical"));
    }

    #[test]
    fn healthy_active_agents_are_eligible() {
        let manager = agent("manager-1", "CTO", "active", None);
        let target = agent("agent-1", "Coder", "active", Some("manager-1"));
        let agents = vec![target.clone(), manager];

        assert!(is_agent_assignable_to_work(&target, &agents));
        assert!(is_agent_invokable(&target, &agents));
        let elig = get_agent_work_eligibility(&target, &agents);
        assert_eq!(
            elig.assignability_reason,
            AgentEligibilityLifecycleReason::Eligible
        );
        assert_eq!(
            elig.invokability_reason,
            AgentEligibilityLifecycleReason::Eligible
        );
        assert_eq!(
            elig.org_chain_health.status,
            AgentOrgChainHealthStatus::Healthy
        );
        assert_eq!(
            elig.org_chain_health.reason,
            AgentOrgChainInvalidReason::Healthy
        );
    }

    #[test]
    fn terminated_and_pending_approval_block_both() {
        let manager = agent("manager-1", "CTO", "active", None);
        for status in ["terminated", "pending_approval"] {
            let target = agent("agent-1", "Coder", status, Some("manager-1"));
            let elig = get_agent_work_eligibility(&target, std::slice::from_ref(&manager));
            assert!(!elig.assignable);
            assert!(!elig.invokable);
            assert_eq!(elig.assignability_reason.as_str(), status);
            assert_eq!(elig.invokability_reason.as_str(), status);
        }
    }

    #[test]
    fn paused_keeps_assignment_but_blocks_invocation() {
        let manager = agent("manager-1", "CTO", "active", None);
        let target = agent("agent-1", "Coder", "paused", Some("manager-1"));
        let elig = get_agent_work_eligibility(&target, &[target.clone(), manager]);
        assert!(elig.assignable);
        assert!(!elig.invokable);
        assert_eq!(
            elig.assignability_reason,
            AgentEligibilityLifecycleReason::Eligible
        );
        assert_eq!(
            elig.invokability_reason,
            AgentEligibilityLifecycleReason::Paused
        );
    }

    #[test]
    fn unknown_status_reported_explicitly() {
        let manager = agent("manager-1", "CTO", "active", None);
        let target = agent("agent-1", "Coder", "sabbatical", Some("manager-1"));
        let elig = get_agent_work_eligibility(&target, &[target.clone(), manager]);
        assert!(!elig.assignable);
        assert!(!elig.invokable);
        assert_eq!(
            elig.assignability_reason,
            AgentEligibilityLifecycleReason::UnknownStatus
        );
        assert_eq!(
            elig.invokability_reason,
            AgentEligibilityLifecycleReason::UnknownStatus
        );
        assert_eq!(
            elig.org_chain_health.status,
            AgentOrgChainHealthStatus::Healthy
        );
    }

    #[test]
    fn terminated_ancestor_blocks_descendants_with_repair_guidance() {
        let target = agent("qa-2", "QA 2", "active", Some("cto-2"));
        let terminated_manager = agent("cto-2", "CTO 2", "terminated", Some("ceo-2"));
        let terminated_root = agent("ceo-2", "CEO 2", "terminated", None);
        let agents = vec![target.clone(), terminated_manager, terminated_root];

        let health = get_agent_org_chain_health(&target, &agents);
        assert_eq!(health.status, AgentOrgChainHealthStatus::InvalidOrgChain);
        assert_eq!(
            health.reason,
            AgentOrgChainInvalidReason::TerminatedAncestor
        );
        assert_eq!(health.full_chain.len(), 3);
        assert_eq!(health.full_chain[0].id, "qa-2");
        assert_eq!(health.full_chain[0].depth, 0);
        assert_eq!(health.full_chain[0].relation, AgentOrgChainRelation::Self_);
        assert_eq!(health.full_chain[1].id, "cto-2");
        assert_eq!(health.full_chain[1].status, "terminated");
        assert_eq!(health.full_chain[1].depth, 1);
        assert_eq!(
            health.full_chain[1].relation,
            AgentOrgChainRelation::Ancestor
        );
        assert_eq!(health.full_chain[2].id, "ceo-2");
        assert_eq!(health.full_chain[2].depth, 2);
        assert_eq!(
            health.first_invalid_ancestor,
            Some(AgentInvalidOrgChainAncestor {
                id: "cto-2".to_string(),
                name: "CTO 2".to_string(),
                status: "terminated".to_string()
            })
        );
        assert_eq!(health.invalid_ancestors.len(), 2);
        assert!(
            health
                .repair_guidance
                .as_deref()
                .unwrap_or("")
                .contains("QA 2 reports through terminated ancestor CTO 2"),
            "repair_guidance = {:?}",
            health.repair_guidance
        );

        let elig = get_agent_work_eligibility(&target, &agents);
        assert!(!elig.assignable);
        assert!(!elig.invokable);
        assert_eq!(
            elig.assignability_reason,
            AgentEligibilityLifecycleReason::InvalidOrgChain
        );
        assert_eq!(
            elig.invokability_reason,
            AgentEligibilityLifecycleReason::InvalidOrgChain
        );
    }

    #[test]
    fn missing_manager_blocks_with_repair_guidance() {
        let target = agent("qa-3", "QA 3", "active", Some("missing-manager"));
        let health = get_agent_org_chain_health(&target, std::slice::from_ref(&target));
        assert_eq!(health.status, AgentOrgChainHealthStatus::InvalidOrgChain);
        assert_eq!(health.reason, AgentOrgChainInvalidReason::MissingManager);
        assert_eq!(health.full_chain.len(), 2);
        assert_eq!(health.full_chain[1].id, "missing-manager");
        assert_eq!(health.full_chain[1].status, "missing");
        assert_eq!(health.full_chain[1].reports_to, None);
        assert!(health
            .repair_guidance
            .as_deref()
            .unwrap_or("")
            .contains("QA 3 reports to missing manager missing-manager"));

        let elig = get_agent_work_eligibility(&target, std::slice::from_ref(&target));
        assert!(!elig.assignable);
        assert!(!elig.invokable);
        assert_eq!(
            elig.assignability_reason,
            AgentEligibilityLifecycleReason::InvalidOrgChain
        );
    }

    #[test]
    fn cycle_blocks_with_repair_guidance() {
        let target = agent("qa-4", "QA 4", "active", Some("cto-4"));
        let manager = agent("cto-4", "CTO 4", "active", Some("qa-4"));
        let agents = vec![target.clone(), manager];

        let health = get_agent_org_chain_health(&target, &agents);
        assert_eq!(health.status, AgentOrgChainHealthStatus::InvalidOrgChain);
        assert_eq!(health.reason, AgentOrgChainInvalidReason::Cycle);
        assert_eq!(health.full_chain.len(), 3);
        assert_eq!(health.full_chain[2].id, "qa-4");
        assert_eq!(health.full_chain[2].status, "cycle");
        assert!(health
            .repair_guidance
            .as_deref()
            .unwrap_or("")
            .contains("QA 4 has a cycle in its reporting chain"));

        let elig = get_agent_work_eligibility(&target, &agents);
        assert!(!elig.assignable);
        assert!(!elig.invokable);
        assert_eq!(
            elig.assignability_reason,
            AgentEligibilityLifecycleReason::InvalidOrgChain
        );
    }

    #[test]
    fn cross_company_manager_is_treated_as_missing() {
        // manager lives in a different company; org chain should be invalid_org_chain / missing_manager.
        let mut other = agent("manager-x", "Other CTO", "active", None);
        other.company_id = "company-2".to_string();
        let target = agent("agent-1", "Coder", "active", Some("manager-x"));
        let agents = vec![target.clone(), other];

        let health = get_agent_org_chain_health(&target, &agents);
        assert_eq!(health.status, AgentOrgChainHealthStatus::InvalidOrgChain);
        assert_eq!(health.reason, AgentOrgChainInvalidReason::MissingManager);
        assert_eq!(health.full_chain.last().unwrap().status, "missing");
    }

    #[test]
    fn root_agent_with_null_reports_to_is_healthy() {
        let root = agent("ceo", "CEO", "active", None);
        let agents = vec![root.clone()];
        let elig = get_agent_work_eligibility(&root, &agents);
        assert!(elig.assignable);
        assert!(elig.invokable);
        assert_eq!(
            elig.org_chain_health.status,
            AgentOrgChainHealthStatus::Healthy
        );
        assert_eq!(elig.org_chain_health.full_chain.len(), 1);
    }

    #[test]
    fn reason_as_str_round_trip() {
        for r in [
            AgentEligibilityLifecycleReason::Eligible,
            AgentEligibilityLifecycleReason::Terminated,
            AgentEligibilityLifecycleReason::PendingApproval,
            AgentEligibilityLifecycleReason::Paused,
            AgentEligibilityLifecycleReason::InvalidOrgChain,
            AgentEligibilityLifecycleReason::UnknownStatus,
        ] {
            let json = serde_json::to_string(&r).unwrap();
            assert_eq!(json, format!("\"{}\"", r.as_str()));
        }
    }
}

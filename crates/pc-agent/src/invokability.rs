#![forbid(unsafe_code)]
//! 评估 agent 是否可被调用（原 `pc-agent-invokability` 已下沉） —— 评估 agent 是否可被调用。
//!
//! 对应 Node `server/src/services/agent-invokability.ts`（164 行）。
//!
//! 设计目标：1:1 复刻
//! - `AgentInvokability` 枚举：`{invokable: true}` 或 `{invokable: false, reason, message, details, invalidOrgChain}`
//! - `AgentInvokabilityBlockReason` 枚举（10 个变体）
//! - `evaluateAgentInvokability(agent, companyAgents)` —— 决策函数
//! - `listInvalidOrgChainDescendantIds` —— DFS 列出因 terminated 上级导致的失效子孙
//! - `shouldCancelRunsForNonInvokableAgent` —— 判断是否取消 run
//!
//! 上层 `evaluateAgentInvokabilityFromDb`（DB 读取）由调用方组装：
//! 先 `db.select(...).from(agents)...` 拿 companyAgents，再调用本 crate。

use std::collections::{HashMap, HashSet};

/// Agent status 枚举 —— 与 Node `AgentStatus` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Paused,
    Terminated,
    PendingApproval,
    Active,
    Unknown,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::Terminated => "terminated",
            Self::PendingApproval => "pending_approval",
            Self::Active => "active",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "paused" => Some(Self::Paused),
            "terminated" => Some(Self::Terminated),
            "pending_approval" => Some(Self::PendingApproval),
            "active" => Some(Self::Active),
            _ => None,
        }
    }
}

/// Agent org row（最小子集）。
#[derive(Debug, Clone)]
pub struct AgentOrgRow {
    pub id: String,
    pub company_id: String,
    pub name: String,
    pub reports_to: Option<String>,
    pub status: AgentStatus,
}

/// Invokability 阻断原因 —— 与 Node `AgentInvokabilityBlockReason` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentInvokabilityBlockReason {
    Missing,
    Paused,
    Terminated,
    PendingApproval,
    UnknownStatus,
    ManagerMissing,
    ManagerCompanyMismatch,
    ManagerTerminated,
    ReportingCycle,
    ReportingChainTooDeep,
}

/// Org chain 健康状态。
#[derive(Debug, Clone)]
pub enum OrgChainHealth {
    Healthy,
    TerminatedAncestor {
        first_invalid_ancestor: Option<AgentOrgRow>,
    },
    Cycle {
        first_invalid_ancestor: Option<AgentOrgRow>,
    },
    Missing {
        first_invalid_ancestor: Option<AgentOrgRow>,
    },
}

/// 评估结果 —— 与 Node `AgentInvokability` 1:1 对齐。
#[derive(Debug, Clone)]
pub enum AgentInvokability {
    Invokable,
    NotInvokable {
        reason: AgentInvokabilityBlockReason,
        message: String,
        details: serde_json::Value,
        invalid_org_chain: bool,
    },
}

impl AgentInvokability {
    pub fn is_invokable(&self) -> bool {
        matches!(self, Self::Invokable)
    }
}

/// 直接判定不可调用的状态集合 —— 与 Node `DIRECT_NON_INVOKABLE_STATUSES` 1:1 对齐。
pub fn direct_non_invokable_statuses() -> &'static [AgentStatus] {
    &[
        AgentStatus::Paused,
        AgentStatus::Terminated,
        AgentStatus::PendingApproval,
    ]
}

fn status_block_reason(status: AgentStatus) -> Option<AgentInvokabilityBlockReason> {
    match status {
        AgentStatus::Paused => Some(AgentInvokabilityBlockReason::Paused),
        AgentStatus::Terminated => Some(AgentInvokabilityBlockReason::Terminated),
        AgentStatus::PendingApproval => Some(AgentInvokabilityBlockReason::PendingApproval),
        _ => None,
    }
}

/// 评估 agent invokability。
///
/// 与 Node `evaluateAgentInvokability` 1:1 对齐：
/// 1. agent 不存在 → `missing`
/// 2. status 阻断 → 对应 status reason
/// 3. org chain 问题 → `manager_*` / `reporting_*`
pub fn evaluate_agent_invokability(
    agent: Option<&AgentOrgRow>,
    company_agents: &[AgentOrgRow],
) -> AgentInvokability {
    let Some(agent) = agent else {
        return AgentInvokability::NotInvokable {
            reason: AgentInvokabilityBlockReason::Missing,
            message: "Agent no longer exists".to_string(),
            details: serde_json::json!({}),
            invalid_org_chain: false,
        };
    };

    let health = compute_org_chain_health(agent, company_agents);

    // 直接 status 阻断
    if let Some(reason) = status_block_reason(agent.status) {
        return AgentInvokability::NotInvokable {
            reason,
            message: "Agent is not invokable in its current state".to_string(),
            details: serde_json::json!({
                "agentId": agent.id,
                "agentStatus": agent.status.as_str(),
            }),
            invalid_org_chain: false,
        };
    }

    // org chain 健康 → 可调用
    if matches!(health, OrgChainHealth::Healthy) {
        return AgentInvokability::Invokable;
    }

    // org chain 不健康
    let (reason, first_invalid_ancestor) = match &health {
        OrgChainHealth::TerminatedAncestor {
            first_invalid_ancestor,
        } => (
            AgentInvokabilityBlockReason::ManagerTerminated,
            first_invalid_ancestor.clone(),
        ),
        OrgChainHealth::Cycle {
            first_invalid_ancestor,
        } => (
            AgentInvokabilityBlockReason::ReportingCycle,
            first_invalid_ancestor.clone(),
        ),
        OrgChainHealth::Missing {
            first_invalid_ancestor,
        } => (
            AgentInvokabilityBlockReason::ManagerMissing,
            first_invalid_ancestor.clone(),
        ),
        OrgChainHealth::Healthy => unreachable!(),
    };

    AgentInvokability::NotInvokable {
        reason,
        message: "Agent is not invokable because its reporting chain is invalid".to_string(),
        details: serde_json::json!({
            "agentId": agent.id,
            "managerId": first_invalid_ancestor.as_ref().map(|a| &a.id),
            "managerStatus": first_invalid_ancestor.as_ref().map(|a| a.status.as_str()),
        }),
        invalid_org_chain: true,
    }
}

/// 计算 org chain 健康状态 —— 等价于 Node `getAgentWorkEligibility` 中的
/// `orgChainHealth` 部分。
fn compute_org_chain_health(agent: &AgentOrgRow, company_agents: &[AgentOrgRow]) -> OrgChainHealth {
    // DFS 上溯 reports_to
    let by_id: HashMap<&str, &AgentOrgRow> =
        company_agents.iter().map(|a| (a.id.as_str(), a)).collect();

    let mut visited: HashSet<String> = HashSet::new();
    let mut current: Option<&AgentOrgRow> = Some(agent);
    while let Some(node) = current {
        if !visited.insert(node.id.clone()) {
            // cycle
            return OrgChainHealth::Cycle {
                first_invalid_ancestor: Some(node.clone()),
            };
        }
        let reports_to = match &node.reports_to {
            Some(id) => id,
            None => return OrgChainHealth::Healthy,
        };
        match by_id.get(reports_to.as_str()) {
            Some(parent) => {
                if parent.status == AgentStatus::Terminated {
                    return OrgChainHealth::TerminatedAncestor {
                        first_invalid_ancestor: Some((*parent).clone()),
                    };
                }
                current = Some(*parent);
            }
            None => {
                return OrgChainHealth::Missing {
                    first_invalid_ancestor: Some(node.clone()),
                };
            }
        }
    }
    OrgChainHealth::Healthy
}

/// 列出因 terminated 上级而失效的所有子孙 id。
///
/// 与 Node `listInvalidOrgChainDescendantIds` 1:1 对齐：
/// - 从 `terminatedAgentId` 出发，BFS/DFS 走 `reports_to`
/// - visited 防止环
/// - 只把 non-terminated 节点加入结果
pub fn list_invalid_org_chain_descendant_ids(
    terminated_agent_id: &str,
    company_agents: &[AgentOrgRow],
) -> Vec<String> {
    let mut by_manager: HashMap<Option<&str>, Vec<&AgentOrgRow>> = HashMap::new();
    for row in company_agents {
        by_manager
            .entry(row.reports_to.as_deref())
            .or_default()
            .push(row);
    }

    let mut invalid: Vec<String> = Vec::new();
    let mut stack: Vec<&AgentOrgRow> = by_manager
        .get(&Some(terminated_agent_id))
        .cloned()
        .unwrap_or_default();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(terminated_agent_id.to_string());

    while let Some(current) = stack.pop() {
        if !seen.insert(current.id.clone()) {
            continue;
        }
        if current.status != AgentStatus::Terminated {
            invalid.push(current.id.clone());
        }
        if let Some(children) = by_manager.get(&Some(current.id.as_str())) {
            stack.extend(children.iter().copied());
        }
    }
    invalid
}

/// 判断是否应该取消 agent 的运行。
///
/// 与 Node `shouldCancelRunsForNonInvokableAgent` 1:1 对齐：
/// - 当 `not invokable` 且 `reason == terminated` 或 `invalidOrgChain == true`
pub fn should_cancel_runs_for_non_invokable_agent(result: &AgentInvokability) -> bool {
    match result {
        AgentInvokability::Invokable => false,
        AgentInvokability::NotInvokable {
            reason,
            invalid_org_chain,
            ..
        } => *reason == AgentInvokabilityBlockReason::Terminated || *invalid_org_chain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, status: AgentStatus, reports_to: Option<&str>) -> AgentOrgRow {
        AgentOrgRow {
            id: id.to_string(),
            company_id: "co1".to_string(),
            name: id.to_string(),
            reports_to: reports_to.map(|s| s.to_string()),
            status,
        }
    }

    #[test]
    fn r707_evaluate_missing_agent() {
        let r = evaluate_agent_invokability(None, &[]);
        assert!(!r.is_invokable());
        match r {
            AgentInvokability::NotInvokable { reason, .. } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::Missing);
            }
            _ => panic!("expected NotInvokable"),
        }
    }

    #[test]
    fn r707_evaluate_active_with_no_reports_to_is_invokable() {
        let a = row("a1", AgentStatus::Active, None);
        let agents: Vec<AgentOrgRow> = vec![a.clone()];
        let r = evaluate_agent_invokability(Some(&a), &agents);
        assert!(r.is_invokable());
    }

    #[test]
    fn r707_evaluate_paused_returns_paused_reason() {
        let a = row("a1", AgentStatus::Paused, None);
        let agents: Vec<AgentOrgRow> = vec![a.clone()];
        let r = evaluate_agent_invokability(Some(&a), &agents);
        match r {
            AgentInvokability::NotInvokable {
                reason,
                invalid_org_chain,
                ..
            } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::Paused);
                assert!(!invalid_org_chain);
            }
            _ => panic!("expected NotInvokable"),
        }
    }

    #[test]
    fn r707_evaluate_terminated_returns_terminated_reason() {
        let a = row("a1", AgentStatus::Terminated, None);
        let agents: Vec<AgentOrgRow> = vec![a.clone()];
        let r = evaluate_agent_invokability(Some(&a), &agents);
        match r {
            AgentInvokability::NotInvokable {
                reason,
                invalid_org_chain,
                ..
            } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::Terminated);
                assert!(!invalid_org_chain);
            }
            _ => panic!("expected NotInvokable"),
        }
    }

    #[test]
    fn r707_evaluate_pending_approval_returns_pending_reason() {
        let a = row("a1", AgentStatus::PendingApproval, None);
        let agents: Vec<AgentOrgRow> = vec![a.clone()];
        let r = evaluate_agent_invokability(Some(&a), &agents);
        match r {
            AgentInvokability::NotInvokable { reason, .. } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::PendingApproval);
            }
            _ => panic!("expected NotInvokable"),
        }
    }

    #[test]
    fn r707_evaluate_manager_terminated() {
        let mgr = row("m1", AgentStatus::Terminated, None);
        let child = row("c1", AgentStatus::Active, Some("m1"));
        let r = evaluate_agent_invokability(Some(&child), &[mgr.clone(), child.clone()]);
        match r {
            AgentInvokability::NotInvokable {
                reason,
                invalid_org_chain,
                ..
            } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::ManagerTerminated);
                assert!(invalid_org_chain);
            }
            _ => panic!("expected NotInvokable"),
        }
    }

    #[test]
    fn r707_evaluate_manager_missing() {
        let child = row("c1", AgentStatus::Active, Some("ghost"));
        let agents = vec![child.clone()];
        let r = evaluate_agent_invokability(Some(&child), &agents);
        match r {
            AgentInvokability::NotInvokable {
                reason,
                invalid_org_chain,
                ..
            } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::ManagerMissing);
                assert!(invalid_org_chain);
            }
            _ => panic!("expected NotInvokable"),
        }
    }

    #[test]
    fn r707_evaluate_manager_cycle() {
        // a reports to b, b reports to a
        let a = row("a", AgentStatus::Active, Some("b"));
        let b = row("b", AgentStatus::Active, Some("a"));
        let agents = vec![a.clone(), b.clone()];
        let r = evaluate_agent_invokability(Some(&a), &agents);
        match r {
            AgentInvokability::NotInvokable {
                reason,
                invalid_org_chain,
                ..
            } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::ReportingCycle);
                assert!(invalid_org_chain);
            }
            _ => panic!("expected NotInvokable"),
        }
    }

    #[test]
    fn r707_evaluate_deep_chain_terminated_at_root() {
        // root(m1, terminated) → m2(active) → c1(active)
        let m1 = row("m1", AgentStatus::Terminated, None);
        let m2 = row("m2", AgentStatus::Active, Some("m1"));
        let c1 = row("c1", AgentStatus::Active, Some("m2"));
        let agents = vec![m1.clone(), m2.clone(), c1.clone()];
        let r = evaluate_agent_invokability(Some(&c1), &agents);
        match r {
            AgentInvokability::NotInvokable { reason, .. } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::ManagerTerminated);
            }
            _ => panic!("expected NotInvokable"),
        }
    }

    #[test]
    fn r707_status_round_trip() {
        for s in [
            AgentStatus::Paused,
            AgentStatus::Terminated,
            AgentStatus::PendingApproval,
            AgentStatus::Active,
        ] {
            assert_eq!(AgentStatus::from_str(s.as_str()), Some(s));
        }
        assert_eq!(AgentStatus::from_str("unknown"), None);
    }

    #[test]
    fn r707_descendants_lists_all_non_terminated() {
        // tree:
        //   root(terminated)
        //     ├─ m1(active)
        //     │   ├─ c1(active)
        //     │   └─ c2(terminated)
        //     └─ m2(active)
        let root = row("root", AgentStatus::Terminated, None);
        let m1 = row("m1", AgentStatus::Active, Some("root"));
        let m2 = row("m2", AgentStatus::Active, Some("root"));
        let c1 = row("c1", AgentStatus::Active, Some("m1"));
        let c2 = row("c2", AgentStatus::Terminated, Some("m1"));
        let agents = vec![root.clone(), m1.clone(), m2.clone(), c1.clone(), c2.clone()];
        let mut invalid = list_invalid_org_chain_descendant_ids("root", &agents);
        invalid.sort();
        assert_eq!(invalid, vec!["c1", "m1", "m2"]);
    }

    #[test]
    fn r707_descendants_empty_when_no_children() {
        let root = row("root", AgentStatus::Terminated, None);
        let invalid = list_invalid_org_chain_descendant_ids("root", &[root]);
        assert!(invalid.is_empty());
    }

    #[test]
    fn r707_descendants_unknown_root() {
        let a = row("a", AgentStatus::Active, None);
        let invalid = list_invalid_org_chain_descendant_ids("ghost", &[a]);
        assert!(invalid.is_empty());
    }

    #[test]
    fn r707_should_cancel_terminated() {
        let a = row("a", AgentStatus::Terminated, None);
        let agents: Vec<AgentOrgRow> = vec![a.clone()];
        let r = evaluate_agent_invokability(Some(&a), &agents);
        assert!(should_cancel_runs_for_non_invokable_agent(&r));
    }

    #[test]
    fn r707_should_cancel_invalid_org_chain() {
        let mgr = row("m1", AgentStatus::Terminated, None);
        let child = row("c1", AgentStatus::Active, Some("m1"));
        let r = evaluate_agent_invokability(Some(&child), &[mgr.clone(), child.clone()]);
        assert!(should_cancel_runs_for_non_invokable_agent(&r));
    }

    #[test]
    fn r707_should_not_cancel_paused() {
        let a = row("a", AgentStatus::Paused, None);
        let agents: Vec<AgentOrgRow> = vec![a.clone()];
        let r = evaluate_agent_invokability(Some(&a), &agents);
        assert!(!should_cancel_runs_for_non_invokable_agent(&r));
    }

    #[test]
    fn r707_should_not_cancel_invokable() {
        let a = row("a", AgentStatus::Active, None);
        let agents: Vec<AgentOrgRow> = vec![a.clone()];
        let r = evaluate_agent_invokability(Some(&a), &agents);
        assert!(!should_cancel_runs_for_non_invokable_agent(&r));
    }

    #[test]
    fn r707_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AgentOrgRow>();
    }
}

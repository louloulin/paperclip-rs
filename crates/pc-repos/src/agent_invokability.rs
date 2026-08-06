//! Agent 可调用性校验（对齐 Node `server/src/services/agent-invokability.ts`，164 行）。
//!
//! 单一职责：复用 `pc_core::agent_eligibility` 的纯规则，
//! 输出 Node 兼容的 `AgentInvokability` 判别式形状 + 取消 run 决策 + 组织树后代枚举。
//!
//! 与 `agent_assignability.rs` 的关系：
//! - assignability 是「能否被分配工作」（paused 仍可被分配）
//! - invokability 是「能否启动 run」（paused 不可启动）
//! - 两者共享 pc-core 纯规则，但上层业务语义不同 → 拆为两个 module

use serde::{Deserialize, Serialize};
use sqlx::types::Json;
use uuid::Uuid;

use pc_core::agent_eligibility::{
    get_agent_work_eligibility, AgentEligibilityAgent, AgentOrgChainHealth,
    AgentOrgChainHealthStatus, AgentOrgChainInvalidReason,
};

use crate::agent::AgentRow;
use crate::Db;

/// Node `AgentOrgRow` 的 Rust 等价：从 `agents` 表投影出的最小列集合。
///
/// 字段顺序与 `id / companyId / name / reportsTo / status` 严格对齐（Node `Pick<...>`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOrgRow {
    pub id: Uuid,
    #[serde(rename = "companyId")]
    pub company_id: Uuid,
    pub name: String,
    #[serde(rename = "reportsTo")]
    pub reports_to: Option<Uuid>,
    pub status: String,
}

impl AgentOrgRow {
    /// 从 `AgentRow` 投影出 `AgentOrgRow`（与 Node `Pick<typeof agents.$inferSelect, ...>` 1:1）。
    pub fn from_agent_row(row: &AgentRow) -> Self {
        Self {
            id: row.id,
            company_id: row.company_id,
            name: row.name.clone(),
            reports_to: row.reports_to,
            status: row.status.clone(),
        }
    }
}

/// Invokability 失败原因（与 Node `AgentInvokabilityBlockReason` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

impl AgentInvokabilityBlockReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Paused => "paused",
            Self::Terminated => "terminated",
            Self::PendingApproval => "pending_approval",
            Self::UnknownStatus => "unknown_status",
            Self::ManagerMissing => "manager_missing",
            Self::ManagerCompanyMismatch => "manager_company_mismatch",
            Self::ManagerTerminated => "manager_terminated",
            Self::ReportingCycle => "reporting_cycle",
            Self::ReportingChainTooDeep => "reporting_chain_too_deep",
        }
    }
}

/// Invokability 阻塞详情（与 Node `details: Record<string, unknown>` 1:1 对齐）。
///
/// Node 端是 free-form object；这里用结构化字段保留 Node 的具体 key 集合，
/// 同时保留 `extra: serde_json::Value` 以接受任何额外字段（保持向后兼容）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInvokabilityDetails {
    #[serde(rename = "agentId", skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,
    #[serde(rename = "agentStatus", skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<String>,
    #[serde(rename = "managerId", skip_serializing_if = "Option::is_none")]
    pub manager_id: Option<String>,
    #[serde(rename = "managerStatus", skip_serializing_if = "Option::is_none")]
    pub manager_status: Option<String>,
    #[serde(
        rename = "reportingChainAgentIds",
        skip_serializing_if = "Option::is_none"
    )]
    pub reporting_chain_agent_ids: Option<Vec<String>>,
    #[serde(rename = "orgChainHealth", skip_serializing_if = "Option::is_none")]
    pub org_chain_health: Option<Json<AgentOrgChainHealth>>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

impl Default for AgentInvokabilityDetails {
    fn default() -> Self {
        Self {
            agent_id: None,
            agent_status: None,
            manager_id: None,
            manager_status: None,
            reporting_chain_agent_ids: None,
            org_chain_health: None,
            extra: serde_json::Value::Object(Default::default()),
        }
    }
}

/// Agent 可调用性评估结果（与 Node `AgentInvokability` 判别式 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "invokable", rename_all = "snake_case")]
pub enum AgentInvokability {
    #[serde(rename = "true")]
    Invokable,
    #[serde(rename = "false")]
    Blocked {
        reason: AgentInvokabilityBlockReason,
        message: String,
        details: AgentInvokabilityDetails,
        #[serde(rename = "invalidOrgChain")]
        invalid_org_chain: bool,
    },
}

impl AgentInvokability {
    pub fn is_invokable(&self) -> bool {
        matches!(self, Self::Invokable)
    }

    pub fn reason(&self) -> Option<AgentInvokabilityBlockReason> {
        match self {
            Self::Invokable => None,
            Self::Blocked { reason, .. } => Some(*reason),
        }
    }
}

/// 直接导致不可调用的 status 集合（与 Node `DIRECT_NON_INVOKABLE_STATUSES` 1:1 对齐）。
pub const DIRECT_NON_INVOKABLE_STATUSES: &[&str] = &["paused", "terminated", "pending_approval"];

/// 评估 agent 可调用性（纯函数，与 Node `evaluateAgentInvokability` 1:1 对齐）。
///
/// - `agent = None` → `Blocked { reason: Missing, message: "Agent no longer exists", ... }`
/// - 调 `pc_core::agent_eligibility::get_agent_work_eligibility` 计算 invokabilityReason
/// - 直接 status 阻断（paused / terminated / pending_approval / unknown_status）走 status 分支
/// - org chain 阻断走 invalid_org_chain 分支，附带 full chain 元数据
pub fn evaluate_agent_invokability(
    agent: Option<&AgentOrgRow>,
    company_agents: &[AgentOrgRow],
) -> AgentInvokability {
    let Some(agent) = agent else {
        return blocked(
            AgentInvokabilityBlockReason::Missing,
            "Agent no longer exists",
            AgentInvokabilityDetails::default(),
            false,
        );
    };

    let eligibility = get_agent_work_eligibility(
        &to_eligibility_agent(agent),
        &to_eligibility_agents(company_agents),
    );
    if eligibility.invokable {
        return AgentInvokability::Invokable;
    }

    let direct_status_reason = if eligibility.invokability_reason
        == pc_core::agent_eligibility::AgentEligibilityLifecycleReason::UnknownStatus
    {
        Some(AgentInvokabilityBlockReason::UnknownStatus)
    } else {
        status_block_reason(&agent.status)
    };
    if let Some(reason) = direct_status_reason {
        let mut details = AgentInvokabilityDetails::default();
        details.agent_id = Some(agent.id);
        details.agent_status = Some(agent.status.clone());
        return blocked(
            reason,
            "Agent is not invokable in its current state",
            details,
            false,
        );
    }

    let health = &eligibility.org_chain_health;
    let first_invalid_ancestor = health.first_invalid_ancestor.as_ref();
    let mut details = AgentInvokabilityDetails::default();
    details.agent_id = Some(agent.id);
    details.manager_id = first_invalid_ancestor.map(|a| a.id.clone());
    details.manager_status = first_invalid_ancestor.map(|a| a.status.clone());
    details.reporting_chain_agent_ids = Some(
        health
            .full_chain
            .iter()
            .filter(|entry| {
                entry.relation == pc_core::agent_eligibility::AgentOrgChainRelation::Ancestor
            })
            .map(|entry| entry.id.clone())
            .collect(),
    );
    details.org_chain_health = Some(Json(health.clone()));

    blocked(
        invalid_chain_reason(health),
        "Agent is not invokable because its reporting chain is invalid",
        details,
        true,
    )
}

/// 从 DB 拉 company agents 后评估 invokability（与 Node `evaluateAgentInvokabilityFromDb` 1:1 对齐）。
///
/// `agent = None` 时直接返回 `evaluate_agent_invokability(None, &[])`，
/// 不发起 DB 查询（与 Node `if (!agent) return evaluateAgentInvokability(agent, [])` 行为一致）。
pub async fn evaluate_agent_invokability_from_db(
    db: &Db,
    agent: Option<&AgentOrgRow>,
) -> Result<AgentInvokability, sqlx::Error> {
    let Some(agent) = agent else {
        return Ok(evaluate_agent_invokability(None, &[]));
    };
    let rows: Vec<AgentOrgRow> = sqlx::query_as::<_, (Uuid, Uuid, String, Option<Uuid>, String)>(
        r#"
        SELECT id, company_id, name, reports_to, status
        FROM agents
        WHERE company_id = $1
        "#,
    )
    .bind(agent.company_id)
    .fetch_all(db.pool())
    .await?
    .into_iter()
    .map(|(id, company_id, name, reports_to, status)| AgentOrgRow {
        id,
        company_id,
        name,
        reports_to,
        status,
    })
    .collect();

    Ok(evaluate_agent_invokability(Some(agent), &rows))
}

/// 列出某个 terminated root agent 的「非 terminated 后代 id」（与 Node `listInvalidOrgChainDescendantIds` 1:1 对齐）。
///
/// 行为：
/// - 按 `reports_to` 建索引，BFS / DFS 遍历以 `terminatedAgentId` 为根的子树
/// - 跳过自身（`seen` 集合初始包含 `terminatedAgentId`）
/// - 只收集 status != "terminated" 的后代
/// - 遇到 cycle 通过 `seen` 集合防环
pub fn list_invalid_org_chain_descendant_ids(
    terminated_agent_id: Uuid,
    company_agents: &[AgentOrgRow],
) -> Vec<Uuid> {
    let mut by_manager: std::collections::HashMap<Option<Uuid>, Vec<Uuid>> =
        std::collections::HashMap::new();
    for row in company_agents {
        by_manager.entry(row.reports_to).or_default().push(row.id);
    }

    let mut invalid_descendant_ids: Vec<Uuid> = Vec::new();
    let mut stack: Vec<Uuid> = by_manager
        .get(&Some(terminated_agent_id))
        .cloned()
        .unwrap_or_default();
    let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    seen.insert(terminated_agent_id);
    // 索引 agent status（按 id 查 status）
    let status_by_id: std::collections::HashMap<Uuid, &str> = company_agents
        .iter()
        .map(|row| (row.id, row.status.as_str()))
        .collect();

    while let Some(current_id) = stack.pop() {
        if seen.contains(&current_id) {
            continue;
        }
        seen.insert(current_id);
        let current_status = status_by_id.get(&current_id).copied().unwrap_or("");
        if current_status != "terminated" {
            invalid_descendant_ids.push(current_id);
        }
        if let Some(children) = by_manager.get(&Some(current_id)) {
            stack.extend(children.iter().copied());
        }
    }
    invalid_descendant_ids
}

/// 是否应取消 agent 的运行（与 Node `shouldCancelRunsForNonInvokableAgent` 1:1 对齐）。
///
/// 判定：`!invokable && (reason == "terminated" || invalid_org_chain)`
#[must_use]
pub fn should_cancel_runs_for_non_invokable_agent(result: &AgentInvokability) -> bool {
    match result {
        AgentInvokability::Invokable => false,
        AgentInvokability::Blocked {
            reason,
            invalid_org_chain,
            ..
        } => *reason == AgentInvokabilityBlockReason::Terminated || *invalid_org_chain,
    }
}

// ---- private helpers ----

fn blocked(
    reason: AgentInvokabilityBlockReason,
    message: impl Into<String>,
    details: AgentInvokabilityDetails,
    invalid_org_chain: bool,
) -> AgentInvokability {
    AgentInvokability::Blocked {
        reason,
        message: message.into(),
        details,
        invalid_org_chain,
    }
}

fn status_block_reason(status: &str) -> Option<AgentInvokabilityBlockReason> {
    if status == "paused" {
        Some(AgentInvokabilityBlockReason::Paused)
    } else if status == "terminated" {
        Some(AgentInvokabilityBlockReason::Terminated)
    } else if status == "pending_approval" {
        Some(AgentInvokabilityBlockReason::PendingApproval)
    } else {
        None
    }
}

fn invalid_chain_reason(health: &AgentOrgChainHealth) -> AgentInvokabilityBlockReason {
    if health.status != AgentOrgChainHealthStatus::InvalidOrgChain {
        return AgentInvokabilityBlockReason::ManagerMissing;
    }
    match health.reason {
        AgentOrgChainInvalidReason::TerminatedAncestor => {
            AgentInvokabilityBlockReason::ManagerTerminated
        }
        AgentOrgChainInvalidReason::Cycle => AgentInvokabilityBlockReason::ReportingCycle,
        AgentOrgChainInvalidReason::MissingManager | AgentOrgChainInvalidReason::Healthy => {
            AgentInvokabilityBlockReason::ManagerMissing
        }
    }
}

fn to_eligibility_agent(row: &AgentOrgRow) -> AgentEligibilityAgent {
    AgentEligibilityAgent {
        id: row.id.to_string(),
        company_id: row.company_id.to_string(),
        name: row.name.clone(),
        status: row.status.clone(),
        reports_to: row.reports_to.map(|r| r.to_string()),
    }
}

fn to_eligibility_agents(rows: &[AgentOrgRow]) -> Vec<AgentEligibilityAgent> {
    rows.iter().map(to_eligibility_agent).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(id: &str, status: &str, reports_to: Option<&str>) -> AgentOrgRow {
        AgentOrgRow {
            id: Uuid::parse_str(id).unwrap_or_else(|_| Uuid::nil()),
            company_id: Uuid::nil(),
            name: id.to_string(),
            reports_to: reports_to.and_then(|s| Uuid::parse_str(s).ok()),
            status: status.to_string(),
        }
    }

    #[test]
    fn blocked_terminated_descendants_are_invalid_org_chain() {
        let rows = vec![
            agent("00000000-0000-0000-0000-000000000001", "terminated", None),
            agent(
                "00000000-0000-0000-0000-000000000002",
                "active",
                Some("00000000-0000-0000-0000-000000000001"),
            ),
            agent(
                "00000000-0000-0000-0000-000000000003",
                "active",
                Some("00000000-0000-0000-0000-000000000002"),
            ),
        ];
        let coder = rows[2].clone();
        let result = evaluate_agent_invokability(Some(&coder), &rows);
        match result {
            AgentInvokability::Blocked {
                reason,
                invalid_org_chain,
                details,
                ..
            } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::ManagerTerminated);
                assert!(invalid_org_chain);
                assert_eq!(
                    details.manager_id.as_deref(),
                    Some("00000000-0000-0000-0000-000000000001")
                );
                let chain = details.reporting_chain_agent_ids.unwrap();
                assert_eq!(chain.len(), 2);
                assert_eq!(chain[0], "00000000-0000-0000-0000-000000000002");
                assert_eq!(chain[1], "00000000-0000-0000-0000-000000000001");
            }
            _ => panic!("expected Blocked"),
        }
    }

    #[test]
    fn missing_manager_and_cycle_report_invalid_org_chain() {
        // missing manager
        let rows = vec![AgentOrgRow {
            id: Uuid::parse_str("00000000-0000-0000-0000-000000000010").unwrap(),
            company_id: Uuid::nil(),
            name: "agent".to_string(),
            reports_to: Some(Uuid::new_v4()),
            status: "active".to_string(),
        }];
        let result = evaluate_agent_invokability(Some(&rows[0]), &rows);
        match result {
            AgentInvokability::Blocked {
                reason,
                invalid_org_chain,
                ..
            } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::ManagerMissing);
                assert!(invalid_org_chain);
            }
            _ => panic!("expected Blocked"),
        }

        // cycle
        let rows = vec![
            agent(
                "00000000-0000-0000-0000-000000000020",
                "active",
                Some("00000000-0000-0000-0000-000000000021"),
            ),
            agent(
                "00000000-0000-0000-0000-000000000021",
                "active",
                Some("00000000-0000-0000-0000-000000000020"),
            ),
        ];
        let result = evaluate_agent_invokability(Some(&rows[0]), &rows);
        match result {
            AgentInvokability::Blocked {
                reason,
                invalid_org_chain,
                ..
            } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::ReportingCycle);
                assert!(invalid_org_chain);
            }
            _ => panic!("expected Blocked"),
        }
    }

    #[test]
    fn list_invalid_org_chain_descendant_ids_skips_terminated_and_other_roots() {
        let rows = vec![
            agent("00000000-0000-0000-0000-000000000030", "terminated", None), // ceo
            agent(
                "00000000-0000-0000-0000-000000000031",
                "active",
                Some("00000000-0000-0000-0000-000000000030"),
            ), // cto
            agent(
                "00000000-0000-0000-0000-000000000032",
                "active",
                Some("00000000-0000-0000-0000-000000000031"),
            ), // coder
            agent(
                "00000000-0000-0000-0000-000000000033",
                "terminated",
                Some("00000000-0000-0000-0000-000000000031"),
            ), // old-coder
            agent("00000000-0000-0000-0000-000000000034", "active", None),     // other-root
        ];
        let mut result = list_invalid_org_chain_descendant_ids(Uuid::nil(), &rows);
        // Use the real id instead
        let ceo = Uuid::parse_str("00000000-0000-0000-0000-000000000030").unwrap();
        result = list_invalid_org_chain_descendant_ids(ceo, &rows);
        let mut sorted: Vec<String> = result.iter().map(|u| u.to_string()).collect();
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                "00000000-0000-0000-0000-000000000031".to_string(),
                "00000000-0000-0000-0000-000000000032".to_string(),
            ]
        );
    }

    #[test]
    fn list_invalid_org_chain_descendant_ids_handles_no_descendants() {
        let rows = vec![agent(
            "00000000-0000-0000-0000-000000000040",
            "active",
            None,
        )];
        let id = Uuid::parse_str("00000000-0000-0000-0000-000000000040").unwrap();
        assert!(list_invalid_org_chain_descendant_ids(id, &rows).is_empty());
    }

    #[test]
    fn list_invalid_org_chain_descendant_ids_protects_against_cycles() {
        // a -> b -> a
        let rows = vec![
            agent(
                "00000000-0000-0000-0000-000000000050",
                "active",
                Some("00000000-0000-0000-0000-000000000051"),
            ),
            agent(
                "00000000-0000-0000-0000-000000000051",
                "active",
                Some("00000000-0000-0000-0000-000000000050"),
            ),
        ];
        let start = Uuid::parse_str("00000000-0000-0000-0000-000000000050").unwrap();
        // Should not infinite-loop. Returns b (non-terminated) at most once.
        let result = list_invalid_org_chain_descendant_ids(start, &rows);
        assert!(result.len() <= 1);
    }

    #[test]
    fn agent_org_row_from_agent_row_preserves_fields() {
        // Construct a minimal AgentRow and verify projection.
        let row = AgentRow {
            id: Uuid::nil(),
            company_id: Uuid::nil(),
            name: "tester".to_string(),
            role: "engineer".to_string(),
            title: None,
            icon: None,
            status: "active".to_string(),
            reports_to: None,
            capabilities: None,
            adapter_type: "codex_local".to_string(),
            adapter_config: json!({}),
            runtime_config: json!({}),
            default_environment_id: None,
            budget_monthly_cents: 0,
            spent_monthly_cents: 0,
            pause_reason: None,
            paused_at: None,
            error_reason: None,
            permissions: json!({}),
            last_heartbeat_at: None,
            metadata: None,
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        };
        let org = AgentOrgRow::from_agent_row(&row);
        assert_eq!(org.id, row.id);
        assert_eq!(org.company_id, row.company_id);
        assert_eq!(org.name, "tester");
        assert_eq!(org.reports_to, None);
        assert_eq!(org.status, "active");
    }

    #[test]
    fn null_agent_returns_missing_block() {
        let result = evaluate_agent_invokability(None, &[]);
        match result {
            AgentInvokability::Blocked {
                reason,
                message,
                invalid_org_chain,
                ..
            } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::Missing);
                assert_eq!(message, "Agent no longer exists");
                assert!(!invalid_org_chain);
            }
            _ => panic!("expected Blocked"),
        }
    }

    #[test]
    fn healthy_active_agent_is_invokable() {
        let rows = vec![
            agent("00000000-0000-0000-0000-000000000060", "active", None),
            agent(
                "00000000-0000-0000-0000-000000000061",
                "active",
                Some("00000000-0000-0000-0000-000000000060"),
            ),
        ];
        let target = rows[1].clone();
        assert!(matches!(
            evaluate_agent_invokability(Some(&target), &rows),
            AgentInvokability::Invokable
        ));
    }

    #[test]
    fn paused_agent_blocked_with_paused_reason() {
        let rows = vec![
            agent("00000000-0000-0000-0000-000000000070", "active", None),
            agent(
                "00000000-0000-0000-0000-000000000071",
                "paused",
                Some("00000000-0000-0000-0000-000000000070"),
            ),
        ];
        let target = rows[1].clone();
        let result = evaluate_agent_invokability(Some(&target), &rows);
        match result {
            AgentInvokability::Blocked {
                reason,
                invalid_org_chain,
                ..
            } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::Paused);
                assert!(!invalid_org_chain);
            }
            _ => panic!("expected Blocked"),
        }
    }

    #[test]
    fn unknown_status_blocked_with_unknown_status_reason() {
        let rows = vec![
            agent("00000000-0000-0000-0000-000000000080", "active", None),
            agent(
                "00000000-0000-0000-0000-000000000081",
                "sabbatical",
                Some("00000000-0000-0000-0000-000000000080"),
            ),
        ];
        let target = rows[1].clone();
        let result = evaluate_agent_invokability(Some(&target), &rows);
        match result {
            AgentInvokability::Blocked {
                reason,
                message,
                details,
                invalid_org_chain,
                ..
            } => {
                assert_eq!(reason, AgentInvokabilityBlockReason::UnknownStatus);
                assert!(!invalid_org_chain);
                assert_eq!(message, "Agent is not invokable in its current state");
                assert_eq!(details.agent_status.as_deref(), Some("sabbatical"));
            }
            _ => panic!("expected Blocked"),
        }
    }

    #[test]
    fn should_cancel_runs_for_terminated_returns_true() {
        let result = AgentInvokability::Blocked {
            reason: AgentInvokabilityBlockReason::Terminated,
            message: "x".into(),
            details: AgentInvokabilityDetails::default(),
            invalid_org_chain: false,
        };
        assert!(should_cancel_runs_for_non_invokable_agent(&result));
    }

    #[test]
    fn should_cancel_runs_for_invalid_org_chain_returns_true() {
        let result = AgentInvokability::Blocked {
            reason: AgentInvokabilityBlockReason::ManagerTerminated,
            message: "x".into(),
            details: AgentInvokabilityDetails::default(),
            invalid_org_chain: true,
        };
        assert!(should_cancel_runs_for_non_invokable_agent(&result));
    }

    #[test]
    fn should_cancel_runs_for_paused_returns_false() {
        let result = AgentInvokability::Blocked {
            reason: AgentInvokabilityBlockReason::Paused,
            message: "x".into(),
            details: AgentInvokabilityDetails::default(),
            invalid_org_chain: false,
        };
        assert!(!should_cancel_runs_for_non_invokable_agent(&result));
    }

    #[test]
    fn should_cancel_runs_for_invokable_returns_false() {
        assert!(!should_cancel_runs_for_non_invokable_agent(
            &AgentInvokability::Invokable
        ));
    }

    #[test]
    fn direct_non_invokable_statuses_matches_node_set() {
        for s in DIRECT_NON_INVOKABLE_STATUSES {
            assert!(status_block_reason(s).is_some());
        }
        assert!(status_block_reason("active").is_none());
        assert!(status_block_reason("idle").is_none());
    }

    #[test]
    fn invokability_serializes_with_invokable_discriminator() {
        let blocked = AgentInvokability::Blocked {
            reason: AgentInvokabilityBlockReason::Paused,
            message: "x".into(),
            details: AgentInvokabilityDetails::default(),
            invalid_org_chain: false,
        };
        let json = serde_json::to_value(&blocked).unwrap();
        // 由于 enum 是非默认 tag 模式，verify 字段存在
        assert!(json.get("reason").is_some());
        assert_eq!(json["reason"], "paused");
        assert_eq!(json["message"], "x");
        assert_eq!(json["invalidOrgChain"], false);
    }

    #[test]
    fn invokability_reason_helper() {
        let invokable = AgentInvokability::Invokable;
        assert!(invokable.is_invokable());
        assert_eq!(invokable.reason(), None);

        let blocked = AgentInvokability::Blocked {
            reason: AgentInvokabilityBlockReason::ManagerTerminated,
            message: "x".into(),
            details: AgentInvokabilityDetails::default(),
            invalid_org_chain: true,
        };
        assert!(!blocked.is_invokable());
        assert_eq!(
            blocked.reason(),
            Some(AgentInvokabilityBlockReason::ManagerTerminated)
        );
    }

    #[test]
    fn block_reason_as_str_round_trip() {
        for r in [
            AgentInvokabilityBlockReason::Missing,
            AgentInvokabilityBlockReason::Paused,
            AgentInvokabilityBlockReason::Terminated,
            AgentInvokabilityBlockReason::PendingApproval,
            AgentInvokabilityBlockReason::UnknownStatus,
            AgentInvokabilityBlockReason::ManagerMissing,
            AgentInvokabilityBlockReason::ManagerCompanyMismatch,
            AgentInvokabilityBlockReason::ManagerTerminated,
            AgentInvokabilityBlockReason::ReportingCycle,
            AgentInvokabilityBlockReason::ReportingChainTooDeep,
        ] {
            assert_eq!(
                r.as_str(),
                serde_json::to_value(r).unwrap().as_str().unwrap()
            );
        }
    }
}

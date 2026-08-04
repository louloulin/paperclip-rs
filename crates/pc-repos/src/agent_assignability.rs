//! Agent 可分配性校验（对齐 Node `server/src/services/agent-assignability.ts`，171 行）。
//!
//! 单一职责：调用 `pc_core::agent_eligibility` 计算 agent 工作资格，
//! 把 Node `conflict(409) / notFound(404) / unprocessable(422)` 三类失败
//! 翻译为 Rust `Result<_, AgentAssignabilityError>`，并产出与 Node 兼容的
//! conflict details 形状（HTTP 层后续可以 1:1 渲染）。
//!
//! 不持有任何业务状态；所有 IO 都通过 `&Db` 完成。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_core::agent_eligibility::{
    get_agent_work_eligibility, AgentEligibilityAgent, AgentEligibilityLifecycleReason,
    AgentInvalidOrgChainAncestor, AgentOrgChainHealth, AgentOrgChainHealthStatus,
    AgentOrgChainInvalidReason, AgentOrgChainRelation, AgentOrgChainEntry, AgentWorkEligibility,
};

use crate::agent::AgentRepo;
use crate::Db;

/// 分配场景（与 Node `AgentAssignmentKind` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentAssignmentKind {
    Work,
    Routine,
}

impl AgentAssignmentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Routine => "routine",
        }
    }

    /// 默认 kind（与 Node `options.kind ?? "work"` 1:1 对齐）。
    pub const fn default_work() -> Self {
        Self::Work
    }
}

impl Default for AgentAssignmentKind {
    fn default() -> Self {
        Self::Work
    }
}

/// 冲突原因分类（与 Node `AgentAssignmentConflictReason` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAssignmentConflictReason {
    PendingApproval,
    AssigneeTerminated,
    AssigneeUnknownStatus,
    AncestorTerminated,
    AncestorMissing,
    AncestorCrossCompany,
    AncestorCycle,
    AncestorDepthExceeded,
}

impl AgentAssignmentConflictReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PendingApproval => "pending_approval",
            Self::AssigneeTerminated => "assignee_terminated",
            Self::AssigneeUnknownStatus => "assignee_unknown_status",
            Self::AncestorTerminated => "ancestor_terminated",
            Self::AncestorMissing => "ancestor_missing",
            Self::AncestorCrossCompany => "ancestor_cross_company",
            Self::AncestorCycle => "ancestor_cycle",
            Self::AncestorDepthExceeded => "ancestor_depth_exceeded",
        }
    }
}

/// 冲突错误详情（与 Node `conflictDetails(...)` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAssignabilityConflictDetails {
    #[serde(rename = "code")]
    pub code: &'static str,
    #[serde(rename = "reason")]
    pub reason: AgentAssignmentConflictReason,
    #[serde(rename = "companyId")]
    pub company_id: Uuid,
    #[serde(rename = "assigneeAgentId")]
    pub assignee_agent_id: Uuid,
    #[serde(rename = "invalidAncestorAgentId")]
    pub invalid_ancestor_agent_id: Option<Uuid>,
    #[serde(rename = "missingAncestorAgentId")]
    pub missing_ancestor_agent_id: Option<Uuid>,
    #[serde(rename = "ancestorChain")]
    pub ancestor_chain: Vec<ConflictChainEntry>,
}

/// 冲突 details 中的 ancestor chain 项（与 Node `chain.map(...)` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictChainEntry {
    pub id: Uuid,
    #[serde(rename = "companyId")]
    pub company_id: Uuid,
    pub status: String,
    #[serde(rename = "reportsTo")]
    pub reports_to: Option<Uuid>,
}

/// Agent 可分配性校验错误（对应 Node `conflict / notFound / unprocessable` 三类）。
#[derive(Debug, thiserror::Error)]
pub enum AgentAssignabilityError {
    #[error("Assignee agent not found")]
    NotFound,

    #[error("Assignee must belong to same company")]
    CrossCompany,

    /// 与 Node `conflict(message, details)` 等价：HTTP 409 + 业务 reason。
    #[error("{message}")]
    Conflict {
        message: String,
        details: AgentAssignabilityConflictDetails,
    },

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// 校验入口（与 Node `assertAssignableAgent` 1:1 对齐）。
///
/// - `agent_id` 为 `None` → 直接返回 `Ok(())`（与 Node `if (!agentId) return;` 对齐）
/// - `kind` 缺省为 `Work`（与 Node `options.kind ?? "work"` 对齐）
///
/// `db` 必须已经连接到目标 `companyId` 所在的 DB；本函数额外做跨公司校验。
pub async fn assert_assignable_agent(
    db: &Db,
    company_id: Uuid,
    agent_id: Option<Uuid>,
    options: AssertAssignableAgentOptions<'_>,
) -> Result<(), AgentAssignabilityError> {
    let Some(agent_id) = agent_id else {
        return Ok(());
    };
    let kind = options.kind.unwrap_or_default();

    let assignee = AgentRepo::new(db)
        .get(agent_id)
        .await?
        .ok_or(AgentAssignabilityError::NotFound)?;
    if assignee.company_id != company_id {
        return Err(AgentAssignabilityError::CrossCompany);
    }

    let company_agents = AgentRepo::new(db).list_by_company(company_id).await?;
    let eligibility = get_agent_work_eligibility(
        &to_eligibility_agent(&assignee),
        &to_eligibility_agents(&company_agents),
    );
    let chain = chain_to_conflict_entries(&eligibility, company_id);

    if eligibility.assignable {
        return Ok(());
    }

    if eligibility.assignability_reason == AgentEligibilityLifecycleReason::PendingApproval {
        return Err(AgentAssignabilityError::Conflict {
            message: assignment_message(kind, AgentAssignmentConflictReason::PendingApproval),
            details: make_conflict_details(
                company_id,
                agent_id,
                AgentAssignmentConflictReason::PendingApproval,
                &chain,
                None,
                None,
            ),
        });
    }
    if eligibility.assignability_reason == AgentEligibilityLifecycleReason::Terminated {
        return Err(AgentAssignabilityError::Conflict {
            message: assignment_message(kind, AgentAssignmentConflictReason::AssigneeTerminated),
            details: make_conflict_details(
                company_id,
                agent_id,
                AgentAssignmentConflictReason::AssigneeTerminated,
                &chain,
                None,
                None,
            ),
        });
    }
    if eligibility.assignability_reason == AgentEligibilityLifecycleReason::UnknownStatus {
        return Err(AgentAssignabilityError::Conflict {
            message: assignment_message(
                kind,
                AgentAssignmentConflictReason::AssigneeUnknownStatus,
            ),
            details: make_conflict_details(
                company_id,
                agent_id,
                AgentAssignmentConflictReason::AssigneeUnknownStatus,
                &chain,
                None,
                None,
            ),
        });
    }

    let reason = assignment_reason_from_health(&eligibility);
    let first_invalid = first_invalid_ancestor_uuid(&eligibility, company_id);
    let invalid_ancestor_agent_id = first_invalid
        .as_ref()
        .filter(|uuid| {
            // Node: `firstInvalidAncestor && firstInvalidAncestor.status !== "missing"`
            // 用 `Option` 上的 chain 表达同样的非 missing 判断。
            !is_missing_ancestor(&eligibility, uuid)
        })
        .copied();
    let missing_ancestor_agent_id = first_invalid
        .as_ref()
        .filter(|uuid| is_missing_ancestor(&eligibility, uuid))
        .copied();

    Err(AgentAssignabilityError::Conflict {
        message: assignment_message(kind, reason),
        details: make_conflict_details(
            company_id,
            agent_id,
            reason,
            &chain,
            invalid_ancestor_agent_id,
            missing_ancestor_agent_id,
        ),
    })
}

/// `assertAssignableAgent` 的选项（与 Node `options` 参数 1:1 对齐）。
#[derive(Debug, Clone, Copy, Default)]
pub struct AssertAssignableAgentOptions<'a> {
    pub kind: Option<AgentAssignmentKind>,
    /// 保留字段，给上层透传扩展；目前未使用。
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> AssertAssignableAgentOptions<'a> {
    pub fn new(kind: AgentAssignmentKind) -> Self {
        Self {
            kind: Some(kind),
            _phantom: std::marker::PhantomData,
        }
    }
}

// ---- pure helpers (testable independently of sqlx) ----

/// 单 agent 适配：`AgentRow` → `AgentEligibilityAgent`。
pub fn to_eligibility_agent(row: &crate::agent::AgentRow) -> AgentEligibilityAgent {
    AgentEligibilityAgent {
        id: row.id.to_string(),
        company_id: row.company_id.to_string(),
        name: row.name.clone(),
        status: row.status.clone(),
        reports_to: row.reports_to.map(|r| r.to_string()),
    }
}

/// 多 agent 批量适配。
pub fn to_eligibility_agents(rows: &[crate::agent::AgentRow]) -> Vec<AgentEligibilityAgent> {
    rows.iter().map(to_eligibility_agent).collect()
}

/// 把 eligibility 的 fullChain 转成 conflict details 用的 chain 项。
///
/// 注：Node 端只保留 `id / companyId / status / reportsTo` 四个字段；
/// 其余字段（name / depth / relation）丢弃，与 Node 行为 1:1。
pub fn chain_to_conflict_entries(
    eligibility: &AgentWorkEligibility,
    expected_company_id: Uuid,
) -> Vec<ConflictChainEntry> {
    eligibility
        .org_chain_health
        .full_chain
        .iter()
        .map(|entry| {
            let company_uuid = Uuid::parse_str(&entry.company_id).unwrap_or(expected_company_id);
            let reports_to = entry.reports_to.as_ref().and_then(|s| Uuid::parse_str(s).ok());
            ConflictChainEntry {
                id: Uuid::parse_str(&entry.id).unwrap_or_else(|_| Uuid::nil()),
                company_id: company_uuid,
                status: entry.status.clone(),
                reports_to,
            }
        })
        .collect()
}

/// 构造与 Node `conflictDetails(...)` 1:1 对齐的 details 对象。
pub fn make_conflict_details(
    company_id: Uuid,
    assignee_agent_id: Uuid,
    reason: AgentAssignmentConflictReason,
    chain: &[ConflictChainEntry],
    invalid_ancestor_agent_id: Option<Uuid>,
    missing_ancestor_agent_id: Option<Uuid>,
) -> AgentAssignabilityConflictDetails {
    AgentAssignabilityConflictDetails {
        code: "agent_not_assignable",
        reason,
        company_id,
        assignee_agent_id,
        invalid_ancestor_agent_id,
        missing_ancestor_agent_id,
        ancestor_chain: chain.to_vec(),
    }
}

/// 业务文案（与 Node `assignmentMessage(kind, reason)` 1:1 对齐）。
pub fn assignment_message(kind: AgentAssignmentKind, reason: AgentAssignmentConflictReason) -> String {
    let subject = match kind {
        AgentAssignmentKind::Work => "work",
        AgentAssignmentKind::Routine => "routines",
    };
    match reason {
        AgentAssignmentConflictReason::PendingApproval => format!(
            "Cannot assign {subject} to pending approval agents"
        ),
        AgentAssignmentConflictReason::AssigneeTerminated => format!(
            "Cannot assign {subject} to terminated agents"
        ),
        AgentAssignmentConflictReason::AssigneeUnknownStatus => format!(
            "Cannot assign {subject} to agents with an unsupported lifecycle status"
        ),
        AgentAssignmentConflictReason::AncestorTerminated
        | AgentAssignmentConflictReason::AncestorMissing
        | AgentAssignmentConflictReason::AncestorCrossCompany
        | AgentAssignmentConflictReason::AncestorCycle
        | AgentAssignmentConflictReason::AncestorDepthExceeded => format!(
            "Cannot assign {subject} to agents with an invalid org chain"
        ),
    }
}

/// 把 pc-core 的 chain health reason 映射为 Node `assignmentReasonFromHealth` 的冲突 reason。
///
/// 注：Node `assignmentReasonFromHealth` 的缺省值是 `ancestor_missing`（fallback case），
/// Rust 用 `match` 的最后一支显式表达同样语义。
pub fn assignment_reason_from_health(eligibility: &AgentWorkEligibility) -> AgentAssignmentConflictReason {
    if eligibility.org_chain_health.status != AgentOrgChainHealthStatus::InvalidOrgChain {
        return AgentAssignmentConflictReason::AncestorMissing;
    }
    match eligibility.org_chain_health.reason {
        AgentOrgChainInvalidReason::TerminatedAncestor => AgentAssignmentConflictReason::AncestorTerminated,
        AgentOrgChainInvalidReason::MissingManager => AgentAssignmentConflictReason::AncestorMissing,
        AgentOrgChainInvalidReason::Cycle => AgentAssignmentConflictReason::AncestorCycle,
        AgentOrgChainInvalidReason::Healthy => AgentAssignmentConflictReason::AncestorMissing,
    }
}

/// 从 eligibility 中提取 first invalid ancestor 的 UUID（如果有）。
///
/// 注意：Node 用 `firstInvalidAncestor.id`，但当 ancestor 状态为 `missing` 或 `cycle`
/// 时 `id` 可能是字符串（如 `"missing-manager"`），不能直接当 UUID 解析。
/// 这里在不能解析时返回 `None`——上层 HTTP 渲染时再决定如何处理。
fn first_invalid_ancestor_uuid(
    eligibility: &AgentWorkEligibility,
    fallback_company_id: Uuid,
) -> Option<Uuid> {
    let first = eligibility.org_chain_health.first_invalid_ancestor.as_ref()?;
    let parsed = Uuid::parse_str(&first.id).ok()?;
    Some(parsed)
}

/// 判断 first invalid ancestor 的状态是否为 `missing`（与 Node 端 `status === "missing"` 对齐）。
fn is_missing_ancestor(eligibility: &AgentWorkEligibility, candidate: &Uuid) -> bool {
    let Some(first) = eligibility.org_chain_health.first_invalid_ancestor.as_ref() else {
        return false;
    };
    if first.status != "missing" {
        return false;
    }
    Uuid::parse_str(&first.id)
        .map(|parsed| &parsed == candidate)
        .unwrap_or(false)
}

#[allow(dead_code)]
fn _relation_marker() -> AgentOrgChainRelation {
    AgentOrgChainRelation::Ancestor
}

#[allow(dead_code)]
fn _invalid_marker() -> AgentInvalidOrgChainAncestor {
    AgentInvalidOrgChainAncestor {
        id: String::new(),
        name: String::new(),
        status: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_message_uses_kind_in_subject() {
        assert_eq!(
            assignment_message(AgentAssignmentKind::Work, AgentAssignmentConflictReason::PendingApproval),
            "Cannot assign work to pending approval agents"
        );
        assert_eq!(
            assignment_message(AgentAssignmentKind::Routine, AgentAssignmentConflictReason::PendingApproval),
            "Cannot assign routines to pending approval agents"
        );
        assert_eq!(
            assignment_message(AgentAssignmentKind::Work, AgentAssignmentConflictReason::AssigneeTerminated),
            "Cannot assign work to terminated agents"
        );
        assert_eq!(
            assignment_message(AgentAssignmentKind::Routine, AgentAssignmentConflictReason::AncestorCycle),
            "Cannot assign routines to agents with an invalid org chain"
        );
    }

    #[test]
    fn assignment_message_for_unknown_status_distinguishes_subject() {
        assert_eq!(
            assignment_message(AgentAssignmentKind::Work, AgentAssignmentConflictReason::AssigneeUnknownStatus),
            "Cannot assign work to agents with an unsupported lifecycle status"
        );
        assert_eq!(
            assignment_message(AgentAssignmentKind::Routine, AgentAssignmentConflictReason::AssigneeUnknownStatus),
            "Cannot assign routines to agents with an unsupported lifecycle status"
        );
    }

    #[test]
    fn conflict_details_shape_matches_node() {
        let company = Uuid::new_v4();
        let agent = Uuid::new_v4();
        let details = make_conflict_details(
            company,
            agent,
            AgentAssignmentConflictReason::AncestorTerminated,
            &[],
            None,
            None,
        );
        assert_eq!(details.code, "agent_not_assignable");
        assert_eq!(details.reason, AgentAssignmentConflictReason::AncestorTerminated);
        assert_eq!(details.company_id, company);
        assert_eq!(details.assignee_agent_id, agent);
        assert_eq!(details.invalid_ancestor_agent_id, None);
        assert_eq!(details.missing_ancestor_agent_id, None);
        assert!(details.ancestor_chain.is_empty());
    }

    #[test]
    fn assignment_reason_from_health_maps_correctly() {
        fn make_health(reason: AgentOrgChainInvalidReason) -> AgentOrgChainHealth {
            AgentOrgChainHealth {
                status: AgentOrgChainHealthStatus::InvalidOrgChain,
                reason,
                full_chain: vec![],
                first_invalid_ancestor: None,
                invalid_ancestors: vec![],
                repair_guidance: None,
            }
        }
        let elig = |r| AgentWorkEligibility {
            assignable: false,
            invokable: false,
            assignability_reason: AgentEligibilityLifecycleReason::InvalidOrgChain,
            invokability_reason: AgentEligibilityLifecycleReason::InvalidOrgChain,
            org_chain_health: make_health(r),
        };
        assert_eq!(
            assignment_reason_from_health(&elig(AgentOrgChainInvalidReason::TerminatedAncestor)),
            AgentAssignmentConflictReason::AncestorTerminated
        );
        assert_eq!(
            assignment_reason_from_health(&elig(AgentOrgChainInvalidReason::MissingManager)),
            AgentAssignmentConflictReason::AncestorMissing
        );
        assert_eq!(
            assignment_reason_from_health(&elig(AgentOrgChainInvalidReason::Cycle)),
            AgentAssignmentConflictReason::AncestorCycle
        );
        assert_eq!(
            assignment_reason_from_health(&elig(AgentOrgChainInvalidReason::Healthy)),
            AgentAssignmentConflictReason::AncestorMissing
        );
    }

    #[test]
    fn chain_to_conflict_entries_strips_extra_fields() {
        let company = Uuid::new_v4();
        let manager = Uuid::new_v4();
        let elig = AgentWorkEligibility {
            assignable: true,
            invokable: true,
            assignability_reason: AgentEligibilityLifecycleReason::Eligible,
            invokability_reason: AgentEligibilityLifecycleReason::Eligible,
            org_chain_health: pc_core::agent_eligibility::AgentOrgChainHealth {
                status: AgentOrgChainHealthStatus::Healthy,
                reason: AgentOrgChainInvalidReason::Healthy,
                full_chain: vec![
                    pc_core::agent_eligibility::AgentOrgChainEntry {
                        id: manager.to_string(),
                        company_id: company.to_string(),
                        name: "CTO".to_string(),
                        status: "active".to_string(),
                        reports_to: None,
                        depth: 0,
                        relation: AgentOrgChainRelation::Self_,
                    },
                ],
                first_invalid_ancestor: None,
                invalid_ancestors: vec![],
                repair_guidance: None,
            },
        };
        let chain = chain_to_conflict_entries(&elig, company);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].id, manager);
        assert_eq!(chain[0].company_id, company);
        assert_eq!(chain[0].status, "active");
        assert_eq!(chain[0].reports_to, None);
    }

    #[test]
    fn options_default_kind_is_work() {
        let opts = AssertAssignableAgentOptions::default();
        assert!(opts.kind.is_none());
        let opts = AssertAssignableAgentOptions::new(AgentAssignmentKind::Routine);
        assert_eq!(opts.kind, Some(AgentAssignmentKind::Routine));
    }

    #[test]
    fn agent_assignment_kind_as_str() {
        assert_eq!(AgentAssignmentKind::Work.as_str(), "work");
        assert_eq!(AgentAssignmentKind::Routine.as_str(), "routine");
        assert_eq!(AgentAssignmentKind::default_work().as_str(), "work");
    }

    #[test]
    fn conflict_reason_as_str() {
        assert_eq!(AgentAssignmentConflictReason::PendingApproval.as_str(), "pending_approval");
        assert_eq!(AgentAssignmentConflictReason::AssigneeTerminated.as_str(), "assignee_terminated");
        assert_eq!(AgentAssignmentConflictReason::AssigneeUnknownStatus.as_str(), "assignee_unknown_status");
        assert_eq!(AgentAssignmentConflictReason::AncestorTerminated.as_str(), "ancestor_terminated");
        assert_eq!(AgentAssignmentConflictReason::AncestorMissing.as_str(), "ancestor_missing");
        assert_eq!(AgentAssignmentConflictReason::AncestorCrossCompany.as_str(), "ancestor_cross_company");
        assert_eq!(AgentAssignmentConflictReason::AncestorCycle.as_str(), "ancestor_cycle");
        assert_eq!(AgentAssignmentConflictReason::AncestorDepthExceeded.as_str(), "ancestor_depth_exceeded");
    }
}

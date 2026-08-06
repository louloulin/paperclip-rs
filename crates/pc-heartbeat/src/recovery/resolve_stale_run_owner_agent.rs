//! `resolveStaleRunOwnerAgentId` —— Node `services/recovery/service.ts:1808`。
//!
//! 业务语义：
//! - 解析 stale active run 的 evaluation owner agent id（用于 evaluation issue 的 assignee）
//! - 候选顺序（与 Node 完全对齐）：
//!   1. `sourceIssue.assigneeAgentId.reportsTo`（若有 source issue 且有 assignee）
//!   2. `runningAgent.reportsTo`
//!   3. 公司 role=cto（按 created_at 升序）
//!   4. 公司 role=ceo（按 created_at 升序）
//! - 取第一个 invokable + 同 company 的 agent 返回
//! - 全部不可 invoke 或不存在 → None
//!
//! 设计意图：
//! - 与 `resolve_recovery_owner_agent` (stranded issue) 分离：
//!   - 候选顺序不同（stale run 优先级是 source assignee.reportsTo，stranded 是 creator.reportsTo）
//!   - 不需要 IssueRow 入参（stale run 是 heartbeat run 视角）
//! - candidate 收集是 pure helper + DB I/O；invokability 检查复用 `pc_repos::agent_invokability`
//! - budget 模块未完全迁移 → 暂 stub 返回 false（无 block）。后续 Round 接 budget 后可启用
//!
//! 调用方：`scan_silent_active_runs` 主循环（Round 337）

use uuid::Uuid;

use pc_repos::agent_invokability::{evaluate_agent_invokability_from_db, AgentOrgRow};
use pc_repos::Db;

/// `resolve_stale_run_owner_agent_id` 入参。
#[derive(Debug, Clone)]
pub struct ResolveStaleRunOwnerAgentInput {
    /// run 所属 company
    pub run_company_id: Uuid,
    /// running agent 的 reports_to（已从 agents 表 fetch 出来）
    pub running_agent_reports_to: Option<Uuid>,
    /// source issue 的 assignee_agent_id（若有 source issue）
    pub source_issue_assignee_agent_id: Option<Uuid>,
}

/// 解析 stale run 的 evaluation owner agent id。
///
/// 流程（与 Node `resolveStaleRunOwnerAgentId` 完全对齐）：
/// 1. 收集 candidates（按 Node 顺序）
/// 2. 去重 + 按序遍历，第一个 invokable 且同 company 的返回
///
/// budget 模块未迁移 → 暂不调用 `budgets.getInvocationBlock`。
pub async fn resolve_stale_run_owner_agent_id(
    db: &Db,
    input: &ResolveStaleRunOwnerAgentInput,
) -> sqlx::Result<Option<Uuid>> {
    let candidates = collect_candidate_ids(db, input).await?;
    first_invokable_candidate(db, input.run_company_id, &candidates).await
}

/// 收集 candidates（pure + DB I/O 混合）。
///
/// 顺序：
/// 1. sourceIssue.assigneeAgentId.reportsTo
/// 2. runningAgent.reportsTo
/// 3. role=cto
/// 4. role=ceo
async fn collect_candidate_ids(
    db: &Db,
    input: &ResolveStaleRunOwnerAgentInput,
) -> sqlx::Result<Vec<Uuid>> {
    let mut candidates: Vec<Uuid> = Vec::new();

    // 1. sourceIssue.assigneeAgentId.reportsTo
    if let Some(assignee_id) = input.source_issue_assignee_agent_id {
        if let Some(reports_to) = fetch_agent_reports_to(db, assignee_id).await? {
            candidates.push(reports_to);
        }
    }

    // 2. runningAgent.reportsTo
    if let Some(reports_to) = input.running_agent_reports_to {
        candidates.push(reports_to);
    }

    // 3. + 4. role=cto, ceo（按 Node 顺序：cto 先）
    let executives = super::resolve_recovery_owner_agent::list_company_executive_agents(
        db,
        input.run_company_id,
    )
    .await?;
    candidates.extend(executives.into_iter().map(|a| a.id));

    Ok(candidates)
}

/// 给定 candidate id 列表，按序去重，返回第一个 invokable 且同 company 的。
async fn first_invokable_candidate(
    db: &Db,
    run_company_id: Uuid,
    candidates: &[Uuid],
) -> sqlx::Result<Option<Uuid>> {
    let mut seen = std::collections::HashSet::new();
    for candidate_id in candidates {
        if !seen.insert(*candidate_id) {
            continue;
        }
        let Some(agent) = fetch_agent_org_row(db, *candidate_id).await? else {
            continue;
        };
        if agent.company_id != run_company_id {
            continue;
        }
        let invokability = evaluate_agent_invokability_from_db(db, Some(&agent)).await?;
        if invokability.is_invokable() {
            return Ok(Some(*candidate_id));
        }
    }
    Ok(None)
}

/// 仅取 agent 的 reports_to（轻量，避免 fetch 整个 AgentOrgRow）。
async fn fetch_agent_reports_to(db: &Db, agent_id: Uuid) -> sqlx::Result<Option<Uuid>> {
    let row: Option<(Option<Uuid>,)> =
        sqlx::query_as("SELECT reports_to FROM agents WHERE id = $1")
            .bind(agent_id)
            .fetch_optional(db.pool())
            .await?;
    Ok(row.and_then(|(r,)| r))
}

/// 取 agent 的 org chart 投影（id/company_id/name/reports_to/status）。
async fn fetch_agent_org_row(db: &Db, agent_id: Uuid) -> sqlx::Result<Option<AgentOrgRow>> {
    let row: Option<(Uuid, Uuid, String, Option<Uuid>, String)> =
        sqlx::query_as("SELECT id, company_id, name, reports_to, status FROM agents WHERE id = $1")
            .bind(agent_id)
            .fetch_optional(db.pool())
            .await?;
    Ok(
        row.map(|(id, company_id, name, reports_to, status)| AgentOrgRow {
            id,
            company_id,
            name,
            reports_to,
            status,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_construction_smoke() {
        let input = ResolveStaleRunOwnerAgentInput {
            run_company_id: Uuid::new_v4(),
            running_agent_reports_to: Some(Uuid::new_v4()),
            source_issue_assignee_agent_id: Some(Uuid::new_v4()),
        };
        assert_eq!(input.running_agent_reports_to.is_some(), true);
    }
}

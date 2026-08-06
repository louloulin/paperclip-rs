//! `resolveStrandedIssueRecoveryOwnerAgentId` + `resolveInvokableRecoveryAgentId` ——
//! Node `services/recovery/service.ts:2524` + `:2564`。
//!
//! 业务语义：
//! - `resolve_invokable_recovery_agent_id(issue, agent_id)` —— 检查指定 agent_id 是否
//!   invokable + 同 company；若否则返回 None
//! - `resolve_stranded_issue_recovery_owner_agent_id(issue, preferred)` ——
//!   1. 收集候选 agents：preferred → assignee.reports_to → creator.reports_to →
//!      creator 本人 → role IN ('cto','ceo')（cto 优先）→ assignee 本人
//!   2. 按顺序去重，取第一个 invokable 的返回；都不可 invoke 则 None
//!
//! 设计原则：
//! - 与 Node `evaluateAgentInvokabilityFromDb` 行为对齐（已存在于 pc-repos）
//! - 与 Node `budgets.getInvocationBlock` 暂时 stub（返回 false = not blocked）
//!   —— budget 模块未完全迁移；此模块独立可用
//! - 候选人收集是 pure 函数 `build_candidate_order` —— 可单测
//! - invokability 查询是 DB helper `fetch_agent_org_row` —— 与 AgentOrgRow 对齐

use pc_repos::agent_invokability::{evaluate_agent_invokability_from_db, AgentOrgRow};
use pc_repos::issue::IssueRow;
use pc_repos::Db;
use uuid::Uuid;

/// 返回 company 中 role IN ('cto','ceo') 的 agent（按优先级排序：cto 先）。
pub async fn list_company_executive_agents(
    db: &Db,
    company_id: Uuid,
) -> sqlx::Result<Vec<AgentOrgRow>> {
    let rows: Vec<(Uuid, Uuid, String, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT id, company_id, name, reports_to, status FROM agents \
         WHERE company_id = $1 AND role IN ('cto','ceo') \
         ORDER BY CASE WHEN role = 'cto' THEN 0 ELSE 1 END, created_at ASC",
    )
    .bind(company_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, company_id, name, reports_to, status)| AgentOrgRow {
            id,
            company_id,
            name,
            reports_to,
            status,
        })
        .collect())
}

/// 按 id 拉取 agent 的 org chart 投影。
pub async fn fetch_agent_org_row(db: &Db, agent_id: Uuid) -> sqlx::Result<Option<AgentOrgRow>> {
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

/// 检查指定 agent_id 是否 invokable 且与 issue 同 company。
///
/// 与 Node `resolveInvokableRecoveryAgentId` 对齐：
/// - agent_id 为 None → None
/// - agent 不存在或不同 company → None
/// - 否则调用 `evaluate_agent_invokability_from_db`，仅当 invokable=true 才返回 Some(agent_id)
pub async fn resolve_invokable_recovery_agent_id(
    db: &Db,
    issue: &IssueRow,
    agent_id: Option<Uuid>,
) -> sqlx::Result<Option<Uuid>> {
    let Some(agent_id) = agent_id else {
        return Ok(None);
    };
    let Some(agent) = fetch_agent_org_row(db, agent_id).await? else {
        return Ok(None);
    };
    if agent.company_id != issue.company_id {
        return Ok(None);
    }
    let invokability = evaluate_agent_invokability_from_db(db, Some(&agent)).await?;
    if invokability.is_invokable() {
        Ok(Some(agent_id))
    } else {
        Ok(None)
    }
}

/// 收集 candidate agent ids（按 Node 顺序，含重复）。重复由 caller 去重。
///
/// 顺序（与 Node `resolveStrandedIssueRecoveryOwnerAgentId` 一致）：
/// 1. `preferred_owner_agent_id`（若有）
/// 2. `issue.assignee_agent_id.reports_to`（若有）
/// 3. `issue.created_by_agent_id.reports_to`（若有）
/// 4. `issue.created_by_agent_id` 本人
/// 5. role=cto（公司内）
/// 6. role=ceo（公司内）
/// 7. `issue.assignee_agent_id` 本人
pub async fn collect_stranded_recovery_candidate_ids(
    db: &Db,
    issue: &IssueRow,
    preferred_owner_agent_id: Option<Uuid>,
) -> sqlx::Result<Vec<Uuid>> {
    let mut candidates: Vec<Uuid> = Vec::new();

    if let Some(pref) = preferred_owner_agent_id {
        candidates.push(pref);
    }

    // assignee.reports_to
    if let Some(assignee_id) = issue.assignee_agent_id {
        if let Some(assignee) = fetch_agent_org_row(db, assignee_id).await? {
            if let Some(reports_to) = assignee.reports_to {
                candidates.push(reports_to);
            }
        }
    }

    // creator.reports_to + creator
    if let Some(creator_id) = issue.created_by_agent_id {
        if let Some(creator) = fetch_agent_org_row(db, creator_id).await? {
            if let Some(reports_to) = creator.reports_to {
                candidates.push(reports_to);
            }
            candidates.push(creator_id);
        }
    }

    // role=cto / ceo（按 Node order：cto 先）
    let executives = list_company_executive_agents(db, issue.company_id).await?;
    candidates.extend(executives.into_iter().map(|a| a.id));

    // assignee 本人
    if let Some(assignee_id) = issue.assignee_agent_id {
        candidates.push(assignee_id);
    }

    Ok(candidates)
}

/// 给定 candidate id 列表，按顺序去重，返回第一个 invokable 且同 company 的。
async fn first_invokable_candidate(
    db: &Db,
    issue: &IssueRow,
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
        if agent.company_id != issue.company_id {
            continue;
        }
        let invokability = evaluate_agent_invokability_from_db(db, Some(&agent)).await?;
        if invokability.is_invokable() {
            return Ok(Some(*candidate_id));
        }
    }
    Ok(None)
}

/// 主入口：解析 stranded issue recovery owner agent id。
///
/// 步骤：
/// 1. 收集候选 ids（按 Node 顺序）
/// 2. 去重 + 按序遍历，第一个 invokable 且同 company 的返回
///
/// 返回 None 当所有候选都不可 invoke 或都不存在。
pub async fn resolve_stranded_issue_recovery_owner_agent_id(
    db: &Db,
    issue: &IssueRow,
    preferred_owner_agent_id: Option<Uuid>,
) -> sqlx::Result<Option<Uuid>> {
    let candidates =
        collect_stranded_recovery_candidate_ids(db, issue, preferred_owner_agent_id).await?;
    first_invokable_candidate(db, issue, &candidates).await
}

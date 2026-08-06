//! DB IO + 校验函数（与 Node `task-watchdog-scope.ts` 3 个 export 1:1 对齐）。

use uuid::Uuid;

use crate::Db;

use super::helpers::{as_plain_record, read_string, read_task_watchdog_context};
use super::types::{
    AgentRunActor, IssueScopeTarget, TaskWatchdogMutationScope, TASK_WATCHDOG_ORIGIN_KIND,
};

/// 子树最大向上遍历深度（与 Node `MAX_WATCHDOG_SCOPE_ANCESTRY_DEPTH = 100` 1:1 对齐）。
pub const MAX_WATCHDOG_SCOPE_ANCESTRY_DEPTH: usize = 100;

/// 解析 agent run actor 的 task watchdog mutation scope（与 Node `resolveTaskWatchdogMutationScope` 1:1 对齐）。
///
/// 行为：
/// 1. `actor.type != "agent"` → None
/// 2. `agent_id` 或 `run_id` 缺失 → None
/// 3. 拉 `heartbeat_runs` 找不到 → None
/// 4. contextSnapshot 没有 taskWatchdog 标记 → None
/// 5. run.agent_id != actor.agent_id 或 companyId 失配 → Invalid
/// 6. contextSnapshot 缺 watchedIssueId → Invalid
/// 7. 拉 `issue_watchdogs` 找不到 active 行 → Invalid
/// 8. 全部命中 → Watchdog
pub async fn resolve_task_watchdog_mutation_scope(
    db: &Db,
    actor: &AgentRunActor,
) -> Result<TaskWatchdogMutationScope, sqlx::Error> {
    if actor.actor_type != "agent" {
        return Ok(TaskWatchdogMutationScope::None);
    }
    let agent_id = match actor
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => s.to_string(),
        None => return Ok(TaskWatchdogMutationScope::None),
    };
    let run_id = match actor
        .run_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => s.to_string(),
        None => return Ok(TaskWatchdogMutationScope::None),
    };
    let actor_company_id = actor
        .company_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    let run_uuid = match Uuid::parse_str(&run_id) {
        Ok(u) => u,
        Err(_) => return Ok(TaskWatchdogMutationScope::None),
    };

    let run: Option<(Uuid, Uuid, Option<Uuid>, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT id, company_id, agent_id, context_snapshot FROM heartbeat_runs WHERE id = $1",
    )
    .bind(run_uuid)
    .fetch_optional(db.pool())
    .await?;

    let Some((_id, run_company_id, run_agent_id, context_snapshot)) = run else {
        return Ok(TaskWatchdogMutationScope::None);
    };
    let task_watchdog = match read_task_watchdog_context(context_snapshot.as_ref()) {
        Some(c) => c,
        None => return Ok(TaskWatchdogMutationScope::None),
    };
    let run_agent_id_str = run_agent_id.map(|u| u.to_string()).unwrap_or_default();
    if run_agent_id_str != agent_id
        || (actor_company_id.is_some()
            && actor_company_id.as_deref() != Some(&run_company_id.to_string()))
    {
        return Ok(TaskWatchdogMutationScope::Invalid {
            detail: "Task-watchdog run context does not belong to this agent.".to_string(),
        });
    }

    let watched_issue_id = match task_watchdog.watched_issue_id {
        Some(s) => s,
        None => {
            return Ok(TaskWatchdogMutationScope::Invalid {
                detail: "Task-watchdog run context is missing a persisted watched issue id."
                    .to_string(),
            });
        }
    };
    let watched_uuid = match Uuid::parse_str(&watched_issue_id) {
        Ok(u) => u,
        Err(_) => {
            return Ok(TaskWatchdogMutationScope::Invalid {
                detail: "Task-watchdog run context has an invalid watched issue id.".to_string(),
            });
        }
    };
    let agent_uuid = match Uuid::parse_str(&agent_id) {
        Ok(u) => u,
        Err(_) => {
            return Ok(TaskWatchdogMutationScope::None);
        }
    };

    let watchdog: Option<(Uuid, Uuid, Uuid, Option<Uuid>, String)> = sqlx::query_as(
        r#"
        SELECT id, company_id, issue_id, watchdog_issue_id, status
        FROM issue_watchdogs
        WHERE company_id = $1
          AND issue_id = $2
          AND watchdog_agent_id = $3
          AND status = 'active'
        "#,
    )
    .bind(run_company_id)
    .bind(watched_uuid)
    .bind(agent_uuid)
    .fetch_optional(db.pool())
    .await?;

    let Some((watchdog_id, company_id, watched, watchdog_issue_id, _status)) = watchdog else {
        return Ok(TaskWatchdogMutationScope::Invalid {
            detail: "Task-watchdog run context is not backed by an active persisted watchdog."
                .to_string(),
        });
    };

    Ok(TaskWatchdogMutationScope::Watchdog {
        watchdog_id: watchdog_id.to_string(),
        company_id: company_id.to_string(),
        watched_issue_id: watched.to_string(),
        watchdog_issue_id: watchdog_issue_id.map(|u| u.to_string()),
        stop_fingerprint: task_watchdog.stop_fingerprint,
    })
}

/// 判断 target issue 是否在 watched issue 的子树内（与 Node `issueIsInTaskWatchdogSubtree` 1:1 对齐）。
///
/// 行为：
/// - 从 `issueId` 沿 `issues.parent_id` 向上遍历，最多 `MAX_WATCHDOG_SCOPE_ANCESTRY_DEPTH` 步
/// - 遇到 cycle（seen 集合）→ 返回 false
/// - 任何 step 遇到 `origin_kind == TASK_WATCHDOG_ORIGIN_KIND` → 返回 false（task-watchdog origin 不属于普通 subtree）
/// - 任何 step 找到 `currentId == watchedIssueId` → 返回 true
/// - 找不到匹配祖先 / 跨最大深度 → 返回 false
pub async fn issue_is_in_task_watchdog_subtree(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    watched_issue_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut current_id: Option<Uuid> = Some(issue_id);
    let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

    for _ in 0..MAX_WATCHDOG_SCOPE_ANCESTRY_DEPTH {
        let cid = match current_id {
            Some(c) => c,
            None => return Ok(false),
        };
        if seen.contains(&cid) {
            return Ok(false);
        }
        seen.insert(cid);

        let parent: Option<(Uuid, Uuid, Option<Uuid>, Option<String>)> = sqlx::query_as(
            "SELECT id, company_id, parent_id, origin_kind FROM issues WHERE id = $1 AND company_id = $2",
        )
        .bind(cid)
        .bind(company_id)
        .fetch_optional(db.pool())
        .await?;

        let Some((_id, _cid, parent_id, origin_kind)) = parent else {
            return Ok(false);
        };
        if origin_kind.as_deref() == Some(TASK_WATCHDOG_ORIGIN_KIND) {
            return Ok(false);
        }
        if cid == watched_issue_id {
            return Ok(true);
        }
        current_id = parent_id;
    }

    Ok(false)
}

/// 校验 scope 是否允许对 target issue 做 mutation（与 Node `taskWatchdogScopeAllowsIssueMutation` 1:1 对齐）。
///
/// 行为：
/// - `scope.kind != "watchdog"` → 原样返回（None 放行，Invalid 拒绝）
/// - target 与 scope 跨公司 → Invalid
/// - `allow_watchdog_issue != false` 且 target 是 watchdogIssueId → 放行
/// - target 在 watched issue 子树内 → 放行
/// - 其他 → Invalid
pub async fn task_watchdog_scope_allows_issue_mutation(
    db: &Db,
    scope: &TaskWatchdogMutationScope,
    issue: &IssueScopeTarget,
    opts: TaskWatchdogScopeAllowsOptions,
) -> Result<TaskWatchdogMutationScope, sqlx::Error> {
    if !matches!(scope, TaskWatchdogMutationScope::Watchdog { .. }) {
        return Ok(scope.clone());
    }
    let scope = match scope {
        TaskWatchdogMutationScope::Watchdog {
            company_id,
            watchdog_issue_id,
            watched_issue_id,
            ..
        } => (
            company_id.clone(),
            watchdog_issue_id.clone(),
            watched_issue_id.clone(),
        ),
        _ => unreachable!(),
    };
    let (scope_company_id, watchdog_issue_id, watched_issue_id) = scope;

    if issue.company_id.to_string() != scope_company_id {
        return Ok(TaskWatchdogMutationScope::Invalid {
            detail: "Task-watchdog mutation target is outside the watchdog company.".to_string(),
        });
    }
    let allow_watchdog_issue = opts.allow_watchdog_issue.unwrap_or(true);
    if allow_watchdog_issue {
        if let Some(wid) = &watchdog_issue_id {
            if wid == &issue.id.to_string() {
                return Ok(TaskWatchdogMutationScope::Watchdog {
                    watchdog_id: String::new(), // Not used downstream; not propagating original fields
                    company_id: scope_company_id,
                    watched_issue_id,
                    watchdog_issue_id,
                    stop_fingerprint: None,
                });
            }
        }
    }
    let watched_uuid = match Uuid::parse_str(&watched_issue_id) {
        Ok(u) => u,
        Err(_) => {
            return Ok(TaskWatchdogMutationScope::Invalid {
                detail: "Task-watchdog scope has an invalid watched issue id.".to_string(),
            });
        }
    };
    if issue_is_in_task_watchdog_subtree(db, issue.company_id, issue.id, watched_uuid).await? {
        // Preserve original scope by reconstructing it
        return Ok(scope_to_watchdog(
            &scope_company_id,
            &watchdog_issue_id,
            &watched_issue_id,
        ));
    }
    Ok(TaskWatchdogMutationScope::Invalid {
        detail: "Task-watchdog runs can only mutate the watched issue subtree.".to_string(),
    })
}

/// `taskWatchdogScopeAllowsIssueMutation` 的 options（与 Node `opts` 1:1 对齐）。
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskWatchdogScopeAllowsOptions {
    pub allow_watchdog_issue: Option<bool>,
}

impl TaskWatchdogScopeAllowsOptions {
    pub fn new(allow_watchdog_issue: bool) -> Self {
        Self {
            allow_watchdog_issue: Some(allow_watchdog_issue),
        }
    }
}

fn scope_to_watchdog(
    company_id: &str,
    watchdog_issue_id: &Option<String>,
    watched_issue_id: &str,
) -> TaskWatchdogMutationScope {
    TaskWatchdogMutationScope::Watchdog {
        watchdog_id: String::new(),
        company_id: company_id.to_string(),
        watched_issue_id: watched_issue_id.to_string(),
        watchdog_issue_id: watchdog_issue_id.clone(),
        stop_fingerprint: None,
    }
}

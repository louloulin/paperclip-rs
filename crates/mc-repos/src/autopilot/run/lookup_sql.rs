//! 派发链需要的**只读**邻表查询 —— `autopilot/run.rs` 的 `run` 子模块（R7 拆分）。
//!
//! 为什么在这里而不是复用 `crate::agent::AgentRepo` / `crate::squad::SquadRepo`：那两个 Repo 是
//! `Db` 形态（`RepoWithDb`），而 `mc-autopilot` **没有** `mc-db` 依赖 ⇒ 它拿不到 `Db` 值，
//! 构造不出 Repo。派发层的既定形态是「收 `&PgPool` 的自由函数」（同 M5-6
//! `wakeup/service.rs`），所以这里提供等价只读查询。文件属 M5-4 写集
//! （`docs/44` §3.2 的 `mc-repos/src/autopilot/run.rs` 一行）。
//!
//! 条目由 `run.rs` 重导出，外部路径仍是 `mc_repos::autopilot::run::*`。

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::agent::AgentRow;
use crate::workspace::map_sqlx_err;
use crate::Result;

/// `resolveAutopilotLeader`（`internal/service/autopilot.go:1461`）的四态结果。
///
/// `squad` 位只在 `Agent` 上：它对应上游的第二个返回值（`squadResolved`），准入闸用它区分
/// 「agent 行没了」与「squad 行没了」两种失败话术。
#[derive(Debug)]
pub enum AssigneeLeader {
    /// 解析成功。`squad = true` 表示这一行是**由 squad 解出来的 leader**。
    Agent {
        /// leader agent 行。
        agent: Box<AgentRow>,
        /// 是否经 squad 解析。
        squad: bool,
    },
    /// squad 行存在但已归档（上游 `errSquadArchived`）——硬跳过，不是失败。
    SquadArchived,
    /// agent / squad 行不存在（上游 `pgx.ErrNoRows`）。
    Missing {
        /// 是否走的是 squad 分支（决定失败话术）。
        squad: bool,
    },
    /// `assignee_type` 未知（上游 `unknown assignee_type %q`）——不是 `ErrNoRows`，
    /// 因此上游把它归到「失败」而不是「跳过」。
    UnknownAssigneeType(String),
}

/// `resolveAutopilotLeader`：把 autopilot 的 assignee 解析成**真正跑活的 agent**。
///
/// `""` / `agent` ⇒ 直连 agent；`squad` ⇒ 先看 squad 是否归档，再取 leader agent。
/// 两次查询都带 `workspace_id` 限定（跨租户的 assignee_id 选不出行）。
pub async fn resolve_assignee_leader(
    pool: &PgPool,
    workspace_id: Uuid,
    assignee_type: &str,
    assignee_id: Uuid,
) -> Result<AssigneeLeader> {
    match assignee_type {
        "" | "agent" => {
            let agent = load_agent(pool, workspace_id, assignee_id).await?;
            Ok(match agent {
                Some(agent) => AssigneeLeader::Agent {
                    agent: Box::new(agent),
                    squad: false,
                },
                None => AssigneeLeader::Missing { squad: false },
            })
        }
        "squad" => {
            let squad: Option<(Option<DateTime<Utc>>, Uuid)> =
                sqlx::query_as("SELECT archived_at, leader_id FROM squad WHERE id = $1 AND workspace_id = $2")
                    .bind(assignee_id)
                    .bind(workspace_id)
                    .fetch_optional(pool)
                    .await
                    .map_err(map_sqlx_err)?;
            let Some((archived_at, leader_id)) = squad else {
                return Ok(AssigneeLeader::Missing { squad: true });
            };
            if archived_at.is_some() {
                return Ok(AssigneeLeader::SquadArchived);
            }
            let agent = load_agent(pool, workspace_id, leader_id).await?;
            Ok(match agent {
                Some(agent) => AssigneeLeader::Agent {
                    agent: Box::new(agent),
                    squad: true,
                },
                None => AssigneeLeader::Missing { squad: true },
            })
        }
        other => Ok(AssigneeLeader::UnknownAssigneeType(other.to_string())),
    }
}

/// 单个 agent 的 workspace 内查询（`AgentRepo::find_in_workspace` 的池形态）。
pub async fn load_agent(
    pool: &PgPool,
    workspace_id: Uuid,
    agent_id: Uuid,
) -> Result<Option<AgentRow>> {
    sqlx::query_as::<_, AgentRow>(&format!(
        "SELECT {} FROM agent WHERE id = $1 AND workspace_id = $2",
        crate::agent::AGENT_COLUMNS
    ))
    .bind(agent_id)
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `resolveAutopilotTriggerTimezone`（`autopilot.go:1749`）的读：trigger 的 IANA 时区名。
///
/// 返回 `None` = 没有 trigger / 云端列为空 —— 调用方落 `UTC`。名字本身合法与否由调用方
/// （`mc-autopilot::dispatch::template`）判定，这里不做校验（上游也在服务层 `LoadLocation`）。
pub async fn load_trigger_timezone(pool: &PgPool, trigger_id: Uuid) -> Result<Option<String>> {
    sqlx::query_scalar("SELECT timezone FROM autopilot_trigger WHERE id = $1")
        .bind(trigger_id)
        .fetch_optional(pool)
        .await
        .map_err(map_sqlx_err)
}

/// `issue_effective_status(p_workspace_id, p_status)`：自定义状态折叠成内建键。
///
/// `SyncRunFromIssue` 的终态判定必须走这个口径（上游 `issuestatus.Effective`），否则归档的
/// `done` 类自定义状态会被误判成「还没结束」。
pub async fn effective_issue_status(
    pool: &PgPool,
    workspace_id: Uuid,
    status: &str,
) -> Result<String> {
    let raw: Option<String> = sqlx::query_scalar("SELECT issue_effective_status($1, $2)")
        .bind(workspace_id)
        .bind(status)
        .fetch_one(pool)
        .await
        .map_err(map_sqlx_err)?;
    Ok(raw.unwrap_or_else(|| status.to_owned()))
}

/// `SyncRunFromIssue` 的第一道闸：issue 的 `origin_type` 与**原始** `status`。
///
/// 返回 `None` = issue 行不存在。`origin_type != 'autopilot'` 由调用方判（不在这里折叠成
/// `None`，好让调用方自己决定日志）。
pub async fn load_issue_origin(
    pool: &PgPool,
    issue_id: Uuid,
) -> Result<Option<(Option<String>, String)>> {
    sqlx::query_as("SELECT origin_type, status FROM issue WHERE id = $1")
        .bind(issue_id)
        .fetch_optional(pool)
        .await
        .map_err(map_sqlx_err)
}

/// `workspace.attribution_fail_closed`（迁移 `188`）：无归属人的 run 是否必须拒派。
///
/// `Some(true)` ⇒ 归属兜底（`owner_fallback`）被禁止，这一跑直接跳成 `attribution_blocked`
/// （上游 `applyAttributionFallback` 的 fail-closed 分支）。
///
/// **`None`（工作区行不存在）不是「放行」**：上游 `applyAttributionFallback` 在「读不到策略」时
/// 是**拒绝**（fail-closed）。所以这里**不**把 `None` 折成 `false`，把判定留给调用方
/// （`mc-autopilot::dispatch::attribution`）：`None` 与 `Err` 一视同仁，都按「无法确认策略」拒绝。
pub async fn workspace_attribution_fail_closed(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<Option<bool>> {
    sqlx::query_scalar(
        "SELECT attribution_fail_closed FROM workspace WHERE id = $1",
    )
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// `GetActiveAutopilotRuleVersion`（`task.go:600` 的 `ruleOwnerAttribution`）：
/// 取该 autopilot **最新**一条规则版本快照，返回 `(id, published_by_id)`。
///
/// `published_by_id` 只在 `published_by_type = 'member'` 时返回（`system` 发布没有成员可问责）。
///
/// 本仓**没有** `autopilot_rule_version` 的写入链（`docs/44` §8：MUL-4302 §3.4 的发布链属 M5
/// 之外的切片）⇒ 实测这张表恒为空、这里恒返回 `None`、`rule_owner` 因此退化成
/// `unattributed`（与上游「没有版本行」时的行为**完全一致**：`RuleOwner(invalid, invalid)`）。
/// 仍然实装这条读：`migrations/upstream/186` 已建表，一旦上游发布链补上，归属立刻按真实
/// 发布者生效，不需要再改派发层。
pub async fn load_active_rule_version(
    pool: &PgPool,
    workspace_id: Uuid,
    autopilot_id: Uuid,
) -> Result<Option<(Uuid, Option<Uuid>)>> {
    sqlx::query_as(
        "SELECT id, CASE WHEN published_by_type = 'member' THEN published_by_id END \
         FROM autopilot_rule_version \
         WHERE workspace_id = $1 AND autopilot_id = $2 \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(workspace_id)
    .bind(autopilot_id)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// 派发时**刷新**项目绑定（上游 `dispatchCreateIssue` 的 `refresh autopilot`）。
///
/// 上游为什么要重读：`Dispatch*` 的调用方（调度器 / webhook worker）可能持有一份**陈旧**的
/// autopilot 快照，拿它建 issue 会把项目绑错；所以建 issue 前以库里的当前行为准。
///
/// 返回 `None` = autopilot 行已不在这个工作区（上游 `GetAutopilotInWorkspace` 的 `ErrNoRows`
/// ⇒ 派发失败）。
pub async fn load_project_binding(
    pool: &PgPool,
    workspace_id: Uuid,
    autopilot_id: Uuid,
) -> Result<Option<AutopilotProjectBinding>> {
    sqlx::query_as::<_, AutopilotProjectBinding>(
        "SELECT project_id FROM autopilot WHERE id = $1 AND workspace_id = $2",
    )
    .bind(autopilot_id)
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_err)
}

/// [`load_project_binding`] 的行结构（只取一列，好和「行不存在」区分开）。
#[derive(Debug, Clone, FromRow)]
pub struct AutopilotProjectBinding {
    /// `autopilot.project_id`（`058` DROP、`097` 加回，FK `ON DELETE SET NULL`）。
    pub project_id: Option<Uuid>,
}

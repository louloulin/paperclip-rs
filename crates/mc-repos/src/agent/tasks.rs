//! `agent_task_queue` 的**读聚合 + 取消**（M3-5 / LUM-1428）。
//!
//! 上游对照：`server/internal/handler/agent.go` + `server/pkg/db/queries/agent.sql`
//! （`ListAgentTasks` / `CancelAgentTasksByAgent` / `ListWorkspaceAgentTaskSnapshot` /
//! `GetWorkspaceAgentRunCounts` / `GetWorkspaceAgentActivity30d`）。
//!
//! **所有权**：任务队列的状态机、lease、重试与结算属 M3-3（状态机）/ M3-6
//! （`crate::task`）。本模块只做两件事：
//! 1. **读**：给 `/api/agents/{id}/tasks` 与 `/api/agent-task-snapshot` 出投影；
//! 2. **状态置位**：`cancel_tasks` 把在飞行置 `cancelled`（上游
//!    `TaskService.CancelTasksForAgent` 的 SQL 部分；广播 / `ReconcileAgentStatus` /
//!    委派失败结算等副作用不在本片）。
//!
//! 投影有意收窄（见 `docs/40-M3-5-AGENTS.md` §5）：daemon-only 字段
//! （`remote_mcp_*`、`plugin_hook_tools`、`workspace_context`、`issue_statuses`、
//! usage hydration、attribution）不返回。

use chrono::{DateTime, Utc};
use mc_core::Id;
use sqlx::FromRow;
use uuid::Uuid;

use crate::agent::AgentRepo;
use crate::workspace::map_sqlx_err;
use crate::Result;

/// 在飞状态（上游 `CancelAgentTasksByAgent` 与 snapshot 的 active 半边）。
pub const ACTIVE_TASK_STATUSES: &[&str] =
    &["queued", "dispatched", "running", "waiting_local_directory"];

/// snapshot 的“最近结果”半边只看这两个终态（`cancelled` 故意排除）。
pub const TASK_SNAPSHOT_OUTCOME_STATUSES: &[&str] = &["completed", "failed"];

/// `agent_task_queue` 的响应投影列（`SELECT` 与 `RETURNING` 共用）。
const TASK_COLUMNS: &str = "id, agent_id, runtime_id, issue_id, status, priority, dispatched_at, \
     started_at, completed_at, created_at, attempt, max_attempts, error, failure_reason, \
     escalation_for_task_id";

/// 同上，但带 `atq.` 前缀（snapshot 的两个 UNION 分支都要限定 alias）。
const TASK_COLUMNS_ALIASED: &str = "atq.id, atq.agent_id, atq.runtime_id, atq.issue_id, \
     atq.status, atq.priority, atq.dispatched_at, atq.started_at, atq.completed_at, \
     atq.created_at, atq.attempt, atq.max_attempts, atq.error, atq.failure_reason, \
     atq.escalation_for_task_id";

/// `agent_task_queue` 行（收窄投影）。
#[derive(Debug, Clone, FromRow)]
pub struct AgentTaskRow {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub runtime_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub status: String,
    pub priority: i32,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub attempt: i32,
    pub max_attempts: i32,
    pub error: Option<String>,
    pub failure_reason: Option<String>,
    pub escalation_for_task_id: Option<Uuid>,
}

impl AgentTaskRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 归属 agent。
    pub fn agent_id(&self) -> Id {
        Id(self.agent_id)
    }

    /// 是否在飞（`queued` / `dispatched` / `running` / `waiting_local_directory`）。
    pub fn is_active(&self) -> bool {
        ACTIVE_TASK_STATUSES.contains(&self.status.as_str())
    }

    /// 是否为终态结果（`completed` / `failed`，不含 `cancelled`）。
    pub fn is_outcome(&self) -> bool {
        TASK_SNAPSHOT_OUTCOME_STATUSES.contains(&self.status.as_str())
    }
}

/// 每个 agent 近 30 天的 run 计数（上游 `GetWorkspaceAgentRunCounts`）。
#[derive(Debug, Clone, FromRow)]
pub struct AgentRunCountRow {
    pub agent_id: Uuid,
    pub run_count: i32,
}

impl AgentRunCountRow {
    /// 归属 agent。
    pub fn agent_id(&self) -> Id {
        Id(self.agent_id)
    }
}

/// 每个 agent 的按天活动桶（上游 `GetWorkspaceAgentActivity30d`）。
#[derive(Debug, Clone, FromRow)]
pub struct AgentActivityBucketRow {
    pub agent_id: Uuid,
    pub bucket: DateTime<Utc>,
    pub task_count: i32,
    pub failed_count: i32,
    pub completed_count: i32,
    pub cancelled_count: i32,
}

impl AgentActivityBucketRow {
    /// 归属 agent。
    pub fn agent_id(&self) -> Id {
        Id(self.agent_id)
    }
}

impl AgentRepo {
    /// 某 agent 的全部任务（上游 `ListAgentTasks`，`ORDER BY created_at DESC`），
    /// 再套上游 `visibleTaskHistory` 的可见性过滤。
    pub async fn list_tasks(&self, agent_id: Id) -> Result<Vec<AgentTaskRow>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM agent_task_queue WHERE agent_id = $1 \
             ORDER BY created_at DESC"
        );
        let rows = sqlx::query_as::<_, AgentTaskRow>(&sql)
            .bind(agent_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows.into_iter().filter(is_visible_task_history).collect())
    }

    /// 取消 agent 的全部在飞任务（上游 `CancelAgentTasksByAgent`），返回被取消行数。
    ///
    /// 与上游的差异：上游包在一个事务里并额外做委派失败结算 + 事件广播 +
    /// `ReconcileAgentStatus`；本片只做状态置位（M3-3/M3-6 接管进程内副作用）。
    pub async fn cancel_tasks(&self, agent_id: Id) -> Result<u64> {
        sqlx::query(
            "UPDATE agent_task_queue SET \
                status = 'cancelled', completed_at = now(), prepare_lease_expires_at = NULL, \
                cancelled_by_type = 'system', cancelled_by_id = NULL, cancelled_by_name = NULL \
             WHERE agent_id = $1 \
               AND status IN ('queued', 'dispatched', 'running', 'waiting_local_directory', 'deferred')",
        )
        .bind(agent_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)
        .map(|done| done.rows_affected())
    }

    /// workspace 级任务快照（上游 `ListWorkspaceAgentTaskSnapshot`）：
    /// 在飞半边 + 每 agent 的 LATERAL Top-1 结果半边。
    pub async fn task_snapshot(&self, workspace_id: Id) -> Result<Vec<AgentTaskRow>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS_ALIASED} FROM agent_task_queue atq \
             JOIN agent a ON a.id = atq.agent_id \
             WHERE a.workspace_id = $1 \
               AND (atq.status IN ('queued', 'dispatched', 'running', 'waiting_local_directory') \
                    OR (atq.status = 'deferred' AND atq.context->>'wakeup_id' IS NOT NULL)) \
             UNION ALL \
             SELECT {TASK_COLUMNS_ALIASED} FROM agent a \
             JOIN LATERAL ( \
                 SELECT atq.* FROM agent_task_queue atq \
                 WHERE atq.agent_id = a.id AND atq.status IN ('completed', 'failed') \
                 ORDER BY atq.completed_at DESC NULLS LAST, atq.created_at DESC, atq.id DESC \
                 LIMIT 1 \
             ) atq ON TRUE \
             WHERE a.workspace_id = $1"
        );
        sqlx::query_as::<_, AgentTaskRow>(&sql)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 近 30 天每 agent 的 run 次数（上游 `GetWorkspaceAgentRunCounts`）。
    pub async fn run_counts_30d(&self, workspace_id: Id) -> Result<Vec<AgentRunCountRow>> {
        sqlx::query_as::<_, AgentRunCountRow>(
            "SELECT atq.agent_id, COUNT(*)::int AS run_count \
             FROM agent_task_queue atq JOIN agent a ON a.id = atq.agent_id \
             WHERE a.workspace_id = $1 AND atq.created_at > now() - INTERVAL '30 days' \
             GROUP BY atq.agent_id",
        )
        .bind(workspace_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 近 30 天每 agent 的按天活动桶（上游 `GetWorkspaceAgentActivity30d`）。
    /// 锚点是 `completed_at`；无完成的日期不产出行（前端零填充）。
    pub async fn activity_30d(&self, workspace_id: Id) -> Result<Vec<AgentActivityBucketRow>> {
        sqlx::query_as::<_, AgentActivityBucketRow>(
            "SELECT atq.agent_id, \
                    DATE_TRUNC('day', atq.completed_at)::timestamptz AS bucket, \
                    COUNT(*)::int AS task_count, \
                    COUNT(*) FILTER (WHERE atq.status = 'failed')::int AS failed_count, \
                    COUNT(*) FILTER (WHERE atq.status = 'completed')::int AS completed_count, \
                    COUNT(*) FILTER (WHERE atq.status = 'cancelled')::int AS cancelled_count \
             FROM agent_task_queue atq JOIN agent a ON a.id = atq.agent_id \
             WHERE a.workspace_id = $1 AND atq.completed_at IS NOT NULL \
               AND atq.completed_at > now() - INTERVAL '30 days' \
             GROUP BY atq.agent_id, bucket ORDER BY atq.agent_id, bucket",
        )
        .bind(workspace_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }
}

/// 上游 `visibleTaskHistory`：未启动就被取消的委派回退行不展示。
pub(crate) fn is_visible_task_history(task: &AgentTaskRow) -> bool {
    let unused_escalation_fallback = task.escalation_for_task_id.is_some()
        && task.started_at.is_none()
        && matches!(task.status.as_str(), "deferred" | "cancelled");
    !unused_escalation_fallback
}

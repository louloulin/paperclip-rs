//! runtime 拆除（teardown）与两条删除路径（M3-4 / LUM-1427）。
//!
//! 对应 upstream `server/internal/service/runtime_teardown.go::TeardownRuntime` 与
//! `handler/runtime.go` 的 `DeleteAgentRuntime` / `UnbindAgentsAndDeleteRuntime`。
//!
//! 为什么需要应用层 teardown：迁移 `120_runtime_profile` 移除了 `ON DELETE CASCADE`，
//! `agent.runtime_id` 是 `ON DELETE RESTRICT`（迁移 `251_agent_runtime_unbind` 之后
//! runtime 删除是「解绑」而不是「销毁」）。所以删 runtime 行之前必须先把引用拆干净：
//! user agent 解绑保留（MUL-5559：不再销毁用户的 agent / 会话 / 任务记录）、
//! 系统 agent 直接删、非终态任务取消、任务历史摘掉、受影响的 autopilot 暂停。
//!
//! 两条删除路径的区别只在「活跃 agent 怎么办」：
//! - [`super::AgentRuntimeRepo::delete_strict`]：有任何非归档 user agent 就拒绝（409），
//!   并在 body 里带上它们，省掉前端一次往返；
//! - [`super::AgentRuntimeRepo::unbind_agents_and_delete`]：用户确认过计划后执行，
//!   先核对「确认时的活跃集合」是否还等于当前集合，漂了就用 409 让前端重新确认。
//!
//! 两条路径共用同一个 teardown，且都在**一个事务**里完成，不留半拆状态。

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use mc_core::Id;

use super::ledger::{count_undrained_tasks, fetch_active_agents, BlockingAgentRow};

/// teardown 对业务对象的实际改动（upstream `RuntimeTeardownResult` 的计数投影）。
///
/// upstream 回的是完整行（要拿去广播事件）；传输层事件属于 M3-7，这里只要计数
/// —— 路由层用它填 `unbind-agents-and-delete` 的响应。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeardownOutcome {
    pub agents_unbound: u64,
    pub tasks_cancelled: u64,
    pub autopilots_paused: u64,
}

/// teardown 失败原因（upstream `ErrRuntimeNotDrained` / `ErrRuntimeWorkspaceMismatch`）。
#[derive(Debug)]
pub enum TeardownError {
    /// runtime 行不存在（并发删掉了，或调用方拿着 stale id）。
    RuntimeNotFound,
    /// 取消之后仍有未完成 task —— 宁可 abort 也不靠级联把行吞掉。
    NotDrained,
    /// agent 与 runtime 跨 workspace 绑定（`agent.runtime_id` 没有复合 workspace 外键，
    /// 只能应用层挡）。
    WorkspaceMismatch,
    Db(String),
}

impl std::fmt::Display for TeardownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuntimeNotFound => write!(f, "runtime not found"),
            Self::NotDrained => write!(f, "runtime still has non-terminal tasks"),
            Self::WorkspaceMismatch => write!(f, "runtime agent workspace mismatch"),
            Self::Db(m) => write!(f, "database error: {m}"),
        }
    }
}

/// 删除路径对外暴露的失败原因（决定路由层的状态码）。
#[derive(Debug)]
pub enum DeleteRuntimeError {
    NotFound,
    /// 严格删除：仍有非归档 user agent 绑着 → 409 `runtime_has_active_agents`。
    HasActiveAgents(Vec<BlockingAgentRow>),
    /// 确认删除：确认过的活跃集合与当前集合不一致 → 409 `runtime_delete_plan_changed`。
    PlanChanged(Vec<BlockingAgentRow>),
    /// 409 `runtime_delete_not_drained`。
    NotDrained,
    /// 409 `runtime_delete_workspace_mismatch`（strict 路径；确认路径上游按 500 处理）。
    WorkspaceMismatch,
    Db(String),
}

impl From<TeardownError> for DeleteRuntimeError {
    fn from(e: TeardownError) -> Self {
        match e {
            TeardownError::RuntimeNotFound => Self::NotFound,
            TeardownError::NotDrained => Self::NotDrained,
            TeardownError::WorkspaceMismatch => Self::WorkspaceMismatch,
            TeardownError::Db(m) => Self::Db(m),
        }
    }
}

impl std::fmt::Display for DeleteRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "runtime not found"),
            Self::HasActiveAgents(_) => write!(f, "runtime has active agents"),
            Self::PlanChanged(_) => write!(f, "active agent set changed"),
            Self::NotDrained => write!(f, "runtime still has tasks in flight"),
            Self::WorkspaceMismatch => write!(f, "runtime agent workspace mismatch"),
            Self::Db(m) => write!(f, "database error: {m}"),
        }
    }
}

/// 取消**每一个**非终态 status：`deferred`（迁移 128，评论路由升级）必须在内 ——
/// 漏了它，`agent_task_queue_active_requires_runtime` 约束会把删除变成 500
/// （upstream 注释里记着的那个历史 bug）。
const CANCEL_NON_TERMINAL_TASKS: &str = "UPDATE agent_task_queue \
     SET status = 'cancelled', completed_at = now(), \
         cancelled_by_type = 'system', cancelled_by_id = NULL, cancelled_by_name = NULL \
     WHERE (runtime_id = $1 OR agent_id = ANY($2)) \
       AND status IN ('queued', 'dispatched', 'running', 'waiting_local_directory', 'deferred')";

/// 摘掉任务历史：只动终态行（活跃行必须保留 runtime，见上面的 CHECK 约束）。
const UNBIND_TASK_HISTORY: &str = "UPDATE agent_task_queue SET runtime_id = NULL \
     WHERE runtime_id = $1 AND completed_at IS NOT NULL";

/// user agent 解绑保留（含已归档的；**不过滤 `archived_at`** —— 归档的同样是用户数据）。
const UNBIND_USER_AGENTS: &str = "UPDATE agent SET runtime_id = NULL, updated_at = now() \
     WHERE runtime_id = $1 AND kind = 'user'";

/// 暂停受影响的 autopilot（直接指派的 + leader 被解绑的 squad），保留配置等重新绑定。
const PAUSE_AUTOPILOTS: &str = "UPDATE autopilot a SET status = 'paused', \
         pause_reason = 'agent_runtime_required', updated_at = now() \
     WHERE a.status = 'active' \
       AND ((a.assignee_type = 'agent' AND a.assignee_id = ANY($1)) \
         OR (a.assignee_type = 'squad' AND EXISTS ( \
               SELECT 1 FROM squad s WHERE s.id = a.assignee_id AND s.leader_id = ANY($1))))";

const COUNT_TASKS_BY_RUNTIME: &str = "SELECT count(*) FROM agent_task_queue WHERE runtime_id = $1";

const DELETE_SYSTEM_AGENTS: &str = "DELETE FROM agent WHERE runtime_id = $1 AND kind = 'system'";

/// 系统 agent 名下**没有 FK**的依赖表，必须在删 agent 行之前同事务清掉。
///
/// 只清 `kind = 'system'` —— 用户 agent 自 MUL-5559 起在 runtime 删除后仍然存在，
/// 顺手清掉它们的 invocation target / 频道安装 / 标签会把还活着的 agent 弄坏。
const SYSTEM_AGENT_PRUNES: &[&str] = &[
    "DELETE FROM agent_invocation_target WHERE agent_id IN \
         (SELECT id FROM agent WHERE runtime_id = $1 AND kind = 'system')",
    "DELETE FROM chat_pinned_agent WHERE agent_id IN \
         (SELECT id FROM agent WHERE runtime_id = $1 AND kind = 'system')",
    "DELETE FROM agent_to_label WHERE agent_id IN \
         (SELECT id FROM agent WHERE runtime_id = $1 AND kind = 'system')",
    // 频道安装及其全部从属行（迁移明确没有 workspace/agent 级联；留下孤儿会继续占着
    // 机器人 (channel_type, app_id) 的路由位，upstream #4810）。
    "WITH system_agents AS ( \
         SELECT id FROM agent WHERE runtime_id = $1 AND kind = 'system' \
     ), doomed_sessions AS ( \
         SELECT id FROM chat_session WHERE agent_id IN (SELECT id FROM system_agents) \
     ), doomed AS ( \
         SELECT id FROM channel_installation WHERE agent_id IN (SELECT id FROM system_agents) \
     ), c1 AS (DELETE FROM dingtalk_group_presence WHERE installation_id IN (SELECT id FROM doomed)), \
        c2 AS (DELETE FROM dingtalk_bot_identity WHERE installation_id IN (SELECT id FROM doomed)), \
        c3 AS (DELETE FROM dingtalk_group_route WHERE installation_id IN (SELECT id FROM doomed)), \
        c4 AS (DELETE FROM channel_reply_delivery WHERE installation_id IN (SELECT id FROM doomed)), \
        c5 AS (DELETE FROM channel_task_delivery WHERE installation_id IN (SELECT id FROM doomed)), \
        c6 AS (DELETE FROM channel_outbound_message WHERE installation_id IN (SELECT id FROM doomed)), \
        c7 AS (DELETE FROM channel_chat_session_binding \
               WHERE installation_id IN (SELECT id FROM doomed) RETURNING chat_session_id), \
        c8 AS (DELETE FROM channel_chat_context_generation \
               WHERE chat_session_id IN (SELECT chat_session_id FROM c7) \
                  OR chat_session_id IN (SELECT id FROM doomed_sessions)), \
        c9 AS (DELETE FROM channel_outbound_card_message \
               WHERE chat_session_id IN (SELECT chat_session_id FROM c7)), \
        c10 AS (DELETE FROM channel_binding_token WHERE installation_id IN (SELECT id FROM doomed)), \
        c11 AS (DELETE FROM channel_user_binding WHERE installation_id IN (SELECT id FROM doomed)), \
        c12 AS (DELETE FROM channel_inbound_message_dedup \
                WHERE installation_id IN (SELECT id FROM doomed)), \
        c13 AS (DELETE FROM channel_inbound_audit WHERE installation_id IN (SELECT id FROM doomed)) \
     DELETE FROM channel_installation WHERE id IN (SELECT id FROM doomed)",
];

/// 与 upstream `LockChatSessionsBySystemRuntimeAgents` 同义：上面两个草稿表在
/// `chat_session` 上没有 FK，且 join 需要会话行，所以先在事务里锁住它们。
const LOCK_SYSTEM_CHAT_SESSIONS: &str =
    "SELECT cs.id FROM chat_session cs JOIN agent a ON a.id = cs.agent_id \
     WHERE a.runtime_id = $1 AND a.kind = 'system' ORDER BY cs.id FOR UPDATE OF cs";

/// 依赖 `chat_session` 的两张草稿表；必须在 `LOCK_SYSTEM_CHAT_SESSIONS` 之后跑。
const CHAT_DRAFT_PRUNES: &[&str] = &[
    "DELETE FROM chat_draft_restore WHERE chat_session_id IN ( \
         SELECT cs.id FROM chat_session cs JOIN agent a ON a.id = cs.agent_id \
         WHERE a.runtime_id = $1 AND a.kind = 'system')",
    "DELETE FROM agent_builder_draft WHERE chat_session_id IN ( \
         SELECT cs.id FROM chat_session cs JOIN agent a ON a.id = cs.agent_id \
         WHERE a.runtime_id = $1 AND a.kind = 'system')",
];

#[allow(clippy::needless_pass_by_value)] // `map_err(db_err)` 的签名就要求按值收。
fn db_err(e: sqlx::Error) -> TeardownError {
    TeardownError::Db(e.to_string())
}

/// 删 runtime 行之前的共享改动阶段（调用方持有事务，且**必须**是同一个事务）。
///
/// 顺序（与 upstream `TeardownRuntime` 一致）：
/// 锁 runtime → 锁 user agent → 校验同 workspace → 取消非终态任务 → drain 检查（fail-closed）
/// → 解绑 user agent → 暂停 autopilot → 摘任务历史 → 复查无残留 → 清非 FK 依赖 → 删系统 agent。
pub(crate) async fn teardown_runtime(
    tx: &mut Transaction<'_, Postgres>,
    runtime_id: Id,
) -> std::result::Result<TeardownOutcome, TeardownError> {
    // `FOR UPDATE` 与 `agent.runtime_id` 外键校验所需的 `FOR KEY SHARE` 冲突，
    // 所以这一步同时挡住「新 agent 指向本 runtime」的并发写入。
    let runtime: Option<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id, workspace_id FROM agent_runtime WHERE id = $1 FOR UPDATE")
            .bind(runtime_id.as_uuid())
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_err)?;
    let Some((_, runtime_ws)) = runtime else {
        return Err(TeardownError::RuntimeNotFound);
    };

    // 活跃 + 已归档的 user agent 一起按 id 序锁：只锁活跃快照会留下「归档行在确认后
    // 被恢复」的竞态。
    let locked_agents: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, workspace_id FROM agent \
         WHERE runtime_id = $1 AND kind = 'user' ORDER BY id FOR UPDATE",
    )
    .bind(runtime_id.as_uuid())
    .fetch_all(&mut **tx)
    .await
    .map_err(db_err)?;

    if locked_agents.iter().any(|(_, ws)| *ws != runtime_ws) {
        return Err(TeardownError::WorkspaceMismatch);
    }
    let agent_ids: Vec<Uuid> = locked_agents.iter().map(|(id, _)| *id).collect();
    let agent_id_values: Vec<Id> = agent_ids.iter().copied().map(Id::from).collect();

    let cancelled = sqlx::query(CANCEL_NON_TERMINAL_TASKS)
        .bind(runtime_id.as_uuid())
        .bind(&agent_ids)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?
        .rows_affected();

    let undrained = count_undrained_tasks(&mut **tx, runtime_id, &agent_id_values)
        .await
        .map_err(db_err)?;
    if undrained > 0 {
        return Err(TeardownError::NotDrained);
    }

    let unbound = sqlx::query(UNBIND_USER_AGENTS)
        .bind(runtime_id.as_uuid())
        .execute(&mut **tx)
        .await
        .map_err(db_err)?
        .rows_affected();

    let paused = sqlx::query(PAUSE_AUTOPILOTS)
        .bind(&agent_ids)
        .execute(&mut **tx)
        .await
        .map_err(db_err)?
        .rows_affected();

    sqlx::query(UNBIND_TASK_HISTORY)
        .bind(runtime_id.as_uuid())
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;

    // fail-closed：历史没摘干净就 abort，而不是靠遗留的 ON DELETE CASCADE 把行吞掉。
    let remaining = sqlx::query_scalar::<_, i64>(COUNT_TASKS_BY_RUNTIME)
        .bind(runtime_id.as_uuid())
        .fetch_one(&mut **tx)
        .await
        .map_err(db_err)?;
    if remaining != 0 {
        return Err(TeardownError::NotDrained);
    }

    for sql in SYSTEM_AGENT_PRUNES {
        sqlx::query(sql)
            .bind(runtime_id.as_uuid())
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
    }
    sqlx::query(LOCK_SYSTEM_CHAT_SESSIONS)
        .bind(runtime_id.as_uuid())
        .fetch_all(&mut **tx)
        .await
        .map_err(db_err)?;
    for sql in CHAT_DRAFT_PRUNES {
        sqlx::query(sql)
            .bind(runtime_id.as_uuid())
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
    }
    // 非 FK 依赖清完之后才能删系统 agent（它们的会话行随 agent 级联消失）。
    sqlx::query(DELETE_SYSTEM_AGENTS)
        .bind(runtime_id.as_uuid())
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;

    Ok(TeardownOutcome {
        agents_unbound: unbound,
        tasks_cancelled: cancelled,
        autopilots_paused: paused,
    })
}

/// 用户确认的那个活跃集合（去重后按字符串比较；顺序无关，与 upstream
/// `activeAgentSetMatches` 同义 —— 前端渲染顺序不保证）。
fn canonical_set(ids: &[Id]) -> Vec<String> {
    let mut set: Vec<String> = ids.iter().copied().map(Id::as_string).collect();
    set.sort_unstable();
    set.dedup();
    set
}

fn live_set(rows: &[BlockingAgentRow]) -> Vec<String> {
    let mut set: Vec<String> = rows.iter().map(|a| a.id.as_string()).collect();
    set.sort_unstable();
    set.dedup();
    set
}

async fn delete_with_plan(
    pool: &PgPool,
    runtime_id: Id,
    expected: Option<&[String]>,
) -> std::result::Result<TeardownOutcome, DeleteRuntimeError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| DeleteRuntimeError::Db(e.to_string()))?;

    // 先确认 runtime 存在并锁住（`FOR UPDATE` 同时挡住新 agent 指向它）。
    let exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM agent_runtime WHERE id = $1 FOR UPDATE")
            .bind(runtime_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| DeleteRuntimeError::Db(e.to_string()))?;
    if exists.is_none() {
        return Err(DeleteRuntimeError::NotFound);
    }

    // 事务内复核活跃集合（逐行加锁），保证「用户确认的就是即将解绑的」。
    let current = fetch_active_agents(&mut *tx, runtime_id, true)
        .await
        .map_err(|e| DeleteRuntimeError::Db(e.to_string()))?;
    let planned = match expected {
        None if current.is_empty() => true,
        None => false,
        Some(expected) => live_set(&current) == expected,
    };
    if !planned {
        return Err(if expected.is_some() {
            DeleteRuntimeError::PlanChanged(current)
        } else {
            DeleteRuntimeError::HasActiveAgents(current)
        });
    }

    let outcome = teardown_runtime(&mut tx, runtime_id).await?;

    sqlx::query("DELETE FROM agent_runtime WHERE id = $1")
        .bind(runtime_id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(|e| DeleteRuntimeError::Db(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| DeleteRuntimeError::Db(e.to_string()))?;
    Ok(outcome)
}

impl super::AgentRuntimeRepo {
    /// 严格删除（`DELETE /api/runtimes/{id}/`）：活跃 agent 还在就是 409。
    ///
    /// # Errors
    /// 见 [`DeleteRuntimeError`]。
    pub async fn delete_strict(
        &self,
        runtime_id: Id,
    ) -> std::result::Result<TeardownOutcome, DeleteRuntimeError> {
        delete_with_plan(self.conn(), runtime_id, None).await
    }

    /// 确认删除（`unbind-agents-and-delete` / `archive-agents-and-delete`）。
    ///
    /// `expected_active_agent_ids` 为空是合法计划（「我确认没有活跃 agent」）。
    ///
    /// # Errors
    /// 计划漂移 → [`DeleteRuntimeError::PlanChanged`]；其余见 [`DeleteRuntimeError`]。
    pub async fn unbind_agents_and_delete(
        &self,
        runtime_id: Id,
        expected_active_agent_ids: &[Id],
    ) -> std::result::Result<TeardownOutcome, DeleteRuntimeError> {
        let expected = canonical_set(expected_active_agent_ids);
        delete_with_plan(self.conn(), runtime_id, Some(&expected)).await
    }
}

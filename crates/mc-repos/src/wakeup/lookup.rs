//! wakeup 面的**读侧小查询 + 事务围栏**（服务层要用的那几块，逐个对应上游 query 名）。
//!
//! - **写者**：M5-6。
//! - **为什么单独一个文件**：`issue.rs` 已经收了 20+ 个「读整行 / 写整行」的函数，
//!   本文件的成员都是**权限判定与锁**用的窄查询（`GetMemberByUserAndWorkspace` /
//!   `GetAgentInWorkspace` / `ListAgentInvocationTargets` / 评论存在性 / `lock_task_owner_rows`），
//!   拆开既避免撞门 ⑩（单文件 ≤800 行），也让「服务层只依赖这些原语」这件事一眼可见。
//! - **泛型 `PgExecutor`**：这些查询既要在 `&PgPool`（handler 直读）上跑，也要在
//!   `&mut PgConnection`（`save`/`dispatch` 的事务内，必须在同一把行锁下读）上跑 ⇒
//!   用 `impl PgExecutor` 而不是写两份。**写路径不受此影响**（写一律 `&mut PgConnection`）。

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgExecutor};
use uuid::Uuid;

use super::map_wakeup_err;
use crate::Result;

/// `GetAgentInWorkspace` 的 wakeup 侧投影（只要权限 + runtime 判定的那几列）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WakeupAgentRow {
    /// agent 主键。
    pub id: Uuid,
    /// 所有者（`CanMemberInvokeAgent` 的第一条规则）。
    pub owner_id: Option<Uuid>,
    /// `private` / `public_to`。
    pub permission_mode: String,
    /// 绑定的 runtime（**缺失 = 不可被 wakeup 唤醒**）。
    pub runtime_id: Option<Uuid>,
    /// 归档时间（非空 = 不可唤醒）。
    pub archived_at: Option<DateTime<Utc>>,
}

/// `GetMemberByUserAndWorkspace`：拿角色；不是成员 ⇒ `None`（上游是 `pgx.ErrNoRows`）。
pub async fn member_role<'e, E>(ex: E, workspace_id: Uuid, user_id: Uuid) -> Result<Option<String>>
where
    E: PgExecutor<'e>,
{
    sqlx::query_scalar::<_, String>("SELECT role FROM member WHERE workspace_id=$1 AND user_id=$2")
        .bind(workspace_id)
        .bind(user_id)
        .fetch_optional(ex)
        .await
        .map_err(map_wakeup_err)
}

/// `GetAgentInWorkspace`：`None` = 行不存在（上游 `pgx.ErrNoRows`）。
pub async fn agent_for_wakeup<'e, E>(
    ex: E,
    workspace_id: Uuid,
    agent_id: Uuid,
) -> Result<Option<WakeupAgentRow>>
where
    E: PgExecutor<'e>,
{
    sqlx::query_as::<_, WakeupAgentRow>(
        "SELECT id, owner_id, permission_mode, runtime_id, archived_at \
         FROM agent WHERE id=$1 AND workspace_id=$2",
    )
    .bind(agent_id)
    .bind(workspace_id)
    .fetch_optional(ex)
    .await
    .map_err(map_wakeup_err)
}

/// `ListAgentInvocationTargets`（只取判定要的两列，`CanMemberInvokeAgent` 用）。
pub async fn invocation_targets<'e, E>(ex: E, agent_id: Uuid) -> Result<Vec<(String, Uuid)>>
where
    E: PgExecutor<'e>,
{
    sqlx::query_as::<_, (String, Uuid)>(
        "SELECT target_type, target_id FROM agent_invocation_target WHERE agent_id=$1",
    )
    .bind(agent_id)
    .fetch_all(ex)
    .await
    .map_err(map_wakeup_err)
}

/// 上游 `save` 内联：父评论必须属于本 issue 且未删除。
pub async fn comment_exists<'e, E>(ex: E, comment_id: Uuid, issue_id: Uuid) -> Result<bool>
where
    E: PgExecutor<'e>,
{
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM comment WHERE id=$1 AND issue_id=$2 AND deleted_at IS NULL)",
    )
    .bind(comment_id)
    .bind(issue_id)
    .fetch_one(ex)
    .await
    .map_err(map_wakeup_err)
}

/// `LockWorkspaceForChatSessionCreate`：`SELECT id FROM workspace WHERE id=$1 FOR KEY SHARE`
/// —— 派发路径在解析不到 agent 时用它串行化 workspace 级操作（与 `DeleteWorkspace` 互斥）。
pub async fn lock_workspace(conn: &mut PgConnection, workspace_id: Uuid) -> Result<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM workspace WHERE id=$1 FOR KEY SHARE")
        .bind(workspace_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_wakeup_err)
}

/// 上游 `dispatch` 的 `SET LOCAL lock_timeout = '50ms'`（**事务内**生效；同时限住随后的
/// `FOR UPDATE`/`NOWAIT` 等锁等待，让 100 条争抢的规则最多烧掉 ~5s 预算）。
pub async fn set_lock_timeout(conn: &mut PgConnection, millis: u32) -> Result<()> {
    sqlx::query(&format!("SET LOCAL lock_timeout = '{millis}ms'"))
        .execute(&mut *conn)
        .await
        .map_err(map_wakeup_err)?;
    Ok(())
}

/// `SELECT now()`（上游在多处把它当**事务内**的时间基准用，别用应用侧时钟）。
pub async fn transaction_now(conn: &mut PgConnection) -> Result<DateTime<Utc>> {
    sqlx::query_scalar::<_, DateTime<Utc>>("SELECT now()")
        .fetch_one(&mut *conn)
        .await
        .map_err(map_wakeup_err)
}

/// `lock_task_owner_rows(agent, issue, runtime)`：`false` ⇒ 上游立刻 `return pgx.ErrNoRows`
/// （由 `Tick` 记进 `last_error`）—— 这是「同一 owner 的行正在被别人改」的乐观围栏。
pub async fn lock_task_owner_rows(
    conn: &mut PgConnection,
    agent_id: Uuid,
    issue_id: Uuid,
    runtime_id: Option<Uuid>,
) -> Result<bool> {
    sqlx::query_scalar::<_, bool>("SELECT lock_task_owner_rows($1,$2,$3)")
        .bind(agent_id)
        .bind(issue_id)
        .bind(runtime_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_wakeup_err)
}

/// 上游 `qCleanupMissingWakeup`：issue 行没了 ⇒ 删 receipt 与 wakeup（**同一事务**内）。
pub async fn delete_wakeup_cascade(conn: &mut PgConnection, wakeup_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM issue_wakeup_receipt WHERE wakeup_id=$1")
        .bind(wakeup_id)
        .execute(&mut *conn)
        .await
        .map_err(map_wakeup_err)?;
    sqlx::query("DELETE FROM issue_wakeup WHERE id=$1")
        .bind(wakeup_id)
        .execute(&mut *conn)
        .await
        .map_err(map_wakeup_err)?;
    Ok(())
}

/// 上游 `GetAgentInWorkspace` 的 runtime 列（`dispatch` 里与 `candidate.RuntimeID` 比对）。
pub async fn agent_runtime<'e, E>(ex: E, workspace_id: Uuid, agent_id: Uuid) -> Result<Option<Uuid>>
where
    E: PgExecutor<'e>,
{
    sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT runtime_id FROM agent WHERE id=$1 AND workspace_id=$2",
    )
    .bind(agent_id)
    .bind(workspace_id)
    .fetch_optional(ex)
    .await
    .map_err(map_wakeup_err)
    .map(Option::flatten)
}

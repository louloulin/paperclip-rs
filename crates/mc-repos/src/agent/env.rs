//! `custom_env` 的写路径 + 审计（上游 `server/internal/handler/agent_env.go`）。
//!
//! 上游把「写 `custom_env`」与「写 `activity_log` 审计行」放进**同一个事务**：
//! 审计写失败 ⇒ 回滚 env 修改（`agent_env.go:225-265`）；读路径反过来是 fail-closed
//! ——审计行落不下去就**拒绝**返回明文（`agent_env.go:159-169`）。本模块是这两条
//! 语义的仓储层落点，handler 只负责合并/掩码与 HTTP 形状。
//!
//! 只有 `activity_log` 的写入在这里：`mc-repos` 目前没有通用 `activity` repo
//! （M2 的 inbox/issue 各自按需读它），env 审计是**只写**的取证行，
//! `issue_id` 恒为 `NULL`（上游注释：env access is not tied to an issue）。

use mc_core::Id;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use super::{AgentRepo, AgentRow, AGENT_COLUMNS};
use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 审计动作名（上游 `agent_env.go:32-35` 的两个常量）。
pub const ACTIVITY_ENV_REVEALED: &str = "agent_env_revealed";
/// 审计动作名：env 更新。
pub const ACTIVITY_ENV_UPDATED: &str = "agent_env_updated";

impl AgentRepo {
    /// 单事务：整表替换 `custom_env` + 落一条 `activity_log` 审计行。
    ///
    /// 返回更新后的行；事务提交前审计行与 env 值同生共死。
    pub async fn update_custom_env_audited(
        &self,
        id: Id,
        custom_env: &JsonValue,
        actor_id: Uuid,
        action: &str,
        details: &JsonValue,
    ) -> Result<AgentRow> {
        let mut tx = self.db().pool().begin().await.map_err(map_sqlx_err)?;
        let sql = format!(
            "UPDATE agent SET custom_env = $2::jsonb, updated_at = now() \
             WHERE id = $1 RETURNING {AGENT_COLUMNS}"
        );
        let row = sqlx::query_as::<_, AgentRow>(&sql)
            .bind(id.0)
            .bind(custom_env)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        insert_env_activity(&mut *tx, row.workspace_id, actor_id, action, details).await?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 只写一条 env 审计行（读路径的 `agent_env_revealed`）。
    ///
    /// handler 必须把它当作**前置条件**：失败就不返回明文（fail-closed）。
    pub async fn record_env_activity(
        &self,
        workspace_id: Id,
        actor_id: Uuid,
        action: &str,
        details: &JsonValue,
    ) -> Result<()> {
        insert_env_activity(self.db().pool(), workspace_id.0, actor_id, action, details).await
    }
}

/// `activity_log` 插入（`issue_id` 恒 NULL；`actor_type` 恒 `'member'`——agent
/// actor 已被 handler 在更早的鉴权阶段拒绝）。
async fn insert_env_activity<'c, E>(
    executor: E,
    workspace_id: Uuid,
    actor_id: Uuid,
    action: &str,
    details: &JsonValue,
) -> Result<()>
where
    E: sqlx::PgExecutor<'c>,
{
    sqlx::query(
        "INSERT INTO activity_log (workspace_id, issue_id, actor_type, actor_id, action, details) \
         VALUES ($1, NULL, 'member', $2, $3, $4::jsonb)",
    )
    .bind(workspace_id)
    .bind(actor_id)
    .bind(action)
    .bind(details)
    .execute(executor)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_actions_match_upstream_constants() {
        assert_eq!(ACTIVITY_ENV_REVEALED, "agent_env_revealed");
        assert_eq!(ACTIVITY_ENV_UPDATED, "agent_env_updated");
    }
}

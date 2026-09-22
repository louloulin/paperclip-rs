//! agent ↔ label 关联（`agent_to_label` + `issue_label`，M3-5 / LUM-1428）。
//!
//! 上游对照：`server/internal/handler/label.go:576/591/620` +
//! `server/pkg/db/queries/issue_label.sql:181/190/206`。
//!
//! 注意（与上游一致）：
//! - 只有 `resource_type = 'agent'` 的 label 能挂到 agent（否则 404
//!   `"agent label not found"`，由 handler 判）；重复挂载靠
//!   `ON CONFLICT DO NOTHING` 静默幂等。
//! - `issue_label` **没有** `usage_count` 列（上游是 `ListLabels` 的 join 计数），
//!   因此 agent 侧响应的 `usage_count` 恒为 `0`。

use mc_core::Id;
use sqlx::FromRow;
use uuid::Uuid;

use crate::agent::AgentRepo;
use crate::workspace::map_sqlx_err;
use crate::Result;

/// `issue_label` 行（agent 视角）。
#[derive(Debug, Clone, FromRow)]
pub struct AgentLabelRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub description: String,
    pub color: String,
    pub resource_type: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl AgentLabelRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }
}

impl AgentRepo {
    /// agent 已挂载的 label（上游 `ListLabelsByAgent`，按 `LOWER(name)` 排序）。
    pub async fn list_labels(&self, agent_id: Id) -> Result<Vec<AgentLabelRow>> {
        sqlx::query_as::<_, AgentLabelRow>(
            "SELECT l.id, l.workspace_id, l.name, l.description, l.color, l.resource_type, \
                    l.created_at, l.updated_at \
             FROM issue_label l JOIN agent_to_label atl ON atl.label_id = l.id \
             WHERE atl.agent_id = $1 AND l.resource_type = 'agent' \
             ORDER BY LOWER(l.name) ASC",
        )
        .bind(agent_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 按主键 + workspace 读 label（上游 `GetLabel`）。handler 用它判
    /// `resource_type`，决定 attach 是否 404。
    pub async fn get_label(&self, workspace_id: Id, label_id: Id) -> Result<AgentLabelRow> {
        sqlx::query_as::<_, AgentLabelRow>(
            "SELECT id, workspace_id, name, description, color, resource_type, created_at, updated_at \
             FROM issue_label WHERE id = $1 AND workspace_id = $2",
        )
        .bind(label_id.0)
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 挂载 label（上游 `AttachLabelToAgent`：带 workspace + `resource_type`
    /// 双重 EXISTS 守卫的 `INSERT ... ON CONFLICT DO NOTHING`）。
    pub async fn attach_label(&self, agent_id: Id, label_id: Id, workspace_id: Id) -> Result<u64> {
        sqlx::query(
            "INSERT INTO agent_to_label (agent_id, label_id) \
             SELECT $1::uuid, $2::uuid \
             WHERE EXISTS (SELECT 1 FROM agent a \
                           WHERE a.id = $1::uuid AND a.workspace_id = $3::uuid) \
               AND EXISTS (SELECT 1 FROM issue_label l \
                           WHERE l.id = $2::uuid AND l.workspace_id = $3::uuid \
                             AND l.resource_type = 'agent') \
             ON CONFLICT DO NOTHING",
        )
        .bind(agent_id.0)
        .bind(label_id.0)
        .bind(workspace_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)
        .map(|done| done.rows_affected())
    }

    /// 卸载 label（上游 `DetachLabelFromAgent`）。
    pub async fn detach_label(&self, agent_id: Id, label_id: Id, workspace_id: Id) -> Result<u64> {
        sqlx::query(
            "DELETE FROM agent_to_label \
             WHERE agent_id = $1 AND label_id = $2 \
               AND EXISTS (SELECT 1 FROM agent a \
                           WHERE a.id = $1::uuid AND a.workspace_id = $3::uuid)",
        )
        .bind(agent_id.0)
        .bind(label_id.0)
        .bind(workspace_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)
        .map(|done| done.rows_affected())
    }
}

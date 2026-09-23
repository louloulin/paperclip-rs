//! GC 探针仓储（R7 拆分自 `daemon.rs`）。
//!
//! 四条**只读**投影，供 `/api/daemon/*/gc-check` 与批量 issue 探针使用；批量一条
//! 在 SQL 层按 workspace 过滤（反枚举），见 `routes/daemon/gc.rs` 的模块文档。

// 本文件的 `impl DaemonRepo` 是 `daemon.rs` 那个 impl 的续块（R7 800 行拆分）。

use super::*;

/// `autopilot_run` 探针的原始行列（`(run_id, status, completed_at, workspace_id)`）。
///
/// 取出来便于让 `type_complexity` 闭嘴；`LEFT JOIN` 拿不到父行时 `workspace_id` 为 `None`。
type AutopilotRunGcRaw = (Uuid, String, Option<DateTime<Utc>>, Option<Uuid>);

impl DaemonRepo {
    /// 单 issue GC 探针（upstream `GetIssueGCStatus`；`None` = 行不存在 ⇒ 404）。
    ///
    /// 带上 `workspace_id` 是为了让调用方在做反枚举门（workspace 不匹配同样回 404）
    /// 之前就能定位该 issue 属于哪个 workspace —— 单条探针不像批量探针那样在 SQL 层
    /// 就把 workspace 作为过滤条件。
    pub async fn issue_gc_probe(&self, issue_id: Id) -> Result<Option<IssueGcProbeRow>> {
        let row: Option<(Uuid, Uuid, String, DateTime<Utc>)> =
            sqlx::query_as("SELECT id, workspace_id, status, updated_at FROM issue WHERE id = $1")
                .bind(issue_id.as_uuid())
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_err)?;
        Ok(
            row.map(|(id, workspace_id, status, updated_at)| IssueGcProbeRow {
                id: Id::from(id),
                workspace_id: Id::from(workspace_id),
                status,
                updated_at,
            }),
        )
    }

    /// 批量 issue GC 探针（upstream `ListIssueGCStatuses`，workspace 内过滤）。
    pub async fn list_issue_gc(
        &self,
        workspace_id: Id,
        issue_ids: &[Id],
    ) -> Result<Vec<IssueGcRow>> {
        if issue_ids.is_empty() {
            return Ok(Vec::new());
        }
        let uuids: Vec<Uuid> = issue_ids.iter().map(|id| id.as_uuid()).collect();
        let rows: Vec<(Uuid, String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT id, status, updated_at FROM issue \
             WHERE workspace_id = $1 AND id = ANY($2)",
        )
        .bind(workspace_id.as_uuid())
        .bind(&uuids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|(id, status, updated_at)| {
                let category = issue_category(&status).to_string();
                IssueGcRow {
                    id: Id::from(id),
                    status,
                    category,
                    updated_at: Some(updated_at),
                }
            })
            .collect())
    }

    /// chat session GC 探针（upstream `GetChatSession`；`None` = 已被硬删 ⇒ 404）。
    pub async fn chat_session_gc(&self, session_id: Id) -> Result<Option<ChatSessionGcRow>> {
        let row: Option<(Uuid, Uuid, String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT id, workspace_id, status, updated_at FROM chat_session WHERE id = $1",
        )
        .bind(session_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.map(|(id, ws, status, updated_at)| ChatSessionGcRow {
            id: Id::from(id),
            workspace_id: Id::from(ws),
            status,
            updated_at: Some(updated_at),
        }))
    }

    /// autopilot run GC 探针（upstream `GetAutopilotRun` + 父 `GetAutopilot` 解析 workspace）。
    pub async fn autopilot_run_gc(&self, run_id: Id) -> Result<Option<AutopilotRunGcRow>> {
        let row: Option<AutopilotRunGcRaw> = sqlx::query_as(
            "SELECT r.id, r.status, r.completed_at, a.workspace_id \
             FROM autopilot_run r LEFT JOIN autopilot a ON a.id = r.autopilot_id \
             WHERE r.id = $1",
        )
        .bind(run_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.map(
            |(id, status, completed_at, workspace_id)| AutopilotRunGcRow {
                id: Id::from(id),
                workspace_id: workspace_id.map(Id::from),
                status,
                completed_at,
            },
        ))
    }
}

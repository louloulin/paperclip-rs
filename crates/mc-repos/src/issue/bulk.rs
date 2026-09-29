use mc_core::Id;
use uuid::Uuid;

use super::{
    derive_move_position, map_sqlx_err, IssueRepo, IssueRow, IssueUpdate, RepoError, Result,
};

impl IssueRepo {
    /// 批量更新：逐行复用 `update`，返回成功行数。
    ///
    /// 与上游一致：patch 为空时返回 0（不做 no-op 写入）。
    pub async fn batch_update(
        &self,
        workspace_id: Id,
        ids: &[Id],
        patch: &IssueUpdate,
    ) -> Result<u64> {
        if patch.is_empty() {
            return Ok(0);
        }
        let mut updated = 0_u64;
        for id in ids {
            if self.update(workspace_id, *id, patch).await.is_ok() {
                updated += 1;
            }
        }
        Ok(updated)
    }

    /// 批量删除：返回删除行数（workspace 隔离）。
    pub async fn batch_delete(&self, workspace_id: Id, ids: &[Id]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let raw: Vec<Uuid> = ids.iter().map(|id| id.0).collect();
        let affected =
            sqlx::query("DELETE FROM issue WHERE workspace_id = $1 AND id = ANY($2::uuid[])")
                .bind(workspace_id.0)
                .bind(&raw)
                .execute(self.db.pool())
                .await
                .map_err(map_sqlx_err)?
                .rows_affected();
        Ok(affected)
    }

    /// 拖拽排序：用 `before_id` / `after_id` 两个锚点推导新 position 并落库。
    ///
    /// 锚点不属于本 workspace → `NotFound`；锚点顺序错乱 / 间距过小 → `Conflict`。
    pub async fn move_issue(
        &self,
        workspace_id: Id,
        id: Id,
        before_id: Option<Id>,
        after_id: Option<Id>,
    ) -> Result<IssueRow> {
        self.move_issue_with_update(
            workspace_id,
            id,
            before_id,
            after_id,
            &IssueUpdate::default(),
        )
        .await
    }

    /// 拖拽排序 + 同一次写入里合并其它字段补丁（上游 `MoveIssue` 把 `position` 塞进
    /// `UpdateIssueRequest` 后委托 `UpdateIssue`，只产生一次 revision 自增）。
    ///
    /// `patch.position` 会被锚点推导出的值覆盖；`patch.expected_revision` 生效
    /// （不匹配 → `Conflict`）。
    pub async fn move_issue_with_update(
        &self,
        workspace_id: Id,
        id: Id,
        before_id: Option<Id>,
        after_id: Option<Id>,
        patch: &IssueUpdate,
    ) -> Result<IssueRow> {
        let current = self.get(workspace_id, id).await?;
        let before = match before_id {
            Some(anchor) => Some(self.anchor_position(workspace_id, anchor).await?),
            None => None,
        };
        let after = match after_id {
            Some(anchor) => Some(self.anchor_position(workspace_id, anchor).await?),
            None => None,
        };
        let position =
            derive_move_position(before, after, current.position).ok_or(RepoError::Conflict)?;
        let merged = IssueUpdate {
            position: Some(position),
            ..patch.clone()
        };
        self.update(workspace_id, id, &merged).await
    }

    /// `candidate_id` 是否是 `issue_id` 的祖先（含直接父）。
    ///
    /// 用于拒绝会在父子链上成环的 reparent（上游在 `issueCycleError` 里做同样的检查）。
    /// 递归用 `UNION`（不是 `UNION ALL`）——万一历史数据已有环也能终止。
    pub async fn has_ancestor(
        &self,
        workspace_id: Id,
        issue_id: Id,
        candidate_id: Id,
    ) -> Result<bool> {
        let found: Option<Uuid> = sqlx::query_scalar(
            "WITH RECURSIVE up(parent_issue_id) AS ( \
                 SELECT parent_issue_id FROM issue WHERE workspace_id = $1 AND id = $2 \
                 UNION \
                 SELECT i.parent_issue_id FROM issue i JOIN up ON i.id = up.parent_issue_id \
             ) SELECT parent_issue_id FROM up WHERE parent_issue_id = $3 LIMIT 1",
        )
        .bind(workspace_id.0)
        .bind(issue_id.0)
        .bind(candidate_id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(found.is_some())
    }

    async fn anchor_position(&self, workspace_id: Id, id: Id) -> Result<f64> {
        let position: Option<f64> =
            sqlx::query_scalar("SELECT position FROM issue WHERE workspace_id = $1 AND id = $2")
                .bind(workspace_id.0)
                .bind(id.0)
                .fetch_optional(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        position.ok_or(RepoError::NotFound)
    }

    /// workspace 内 issue 总数（`GET /api/issues/limit-usage`）。
    pub async fn count_in_workspace(&self, workspace_id: Id) -> Result<i64> {
        sqlx::query_scalar("SELECT COUNT(*)::bigint FROM issue WHERE workspace_id = $1")
            .bind(workspace_id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

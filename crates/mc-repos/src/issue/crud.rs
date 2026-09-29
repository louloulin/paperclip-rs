use mc_core::issue::{AssigneeType, IssueOrigin};
use mc_core::priority::Priority;
use mc_core::Id;

use super::{
    issue_prefix_from_slug, map_sqlx_err, IssueRepo, IssueRow, IssueUpdate, NewIssue, RepoError,
    Result, ISSUE_COLUMNS, NUMBER_ALLOC_RETRIES,
};

impl IssueRepo {
    /// `workspace.slug` → identifier 前缀。
    pub async fn workspace_prefix(&self, workspace_id: Id) -> Result<String> {
        let slug: Option<String> = sqlx::query_scalar("SELECT slug FROM workspace WHERE id = $1")
            .bind(workspace_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(slug
            .as_deref()
            .map_or_else(|| "ISS".to_string(), issue_prefix_from_slug))
    }

    /// `MAX(number) + 1`（并发安全由 `UNIQUE(workspace_id, number)` + 重试兜底）。
    pub async fn next_number(&self, workspace_id: Id) -> Result<i32> {
        let next: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(number), 0) + 1 FROM issue WHERE workspace_id = $1",
        )
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(next)
    }

    /// 终态 status key 集合：内置 done/cancelled + 目录里终态 category 的自定义 status。
    ///
    /// 终态在上游是**两档**（`done` / `closed`），只比 `'closed'` 会漏掉
    /// `category = 'done'` 的自定义 status（内置 `done` 本身仍由上面那行硬编码补上）。
    pub async fn terminal_status_keys(&self, workspace_id: Id) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT key FROM issue_status WHERE workspace_id = $1 \
             AND category IN ('done', 'closed') ORDER BY position",
        )
        .bind(workspace_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        let mut keys = vec!["done".to_string(), "cancelled".to_string()];
        for (key,) in rows {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        Ok(keys)
    }

    /// 新建 issue：分配 `number` + `identifier`，position 追加到同 (workspace, status) 列尾。
    ///
    /// `UNIQUE(workspace_id, number)` 冲突时重试（并发创建同 workspace 的两个 issue）。
    pub async fn create(&self, input: NewIssue) -> Result<IssueRow> {
        let prefix = self.workspace_prefix(input.workspace_id).await?;
        let mut last_err = RepoError::Conflict;
        for _ in 0..NUMBER_ALLOC_RETRIES {
            let number = self.next_number(input.workspace_id).await?;
            let identifier = format!("{prefix}-{number}");
            match self.insert_new(&input, number, &identifier).await {
                Ok(row) => return Ok(row),
                Err(e) => {
                    if !matches!(e, RepoError::Conflict) {
                        return Err(e);
                    }
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    /// 单次 INSERT（`create` 的重试体）。
    async fn insert_new(
        &self,
        input: &NewIssue,
        number: i32,
        identifier: &str,
    ) -> Result<IssueRow> {
        sqlx::query_as::<_, IssueRow>(&format!(
            "INSERT INTO issue (workspace_id, number, identifier, title, description, status, \
                 status_name, priority, assignee_type, assignee_id, creator_type, creator_id, \
                 parent_issue_id, project_id, position, stage, start_date, due_date, \
                 metadata, properties, origin, last_activity_at, revision) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::uuid, $11, $12::uuid, $13, $14, \
                 (SELECT COALESCE(MAX(position), 0) + 1 FROM issue \
                   WHERE workspace_id = $1 AND status = $6), \
                 $15, $16, $17, $18, $19, $20, now(), 1) \
             RETURNING {ISSUE_COLUMNS}"
        ))
        .bind(input.workspace_id.0)
        .bind(number)
        .bind(identifier)
        .bind(input.title.as_str())
        .bind(input.description.as_deref())
        .bind(input.status.as_str())
        .bind(input.status_name.as_deref())
        .bind(input.priority.as_str())
        .bind(input.assignee_type.map(AssigneeType::as_str))
        .bind(input.assignee_id.as_deref())
        .bind(input.creator_type.as_str())
        .bind(input.creator_id.as_str())
        .bind(input.parent_issue_id.map(Id::as_uuid))
        .bind(input.project_id.map(Id::as_uuid))
        .bind(input.stage)
        .bind(input.start_date)
        .bind(input.due_date)
        .bind(input.metadata.clone())
        .bind(input.properties.clone())
        .bind(input.origin.map(IssueOrigin::as_str))
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 按 id 取（workspace 隔离：跨 workspace 的 id 一律 `NotFound`）。
    pub async fn get(&self, workspace_id: Id, id: Id) -> Result<IssueRow> {
        sqlx::query_as::<_, IssueRow>(&format!(
            "SELECT {ISSUE_COLUMNS} FROM issue WHERE workspace_id = $1 AND id = $2"
        ))
        .bind(workspace_id.0)
        .bind(id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }

    /// 按 identifier（如 `LUM-1`）取。
    pub async fn get_by_identifier(&self, workspace_id: Id, identifier: &str) -> Result<IssueRow> {
        sqlx::query_as::<_, IssueRow>(&format!(
            "SELECT {ISSUE_COLUMNS} FROM issue WHERE workspace_id = $1 AND identifier = $2"
        ))
        .bind(workspace_id.0)
        .bind(identifier)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }

    /// 更新 issue（`revision + 1`；`expected_revision` 不匹配 → `Conflict`，行不存在 → `NotFound`）。
    ///
    /// 每次成功更新都会刷新 `updated_at` / `last_activity_at`（一次写入即一次活动）。
    pub async fn update(&self, workspace_id: Id, id: Id, patch: &IssueUpdate) -> Result<IssueRow> {
        let sql = format!(
            "UPDATE issue SET \
                 title = CASE WHEN $3::boolean THEN $4::text ELSE title END, \
                 description = CASE WHEN $5::boolean THEN $6::text ELSE description END, \
                 status = CASE WHEN $7::boolean THEN $8::text ELSE status END, \
                 status_name = CASE WHEN $7::boolean THEN $9::text ELSE status_name END, \
                 priority = CASE WHEN $10::boolean THEN $11::text ELSE priority END, \
                 assignee_type = CASE WHEN $12::boolean THEN $13::text ELSE assignee_type END, \
                 assignee_id = CASE WHEN $12::boolean THEN $14::uuid ELSE assignee_id END, \
                 parent_issue_id = CASE WHEN $15::boolean THEN $16::uuid ELSE parent_issue_id END, \
                 project_id = CASE WHEN $17::boolean THEN $18::uuid ELSE project_id END, \
                 position = CASE WHEN $19::boolean THEN $20::double precision ELSE position END, \
                 stage = CASE WHEN $21::boolean THEN $22::int4 ELSE stage END, \
                 start_date = CASE WHEN $23::boolean THEN $24::date ELSE start_date END, \
                 due_date = CASE WHEN $25::boolean THEN $26::date ELSE due_date END, \
                 metadata = CASE WHEN $27::boolean THEN $28::jsonb ELSE metadata END, \
                 properties = CASE WHEN $29::boolean THEN $30::jsonb ELSE properties END, \
                 revision = revision + 1, \
                 updated_at = now(), \
                 last_activity_at = now() \
             WHERE workspace_id = $1 AND id = $2 \
               AND ($31::bigint IS NULL OR revision = $31::bigint) \
             RETURNING {ISSUE_COLUMNS}"
        );

        let row = sqlx::query_as::<_, IssueRow>(&sql)
            .bind(workspace_id.0)
            .bind(id.0)
            .bind(patch.title.is_some())
            .bind(patch.title.as_deref())
            .bind(patch.description.is_some())
            .bind(patch.description.clone().flatten())
            .bind(patch.status.is_some())
            .bind(patch.status.as_deref())
            .bind(patch.status_name.clone().flatten())
            .bind(patch.priority.is_some())
            .bind(patch.priority.map(Priority::as_str))
            .bind(patch.assignee_type.is_some() || patch.assignee_id.is_some())
            .bind(patch.assignee_type.flatten().map(AssigneeType::as_str))
            .bind(patch.assignee_id.clone().flatten())
            .bind(patch.parent_issue_id.is_some())
            .bind(patch.parent_issue_id.flatten().map(Id::as_uuid))
            .bind(patch.project_id.is_some())
            .bind(patch.project_id.flatten().map(Id::as_uuid))
            .bind(patch.position.is_some())
            .bind(patch.position)
            .bind(patch.stage.is_some())
            .bind(patch.stage.flatten())
            .bind(patch.start_date.is_some())
            .bind(patch.start_date.flatten())
            .bind(patch.due_date.is_some())
            .bind(patch.due_date.flatten())
            .bind(patch.metadata.is_some())
            .bind(patch.metadata.clone())
            .bind(patch.properties.is_some())
            .bind(patch.properties.clone())
            .bind(patch.expected_revision)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;

        if let Some(row) = row {
            return Ok(row);
        }

        // 没有更新到行：区分「不存在」和「revision 不匹配」。
        let current: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM issue WHERE workspace_id = $1 AND id = $2")
                .bind(workspace_id.0)
                .bind(id.0)
                .fetch_optional(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        match current {
            None => Err(RepoError::NotFound),
            Some(_) => Err(RepoError::Conflict),
        }
    }

    /// 删除 issue（子 issue 的 `parent_issue_id` 由 FK `ON DELETE SET NULL` 处理）。
    pub async fn delete(&self, workspace_id: Id, id: Id) -> Result<()> {
        let affected = sqlx::query("DELETE FROM issue WHERE workspace_id = $1 AND id = $2")
            .bind(workspace_id.0)
            .bind(id.0)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?
            .rows_affected();
        if affected == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }
}

//! M4 anchor scaffold（LUM-1470）：`project_resource` 仓储 —— M4-1 已填充实现。
//!
//! 归属：M4-1（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。填充内容 = project resource 的列出 /
//! 新建 / 更新 / 删除，覆盖上游 `router.go` #32–#35
//! （`/api/projects/{id}/resources[/{resourceId}]`）。
//!
//! 上游真值：表 `project_resource`（`migrations/upstream/065_project_resources.up.sql`，9 列）；
//! 查询面 `server/pkg/db/queries/project_resource.sql`（52 行 / 10 条 query）；
//! handler `server/internal/handler/project_resource.go`（1061 行）。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::project` / `crate::issue`）：
//! - `Row` 用原始 `Uuid`/`String` 字段 + `Id` / 领域类型访问器，`#[derive(sqlx::FromRow)]`
//!   （`mc_core::Id` 没有 sqlx impl，所以字段不能直接写 `Id`）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，不允许静默跳过）
//!
//! 硬约束：**不引入本仓自造列**；不加迁移；resource 的 `type`/`url` 等取值面照上游列约束
//! 逐条对齐（不要按印象收窄成 enum，除非上游就是 enum/CHECK）。
//!
//! # 与上游的有意偏离
//!
//! - `UpdateProjectResource` / `DeleteProjectResource` 上游只有 `WHERE id = $1`
//!   （handler 已先用 `GetProjectResourceInWorkspace` 校验租户）；本仓加
//!   `AND workspace_id = $N` 作纵深防御，语义等价（不匹配 → 404）。
//! - 唯一违反（`UNIQUE (project_id, resource_type, resource_ref)`，SQLSTATE 23505）走
//!   [`crate::project::WriteError::UniqueViolation`]，路由层回 409
//!   `this resource is already attached to the project`（上游 `isUniqueViolation`）。
//!
//! # 不在这里的判定
//!
//! 「一个 (project, `daemon_id`) 最多一条 `local_directory`」是**应用层**判定
//! （上游 `findLocalDirectoryConflict`，见 `handler/project_resource.go:826`），因为
//! DB 唯一约束只在整份 ref JSON 逐字相等时才触发。该判定放在路由层
//! （`routes/projects/resources.rs`），仓储只提供 `list`。

use mc_core::Id;
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use std::collections::HashMap;
use uuid::Uuid;

use mc_db::Db;

use crate::project::{map_write_err, WriteError};
use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `project_resource` 表的响应相关列（9 列全量）。
pub const PROJECT_RESOURCE_COLUMNS: &str = "id, project_id, workspace_id, resource_type, \
     resource_ref, label, position, created_at, created_by";

/// 单个 resource 行。
#[derive(Debug, Clone, FromRow)]
pub struct ProjectResourceRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub workspace_id: Uuid,
    /// 自由字符串（上游刻意不做 enum：加新类型零 schema 变更）。
    pub resource_type: String,
    /// JSONB；空对象 `{}` 是合法值（上游 `projectResourceToResponse` 的空 ref 兜底）。
    pub resource_ref: JsonValue,
    pub label: Option<String>,
    pub position: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub created_by: Option<Uuid>,
}

impl ProjectResourceRow {
    /// 主键。
    #[must_use]
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 project。
    #[must_use]
    pub fn project_id(&self) -> Id {
        Id(self.project_id)
    }

    /// 所属 workspace。
    #[must_use]
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// 是否为 `local_directory`。
    #[must_use]
    pub fn is_local_directory(&self) -> bool {
        self.resource_type == "local_directory"
    }
}

/// `CreateProjectResource` 入参。
#[derive(Debug, Clone)]
pub struct NewProjectResource {
    pub project_id: Uuid,
    pub workspace_id: Uuid,
    /// 已由路由层 `TrimSpace` 且非空。
    pub resource_type: String,
    /// 已由路由层归一化（`validate_and_normalize_resource_ref`）。
    pub resource_ref: JsonValue,
    /// 已由路由层 TrimSpace；空串按 NULL 处理（上游 `pgtype.Text` 语义）。
    pub label: Option<String>,
    pub position: i32,
    pub created_by: Option<Uuid>,
}

/// 一条 resource 的 `resource_count` 聚合行。
#[derive(Debug, Clone, FromRow)]
pub struct ProjectResourceCount {
    pub project_id: Uuid,
    pub resource_count: i64,
}

/// `project_resource` 仓储。
#[derive(Clone)]
pub struct ProjectResourceRepo {
    db: Db,
}

impl ProjectResourceRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// `ListProjectResources`（`position ASC, created_at ASC`）。
    pub async fn list(&self, project_id: Uuid) -> Result<Vec<ProjectResourceRow>> {
        let sql = format!(
            "SELECT {PROJECT_RESOURCE_COLUMNS} FROM project_resource \
             WHERE project_id = $1 ORDER BY position ASC, created_at ASC"
        );
        sqlx::query_as::<_, ProjectResourceRow>(&sql)
            .bind(project_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// `ListProjectResourcesInWorkspace`（daemon claim 路径用；租户守卫在 SQL 里）。
    pub async fn list_in_workspace(
        &self,
        project_id: Uuid,
        workspace_id: Id,
    ) -> Result<Vec<ProjectResourceRow>> {
        let sql = format!(
            "SELECT {PROJECT_RESOURCE_COLUMNS} FROM project_resource \
             WHERE project_id = $1 AND workspace_id = $2 ORDER BY position ASC, created_at ASC"
        );
        sqlx::query_as::<_, ProjectResourceRow>(&sql)
            .bind(project_id)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// `ListProjectResourcesForProjects`（claim 响应里批量取，省 N 次往返）。
    pub async fn list_for_projects(&self, project_ids: &[Uuid]) -> Result<Vec<ProjectResourceRow>> {
        if project_ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {PROJECT_RESOURCE_COLUMNS} FROM project_resource \
             WHERE project_id = ANY($1::uuid[]) ORDER BY project_id, position ASC, created_at ASC"
        );
        sqlx::query_as::<_, ProjectResourceRow>(&sql)
            .bind(project_ids.to_vec())
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// `GetProjectResourceInWorkspace`。
    pub async fn get_in_workspace(
        &self,
        id: Uuid,
        workspace_id: Id,
    ) -> Result<Option<ProjectResourceRow>> {
        let sql = format!(
            "SELECT {PROJECT_RESOURCE_COLUMNS} FROM project_resource \
             WHERE id = $1 AND workspace_id = $2"
        );
        sqlx::query_as::<_, ProjectResourceRow>(&sql)
            .bind(id)
            .bind(workspace_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// `GetProjectResource`（无租户过滤，仅内部/测试用）。
    pub async fn get(&self, id: Uuid) -> Result<Option<ProjectResourceRow>> {
        let sql = format!("SELECT {PROJECT_RESOURCE_COLUMNS} FROM project_resource WHERE id = $1");
        sqlx::query_as::<_, ProjectResourceRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// `CreateProjectResource`。
    ///
    /// SQL 与 [`crate::project::ProjectRepo::create_with_resources`] 共用
    /// （`project::insert_resource`），避免两份 INSERT 漂移。
    pub async fn create(
        &self,
        new: &NewProjectResource,
    ) -> std::result::Result<ProjectResourceRow, WriteError> {
        let mut conn = self.db.pool().acquire().await.map_err(map_write_err)?;
        crate::project::insert_resource(&mut conn, new).await
    }

    /// `UpdateProjectResource`（`resource_ref` / `label` / `position` 全量覆盖，
    /// `resource_type` 不可变）。
    ///
    /// 上游 SQL 只有 `WHERE id = $1`；本仓加租户守卫（见模块文档）。
    pub async fn update(
        &self,
        id: Uuid,
        workspace_id: Id,
        resource_ref: &JsonValue,
        label: Option<&str>,
        position: i32,
    ) -> std::result::Result<ProjectResourceRow, WriteError> {
        let sql = format!(
            "UPDATE project_resource SET resource_ref = $2::jsonb, label = $3, position = $4 \
             WHERE id = $1 AND workspace_id = $5 \
             RETURNING {PROJECT_RESOURCE_COLUMNS}"
        );
        sqlx::query_as::<_, ProjectResourceRow>(&sql)
            .bind(id)
            .bind(resource_ref)
            .bind(label)
            .bind(position)
            .bind(workspace_id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_write_err)
    }

    /// `DeleteProjectResource`（+ 租户守卫）。
    pub async fn delete(&self, id: Uuid, workspace_id: Id) -> Result<u64> {
        let affected =
            sqlx::query("DELETE FROM project_resource WHERE id = $1 AND workspace_id = $2")
                .bind(id)
                .bind(workspace_id.0)
                .execute(self.db.pool())
                .await
                .map_err(map_sqlx_err)?
                .rows_affected();
        Ok(affected)
    }

    /// `CountProjectResources`（新建时 `position` 缺省 = 追加到末尾）。
    pub async fn count(&self, project_id: Uuid) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM project_resource WHERE project_id = $1",
        )
        .bind(project_id)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(count)
    }

    /// `GetProjectResourceCounts`（列表响应里的 `resource_count`）。
    pub async fn resource_counts(&self, project_ids: &[Uuid]) -> Result<HashMap<Uuid, i64>> {
        if project_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<ProjectResourceCount> = sqlx::query_as(
            "SELECT project_id, count(*)::bigint AS resource_count FROM project_resource \
             WHERE project_id = ANY($1::uuid[]) GROUP BY project_id",
        )
        .bind(project_ids.to_vec())
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|r| (r.project_id, r.resource_count))
            .collect())
    }
}

impl RepoWithDb for ProjectResourceRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// 租户守卫命中时的 404 资源名（路由层据此回中文/英文消息）。
pub const RESOURCE_NOT_FOUND: &str = "project resource";

// ---------------------------------------------------------------------------
// PG 集成测试
// ---------------------------------------------------------------------------
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::RepoError;

    async fn setup() -> Option<(Db, Id)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m4-1-res', $1) RETURNING id",
        )
        .bind(format!("itest-m4-1-r-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        Some((db, Id::from(workspace_id)))
    }

    macro_rules! fixture {
        () => {
            match setup().await {
                Some(v) => v,
                None => {
                    eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                    return;
                }
            }
        };
    }

    async fn seed_project(db: &Db, ws: Id, title: &str) -> Uuid {
        sqlx::query_scalar("INSERT INTO project(workspace_id, title) VALUES ($1, $2) RETURNING id")
            .bind(ws.0)
            .bind(title)
            .fetch_one(db.pool())
            .await
            .expect("seed project")
    }

    async fn teardown(db: &Db, ws: Id) {
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(ws.0)
            .execute(db.pool())
            .await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    #[allow(clippy::too_many_lines)] // 端到端：建/列/计数/改/删按上游语义平铺，拆开反而难看
    async fn db_create_list_count_update_delete() {
        let (db, ws) = fixture!();
        let repo = ProjectResourceRepo::new(db.clone());
        let project_id = seed_project(&db, ws, "res lifecycle").await;

        let first = repo
            .create(&NewProjectResource {
                project_id,
                workspace_id: ws.0,
                resource_type: "github_repo".into(),
                resource_ref: serde_json::json!({
                    "url": "https://github.com/louloulin/paperclip-rs",
                    "default_branch_hint": "main"
                }),
                label: Some("repo".into()),
                position: 0,
                created_by: None,
            })
            .await
            .expect("create");
        assert_eq!(first.position, 0);
        assert_eq!(first.resource_type, "github_repo");
        assert_eq!(repo.count(project_id).await.expect("count"), 1);

        // 冲突：完全相同的 ref → UNIQUE(project_id, resource_type, resource_ref) → 409。
        let dup = repo
            .create(&NewProjectResource {
                project_id,
                workspace_id: ws.0,
                resource_type: "github_repo".into(),
                resource_ref: first.resource_ref.clone(),
                label: None,
                position: 1,
                created_by: None,
            })
            .await;
        assert!(matches!(dup, Err(WriteError::UniqueViolation)));

        // 不同 ref → 允许，且 count 参与 position 缺省。
        let second = repo
            .create(&NewProjectResource {
                project_id,
                workspace_id: ws.0,
                resource_type: "local_directory".into(),
                resource_ref: serde_json::json!({
                    "local_path": "/home/dev/repo",
                    "daemon_id": "daemon-1",
                    "execution_mode": "worktree"
                }),
                label: None,
                position: 1,
                created_by: None,
            })
            .await
            .expect("create second");
        assert!(second.is_local_directory());
        assert!(second.label.is_none());

        let listed = repo.list(project_id).await.expect("list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, first.id, "position ASC");
        assert_eq!(
            repo.resource_counts(&[project_id])
                .await
                .expect("counts")
                .get(&project_id),
            Some(&2)
        );
        assert_eq!(
            repo.list_in_workspace(project_id, ws)
                .await
                .expect("ws list")
                .len(),
            2
        );
        assert!(repo
            .list_in_workspace(project_id, Id(Uuid::new_v4()))
            .await
            .expect("other ws")
            .is_empty());

        // update：label 显式清空 → NULL；ref 覆盖。
        let updated = repo
            .update(
                second.id,
                ws,
                &serde_json::json!({"local_path": "/home/dev/other", "daemon_id": "daemon-1"}),
                None,
                5,
            )
            .await
            .expect("update");
        assert_eq!(updated.position, 5);
        assert!(updated.label.is_none());
        assert_eq!(updated.resource_ref["local_path"], "/home/dev/other");

        // 租户守卫：换一个 workspace 的 update/delete 都打不到行。
        let other = Id(Uuid::new_v4());
        assert!(matches!(
            repo.update(second.id, other, &serde_json::json!({}), None, 0)
                .await,
            Err(WriteError::Repo(RepoError::NotFound))
        ));
        assert_eq!(
            repo.delete(second.id, other).await.expect("delete other"),
            0
        );

        assert_eq!(repo.delete(first.id, ws).await.expect("delete"), 1);
        assert_eq!(repo.delete(second.id, ws).await.expect("delete second"), 1);
        assert!(repo.list(project_id).await.expect("list after").is_empty());

        teardown(&db, ws).await;
        db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_project_delete_cascades_resources() {
        let (db, ws) = fixture!();
        let repo = ProjectResourceRepo::new(db.clone());
        let project_id = seed_project(&db, ws, "res cascade").await;
        repo.create(&NewProjectResource {
            project_id,
            workspace_id: ws.0,
            resource_type: "github_repo".into(),
            resource_ref: serde_json::json!({"url": "git@github.com:louloulin/multica.git"}),
            label: None,
            position: 0,
            created_by: None,
        })
        .await
        .expect("create");

        // FK 是 ON DELETE CASCADE ⇒ 删 project 自动清 resource。
        sqlx::query("DELETE FROM project WHERE id = $1")
            .bind(project_id)
            .execute(db.pool())
            .await
            .expect("delete project");
        assert!(repo.list(project_id).await.expect("list").is_empty());

        teardown(&db, ws).await;
        db.close().await;
    }
}

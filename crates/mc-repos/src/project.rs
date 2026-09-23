//! project 仓储（M4-1 / LUM-1472）：`/api/projects*` 的集合 / 单体读写 + 搜索。
//!
//! 归属：M4-1（`docs/42-M4-PLAN.md` §4.2 写集矩阵）。覆盖上游 `router.go` #26–#31
//! （`SearchProjects` / `ListProjects` / `CreateProject` / `GetProject` / `UpdateProject` /
//! `DeleteProject`）。
//!
//! 上游真值：`server/pkg/db/queries/project.sql`（64 行 / 9 条 query）、
//! `server/internal/handler/project.go`（962 行）。
//!
//! 表 `project` 已在 `migrations/upstream/034_projects.up.sql`（+ `035` 加 `priority`、
//! `166` 加 `start_date`/`due_date`）⇒ **本片不写迁移**。
//!
//! 约定与 M1/M2/M3 各 Repo 一致（见 `crate::issue` / `crate::agent`）：
//! - 行结构用裸 `Uuid`/`String`/`NaiveDate` 字段 + `Id` 领域访问器，`#[derive(sqlx::FromRow)]`
//!   —— `mc_core::Id` 没有 sqlx impl，所以字段不能直接写 `Id`
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，见 `docs/15` §8.1 的 ⑥ 门）
//!
//! # 与上游的有意偏离（逐条记账在 PR / issue 评论里）
//!
//! - `UpdateProject` 上游只有 `WHERE id = $1`（handler 已先用 `GetProjectInWorkspace`
//!   校验租户）；本仓额外加 `AND workspace_id = $N` 作纵深防御，语义等价。
//! - 唯一 / CHECK 违反在本仓由 [`WriteError`] 显式区分（上游 `isUniqueViolation` /
//!   `isCheckViolation`），路由层据此回 409 / 400，而不是把 CHECK 违反漏成 500。
//! - 搜索的 `statement_timeout` / `work_mem` 用事务级 `SET LOCAL` 复刻
//!   （`handler/search.go:111` `runSearchQuery`），超时（SQLSTATE 57014）以
//!   [`ProjectSearchError::Timeout`] 上抛，路由层回 503。
//!
//! # 契约证据缺口（`docs/42` §6.1）
//!
//! `contracts/golden/projects/` 的 3 条 fixture **不是** project 契约测试（只把
//! `POST /api/projects` 当装置），⑨ 里全 `unevaluable` ⇒ 本模块没有 ⑨ 兜底，
//! 以 `router.go` / `project.sql` / handler 源码为真值。

use chrono::{DateTime, NaiveDate, Utc};
use mc_core::Id;
use sqlx::FromRow;
use uuid::Uuid;

use mc_db::Db;

use crate::project_resource::{NewProjectResource, ProjectResourceRow, PROJECT_RESOURCE_COLUMNS};
use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

mod search;

/// 搜索用到的纯函数：`escape_like` / `split_search_terms` / `extract_snippet`
/// （实现在 `project::search`，此处 re-export 保持 `mc_repos::project::…` 路径稳定）。
pub use search::{escape_like, extract_snippet, split_search_terms};

/// `project` 表响应相关列（所有 `SELECT` 共用，避免列顺序漂移）。
///
/// 与上游 `projectToResponse` 读的列一一对应；`project` 表共 13 列（`034`+`035`+`166`）。
pub const PROJECT_COLUMNS: &str = "id, workspace_id, title, description, icon, status, priority, \
     lead_type, lead_id, start_date, due_date, created_at, updated_at";

/// 搜索默认页大小（上游 `SearchProjects` 的 `limit=20`）。
pub const SEARCH_DEFAULT_LIMIT: i64 = 20;
/// 搜索页大小上限（上游 `limit > 50 ⇒ 50`）。
pub const SEARCH_MAX_LIMIT: i64 = 50;

/// 搜索事务的 `statement_timeout`（毫秒；上游 `searchStatementTimeout = 8 * time.Second`）。
pub const SEARCH_STATEMENT_TIMEOUT_MS: u64 = 8_000;

/// 搜索事务的 `work_mem` 默认值（MB；上游 `defaultSearchWorkMemMB`）。
const SEARCH_DEFAULT_WORK_MEM_MB: u32 = 64;
/// 搜索 `work_mem` 的环境变量（与上游同名；`0` = 不覆盖库默认）。
const SEARCH_WORK_MEM_ENV: &str = "DATABASE_SEARCH_WORK_MEM_MB";

/// 删除 project 前先锁行（与 `chat_session` 创建互斥）。上游 `LockProjectForDelete`。
///
/// 公开给 M4-3 的 chat 切片复用（`CreateChatSession` 侧用 KEY SHARE 形态），
/// 本文件只提供 SQL 常量，事务由调用方自己持有。
pub const LOCK_PROJECT_FOR_DELETE_SQL: &str =
    "SELECT id FROM project WHERE id = $1 AND workspace_id = $2 FOR UPDATE";

/// `LockProjectForChatSessionCreate`（M4-3 `chat_session` 创建路径用）。
pub const LOCK_PROJECT_FOR_CHAT_SESSION_CREATE_SQL: &str =
    "SELECT id FROM project WHERE id = $1 AND workspace_id = $2 FOR KEY SHARE";

/// `ClearChatSessionProjectByProject`：project 引用是软引用（无 FK），删除 project 时
/// 只清上下文选择，**不碰 `updated_at`**（上游注释：上下文清理不是聊天活动）。
pub const CLEAR_CHAT_SESSION_PROJECT_SQL: &str =
    "UPDATE chat_session SET project_id = NULL WHERE project_id = $1 AND workspace_id = $2";

/// `DeleteIssueViewsByProjectScope`：project 面保存的 view 随 project 一起消失，
/// 同一条语句里连带清掉它们的侧栏 pin（上游 `issue_view.sql:66`）。
///
/// `pin` 用 `item_type = 'view'` + `workspace_id` 双重限定，与上游逐字一致。
pub const DELETE_ISSUE_VIEWS_BY_PROJECT_SCOPE_SQL: &str = "WITH deleted AS ( \
         DELETE FROM issue_view \
         WHERE issue_view.workspace_id = $1 AND issue_view.scope_type = 'project' \
           AND issue_view.scope_id = $2 \
         RETURNING issue_view.id \
     ) \
     DELETE FROM pinned_item \
     WHERE pinned_item.item_type = 'view' \
       AND pinned_item.workspace_id = $1 \
       AND pinned_item.item_id IN (SELECT deleted.id FROM deleted)";

// ---------------------------------------------------------------------------
// 行 / 入参结构
// ---------------------------------------------------------------------------

/// `project` 表的响应相关行。
#[derive(Debug, Clone, FromRow)]
pub struct ProjectRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub status: String,
    pub priority: String,
    pub lead_type: Option<String>,
    pub lead_id: Option<Uuid>,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ProjectRow {
    /// 主键。
    #[must_use]
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 workspace。
    #[must_use]
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }
}

/// `CreateProject` 入参（上游 `db.CreateProjectParams`）。
#[derive(Debug, Clone)]
pub struct NewProject {
    pub workspace_id: Id,
    pub title: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    /// 已由路由层预校验（`validProjectStatuses`）；缺省 `planned`。
    pub status: String,
    /// 已由路由层预校验（`validProjectPriorities`）；缺省 `none`。
    pub priority: String,
    pub lead_type: Option<String>,
    pub lead_id: Option<Uuid>,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
}

/// `UpdateProject` 入参（上游 `db.UpdateProjectParams`）。
///
/// 语义逐字对齐上游 SQL：
/// - `title` / `status` / `priority` 走 `COALESCE`（`None` = 保持原值，上游 `narg`）
/// - 其余字段**直接赋值**（`None` = 写 NULL）——「未提供 = 保持原值」由路由层用
///   `rawFields` 判定后把原值抄进来（见 `handler/project.go` `UpdateProject`），
///   仓储层不重复这份判定
#[derive(Debug, Clone)]
pub struct ProjectUpdate {
    pub id: Uuid,
    /// 纵深防御用的租户守卫（上游 SQL 没有这个条件）。
    pub workspace_id: Id,
    pub title: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub lead_type: Option<String>,
    pub lead_id: Option<Uuid>,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
}

/// `GetProjectIssueStats` 的一行（`project_id` → 总 issue 数 / 终态 issue 数）。
#[derive(Debug, Clone, FromRow)]
pub struct ProjectIssueStats {
    pub project_id: Uuid,
    /// 总 issue 数。
    pub total_count: i64,
    /// 终态 issue 数（`status = ANY(terminal_status_keys)`）。
    pub done_count: i64,
}

/// 一条 project 搜索结果：project 行 + SQL 侧算出的 `match_source`。
#[derive(Debug, Clone)]
pub struct ProjectSearchHit {
    pub project: ProjectRow,
    /// `title` 或 `description`（上游 `matchSourceExpr`）。
    pub match_source: String,
}

/// project 写入（INSERT / UPDATE）的失败原因。
///
/// 上游 `writeProjectWriteError`：CHECK 违反是客户端错误（400），其余是 500；
/// 唯一违反（资源面的 `UNIQUE (project_id, resource_type, resource_ref)`）是 409。
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// 约束违反（SQLSTATE `23514`）→ 400。
    #[error("a field value failed a database constraint")]
    CheckViolation,
    /// 唯一违反（SQLSTATE `23505`）→ 409。
    #[error("already exists")]
    UniqueViolation,
    /// 捆绑创建（`POST /api/projects` 带 `resources[]`）时第 `index` 个资源撞唯一约束 → 409。
    ///
    /// 单独一个变体是为了让路由层还原上游的逐条报错文案
    /// （`resources[i]: this resource is already attached`）——那时事务里已经有先落的
    /// 资源行，只有仓储知道是哪一条撞了。
    #[error("resource at index {index} is already attached")]
    ResourceConflict { index: usize },
    /// 其余仓储错误（`NotFound` → 404、`Db` → 500）。
    #[error("{0}")]
    Repo(#[from] RepoError),
}

/// project 搜索的失败原因。
#[derive(Debug, thiserror::Error)]
pub enum ProjectSearchError {
    /// 事务级 `statement_timeout` 触发（SQLSTATE `57014`）→ 503。
    #[error("search timed out")]
    Timeout,
    /// 其余错误 → 500。
    #[error("{0}")]
    Repo(#[from] RepoError),
}

/// SQLSTATE 判定（唯一 / CHECK 违反、超时都只看这一个码）。
fn is_sqlstate(err: &sqlx::Error, code: &str) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some(code))
}

/// 把 sqlx 错误分类成 [`WriteError`]（唯一 / CHECK 违反在上游是 409 / 400）。
pub(crate) fn map_write_err(err: sqlx::Error) -> WriteError {
    if is_sqlstate(&err, "23514") {
        WriteError::CheckViolation
    } else if is_sqlstate(&err, "23505") {
        WriteError::UniqueViolation
    } else {
        WriteError::Repo(map_sqlx_err(err))
    }
}

// ---------------------------------------------------------------------------
// SQL 执行体（单条 / 事务两条路径共用）
// ---------------------------------------------------------------------------

/// `CreateProject` 的 INSERT，`create` 与 `create_with_resources` 共用一份 SQL。
async fn insert_project(
    conn: &mut sqlx::PgConnection,
    new: &NewProject,
) -> std::result::Result<ProjectRow, WriteError> {
    let sql = format!(
        "INSERT INTO project (workspace_id, title, description, icon, status, \
             lead_type, lead_id, priority, start_date, due_date) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         RETURNING {PROJECT_COLUMNS}"
    );
    sqlx::query_as::<_, ProjectRow>(&sql)
        .bind(new.workspace_id.0)
        .bind(&new.title)
        .bind(&new.description)
        .bind(&new.icon)
        .bind(&new.status)
        .bind(&new.lead_type)
        .bind(new.lead_id)
        .bind(&new.priority)
        .bind(new.start_date)
        .bind(new.due_date)
        .fetch_one(conn)
        .await
        .map_err(map_write_err)
}

/// `CreateProjectResource` 的 INSERT（与 [`ProjectResourceRepo::create`] 同一份 SQL）。
pub(crate) async fn insert_resource(
    conn: &mut sqlx::PgConnection,
    new: &NewProjectResource,
) -> std::result::Result<ProjectResourceRow, WriteError> {
    let sql = format!(
        "INSERT INTO project_resource (project_id, workspace_id, resource_type, \
             resource_ref, label, position, created_by) \
         VALUES ($1, $2, $3, $4::jsonb, $5, $6, $7) \
         RETURNING {PROJECT_RESOURCE_COLUMNS}"
    );
    sqlx::query_as::<_, ProjectResourceRow>(&sql)
        .bind(new.project_id)
        .bind(new.workspace_id)
        .bind(&new.resource_type)
        .bind(&new.resource_ref)
        .bind(new.label.as_deref())
        .bind(new.position)
        .bind(new.created_by)
        .fetch_one(conn)
        .await
        .map_err(map_write_err)
}

// ---------------------------------------------------------------------------
// Repo
// ---------------------------------------------------------------------------

/// project 仓储。
#[derive(Clone)]
pub struct ProjectRepo {
    db: Db,
}

impl ProjectRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// `ListProjects`：按 workspace 列出，可按 `status` / `priority` 过滤，`created_at DESC`。
    pub async fn list(
        &self,
        workspace_id: Id,
        status: Option<&str>,
        priority: Option<&str>,
    ) -> Result<Vec<ProjectRow>> {
        let sql = format!(
            "SELECT {PROJECT_COLUMNS} FROM project \
             WHERE workspace_id = $1 \
               AND ($2::text IS NULL OR status = $2::text) \
               AND ($3::text IS NULL OR priority = $3::text) \
             ORDER BY created_at DESC"
        );
        sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(workspace_id.0)
            .bind(status.map(ToString::to_string))
            .bind(priority.map(ToString::to_string))
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// `GetProjectInWorkspace`。
    pub async fn get_in_workspace(&self, id: Uuid, workspace_id: Id) -> Result<Option<ProjectRow>> {
        let sql =
            format!("SELECT {PROJECT_COLUMNS} FROM project WHERE id = $1 AND workspace_id = $2");
        sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(id)
            .bind(workspace_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// `CreateProject`（返回新行，供响应直接复用）。
    pub async fn create(&self, new: &NewProject) -> std::result::Result<ProjectRow, WriteError> {
        let mut conn = self.db.pool().acquire().await.map_err(map_write_err)?;
        insert_project(&mut conn, new).await
    }

    /// `CreateProject` 的事务分支（上游 `CreateProject` 带 `resources[]` 时走的那条路）：
    /// project 行与全部 resource 行**原子**写入，任一条失败整体回滚。
    ///
    /// `resources` 的 `project_id` / `workspace_id` 由新落地的 project 行回填
    /// （上游同样用 `project.ID` / `project.WorkspaceID` 覆写入参）。
    /// 第 `index` 条撞唯一约束 → [`WriteError::ResourceConflict`]。
    pub async fn create_with_resources(
        &self,
        new: &NewProject,
        resources: &[NewProjectResource],
    ) -> std::result::Result<(ProjectRow, Vec<ProjectResourceRow>), WriteError> {
        let mut tx = self.db.pool().begin().await.map_err(map_write_err)?;
        let project = insert_project(&mut tx, new).await?;
        let mut rows = Vec::with_capacity(resources.len());
        for (index, resource) in resources.iter().enumerate() {
            let mut bound = resource.clone();
            bound.project_id = project.id;
            bound.workspace_id = project.workspace_id;
            match insert_resource(&mut tx, &bound).await {
                Ok(row) => rows.push(row),
                Err(WriteError::UniqueViolation) => {
                    // tx 随 drop 回滚。
                    return Err(WriteError::ResourceConflict { index });
                }
                Err(err) => return Err(err),
            }
        }
        tx.commit().await.map_err(map_write_err)?;
        Ok((project, rows))
    }

    /// `UpdateProject`（`COALESCE` + 直接赋值语义见 [`ProjectUpdate`]）。
    ///
    /// 上游 SQL 只有 `WHERE id = $1`；本仓加 `workspace_id` 守卫作纵深防御。
    pub async fn update(
        &self,
        patch: &ProjectUpdate,
    ) -> std::result::Result<ProjectRow, WriteError> {
        let sql = format!(
            "UPDATE project SET \
                 title = COALESCE($2::text, title), \
                 description = $3::text, \
                 icon = $4::text, \
                 status = COALESCE($5::text, status), \
                 priority = COALESCE($6::text, priority), \
                 lead_type = $7::text, \
                 lead_id = $8::uuid, \
                 start_date = $9::date, \
                 due_date = $10::date, \
                 updated_at = now() \
             WHERE id = $1 AND workspace_id = $11 \
             RETURNING {PROJECT_COLUMNS}"
        );
        sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(patch.id)
            .bind(&patch.title)
            .bind(&patch.description)
            .bind(&patch.icon)
            .bind(&patch.status)
            .bind(&patch.priority)
            .bind(&patch.lead_type)
            .bind(patch.lead_id)
            .bind(patch.start_date)
            .bind(patch.due_date)
            .bind(patch.workspace_id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_write_err)
    }

    /// `DeleteProject` + 同事务的两处清理（上游 `DeleteProject` handler 的应用事务）。
    ///
    /// 顺序逐条对齐上游：`FOR UPDATE` 锁行 → 清 chat session 软引用 → 删 project 面
    /// view（连带 pin）→ 删 project。行已被并发删除时返回 [`RepoError::NotFound`]
    /// （上游 `ErrNoRows` → 404 `project not found`）。
    ///
    /// 返回 `RowsAffected`（正常情况下恒为 1）。
    pub async fn delete_cascade(&self, id: Uuid, workspace_id: Id) -> Result<u64> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;

        let locked: Option<(Uuid,)> = sqlx::query_as(LOCK_PROJECT_FOR_DELETE_SQL)
            .bind(id)
            .bind(workspace_id.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        if locked.is_none() {
            // tx 随 drop 回滚。
            return Err(RepoError::NotFound);
        }

        sqlx::query(CLEAR_CHAT_SESSION_PROJECT_SQL)
            .bind(id)
            .bind(workspace_id.0)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;

        sqlx::query(DELETE_ISSUE_VIEWS_BY_PROJECT_SCOPE_SQL)
            .bind(workspace_id.0)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;

        let deleted = sqlx::query("DELETE FROM project WHERE id = $1 AND workspace_id = $2")
            .bind(id)
            .bind(workspace_id.0)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?
            .rows_affected();

        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(deleted)
    }

    /// `GetProjectIssueStats`：批量拿「总 issue 数 / 终态 issue 数」。
    ///
    /// 空 `project_ids` 直接返回空表（不发出 `= ANY('{}')` 查询）。
    pub async fn issue_stats(
        &self,
        workspace_id: Id,
        project_ids: &[Uuid],
        terminal_status_keys: &[String],
    ) -> Result<Vec<ProjectIssueStats>> {
        if project_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<ProjectIssueStats> = sqlx::query_as(
            "SELECT project_id, count(*)::bigint AS total_count, \
                    count(*) FILTER (WHERE status = ANY($3::text[]))::bigint AS done_count \
             FROM issue \
             WHERE workspace_id = $1 AND project_id = ANY($2::uuid[]) \
             GROUP BY project_id",
        )
        .bind(workspace_id.0)
        .bind(project_ids.to_vec())
        .bind(terminal_status_keys.to_vec())
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows)
    }

    /// `SearchProjects`：短语 / 多词动态排名搜索（实现在 `project::search`）。
    ///
    /// 上游 `runSearchQuery`：短命只读事务 + 事务级 `SET LOCAL statement_timeout` /
    /// `work_mem`，超时（57014）→ [`ProjectSearchError::Timeout`]，路由层回 503。
    pub async fn search(
        &self,
        workspace_id: Id,
        query: &str,
        limit: i64,
        offset: i64,
        include_closed: bool,
    ) -> std::result::Result<Vec<ProjectSearchHit>, ProjectSearchError> {
        search::run_search(&self.db, workspace_id, query, limit, offset, include_closed).await
    }
}

impl RepoWithDb for ProjectRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod tests;

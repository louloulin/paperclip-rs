//! `InboxRepo` — DB-backed inbox 仓储（表 `inbox_item`）。
//!
//! 对应 upstream `multica/server/pkg/db/queries/{inbox,inbox_archive}.sql` 与
//! `server/internal/handler/{inbox,inbox_archive}.go`。
//!
//! ## 与上游的字段偏离（本仓 `0001_init.up.sql` 的 `inbox_item` 词汇表）
//!
//! | 上游列 | 本仓列 | 说明 |
//! | --- | --- | --- |
//! | `recipient_type` + `recipient_id` | `user_id UUID` | 本仓的 inbox 只投递给人类 user（agent 收件箱属 M3+），故收件人是单一列 |
//! | `read BOOLEAN` | `read_at TIMESTAMPTZ` | 语义等价（`read_at IS NOT NULL` ⟺ 已读），且多保留"何时读的" |
//! | `archived BOOLEAN` | `archived_at TIMESTAMPTZ` | 同上 |
//! | `type` | `category TEXT` | 本仓沿用 `category`（无 CHECK，取值域由写入方约定） |
//! | `severity` / `details` | **不存在** | 故 archived 视图没有 comment anchor 行（上游靠 `details->>'comment_id'` 二次补行）；本仓一组只返回最新一行 |
//! | `actor_type/actor_id` | 同名 | 本仓 `actor_id` 是 `TEXT`（上游 `UUID`） |
//!
//! 所有"幂等"语义（`mark_read` / `mark_unread` / `archive` / `unarchive`）靠
//! `COALESCE` + `IS NULL` 条件实现：重复调用第二次是空写，返回值不变。
//!
//! ## 分组（group）语义
//!
//! 上游 inbox 是 **Linear 式按 issue 分组**：同一个 issue 的多条通知在 UI 上只渲染
//! 最新一条，且已读/归档都作用在**整组**。本仓照搬该分组键：
//! `COALESCE(i.issue_id, i.id)`（无 issue 的通知自成一组）。因此：
//! - `unread_count` 数的是**原始未读行**（与上游 `CountUnreadInbox` 一致）；
//! - `unread_summary` 与 `archive_all_read` 数的是**组**（与上游
//!   `CountUnreadInboxByWorkspace` / `ArchiveAllReadInbox` 一致）；
//! - `archive` / `unarchive` 是 issue 级（同组一起动），`mark_read` / `mark_unread`
//!   是 item 级（上游注释里明确解释了为什么这两者粒度不同）。

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

/// item 列表/单条的公共列清单 + issue 投影。
///
/// 所有查询共用同一份列清单（`ITEM_COLUMNS`），任一查询的列漂移都会在运行时
/// 立刻暴露（`try_get` 失败），与上游"两个查询列不一致就编译不过"的意图一致。
const ITEM_COLUMNS: &str = "i.id, i.workspace_id, i.user_id, i.issue_id, i.actor_type, \
     i.actor_id, i.category, i.title, i.body, i.read_at, i.archived_at, i.created_at, \
     iss.status AS issue_status, iss.priority AS issue_priority";

/// `inbox_item` 连接 `issue` 的 FROM 子句（issue 投影必需）。
const ITEM_FROM: &str = "FROM inbox_item i \
     LEFT JOIN issue iss ON iss.id = i.issue_id AND iss.workspace_id = i.workspace_id";

/// 归档视图的"分组代表行"CTE：每个 issue 组只取最新一条，且排除了本组仍有
/// 活跃（未归档）行的 issue。
///
/// 排除逻辑与上游 `ListArchivedInboxItems` 完全一致：归档是 issue 级的，某 issue
/// 归档后又来了新通知，旧归档行会留在原处、新活跃行进入主列表——若不排除，同一个
/// issue 会同时出现在两个列表里。
const NEWEST_ARCHIVED_CTE: &str = "WITH newest AS MATERIALIZED ( \
        SELECT DISTINCT ON (COALESCE(i.issue_id, i.id)) \
               i.id, i.issue_id, i.created_at, (i.read_at IS NOT NULL) AS is_read, \
               CASE WHEN i.actor_type = 'system' THEN 'system' \
                    ELSE i.actor_type || ':' || i.actor_id END AS actor \
        FROM inbox_item i \
        WHERE i.workspace_id = $1 AND i.user_id = $2 AND i.archived_at IS NOT NULL \
          AND (i.issue_id IS NULL OR NOT EXISTS ( \
              SELECT 1 FROM inbox_item active \
              WHERE active.workspace_id = i.workspace_id \
                AND active.user_id = i.user_id \
                AND active.issue_id = i.issue_id \
                AND active.archived_at IS NULL)) \
        ORDER BY COALESCE(i.issue_id, i.id), i.created_at DESC, i.id DESC \
     ), projected AS ( \
        SELECT newest.*, iss.status AS issue_status, iss.priority AS issue_priority \
        FROM newest \
        LEFT JOIN issue iss ON iss.id = newest.issue_id AND iss.workspace_id = $1 \
     ), matched AS ( \
        SELECT projected.*, \
               (cardinality($3::text[]) = 0 OR issue_status = ANY($3::text[])) AS status_match, \
               (cardinality($4::text[]) = 0 OR issue_priority = ANY($4::text[])) AS priority_match, \
               (cardinality($5::text[]) = 0 OR actor = ANY($5::text[])) AS actor_match, \
               (NOT $6::boolean OR NOT is_read) AS read_match \
        FROM projected \
     )";

/// 一个 inbox item（含关联 issue 的 status / priority 投影）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InboxItemRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub issue_id: Option<Uuid>,
    pub actor_type: String,
    pub actor_id: String,
    /// 上游的 `type`（本仓列名 `category`）。
    pub category: String,
    pub title: String,
    pub body: Option<String>,
    pub read_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// 关联 issue 的 `status`；无 issue 时 `None`。
    pub issue_status: Option<String>,
    /// 关联 issue 的 `priority`；无 issue 时 `None`。
    pub issue_priority: Option<String>,
}

impl InboxItemRow {
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    pub fn is_read(&self) -> bool {
        self.read_at.is_some()
    }

    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

// `mc_core::Id` 尚未实现 sqlx `Decode`/`Encode`，故本结构用原始 `Uuid`/`String`
// 字段（见模块头与 M1 各 Repo 的同一约定）。
impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for InboxItemRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        Ok(Self {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
            user_id: row.try_get("user_id")?,
            issue_id: row.try_get("issue_id")?,
            actor_type: row.try_get("actor_type")?,
            actor_id: row.try_get("actor_id")?,
            category: row.try_get("category")?,
            title: row.try_get("title")?,
            body: row.try_get("body")?,
            read_at: row.try_get("read_at")?,
            archived_at: row.try_get("archived_at")?,
            created_at: row.try_get("created_at")?,
            // 单条的 `RETURNING` 没有 issue 投影列；列表查询有。
            issue_status: opt_text(row, "issue_status")?,
            issue_priority: opt_text(row, "issue_priority")?,
        })
    }
}

/// `try_get` 一个可能不在结果集里的可空文本列。
fn opt_text(row: &sqlx::postgres::PgRow, name: &str) -> sqlx::Result<Option<String>> {
    match row.try_get::<Option<String>, _>(name) {
        Ok(v) => Ok(v),
        // 列不存在（`RETURNING` 子句）→ None；其他错误（类型不符）原样抛出。
        Err(sqlx::Error::ColumnNotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// 新建 inbox item 的入参（item 的**产生**逻辑属 M3，本仓只提供写入面 + 测试夹具）。
#[derive(Debug, Clone)]
pub struct NewInboxItem {
    /// 缺省时由仓储生成 v4 UUID。
    pub id: Option<Id>,
    pub workspace_id: Id,
    pub user_id: Id,
    pub issue_id: Option<Id>,
    pub actor_type: String,
    pub actor_id: String,
    pub category: String,
    pub title: String,
    pub body: Option<String>,
}

/// 归档视图的过滤条件（上游 `ArchivedInboxFacetsParams` 的可过滤部分）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchivedInboxFilter {
    /// 有序、去重后的 issue status 集合（空 = 不过滤）。
    pub statuses: Vec<String>,
    /// 有序、去重后的 issue priority 集合（空 = 不过滤）。
    pub priorities: Vec<String>,
    /// 有序、去重后的 actor 集合，形如 `user:<uuid>` / `system`（空 = 不过滤）。
    pub actors: Vec<String>,
    /// 只看未读组。
    pub unread_only: bool,
    /// 只取某个 issue 组（`None` = 全部）。
    pub group_id: Option<Id>,
}

/// `archived/page` 的游标（`(created_at, id)` 位置，倒序翻页）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedCursor {
    pub created_at: DateTime<Utc>,
    pub id: Id,
}

/// `archived/page` 的一页。
#[derive(Debug, Clone)]
pub struct ArchivedInboxPage {
    pub items: Vec<InboxItemRow>,
    /// 还有下一页（内部多取一行判断，返回时已截断）。
    pub has_more: bool,
}

/// 归档视图的 facet 计数（`archived/facets`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchivedInboxFacets {
    pub statuses: BTreeMap<String, i64>,
    pub priorities: BTreeMap<String, i64>,
    pub actors: BTreeMap<String, i64>,
    pub unread_count: i64,
}

/// 跨 workspace 的未读汇总项（`unread-summary`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceUnread {
    pub workspace_id: Id,
    pub count: i64,
}

/// inbox 仓储。
#[derive(Clone)]
pub struct InboxRepo {
    pool: Arc<PgPool>,
}

impl InboxRepo {
    #[allow(clippy::needless_pass_by_value)] // 入参保留 `Db` 所有权，调用方直接 `state.db.clone()`。
    pub fn new(db: Db) -> Self {
        Self {
            pool: Arc::new(db.pool().clone()),
        }
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool: Arc::new(pool),
        }
    }

    fn pool(&self) -> &PgPool {
        &self.pool
    }

    // -----------------------------------------------------------------------
    // 写入面（生产者 = M3 事件层；本切片用测试夹具驱动）
    // -----------------------------------------------------------------------

    /// 插入一条 inbox item（`read_at` / `archived_at` 均为 NULL）。
    pub async fn create(&self, input: NewInboxItem) -> Result<InboxItemRow> {
        let id = input.id.unwrap_or_default().as_uuid();
        // 先 INSERT 再 `get`：`ITEM_COLUMNS` 里的 issue 投影需要 JOIN，而
        // `INSERT ... RETURNING` **不能**引用 JOIN 进来的表（`RETURNING` 只能引用
        // 被写入的那张表；带别名的 `RETURNING i.col` 也会报 missing FROM-clause）。
        sqlx::query(
            "INSERT INTO inbox_item \
                 (id, workspace_id, user_id, issue_id, actor_type, actor_id, category, title, body, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())",
        )
        .bind(id)
        .bind(input.workspace_id.as_uuid())
        .bind(input.user_id.as_uuid())
        .bind(input.issue_id.map(Id::as_uuid))
        .bind(&input.actor_type)
        .bind(&input.actor_id)
        .bind(&input.category)
        .bind(&input.title)
        .bind(input.body.as_deref())
        .execute(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        self.get(Id::from(id)).await
    }

    // -----------------------------------------------------------------------
    // 读
    // -----------------------------------------------------------------------

    /// 单条 item（不做归属校验；路由层用 `get_for_user`）。
    pub async fn get(&self, id: Id) -> Result<InboxItemRow> {
        let sql = format!("SELECT {ITEM_COLUMNS} {ITEM_FROM} WHERE i.id = $1");
        sqlx::query_as::<_, InboxItemRow>(&sql)
            .bind(id.as_uuid())
            .fetch_optional(self.pool())
            .await
            .map_err(map_sqlx_err)?
            .ok_or(RepoError::NotFound)
    }

    /// 单条 item，且必须属于 `(workspace_id, user_id)`；否则 `NotFound`。
    ///
    /// 上游 `loadInboxItemForUser` 的等价物：把"别人的通知"与"不存在的通知"都收敛
    /// 成 404，不泄漏资源存在性。
    pub async fn get_for_user(&self, id: Id, workspace_id: Id, user_id: Id) -> Result<InboxItemRow> {
        let sql = format!(
            "SELECT {ITEM_COLUMNS} {ITEM_FROM} \
             WHERE i.id = $1 AND i.workspace_id = $2 AND i.user_id = $3"
        );
        sqlx::query_as::<_, InboxItemRow>(&sql)
            .bind(id.as_uuid())
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .fetch_optional(self.pool())
            .await
            .map_err(map_sqlx_err)?
            .ok_or(RepoError::NotFound)
    }

    /// 主列表：该 workspace 下该用户**未归档**的通知，按 `created_at` 倒序分页。
    ///
    /// 上游 `GET /api/inbox` 不分页（返回全部活跃行）；本仓按 sub-issue 要求加
    /// `limit` / `offset`（路由默认 `limit=200`，见 `docs/13-M2-INBOX.md`）。
    pub async fn list(&self, workspace_id: Id, user_id: Id, limit: i64, offset: i64) -> Result<Vec<InboxItemRow>> {
        let sql = format!(
            "SELECT {ITEM_COLUMNS} {ITEM_FROM} \
             WHERE i.workspace_id = $1 AND i.user_id = $2 AND i.archived_at IS NULL \
             ORDER BY i.created_at DESC, i.id DESC \
             LIMIT $3 OFFSET $4"
        );
        sqlx::query_as::<_, InboxItemRow>(&sql)
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .bind(limit)
            .bind(offset)
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 归档列表（不分页）：最多 `group_limit` 个 issue 组，每组只返回最新一行。
    ///
    /// 上游固定 200 组 + 额外补 comment anchor 行；本仓 schema 无 `details` 列，
    /// 故只有每组最新一行。
    pub async fn list_archived(
        &self,
        workspace_id: Id,
        user_id: Id,
        group_limit: i64,
    ) -> Result<Vec<InboxItemRow>> {
        let filter = ArchivedInboxFilter::default();
        let page = self
            .list_archived_page(workspace_id, user_id, &filter, None, group_limit)
            .await?;
        Ok(page.items)
    }

    /// 归档列表分页（`archived/page`）：先按组选出代表行，再套过滤 / 游标 / limit。
    ///
    /// 游标是 `(created_at, id)` 的行比较，`limit` 计的是**组**而不是原始行——
    /// 与上游一致（否则一个噪音 issue 能吃光整页）。
    pub async fn list_archived_page(
        &self,
        workspace_id: Id,
        user_id: Id,
        filter: &ArchivedInboxFilter,
        cursor: Option<&ArchivedCursor>,
        limit: i64,
    ) -> Result<ArchivedInboxPage> {
        let sql = format!(
            "{NEWEST_ARCHIVED_CTE}, selected AS ( \
                SELECT * FROM matched \
                WHERE status_match AND priority_match AND actor_match AND read_match \
                  AND ($9::uuid IS NULL OR issue_id = $9::uuid \
                       OR (issue_id IS NULL AND id = $9::uuid)) \
                  AND ($7::timestamptz IS NULL \
                       OR (created_at, id) < ($7::timestamptz, $8::uuid)) \
                ORDER BY created_at DESC, id DESC \
                LIMIT $10 \
             ) \
             SELECT {ITEM_COLUMNS} \
             FROM selected \
             JOIN inbox_item i ON i.id = selected.id \
             LEFT JOIN issue iss ON iss.id = i.issue_id AND iss.workspace_id = $1 \
             ORDER BY i.created_at DESC, i.id DESC"
        );
        let rows = sqlx::query_as::<_, InboxItemRow>(&sql)
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .bind(&filter.statuses)
            .bind(&filter.priorities)
            .bind(&filter.actors)
            .bind(filter.unread_only)
            .bind(cursor.map(|c| c.created_at))
            .bind(cursor.map(|c| c.id.as_uuid()))
            .bind(filter.group_id.map(Id::as_uuid))
            // 多取一行判断 has_more，返回前截断。
            .bind(limit.saturating_add(1))
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        let has_more = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
        let mut items = rows;
        if has_more {
            items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        }
        Ok(ArchivedInboxPage { items, has_more })
    }

    /// 归档视图的 facet 计数（`archived/facets`）：一次查询返回
    /// `(dimension, key, count)` 三元组，由调用方分派到四个桶。
    ///
    /// 每个维度的计数都按**其他维度**过滤（facet 互斥计数的常规语义），
    /// 与上游 `ArchivedInboxFacets` 逐字对应。
    pub async fn archived_facets(
        &self,
        workspace_id: Id,
        user_id: Id,
        filter: &ArchivedInboxFilter,
    ) -> Result<ArchivedInboxFacets> {
        let sql = format!(
            "{NEWEST_ARCHIVED_CTE} \
             SELECT 'statuses'::text AS dimension, issue_status::text AS key, \
                    count(*) FILTER (WHERE priority_match AND actor_match AND read_match) AS count \
             FROM matched WHERE issue_status IS NOT NULL GROUP BY issue_status \
             UNION ALL \
             SELECT 'priorities'::text, issue_priority::text, \
                    count(*) FILTER (WHERE status_match AND actor_match AND read_match) \
             FROM matched WHERE issue_priority IS NOT NULL GROUP BY issue_priority \
             UNION ALL \
             SELECT 'actors'::text, actor::text, \
                    count(*) FILTER (WHERE status_match AND priority_match AND read_match) \
             FROM matched WHERE actor IS NOT NULL GROUP BY actor \
             UNION ALL \
             SELECT 'unread'::text, 'unread'::text, \
                    count(*) FILTER (WHERE status_match AND priority_match AND actor_match AND NOT is_read) \
             FROM matched"
        );
        let rows = sqlx::query_as::<_, (String, String, i64)>(&sql)
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .bind(&filter.statuses)
            .bind(&filter.priorities)
            .bind(&filter.actors)
            .bind(filter.unread_only)
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        let mut facets = ArchivedInboxFacets::default();
        for (dimension, key, count) in rows {
            match dimension.as_str() {
                "statuses" => {
                    facets.statuses.insert(key, count);
                }
                "priorities" => {
                    facets.priorities.insert(key, count);
                }
                "actors" => {
                    facets.actors.insert(key, count);
                }
                // `unread` 维度的 key 恒为字面量 "unread"。
                _ => {
                    facets.unread_count = count;
                }
            }
        }
        Ok(facets)
    }

    /// 该 workspace 的**原始行**未读数（上游 `CountUnreadInbox`）。
    pub async fn unread_count(&self, workspace_id: Id, user_id: Id) -> Result<i64> {
        let row = sqlx::query_as::<_, (i64,)>(
            "SELECT count(*) FROM inbox_item \
             WHERE workspace_id = $1 AND user_id = $2 \
               AND read_at IS NULL AND archived_at IS NULL",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .fetch_one(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.0)
    }

    /// 跨 workspace 的未读**组**数汇总（账户级；`unread-summary`）。
    ///
    /// - 按 issue 分组，组内最新一条未读则该组计 1（`DISTINCT ON`）；
    /// - `JOIN member` 把范围限定在用户当前仍加入的 workspace（已退出的 workspace
    ///   里残留的通知不能点亮侧边栏小圆点）。
    pub async fn unread_summary(&self, user_id: Id) -> Result<Vec<WorkspaceUnread>> {
        let rows = sqlx::query_as::<_, (Uuid, i64)>(
            "SELECT newest.workspace_id, count(*) \
             FROM ( \
                 SELECT DISTINCT ON (i.workspace_id, COALESCE(i.issue_id, i.id)) \
                        i.workspace_id, i.read_at \
                 FROM inbox_item i \
                 JOIN member m ON m.workspace_id = i.workspace_id AND m.user_id = i.user_id \
                 WHERE i.user_id = $1 AND i.archived_at IS NULL \
                 ORDER BY i.workspace_id, COALESCE(i.issue_id, i.id), i.created_at DESC, i.id DESC \
             ) newest \
             WHERE newest.read_at IS NULL \
             GROUP BY newest.workspace_id \
             ORDER BY newest.workspace_id",
        )
        .bind(user_id.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|(workspace_id, count)| WorkspaceUnread {
                workspace_id: Id::from(workspace_id),
                count,
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // 状态迁移（全部幂等）
    // -----------------------------------------------------------------------

    /// 标记已读（item 级，幂等）：已读行保持不变，`read_at` 不会被刷新。
    pub async fn mark_read(&self, id: Id) -> Result<InboxItemRow> {
        self.retouch(
            "read_at = COALESCE(read_at, now())",
            id.as_uuid(),
        )
        .await
    }

    /// 标记未读（item 级，幂等）。
    ///
    /// 刻意只翻**这一行**：UI 渲染的是组内最新一条，组状态就是这行的状态。
    /// 翻整组会把用户已经处理完的旧兄弟节点变回未读。
    pub async fn mark_unread(&self, id: Id) -> Result<InboxItemRow> {
        self.retouch("read_at = NULL", id.as_uuid()).await
    }

    /// 归档（**issue 级**，幂等）：同 issue 的全部兄弟行一起归档。
    ///
    /// 无 issue 的通知只归档自己。返回目标行归档后的最新状态。
    pub async fn archive(&self, id: Id) -> Result<InboxItemRow> {
        self.archive_scope(id, true).await
    }

    /// 取消归档（issue 级，幂等）。刻意不碰 `read_at`：还原时保持归档前的读写状态。
    pub async fn unarchive(&self, id: Id) -> Result<InboxItemRow> {
        self.archive_scope(id, false).await
    }

    async fn retouch(&self, set_clause: &str, id: Uuid) -> Result<InboxItemRow> {
        // 同 `create`：`RETURNING` 不能带 JOIN 投影，故写完再读回完整行。
        let sql = format!("UPDATE inbox_item SET {set_clause} WHERE id = $1");
        let res = sqlx::query(&sql)
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        if res.rows_affected() == 0 {
            return Err(RepoError::NotFound);
        }
        self.get(Id::from(id)).await
    }

    async fn archive_scope(&self, id: Id, archive: bool) -> Result<InboxItemRow> {
        let item = self.get(id).await?;
        let set_clause = if archive {
            "archived_at = COALESCE(archived_at, now())"
        } else {
            "archived_at = NULL"
        };
        // 幂等条件：归档只动未归档行，取消归档只动已归档行。
        let guard = if archive {
            "archived_at IS NULL"
        } else {
            "archived_at IS NOT NULL"
        };
        if let Some(issue_id) = item.issue_id {
            let sql = format!(
                "UPDATE inbox_item SET {set_clause} \
                 WHERE workspace_id = $1 AND user_id = $2 AND issue_id = $3 AND {guard}"
            );
            sqlx::query(&sql)
                .bind(item.workspace_id)
                .bind(item.user_id)
                .bind(issue_id)
                .execute(self.pool())
                .await
                .map_err(map_sqlx_err)?;
        } else {
            let sql = format!("UPDATE inbox_item SET {set_clause} WHERE id = $1 AND {guard}");
            sqlx::query(&sql)
                .bind(id.as_uuid())
                .execute(self.pool())
                .await
                .map_err(map_sqlx_err)?;
        }
        // 目标行必然已被上一句覆盖（同组 / 自己），重读拿归档后的状态。
        self.get(id).await
    }

    /// 全部标记已读（组语义上等价于"全部行已读"，因为读状态的粒度是行）。
    pub async fn mark_all_read(&self, workspace_id: Id, user_id: Id) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE inbox_item SET read_at = COALESCE(read_at, now()) \
             WHERE workspace_id = $1 AND user_id = $2 \
               AND archived_at IS NULL AND read_at IS NULL",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .execute(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(res.rows_affected())
    }

    /// 归档全部未归档通知（不分读写状态）。
    pub async fn archive_all(&self, workspace_id: Id, user_id: Id) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE inbox_item SET archived_at = COALESCE(archived_at, now()) \
             WHERE workspace_id = $1 AND user_id = $2 AND archived_at IS NULL",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .execute(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(res.rows_affected())
    }

    /// 归档"已读"的组：组内最新一条已读 → 整组归档；未读组一行不动。
    ///
    /// 直接更新"读过的行"是错的：最新行归档后，旧兄弟节点会重新冒出来，
    /// 也可能把未读组里的旧已读行归档掉。
    pub async fn archive_all_read(&self, workspace_id: Id, user_id: Id) -> Result<u64> {
        let res = sqlx::query(
            "WITH newest_groups AS ( \
                 SELECT DISTINCT ON (COALESCE(i.issue_id, i.id)) \
                        COALESCE(i.issue_id, i.id) AS group_id, (i.read_at IS NOT NULL) AS is_read \
                 FROM inbox_item i \
                 WHERE i.workspace_id = $1 AND i.user_id = $2 AND i.archived_at IS NULL \
                 ORDER BY COALESCE(i.issue_id, i.id), i.created_at DESC, i.id DESC \
             ), read_groups AS ( \
                 SELECT group_id FROM newest_groups WHERE is_read \
             ) \
             UPDATE inbox_item i SET archived_at = COALESCE(i.archived_at, now()) \
             FROM read_groups selected \
             WHERE i.workspace_id = $1 AND i.user_id = $2 AND i.archived_at IS NULL \
               AND COALESCE(i.issue_id, i.id) = selected.group_id",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .execute(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(res.rows_affected())
    }

    /// 归档"已完成"issue 的通知：issue 状态属于终结态（`done` / `cancelled`）。
    ///
    /// 终结态取值 = 本 workspace `issue_status.category = 'closed'` 的 key，并集
    /// 内置终结 key（本仓 `issue_status.category` 的 CHECK 只有 `open`/`closed`，
    /// 没有上游的 `done` 分类，故显式并上内置 key，见 `docs/13-M2-INBOX.md`）。
    pub async fn archive_completed(&self, workspace_id: Id, user_id: Id) -> Result<u64> {
        let res = sqlx::query(
            "WITH terminal AS ( \
                 SELECT key FROM issue_status \
                 WHERE workspace_id = $1 AND category = 'closed' \
                 UNION SELECT unnest($3::text[]) \
             ) \
             UPDATE inbox_item i SET archived_at = COALESCE(i.archived_at, now()) \
             WHERE i.workspace_id = $1 AND i.user_id = $2 AND i.archived_at IS NULL \
               AND i.issue_id IN ( \
                   SELECT id FROM issue \
                   WHERE workspace_id = $1 AND status IN (SELECT key FROM terminal))",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .bind(BUILTIN_TERMINAL_STATUS_KEYS.to_vec())
        .execute(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(res.rows_affected())
    }
}

/// 内置终结状态 key（上游 `issuestatus` 的 `done` / `cancelled`）。
pub const BUILTIN_TERMINAL_STATUS_KEYS: &[&str] = &["done", "cancelled"];

// ---------------------------------------------------------------------------
// 单元测试（不依赖 DB）
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_row() -> InboxItemRow {
        InboxItemRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            issue_id: Some(Uuid::new_v4()),
            actor_type: "user".into(),
            actor_id: Uuid::new_v4().to_string(),
            category: "new_comment".into(),
            title: "t".into(),
            body: None,
            read_at: None,
            archived_at: None,
            created_at: Utc.with_ymd_and_hms(2026, 9, 22, 10, 0, 0).unwrap(),
            issue_status: Some("in_progress".into()),
            issue_priority: None,
        }
    }

    #[test]
    fn read_and_archived_are_derived_from_timestamps() {
        let mut row = sample_row();
        assert!(!row.is_read());
        assert!(!row.is_archived());
        row.read_at = Some(Utc::now());
        row.archived_at = Some(Utc::now());
        assert!(row.is_read());
        assert!(row.is_archived());
    }

    #[test]
    fn default_filter_is_empty() {
        let f = ArchivedInboxFilter::default();
        assert!(f.statuses.is_empty() && f.priorities.is_empty() && f.actors.is_empty());
        assert!(!f.unread_only);
        assert!(f.group_id.is_none());
    }

    #[test]
    fn default_status_keys_are_done_and_cancelled() {
        assert_eq!(BUILTIN_TERMINAL_STATUS_KEYS, &["done", "cancelled"]);
    }

    // ---- DB 集成测试（`cargo test -- --ignored` + MULTICA_TEST_DATABASE_URL）----
    //
    // 前置：目标库已跑过 `migrations/`（含 0004 的 `issue_subscriber`）。
    // 每个用例自建 workspace/user/issue，互不干扰。

    mod db {
        use super::*;

        async fn connect() -> Option<InboxRepo> {
            let Ok(url) = std::env::var("MULTICA_TEST_DATABASE_URL") else {
                return None;
            };
            let db = Db::connect(&url, 4, 0).await.expect("db connect");
            Some(InboxRepo::new(db))
        }

        async fn new_user(pool: &PgPool) -> Uuid {
            let user_id = Uuid::new_v4();
            sqlx::query(r#"INSERT INTO "user" (id, name, email) VALUES ($1, 'fixture', $2)"#)
                .bind(user_id)
                .bind(format!("{user_id}@fixture.local"))
                .execute(pool)
                .await
                .expect("insert user");
            user_id
        }

        async fn new_workspace(pool: &PgPool) -> Uuid {
            let workspace_id = Uuid::new_v4();
            sqlx::query("INSERT INTO workspace (id, name, slug) VALUES ($1, 'fixture', $2)")
                .bind(workspace_id)
                .bind(format!("fx-{}", workspace_id.simple()))
                .execute(pool)
                .await
                .expect("insert workspace");
            workspace_id
        }

        async fn add_member(pool: &PgPool, ws: Uuid, user: Uuid, role: &str) {
            sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ($1, $2, $3)")
                .bind(ws)
                .bind(user)
                .bind(role)
                .execute(pool)
                .await
                .expect("insert member");
        }

        /// 建 user + workspace + owner membership，返回 `(user_id, workspace_id)`。
        async fn fixture(pool: &PgPool) -> (Uuid, Uuid) {
            let user = new_user(pool).await;
            let ws = new_workspace(pool).await;
            add_member(pool, ws, user, "owner").await;
            (user, ws)
        }

        /// 追加一个 issue（`number` 由调用方给，避开 `UNIQUE(workspace_id, number)`）。
        async fn new_issue(pool: &PgPool, ws: Uuid, user: Uuid, status: &str, number: i32) -> Uuid {
            let issue_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO issue (id, workspace_id, number, identifier, title, status, \
                  priority, creator_type, creator_id) \
                 VALUES ($1, $2, $3, $4, 'fixture issue', $5, 'medium', 'user', $6)",
            )
            .bind(issue_id)
            .bind(ws)
            .bind(number)
            .bind(format!("FX-{}", &issue_id.simple().to_string()[..6]))
            .bind(status)
            .bind(user.to_string())
            .execute(pool)
            .await
            .expect("insert issue");
            issue_id
        }

        /// 构造一条待写入的 item：参数顺序与所有调用点一致（`user` 在前）。
        fn item(user: Uuid, ws: Uuid, issue: Option<Uuid>, title: &str) -> NewInboxItem {
            NewInboxItem {
                id: None,
                workspace_id: Id::from(ws),
                user_id: Id::from(user),
                issue_id: issue.map(Id::from),
                actor_type: "user".into(),
                actor_id: Uuid::new_v4().to_string(),
                category: "new_comment".into(),
                title: title.into(),
                body: Some("body".into()),
            }
        }

        #[tokio::test]
        #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
        async fn list_unread_and_mark_read_are_idempotent() {
            let Some(repo) = connect().await else { return };
            let (user, ws) = fixture(repo.pool()).await;
            let ws = Id::from(ws);
            let user_id = Id::from(user);

            assert_eq!(repo.unread_count(ws, user_id).await.unwrap(), 0);
            for i in 0..3 {
                repo.create(item(user, ws.as_uuid(), None, &format!("n{i}")))
                    .await
                    .unwrap();
            }
            let listed = repo.list(ws, user_id, 50, 0).await.unwrap();
            assert_eq!(listed.len(), 3);
            assert_eq!(repo.unread_count(ws, user_id).await.unwrap(), 3);

            let first = repo.mark_read(listed[0].id()).await.unwrap();
            assert!(first.is_read());
            let again = repo.mark_read(listed[0].id()).await.unwrap();
            assert_eq!(again.read_at, first.read_at, "mark_read is idempotent");
            assert_eq!(repo.unread_count(ws, user_id).await.unwrap(), 2);

            let unread = repo.mark_unread(listed[0].id()).await.unwrap();
            assert!(unread.read_at.is_none());
            assert_eq!(repo.unread_count(ws, user_id).await.unwrap(), 3);

            assert_eq!(repo.mark_all_read(ws, user_id).await.unwrap(), 3);
            assert_eq!(
                repo.mark_all_read(ws, user_id).await.unwrap(),
                0,
                "mark_all_read is idempotent"
            );
            assert_eq!(repo.unread_count(ws, user_id).await.unwrap(), 0);

            // 分页
            assert_eq!(repo.list(ws, user_id, 2, 0).await.unwrap().len(), 2);
            assert_eq!(repo.list(ws, user_id, 2, 2).await.unwrap().len(), 1);
        }

        #[tokio::test]
        #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
        async fn archive_is_issue_level_and_idempotent() {
            let Some(repo) = connect().await else { return };
            let (user, ws) = fixture(repo.pool()).await;
            let ws = Id::from(ws);
            let user_id = Id::from(user);
            let issue = new_issue(repo.pool(), ws.as_uuid(), user, "todo", 1).await;

            let a = repo
                .create(item(user, ws.as_uuid(), Some(issue), "a1"))
                .await
                .unwrap();
            let b = repo
                .create(item(user, ws.as_uuid(), Some(issue), "a2"))
                .await
                .unwrap();
            let solo = repo.create(item(user, ws.as_uuid(), None, "solo")).await.unwrap();

            let archived = repo.archive(a.id()).await.unwrap();
            assert!(archived.is_archived());
            assert!(
                repo.get(b.id()).await.unwrap().is_archived(),
                "sibling of the same issue is archived together"
            );
            assert!(
                !repo.get(solo.id()).await.unwrap().is_archived(),
                "issue-less item is untouched"
            );
            let stamp = repo.get(b.id()).await.unwrap().archived_at;
            let again = repo.archive(b.id()).await.unwrap();
            assert_eq!(again.archived_at, stamp, "archive is idempotent");

            // 同组全部归档后，归档视图能看到该组（且只有最新一行）。
            let archived_rows = repo.list_archived(ws, user_id, 200).await.unwrap();
            assert_eq!(archived_rows.len(), 1);
            assert_eq!(archived_rows[0].id, b.id, "group representative is newest");

            let restored = repo.unarchive(a.id()).await.unwrap();
            assert!(!restored.is_archived());
            assert!(!repo.get(b.id()).await.unwrap().is_archived());
            let again = repo.unarchive(a.id()).await.unwrap();
            assert!(again.archived_at.is_none(), "unarchive is idempotent");

            // 取消归档后，该组回到主列表，归档列表里不再有它。
            assert_eq!(repo.list(ws, user_id, 50, 0).await.unwrap().len(), 3);
            assert!(repo.list_archived(ws, user_id, 200).await.unwrap().is_empty());
        }

        #[tokio::test]
        #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
        async fn archive_all_read_uses_group_newest() {
            let Some(repo) = connect().await else { return };
            let (user, ws) = fixture(repo.pool()).await;
            let ws = Id::from(ws);
            let user_id = Id::from(user);
            let issue_a = new_issue(repo.pool(), ws.as_uuid(), user, "todo", 1).await;
            let issue_b = new_issue(repo.pool(), ws.as_uuid(), user, "todo", 2).await;

            // 组 A：最新一条已读 → 整组归档（含旧的未读兄弟）。
            let old_unread = repo
                .create(item(user, ws.as_uuid(), Some(issue_a), "old"))
                .await
                .unwrap();
            let newest_read = repo
                .create(item(user, ws.as_uuid(), Some(issue_a), "newest"))
                .await
                .unwrap();
            repo.mark_read(newest_read.id()).await.unwrap();
            // 组 B：旧兄弟已读、最新一条未读 → 一行不动。
            let old_read = repo
                .create(item(user, ws.as_uuid(), Some(issue_b), "old-read"))
                .await
                .unwrap();
            repo.mark_read(old_read.id()).await.unwrap();
            let newest_unread = repo
                .create(item(user, ws.as_uuid(), Some(issue_b), "newest-unread"))
                .await
                .unwrap();
            assert!(newest_unread.read_at.is_none());

            let affected = repo.archive_all_read(ws, user_id).await.unwrap();
            assert_eq!(affected, 2, "only the read group's two rows");
            assert!(repo.get(old_unread.id()).await.unwrap().is_archived());
            assert!(repo.get(newest_read.id()).await.unwrap().is_archived());
            assert!(
                !repo.get(old_read.id()).await.unwrap().is_archived(),
                "unread group is untouched even though an old sibling was read"
            );
        }

        #[tokio::test]
        #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
        async fn archived_page_filters_and_cursor() {
            let Some(repo) = connect().await else { return };
            let (user, ws) = fixture(repo.pool()).await;
            let ws = Id::from(ws);
            let user_id = Id::from(user);

            let mut newest_id = None;
            let mut last_issue = None;
            for i in 0..3 {
                let issue = new_issue(repo.pool(), ws.as_uuid(), user, "in_progress", i + 1).await;
                let row = repo
                    .create(item(user, ws.as_uuid(), Some(issue), &format!("p{i}")))
                    .await
                    .unwrap();
                repo.mark_read(row.id()).await.unwrap();
                repo.archive(row.id()).await.unwrap();
                newest_id = Some(row.id());
                last_issue = Some(Id::from(issue));
            }

            let filter = ArchivedInboxFilter {
                unread_only: true,
                ..ArchivedInboxFilter::default()
            };
            let page = repo
                .list_archived_page(ws, user_id, &filter, None, 50)
                .await
                .unwrap();
            assert!(page.items.is_empty(), "all rows are read");

            let filter = ArchivedInboxFilter::default();
            let page = repo
                .list_archived_page(ws, user_id, &filter, None, 1)
                .await
                .unwrap();
            assert_eq!(page.items.len(), 1);
            assert!(page.has_more);
            let last = page.items[0].clone();
            assert_eq!(Some(last.id()), newest_id);
            let cursor = ArchivedCursor {
                created_at: last.created_at,
                id: last.id(),
            };
            let next = repo
                .list_archived_page(ws, user_id, &filter, Some(&cursor), 50)
                .await
                .unwrap();
            assert_eq!(next.items.len(), 2, "cursor skips the first group");
            assert!(!next.has_more);

            let facets = repo.archived_facets(ws, user_id, &filter).await.unwrap();
            assert_eq!(facets.unread_count, 0);
            assert_eq!(facets.actors.values().sum::<i64>(), 3, "one actor per group");
            assert_eq!(facets.statuses.get("in_progress"), Some(&3));
            assert_eq!(facets.priorities.get("medium"), Some(&3));

            // 单组过滤：组键是 `COALESCE(issue_id, id)`（与上游 SQL 逐字一致），
            // 所以有 issue 的通知要用 **issue id** 过滤。
            let filter = ArchivedInboxFilter {
                group_id: last_issue,
                ..ArchivedInboxFilter::default()
            };
            let page = repo
                .list_archived_page(ws, user_id, &filter, None, 50)
                .await
                .unwrap();
            assert_eq!(page.items.len(), 1);
            assert_eq!(Some(page.items[0].id()), newest_id, "组代表行是最新一条");

            // 用**行 id** 过滤一个有 issue 的组应得 0 行（组键是 issue_id）。
            let filter = ArchivedInboxFilter {
                group_id: newest_id,
                ..ArchivedInboxFilter::default()
            };
            assert!(repo
                .list_archived_page(ws, user_id, &filter, None, 50)
                .await
                .unwrap()
                .items
                .is_empty());

            // 无 issue 的通知：组键就是自己的行 id。
            let solo = repo.create(item(user, ws.as_uuid(), None, "solo")).await.unwrap();
            repo.archive(solo.id()).await.unwrap();
            let filter = ArchivedInboxFilter {
                group_id: Some(solo.id()),
                ..ArchivedInboxFilter::default()
            };
            let page = repo
                .list_archived_page(ws, user_id, &filter, None, 50)
                .await
                .unwrap();
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.items[0].id, solo.id);
        }

        #[tokio::test]
        #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
        async fn unread_summary_spans_workspaces_and_ignores_left_ones() {
            let Some(repo) = connect().await else { return };
            let pool = repo.pool().clone();
            let user = new_user(&pool).await;
            let ws_a = new_workspace(&pool).await;
            let ws_b = new_workspace(&pool).await;
            let ws_c = new_workspace(&pool).await;
            add_member(&pool, ws_a, user, "owner").await;
            add_member(&pool, ws_b, user, "member").await;
            // ws_c：用户不是成员 → 必须被排除。
            let user_id = Id::from(user);

            repo.create(item(user, ws_a, None, "a")).await.unwrap();
            repo.create(item(user, ws_a, None, "a2")).await.unwrap();
            repo.create(item(user, ws_b, None, "b")).await.unwrap();
            repo.create(item(user, ws_c, None, "c")).await.unwrap();

            let summary = repo.unread_summary(user_id).await.unwrap();
            let mut got: Vec<(Uuid, i64)> = summary
                .iter()
                .map(|w| (w.workspace_id.as_uuid(), w.count))
                .collect();
            got.sort_by_key(|(id, _)| *id);
            assert_eq!(got.len(), 2, "workspace the user left is excluded");
            assert!(got.iter().any(|(id, c)| *id == ws_a && *c == 2));
            assert!(got.iter().any(|(id, c)| *id == ws_b && *c == 1));
            assert!(!got.iter().any(|(id, _)| *id == ws_c));
        }

        #[tokio::test]
        #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
        async fn archive_completed_matches_closed_and_builtin_keys() {
            let Some(repo) = connect().await else { return };
            let pool = repo.pool().clone();
            let (user, ws) = fixture(&pool).await;
            let ws = Id::from(ws);
            let user_id = Id::from(user);

            // workspace 自定义终结状态（category = 'closed'）
            sqlx::query(
                "INSERT INTO issue_status (workspace_id, name, key, category) \
                 VALUES ($1, 'Shipped', 'shipped', 'closed')",
            )
            .bind(ws.as_uuid())
            .execute(&pool)
            .await
            .unwrap();

            for (i, status) in ["shipped", "done", "in_progress"].iter().enumerate() {
                let issue =
                    new_issue(&pool, ws.as_uuid(), user, status, i32::try_from(i).unwrap() + 10)
                        .await;
                repo.create(item(user, ws.as_uuid(), Some(issue), status))
                    .await
                    .unwrap();
            }

            let affected = repo.archive_completed(ws, user_id).await.unwrap();
            assert_eq!(affected, 2, "shipped (catalog closed) + done (builtin)");
            let remaining = repo.list(ws, user_id, 50, 0).await.unwrap();
            assert_eq!(remaining.len(), 1, "in_progress notification stays");
            assert_eq!(remaining[0].issue_status.as_deref(), Some("in_progress"));
        }

        #[tokio::test]
        #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
        async fn archive_all_covers_every_active_row() {
            let Some(repo) = connect().await else { return };
            let (user, ws) = fixture(repo.pool()).await;
            let ws = Id::from(ws);
            let user_id = Id::from(user);
            let issue = new_issue(repo.pool(), ws.as_uuid(), user, "todo", 1).await;
            for i in 0..2 {
                let row = repo
                    .create(item(user, ws.as_uuid(), Some(issue), &format!("x{i}")))
                    .await
                    .unwrap();
                if i == 0 {
                    repo.mark_read(row.id()).await.unwrap();
                }
            }
            assert_eq!(repo.archive_all(ws, user_id).await.unwrap(), 2);
            assert_eq!(repo.list(ws, user_id, 50, 0).await.unwrap().len(), 0);
            assert_eq!(repo.archive_all(ws, user_id).await.unwrap(), 0);
            // 归档页：整组只有一行
            assert_eq!(repo.list_archived(ws, user_id, 200).await.unwrap().len(), 1);
        }

        #[tokio::test]
        #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
        async fn get_for_user_hides_other_users_items() {
            let Some(repo) = connect().await else { return };
            let pool = repo.pool().clone();
            let (user, ws) = fixture(&pool).await;
            let intruder = new_user(&pool).await;
            add_member(&pool, ws, intruder, "member").await;
            let row = repo.create(item(user, ws, None, "mine")).await.unwrap();

            assert!(repo
                .get_for_user(row.id(), Id::from(ws), Id::from(intruder))
                .await
                .is_err());
            assert!(repo
                .get_for_user(row.id(), Id::from(ws), Id::from(user))
                .await
                .is_ok());
        }
    }
}

//! `PinRepo` —— `pinned_item`（侧栏钉住项）的行访问（M2-A 尾 / LUM-1691）。
//!
//! 对应上游 `server/internal/handler/pin.go` + `server/pkg/db/queries/pinned_item.sql`。
//! 表已在 W0-B2 的上游迁移链上（`038_pinned_items` + `270_pinned_item_view`）⇒ 本片
//! **不新增迁移**，schema 逐字同构（实测约束）：
//!
//! ```text
//! pinned_item(id PK, workspace_id → workspace ON DELETE CASCADE,
//!             user_id → "user" ON DELETE CASCADE, item_type, item_id, position float8,
//!             created_at)   -- UNIQUE (workspace_id, user_id, item_type, item_id)
//! item_type CHECK IN ('issue','project','view')      -- 270 把 038 的两值扩到三值
//! INDEX idx_pinned_item_user_ws (workspace_id, user_id, position)
//! ```
//!
//! 关键语义（照上游）：
//! - **pin 是「每用户每 workspace」的私有数据**：所有查询都带 `workspace_id AND user_id`
//!   两个谓词 ⇒ 别人的 pin 既读不到也改不到（不是权限判定，是**行的归属**）。
//! - **追加位置**：`COALESCE(MAX(position), 0) + 1`（上游 `CreatePin` 两步：取 max 再插）。
//!   两步之间没有事务，上游也没有 —— 这里保持同款，不发明比上游更强的保证。
//! - **重复 pin = 409**：`pinned_item_..._key` 唯一约束 ⇒ SQLSTATE `23505` ⇒
//!   [`crate::RepoError::Conflict`] ⇒ 路由层 409 `item already pinned`（上游 `isUniqueViolation`
//!   分支）。**注意这不是「幂等插入」**：上游确实报错，幂等只体现在 [`PinRepo::delete`]。
//! - **幂等的只有删除**：`DELETE` 不看 rows affected ⇒ 未钉过的项再删一次仍 `Ok`（上游
//!   `DeletePin` 直接忽略错误并返回 204）。
//! - **排序字段**：`list` 用 `ORDER BY position ASC, created_at ASC`（上游
//!   `ListPinnedItems`）；`reorder` 只改 `position`，不去动 `created_at`。
//! - **`view` 分支的兼容闸门**：`GET /api/pins` 默认**不返回** `item_type='view'` 的行，
//!   除非调用者声明 `?include=view`（上游 `ListPins` 的能力选择位；老客户端会把不认识的
//!   非 issue pin 当项目 pin 拉详情 → 404 → 永久自动取消钉住）。判定抽成纯函数
//!   [`visible_without_view_capability`]，有单测钉住。

use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 合法的 `item_type`（上游 `CreatePin` 的三值校验；`270_pinned_item_view` 已把
/// `pinned_item_item_type_check` 扩到这 3 个值）。
pub const ITEM_TYPES: [&str; 3] = ["issue", "project", "view"];

/// 非法 `item_type` 的 400 文案（上游 `CreatePin` 逐字）。
pub const ITEM_TYPE_ERROR: &str = "item_type must be 'issue', 'project' or 'view'";

/// `item_type` 是否合法（上游 `req.ItemType != "issue" && != "project" && != "view"`）。
pub fn is_valid_item_type(raw: &str) -> bool {
    ITEM_TYPES.contains(&raw)
}

/// `GET /api/pins` 的旧契约可见性：`item_type='view'` 的行只对声明了 `?include=view`
/// 的调用者可见（上游 `ListPins` 的 `p.ItemType == "view" && !includeViews` 跳过）。
pub fn visible_without_view_capability(item_type: &str, include_views: bool) -> bool {
    include_views || item_type != "view"
}

/// `pinned_item` 行。
#[derive(Debug, Clone, FromRow)]
pub struct PinnedItemRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    /// `issue` / `project` / `view`
    pub item_type: String,
    /// 被钉对象的 id（**故意没有外键** —— 上游按 `item_type` 在应用层判归属）。
    pub item_id: Uuid,
    /// 侧栏排序位（`float8`，`max + 1` 追加）。
    pub position: f64,
    pub created_at: DateTime<Utc>,
}

impl PinnedItemRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// 被钉对象的 id。
    pub fn item_id(&self) -> Id {
        Id::from(self.item_id)
    }
}

/// `pinned_item` 列清单（`SELECT` / `RETURNING` 两处共用，防漂移）。
const PIN_COLUMNS: &str = "id, workspace_id, user_id, item_type, item_id, position, created_at";

/// `pinned_item` 仓储。
pub struct PinRepo {
    db: Db,
}

impl PinRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 本用户在本 workspace 的钉住项，按 `position ASC, created_at ASC`（上游 `ListPinnedItems`）。
    ///
    /// 行序即侧栏序，`view` 行的过滤**不在这里**（上游在 handler 里过滤）。
    pub async fn list(&self, workspace_id: Id, user_id: Id) -> Result<Vec<PinnedItemRow>> {
        let sql = format!(
            "SELECT {PIN_COLUMNS} FROM pinned_item \
             WHERE workspace_id = $1 AND user_id = $2 \
             ORDER BY position ASC, created_at ASC"
        );
        sqlx::query_as::<_, PinnedItemRow>(&sql)
            .bind(workspace_id.0)
            .bind(user_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 当前最大 `position`；表里没有行时 `0`（上游 `GetMaxPinnedItemPosition` 的
    /// `COALESCE(MAX(position), 0)::float8`）。
    pub async fn max_position(&self, workspace_id: Id, user_id: Id) -> Result<f64> {
        let (max,): (f64,) = sqlx::query_as(
            "SELECT COALESCE(MAX(position), 0)::float8 FROM pinned_item \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(workspace_id.0)
        .bind(user_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(max)
    }

    /// 追加一行（上游 `CreatePinnedItem`）。
    ///
    /// 重复钉同一项 ⇒ 唯一约束 `23505` ⇒ [`crate::RepoError::Conflict`]（路由层 409）。
    pub async fn create(
        &self,
        workspace_id: Id,
        user_id: Id,
        item_type: &str,
        item_id: Id,
        position: f64,
    ) -> Result<PinnedItemRow> {
        let sql = format!(
            "INSERT INTO pinned_item (workspace_id, user_id, item_type, item_id, position) \
             VALUES ($1, $2, $3, $4, $5) RETURNING {PIN_COLUMNS}"
        );
        sqlx::query_as::<_, PinnedItemRow>(&sql)
            .bind(workspace_id.0)
            .bind(user_id.0)
            .bind(item_type)
            .bind(item_id.0)
            .bind(position)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 删除一行，**幂等**：未命中不报错（上游 `DeletePin` 忽略 rows affected 并返回 204）。
    ///
    /// 返回实际删掉的行数，仅供调用方/测试观察，不影响 204。
    pub async fn delete(
        &self,
        workspace_id: Id,
        user_id: Id,
        item_type: &str,
        item_id: Id,
    ) -> Result<u64> {
        let done = sqlx::query(
            "DELETE FROM pinned_item \
             WHERE workspace_id = $1 AND user_id = $2 AND item_type = $3 AND item_id = $4",
        )
        .bind(workspace_id.0)
        .bind(user_id.0)
        .bind(item_type)
        .bind(item_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(done.rows_affected())
    }

    /// 改写一行的 `position`（上游 `UpdatePinnedItemPosition`）。
    ///
    /// 归属由 `workspace_id AND user_id` 收窄；别人的 pin id ⇒ 0 行、**不报错**
    /// （上游 `ReorderPins` 逐个 `if err != nil` 只在 SQL 错误时 500，0 行不是错误）。
    pub async fn set_position(
        &self,
        workspace_id: Id,
        user_id: Id,
        pin_id: Id,
        position: f64,
    ) -> Result<u64> {
        let done = sqlx::query(
            "UPDATE pinned_item SET position = $1 \
             WHERE id = $2 AND workspace_id = $3 AND user_id = $4",
        )
        .bind(position)
        .bind(pin_id.0)
        .bind(workspace_id.0)
        .bind(user_id.0)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(done.rows_affected())
    }

    /// 某被钉对象在本 workspace 下的全部 pin 行（**所有用户**）—— 供 `view` 删除的清扫断言用。
    pub async fn count_for_item(
        &self,
        workspace_id: Id,
        item_type: &str,
        item_id: Id,
    ) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM pinned_item \
             WHERE workspace_id = $1 AND item_type = $2 AND item_id = $3",
        )
        .bind(workspace_id.0)
        .bind(item_type)
        .bind(item_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(count)
    }
}

impl RepoWithDb for PinRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

// ---------------------------------------------------------------------------
// 纯单测（无需 DB）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_type_vocabulary_matches_the_check_constraint() {
        // `270_pinned_item_view` 之后 `pinned_item_item_type_check` 恰好是这 3 个值。
        assert!(is_valid_item_type("issue"));
        assert!(is_valid_item_type("project"));
        assert!(is_valid_item_type("view"));
        assert!(!is_valid_item_type("label"));
        assert!(!is_valid_item_type(""));
        assert!(!is_valid_item_type("ISSUE"));
        assert_eq!(ITEM_TYPES.len(), 3);
    }

    #[test]
    fn view_pins_hide_from_a_client_without_the_capability() {
        // 旧契约：view 行必须消失（否则老客户端会把它当项目 pin 拉详情 → 404 → 自动取消）。
        assert!(!visible_without_view_capability("view", false));
        assert!(visible_without_view_capability("view", true));
        // issue / project 两种 item_type 两种形态都在。
        for item_type in ["issue", "project"] {
            assert!(visible_without_view_capability(item_type, false));
            assert!(visible_without_view_capability(item_type, true));
        }
    }
}

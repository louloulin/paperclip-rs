//! `InboxRepo` 的构造与写入面（`create`；生产者 = M3 事件层，本切片用测试夹具驱动）。

use std::sync::Arc;

use super::input::NewInboxItem;
use super::row::InboxItemRow;
use crate::workspace::map_sqlx_err;
use crate::Result;
use mc_core::Id;
use mc_db::Db;
use sqlx::PgPool;

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

    pub(super) fn pool(&self) -> &PgPool {
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
                 (id, workspace_id, recipient_type, recipient_id, issue_id, \
                  actor_type, actor_id, type, title, body, created_at) \
             VALUES ($1, $2, 'user', $3, $4, $5, $6::uuid, $7, $8, $9, now())",
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
}

//! `user_composio_connection` 仓储面 —— **M8-6 落地（`LUM-1803`）**。
//!
//! - **上游**：`internal/integrations/composio/service.go` 的落库部分 +
//!   `pkg/db/queries/composio.sql`（4 条查询：upsert / list-active / get / mark-revoked）。
//! - **表的落法**（1 张，**本波 0 新迁移**）：`user_composio_connection`（迁移 `127`）。
//! - **归属**：连接属于**用户**，不属于 workspace（`docs/61` §1.1 第 4 簇）⇒
//!   [`ComposioConnectionRepo::list_active`] / [`ComposioConnectionRepo::get`] /
//!   [`ComposioConnectionRepo::mark_revoked`] **每一条**都带 `user_id` 收窄。
//! - **凭据纪律**：`connected_account_id` / `composio_user_id` 是**外部标识**（非密钥）；
//!   bearer 只在 `mc-composio::service` 的会话 URL 里，**不进**本模块。
//! - **本仓约定**：裸 `Uuid` + 手写 `sqlx::FromRow`、`map_sqlx_err`、运行时 builder +
//!   参数绑定；`UNIQUE (user_id, connected_account_id)` 是 connect 的幂等键。
//!
//! # 幂等语义（上游逐字）
//!
//! - upsert 以 `(user_id, connected_account_id)` 为冲突键 ⇒ **重复 callback 重新激活同一行**，
//!   不会产生第二行；冲突时把 `toolkit_slug` / `auth_config_id` / `composio_user_id` 一并刷新
//!   （上游 `ON CONFLICT ... DO UPDATE` 的口径：**镜像**上游的当前事实）；
//! - `status` 在 upsert 时**显式**写回 `'active'`：一行被 `revoked` 过之后，同一次授权再回来
//!   （Composio 侧重建了同一个 `connected_account_id`）应当重新可用；
//! - `mark_revoked` 是 `WHERE id = $ AND user_id = $` 的**收窄**更新：别人的连接**改不动**
//!   （改成 0 行，调用侧按 [`RepoError::NotFound`] 处理）。
//!
//! # 时间列
//!
//! `connected_at` / `created_at` / `updated_at` 都有 DEFAULT；本模块**只**显式写
//! `last_used_at = now()` 于 upsert（「最近一次使用」= 最近一次被 callback 认领），
//! 其余交给库的默认值 —— 与迁移 `127` 的默认值同源，避免了「应用侧发明时间」这类漂移。

use chrono::{DateTime, Utc};
use mc_core::composio::{ComposioConnection, ComposioConnectionStatus};
use mc_core::id::Id;
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

/// `user_composio_connection` 的一行（迁移 `127`；**11 列**）。
///
/// 派生 `Debug` 是安全的：这 11 列里没有密钥（`connected_account_id` / `composio_user_id` 是
/// 外部标识，`auth_config_id` 是不透明的配置句柄 `ac_…`）。bearer **不在**这张表里
/// （`docs/61` §2.4 的 redaction 第 1 条）。
#[derive(Debug, Clone, FromRow, PartialEq, Eq)]
pub struct ComposioConnectionRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub toolkit_slug: String,
    pub auth_config_id: String,
    pub connected_account_id: String,
    pub composio_user_id: String,
    pub status: String,
    pub connected_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ComposioConnectionRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属用户。
    pub fn user_id(&self) -> Id {
        Id(self.user_id)
    }

    /// `status` 的判别式：未知值 ⇒ `None`（**不** panic、**不**静默回落成 `active`）。
    ///
    /// 未知识别标识一律当「不是 active」处理 —— 把不认识的状态当成活跃连接会让越界数据
    /// 参与会话构建（`crate::service::create_mcp_session` 只收 `active` 行）。
    pub fn status_kind(&self) -> Option<ComposioConnectionStatus> {
        ComposioConnectionStatus::from_str(&self.status)
    }

    /// 是否活跃（派生的便利判据）。
    pub fn is_active(&self) -> bool {
        self.status_kind() == Some(ComposioConnectionStatus::Active)
    }

    /// 领域投影（`mc_core::composio::ComposioConnection`）。
    ///
    /// **未知状态 ⇒ `None`**：领域类型的 `status` 是枚举，没有「未知」这一支。
    pub fn to_domain(&self) -> Option<ComposioConnection> {
        Some(ComposioConnection {
            id: self.id(),
            user_id: self.user_id(),
            toolkit_slug: self.toolkit_slug.clone(),
            auth_config_id: self.auth_config_id.clone(),
            connected_account_id: self.connected_account_id.clone(),
            composio_user_id: self.composio_user_id.clone(),
            status: self.status_kind()?,
            connected_at: mc_core::Timestamp::from_unix(self.connected_at.timestamp()),
            last_used_at: self
                .last_used_at
                .map(|at| mc_core::Timestamp::from_unix(at.timestamp())),
            created_at: mc_core::Timestamp::from_unix(self.created_at.timestamp()),
            updated_at: mc_core::Timestamp::from_unix(self.updated_at.timestamp()),
        })
    }
}

/// upsert 一行连接的入参（上游 `UpsertUserComposioConnectionParams`）。
#[derive(Debug, Clone)]
pub struct NewComposioConnection {
    pub user_id: Id,
    /// 规范化后的小写 slug（`mc_composio::catalog::normalize_slug` 的产物）。
    pub toolkit_slug: String,
    /// `ac_…`（不透明配置句柄）。
    pub auth_config_id: String,
    /// `ca_…`（外部标识；与 `user_id` 组成幂等键）。
    pub connected_account_id: String,
    /// **不变量**：等于 `user_id.to_string()`（上游 `service.go` 逐字：`composio_user_id ==
    /// Multica user id`）。显式存下来，好让将来映射变化不会静默破坏已连接的账号。
    pub composio_user_id: String,
}

/// `user_composio_connection` 的仓储。
#[derive(Clone)]
pub struct ComposioConnectionRepo {
    db: Db,
}

impl RepoWithDb for ComposioConnectionRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// 全部列（顺序与 [`ComposioConnectionRow`] 的字段一一对应）。
const CONNECTION_COLUMNS: &str =
    "id, user_id, toolkit_slug, auth_config_id, connected_account_id, \
                                  composio_user_id, status, connected_at, last_used_at, \
                                  created_at, updated_at";

/// upsert 语句（**唯一**实现点：单测断言的是这条真的会被执行的 SQL，不是副本）。
fn upsert_sql() -> String {
    format!(
        "INSERT INTO user_composio_connection \
         (user_id, toolkit_slug, auth_config_id, connected_account_id, composio_user_id, status) \
         VALUES ($1, $2, $3, $4, $5, 'active') \
         ON CONFLICT (user_id, connected_account_id) DO UPDATE SET \
           toolkit_slug = EXCLUDED.toolkit_slug, \
           auth_config_id = EXCLUDED.auth_config_id, \
           composio_user_id = EXCLUDED.composio_user_id, \
           status = 'active', \
           last_used_at = now(), \
           updated_at = now() \
         RETURNING {CONNECTION_COLUMNS}"
    )
}

/// 活跃连接列表语句（`connected_at DESC` 是契约：最新者胜建立在这个序上）。
fn list_active_sql() -> String {
    format!(
        "SELECT {CONNECTION_COLUMNS} FROM user_composio_connection \
         WHERE user_id = $1 AND status = 'active' ORDER BY connected_at DESC"
    )
}

/// 单行读取语句（按 `id` **+ `user_id`** 收窄）。
fn get_sql() -> String {
    format!(
        "SELECT {CONNECTION_COLUMNS} FROM user_composio_connection \
         WHERE id = $1 AND user_id = $2"
    )
}

/// 标记 `revoked` 的语句（按 `id` **+ `user_id`** 收窄；0 行 ⇒ `NotFound`）。
const MARK_REVOKED_SQL: &str = "UPDATE user_composio_connection SET status = 'revoked', \
     updated_at = now() WHERE id = $1 AND user_id = $2";

impl ComposioConnectionRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `UpsertUserComposioConnection`：按 `(user_id, connected_account_id)` upsert。
    ///
    /// 冲突时刷新 `toolkit_slug` / `auth_config_id` / `composio_user_id` / `last_used_at` /
    /// `updated_at`，并把 `status` **显式**写回 `'active'`（重新授权 ⇒ 重新可用）。
    /// `connected_at` **不动**：它记的是这条连接**第一次**建立的时间。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`RepoError`]。
    pub async fn upsert(&self, new: NewComposioConnection) -> Result<ComposioConnectionRow> {
        sqlx::query_as::<_, ComposioConnectionRow>(&upsert_sql())
            .bind(new.user_id.0)
            .bind(&new.toolkit_slug)
            .bind(&new.auth_config_id)
            .bind(&new.connected_account_id)
            .bind(&new.composio_user_id)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `ListActiveUserComposioConnections`：调用者的活跃连接，按 `connected_at DESC`
    /// （**列表顺序是契约的一部分**：`sort_by=usage` 之外的「最新者胜」语义就建立在这个序上，
    /// 上游 `dispatch.go` 的 `pinConnectedAccounts` 逐字依赖它）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`RepoError`]。
    pub async fn list_active(&self, user_id: Id) -> Result<Vec<ComposioConnectionRow>> {
        sqlx::query_as::<_, ComposioConnectionRow>(&list_active_sql())
            .bind(user_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `GetUserComposioConnection`：**按 id + `user_id` 收窄**取一行。
    ///
    /// 返回 `Ok(None)`（而不是错误）区分「不属于你 / 不存在」与「库坏了」—— 调用侧把两者都
    /// 折成 404（上游注释逐字：without leaking existence across users）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`RepoError`]。
    pub async fn get(
        &self,
        connection_id: Id,
        user_id: Id,
    ) -> Result<Option<ComposioConnectionRow>> {
        sqlx::query_as::<_, ComposioConnectionRow>(&get_sql())
            .bind(connection_id.0)
            .bind(user_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `MarkUserComposioConnectionRevoked`：把一行标成 `revoked`（**收窄**到调用者）。
    ///
    /// # Errors
    ///
    /// 行不存在 / 不属于调用者 ⇒ [`RepoError::NotFound`]；其余库错误 ⇒ [`RepoError::Db`]。
    pub async fn mark_revoked(&self, connection_id: Id, user_id: Id) -> Result<()> {
        let affected = sqlx::query(MARK_REVOKED_SQL)
            .bind(connection_id.0)
            .bind(user_id.0)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str) -> ComposioConnectionRow {
        let at = DateTime::<Utc>::from_timestamp(1_800_000_000, 0).expect("timestamp");
        ComposioConnectionRow {
            id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("uuid"),
            user_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").expect("uuid"),
            toolkit_slug: "notion".into(),
            auth_config_id: "ac_notion".into(),
            connected_account_id: "ca_1".into(),
            composio_user_id: "22222222-2222-2222-2222-222222222222".into(),
            status: status.into(),
            connected_at: at,
            last_used_at: Some(at),
            created_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn status_is_parsed_explicitly_and_unknown_is_not_active() {
        assert!(row("active").is_active());
        assert!(!row("revoked").is_active());
        assert!(!row("expired").is_active());
        assert!(!row("ACTIVE").is_active(), "存储字面量是小写");
        assert!(!row("weird").is_active());
        assert_eq!(row("weird").status_kind(), None);
    }

    #[test]
    fn domain_projection_maps_every_column_and_rejects_unknown_status() {
        let domain = row("active").to_domain().expect("domain");
        assert_eq!(domain.toolkit_slug, "notion");
        assert_eq!(domain.auth_config_id, "ac_notion");
        assert_eq!(domain.connected_account_id, "ca_1");
        assert_eq!(domain.status, ComposioConnectionStatus::Active);
        assert_eq!(
            domain.user_id.0.to_string(),
            row("active").user_id.to_string()
        );
        assert!(domain.last_used_at.is_some());
        assert!(row("weird").to_domain().is_none(), "未知状态没有领域对应物");
    }

    #[test]
    fn the_upsert_statement_conflicts_on_the_unique_key_and_reactivates() {
        let sql = upsert_sql();
        assert!(sql.contains("ON CONFLICT (user_id, connected_account_id)"));
        assert!(sql.contains("status = 'active'"));
        assert!(sql.contains("last_used_at = now()"));
        assert!(
            !sql.contains("connected_at = now()"),
            "connected_at 记的是第一次建立的时间，冲突时不刷新"
        );
        assert!(sql.contains("RETURNING id, user_id, toolkit_slug"));
    }

    #[test]
    fn read_and_write_statements_are_narrowed_to_the_user() {
        let statements = [list_active_sql(), get_sql(), MARK_REVOKED_SQL.to_string()];
        for sql in statements {
            assert!(
                sql.contains("user_id"),
                "每条查询都必须带 user_id 收窄（连接属于用户，不属于 workspace）：{sql}"
            );
        }
        assert!(
            list_active_sql().contains("ORDER BY connected_at DESC"),
            "最新者胜靠这个序"
        );
        assert!(MARK_REVOKED_SQL.contains("status = 'revoked'"));
        assert!(CONNECTION_COLUMNS.contains("composio_user_id"));
    }
}

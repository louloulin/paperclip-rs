//! `plugin_storage` 的读写（公开 API `/v1/*/storage` 与 bridge 的 storage 面**共用**）。
//!
//! - **写者**：M6-7（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/service/plugin_storage.go`（`PluginStorageWorkspace` / `PluginStorageUser` /
//!   四个配额常量 / `ResolveStorageScope` / `EnforceStorageQuota` / `ListStorageKeys` /
//!   `GetStorageValue` / `SetStorageValue` / `DeleteStorageValue`）。
//! - **8 列**：`id, installation_id, scope_type, scope_id, key, value, created_at, updated_at`；
//!   `scope_type CHECK IN ('workspace','user')`；`key` ≤1024、`value` ≤102400（`octet_length`）。
//! - **三条硬语义**：
//!   1. `scope_id` 的取值取决于 `scope_type`：`workspace` ⇒ 工作区 id，`user` ⇒ **用户 id**
//!      （不是安装 id）—— 写错会出现「A 用户读到 B 用户的键」；
//!   2. **软配额**（1000 键 / 5 MiB/安装）**没有淘汰**：超了要返回明确错误，不能 LRU 掉别人的数据；
//!   3. `value` 是不透明的文本（宿主不理解内容，只做大小与配额校验）。
//! - **本仓约定**：`(installation_id, scope_type, scope_id, key)` 是逻辑主键（表上没有唯一约束，
//!   `ON CONFLICT` 用不了 ⇒ 走「先 UPDATE，影响 0 行再 INSERT」），**别**假设有唯一索引。
//! - **不做什么**：不做加密（`plugin_secret` 才是密文面，本文件不碰它）。
//!
//! ## 配额判定的**唯一实现点**（上游 `EnforceStorageQuota` 的逐字口径）
//!
//! `usage` 查询**排除**正在写的那个键 ⇒ 覆盖写被当作「替换」计量，不会因为「已有行占满额度」
//! 而失败。判定函数是纯函数 [`enforce_quota`]，好让它和三个上限一起被单测钉住。
//!
//! ⚠️ **已知边界（照抄上游注释）**：usage 是**无锁预读**，同一 scope 的并发写可以超出各自的
//! 体积。把每个插件 KV 写串行化的代价高于这点超出的代价 —— 上限是用来拦住失控增长的，不是
//! 精确到字节的会计。
//!
//! ## 与上游的两处**有意**差异（M6-7 登记）
//!
//! 1. **upstream `value` 是 `TEXT`，本仓也是 `TEXT`**（迁移 `344` 逐字），所以「不透明 JSONB」
//!    这一说法在 `docs/57` 的锚点注释里是**过期口径**：列类型是 `TEXT`，宿主只做字节数校验。
//! 2. `DeleteStorageValue` 在键不存在时上游返回 **404**（`pluginErrf(PluginErrorNotFound,
//!    "storage key not found")`），而 M6-0 的 `routes/v1/storage.rs` 桩注释写「DELETE 幂等」——
//!    本文件按**上游代码**实现（404），该行注释已在 docs/32 §9 登记为需要更正的过期口径。
//!
//! **状态：M6-7 已落地。**
//!
//! 行预算（门 ⑩）：本文件 ≤420 行。

use chrono::{DateTime, Utc};
use sqlx::postgres::PgQueryResult;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use mc_core::Id;

use crate::workspace::map_sqlx_err;
use crate::Db;

/// 事务类型别名（与 `plugin/*` 其余子文件同款）。
pub type Tx<'a> = Transaction<'a, Postgres>;

/// `plugin_storage.scope_type` 的两个取值（上游 `PluginStorageWorkspace` / `PluginStorageUser`）。
pub const SCOPE_WORKSPACE: &str = "workspace";
/// 每成员态（`scope_id` = user id）。
pub const SCOPE_USER: &str = "user";

/// 键的字节上限（上游 `MaxPluginStorageKeyBytes`；列上也有 `octet_length BETWEEN 1 AND 1024`）。
pub const MAX_KEY_BYTES: usize = 1024;
/// 单值的字节上限（上游 `MaxPluginStorageValueBytes`；列上是 `<= 102400`）。
pub const MAX_VALUE_BYTES: usize = 100 * 1024;
/// 单 scope 的键数上限（上游 `MaxPluginStorageKeys`）。
pub const MAX_KEYS: i64 = 1000;
/// 单 scope 的字节总量上限（上游 `MaxPluginStorageTotalBytes`）。
pub const MAX_TOTAL_BYTES: i64 = 5 * 1024 * 1024;

/// `plugin_storage` 的失败（上游 `PluginError` 的四种取值在本面的投影）。
///
/// 不复用 [`crate::RepoError`]：那个类型只有 `NotFound` / `Conflict` / `Db` 三态，而存储面
/// 必须能区分「入参非法」「超配额」「键不存在」——三者在上游分别是 400 / 507 / 404，混成一个
/// `NotFound` 会让 507 变成 404（客户端会误以为「重试就好」）。
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// 入参非法（scope 名未知、key 为空/超长）。
    #[error("{0}")]
    Invalid(String),
    /// 键不存在。
    #[error("{0}")]
    NotFound(String),
    /// 配额超限（上游 507）。
    #[error("{0}")]
    Quota(String),
    /// 存储故障（上游 502）。
    #[error("{0}")]
    Unavailable(String),
}

/// 一行键的**摘要**（上游 `PluginStorageKey`）：**不含** value —— 列表是给插件看「自己写过
/// 什么」，不是批量读状态的通道。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct StorageKeyRow {
    pub key: String,
    /// `octet_length(value)`（列口径就是字节数，非 ASCII 时与 `char_length` 不同）。
    pub size_bytes: i64,
    pub updated_at: DateTime<Utc>,
}

/// 一个 scope 的用量（上游 `GetPluginStorageUsageRow`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct StorageUsageRow {
    pub key_count: i64,
    pub total_bytes: i64,
}

/// `plugin_storage` 的读口 + 写口。
#[derive(Debug, Clone)]
pub struct StorageRepo {
    pool: PgPool,
}

impl StorageRepo {
    /// 从应用共享 `Db` 句柄构造。
    pub fn new(db: &Db) -> Self {
        Self {
            pool: db.pool().clone(),
        }
    }

    /// 用自定义 pool 构造（集成测试用）。
    pub fn with_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 池引用（调用方要在同一池上开事务时用）。
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 一个 scope 的全部键（按 key 升序；**不含** value）。
    ///
    /// # Errors
    ///
    /// [`StorageError::Unavailable`]。
    pub async fn list_keys(
        &self,
        installation_id: Id,
        scope_type: &str,
        scope_id: Id,
    ) -> Result<Vec<StorageKeyRow>, StorageError> {
        sqlx::query_as::<_, StorageKeyRow>(
            "SELECT key, octet_length(value)::bigint AS size_bytes, updated_at \
             FROM plugin_storage \
             WHERE installation_id = $1 AND scope_type = $2 AND scope_id = $3 \
             ORDER BY key ASC",
        )
        .bind(installation_id.0)
        .bind(scope_type)
        .bind(scope_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| StorageError::Unavailable(format!("list plugin storage: {error}")))
    }

    /// 读一个值；不存在 ⇒ [`StorageError::NotFound`]（上游 404）。
    ///
    /// 这条路径**按构造**够不到 `plugin_secret`：密文在另一张表，且没有任何返回 ciphertext 的
    /// 查询（上游注释逐字）。
    ///
    /// # Errors
    ///
    /// [`StorageError::Invalid`]（key 为空/超长）、[`StorageError::NotFound`]、
    /// [`StorageError::Unavailable`]。
    pub async fn get_value(
        &self,
        installation_id: Id,
        scope_type: &str,
        scope_id: Id,
        key: &str,
    ) -> Result<String, StorageError> {
        validate_key(key)?;
        sqlx::query_scalar::<_, String>(
            "SELECT value FROM plugin_storage \
             WHERE installation_id = $1 AND scope_type = $2 AND scope_id = $3 AND key = $4",
        )
        .bind(installation_id.0)
        .bind(scope_type)
        .bind(scope_id.0)
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| StorageError::Unavailable(format!("read plugin storage: {error}")))?
        .ok_or_else(|| StorageError::NotFound("storage key not found".to_string()))
    }

    /// 用量：**排除** `key` 自己（上游 `GetPluginStorageUsage`）。
    ///
    /// # Errors
    ///
    /// [`StorageError::Unavailable`]。
    pub async fn usage_excluding(
        &self,
        installation_id: Id,
        scope_type: &str,
        scope_id: Id,
        key: &str,
    ) -> Result<StorageUsageRow, StorageError> {
        sqlx::query_as::<_, StorageUsageRow>(
            "SELECT COUNT(*)::bigint AS key_count, \
                    COALESCE(SUM(octet_length(value)), 0)::bigint AS total_bytes \
             FROM plugin_storage \
             WHERE installation_id = $1 AND scope_type = $2 AND scope_id = $3 AND key <> $4",
        )
        .bind(installation_id.0)
        .bind(scope_type)
        .bind(scope_id.0)
        .bind(key)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| StorageError::Unavailable(format!("read plugin storage usage: {error}")))
    }

    /// 写一个值：先查用量并过配额，再「UPDATE，影响 0 行则 INSERT」。
    ///
    /// 三步在**同一事务**里，好让配额预读与写入看到同一份快照（并发超出见文件头的已知边界）。
    ///
    /// # Errors
    ///
    /// [`StorageError::Invalid`]、[`StorageError::Quota`]、[`StorageError::Unavailable`]。
    pub async fn set_value(
        &self,
        installation_id: Id,
        scope_type: &str,
        scope_id: Id,
        key: &str,
        value: &str,
    ) -> Result<(), StorageError> {
        validate_key(key)?;
        let mut tx =
            self.pool.begin().await.map_err(|error| {
                StorageError::Unavailable(format!("begin plugin storage: {error}"))
            })?;

        let usage = usage_excluding_tx(&mut tx, installation_id, scope_type, scope_id, key).await?;
        enforce_quota(usage, value.len())?;

        let updated = sqlx::query(
            "UPDATE plugin_storage SET value = $5, updated_at = now() \
             WHERE installation_id = $1 AND scope_type = $2 AND scope_id = $3 AND key = $4",
        )
        .bind(installation_id.0)
        .bind(scope_type)
        .bind(scope_id.0)
        .bind(key)
        .bind(value)
        .execute(&mut *tx)
        .await
        .map_err(|error| StorageError::Unavailable(format!("write plugin storage: {error}")))?;

        if updated.rows_affected() == 0 {
            insert_value_tx(&mut tx, installation_id, scope_type, scope_id, key, value).await?;
        }

        tx.commit()
            .await
            .map_err(|error| StorageError::Unavailable(format!("commit plugin storage: {error}")))
    }

    /// 删一个键；不存在 ⇒ [`StorageError::NotFound`]（上游 404，见文件头差异 2）。
    ///
    /// # Errors
    ///
    /// [`StorageError::Invalid`]、[`StorageError::NotFound`]、[`StorageError::Unavailable`]。
    pub async fn delete_value(
        &self,
        installation_id: Id,
        scope_type: &str,
        scope_id: Id,
        key: &str,
    ) -> Result<(), StorageError> {
        validate_key(key)?;
        let deleted: PgQueryResult = sqlx::query(
            "DELETE FROM plugin_storage \
             WHERE installation_id = $1 AND scope_type = $2 AND scope_id = $3 AND key = $4",
        )
        .bind(installation_id.0)
        .bind(scope_type)
        .bind(scope_id.0)
        .bind(key)
        .execute(&self.pool)
        .await
        .map_err(|error| StorageError::Unavailable(format!("delete plugin storage: {error}")))?;

        if deleted.rows_affected() == 0 {
            return Err(StorageError::NotFound("storage key not found".to_string()));
        }
        Ok(())
    }
}

/// scope 名 → `scope_id`（上游 `ResolveStorageScope`）。
///
/// `user` scope 的 `scope_id` 是**用户 id**（不是安装 id、更不是 workspace id）：
/// 混用会让「A 成员的键」变成「B 成员的键」。
///
/// # Errors
///
/// [`StorageError::Invalid`]（未知 scope 名 ⇒ 上游 400，不是 404）。
pub fn resolve_scope(
    scope_type: &str,
    workspace_id: Id,
    user_id: Option<Id>,
) -> Result<Id, StorageError> {
    match scope_type {
        SCOPE_WORKSPACE => Ok(workspace_id),
        SCOPE_USER => user_id.ok_or_else(|| {
            StorageError::Invalid("this Plugin was not granted the storage:user scope".to_string())
        }),
        other => Err(StorageError::Invalid(format!(
            "storage scope must be {SCOPE_WORKSPACE:?} or {SCOPE_USER:?}, got {other:?}"
        ))),
    }
}

/// 入参校验（上游 `validateStorageKey`）。
///
/// # Errors
///
/// [`StorageError::Invalid`]（空 key）、[`StorageError::Quota`]（key 超 1024 字节）。
pub fn validate_key(key: &str) -> Result<(), StorageError> {
    if key.is_empty() {
        return Err(StorageError::Invalid("storage key is required".to_string()));
    }
    if key.len() > MAX_KEY_BYTES {
        return Err(StorageError::Quota(format!(
            "storage key exceeds {MAX_KEY_BYTES} bytes"
        )));
    }
    Ok(())
}

/// 配额判定（上游 `EnforceStorageQuota`）：纯函数，三个上限一起被单测钉住。
///
/// # Errors
///
/// [`StorageError::Quota`]（三条上限任一被撞）。
pub fn enforce_quota(usage: StorageUsageRow, value_bytes: usize) -> Result<(), StorageError> {
    if value_bytes > MAX_VALUE_BYTES {
        return Err(StorageError::Quota(format!(
            "storage value exceeds {MAX_VALUE_BYTES} bytes"
        )));
    }
    if usage.key_count + 1 > MAX_KEYS {
        return Err(StorageError::Quota(format!(
            "storage scope already holds the maximum of {MAX_KEYS} keys"
        )));
    }
    // `usize → i64` 用饱和转换而不是 `as`：`as` 在 64 位目标上可能回绕（clippy
    // `cast_possible_wrap`），回绕会把一次超配额写算成很小的正数而放行。
    let value_bytes = i64::try_from(value_bytes).unwrap_or(i64::MAX);
    if usage.total_bytes + value_bytes > MAX_TOTAL_BYTES {
        return Err(StorageError::Quota(format!(
            "storage scope exceeds its {MAX_TOTAL_BYTES} byte budget"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 事务内实体（供上面的三条语句复用；也开放给需要在同一事务里写的调用方）
// ---------------------------------------------------------------------------

/// 事务内读用量（`key` 被排除）。
///
/// # Errors
///
/// [`StorageError::Unavailable`]。
pub async fn usage_excluding_tx(
    tx: &mut Tx<'_>,
    installation_id: Id,
    scope_type: &str,
    scope_id: Id,
    key: &str,
) -> Result<StorageUsageRow, StorageError> {
    sqlx::query_as::<_, StorageUsageRow>(
        "SELECT COUNT(*)::bigint AS key_count, \
                COALESCE(SUM(octet_length(value)), 0)::bigint AS total_bytes \
         FROM plugin_storage \
         WHERE installation_id = $1 AND scope_type = $2 AND scope_id = $3 AND key <> $4",
    )
    .bind(installation_id.0)
    .bind(scope_type)
    .bind(scope_id.0)
    .bind(key)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| StorageError::Unavailable(format!("read plugin storage usage: {error}")))
}

/// 事务内插入一行（调用方已确认 UPDATE 影响 0 行）。
///
/// # Errors
///
/// [`StorageError::Unavailable`]。
pub async fn insert_value_tx(
    tx: &mut Tx<'_>,
    installation_id: Id,
    scope_type: &str,
    scope_id: Id,
    key: &str,
    value: &str,
) -> Result<Uuid, StorageError> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO plugin_storage (installation_id, scope_type, scope_id, key, value) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(installation_id.0)
    .bind(scope_type)
    .bind(scope_id.0)
    .bind(key)
    .bind(value)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| StorageError::Unavailable(format!("insert plugin storage: {error}")))?;
    Ok(id)
}

/// `map_sqlx_err` 的存储面包装：让调用方在混用两条错误通道时不必各自写一遍闭包。
///
/// # Errors
///
/// [`StorageError::Unavailable`]。
pub fn map_db_err(error: sqlx::Error) -> StorageError {
    let mapped = map_sqlx_err(error);
    StorageError::Unavailable(mapped.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(keys: i64, bytes: i64) -> StorageUsageRow {
        StorageUsageRow {
            key_count: keys,
            total_bytes: bytes,
        }
    }

    #[test]
    fn quota_bounds_match_upstream_constants() {
        // 三个常量逐字对齐上游（列上的 CHECK 也同值：key ≤1024、value ≤102400）。
        assert_eq!(MAX_KEY_BYTES, 1024);
        assert_eq!(MAX_VALUE_BYTES, 100 * 1024);
        assert_eq!(MAX_KEYS, 1000);
        assert_eq!(MAX_TOTAL_BYTES, 5 * 1024 * 1024);
    }

    #[test]
    fn enforce_quota_rejects_each_bound_independently() {
        // 单值超限（与 key_count / total 无关）。
        let err = enforce_quota(usage(0, 0), MAX_VALUE_BYTES + 1).unwrap_err();
        assert!(matches!(err, StorageError::Quota(ref m) if m.contains("value exceeds")));

        // 恰好等于上限 ⇒ 通过（上游是 `>` 不是 `>=`）。
        assert!(enforce_quota(usage(0, 0), MAX_VALUE_BYTES).is_ok());

        // 键数：已 1000 键 ⇒ `+1 > 1000` 拒。
        let err = enforce_quota(usage(MAX_KEYS, 0), 1).unwrap_err();
        assert!(matches!(err, StorageError::Quota(ref m) if m.contains("maximum of 1000 keys")));
        // 已 999 键 ⇒ 放行。
        assert!(enforce_quota(usage(MAX_KEYS - 1, 0), 1).is_ok());

        // 字节总量。
        let err = enforce_quota(usage(1, MAX_TOTAL_BYTES), 1).unwrap_err();
        assert!(matches!(err, StorageError::Quota(ref m) if m.contains("byte budget")));
        assert!(enforce_quota(usage(1, MAX_TOTAL_BYTES - 1), 1).is_ok());
    }

    #[test]
    fn validate_key_rejects_empty_and_oversized() {
        assert!(matches!(
            validate_key("").unwrap_err(),
            StorageError::Invalid(_)
        ));
        assert!(validate_key(&"k".repeat(MAX_KEY_BYTES)).is_ok());
        // 1025 字节 ⇒ 配额错（上游用的是 PluginErrorQuota，不是 Invalid）。
        assert!(matches!(
            validate_key(&"k".repeat(MAX_KEY_BYTES + 1)).unwrap_err(),
            StorageError::Quota(_)
        ));
        // 非 ASCII 按**字节**计（列上也是 octet_length）。
        assert!(matches!(
            validate_key(&"中".repeat(342)).unwrap_err(),
            StorageError::Quota(_)
        ));
        assert_eq!("中".repeat(341).len(), 1023);
        assert!(validate_key(&"中".repeat(341)).is_ok());
    }

    #[test]
    fn resolve_scope_maps_the_two_names_and_rejects_the_rest() {
        let ws = Id::new();
        let user = Id::new();
        assert_eq!(resolve_scope(SCOPE_WORKSPACE, ws, None).unwrap(), ws);
        assert_eq!(resolve_scope(SCOPE_WORKSPACE, ws, Some(user)).unwrap(), ws);
        // user scope 的 scope_id 是**用户** id（不是安装 id、不是 workspace id）。
        assert_eq!(resolve_scope(SCOPE_USER, ws, Some(user)).unwrap(), user);
        // user scope 但没有成员身份 ⇒ 明确错误（不是退化成 workspace）。
        assert!(matches!(
            resolve_scope(SCOPE_USER, ws, None).unwrap_err(),
            StorageError::Invalid(_)
        ));
        // 未知 scope 名 ⇒ 400 形态的 Invalid（不是 404）。
        assert!(matches!(
            resolve_scope("global", ws, Some(user)).unwrap_err(),
            StorageError::Invalid(_)
        ));
    }
}

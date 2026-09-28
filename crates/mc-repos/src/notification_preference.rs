//! `NotificationPreferenceRepo` —— `notification_preference` 表的读写（**写者 M9-5** / `LUM-1820`）。
//!
//! # 上游面（`internal/handler/notification_preference.go`，172 行）
//!
//! 本文件只做**仓储**：三条 SQL 与上游 `server/pkg/db/queries/notification_preference.sql`
//! 逐字对齐（`GetNotificationPreference` / `UpsertNotificationPreference` /
//! `PatchNotificationPreference`）。**第四条** `ListNotificationPreferencesByUsers` 在上游是
//! 投递批注用的内部查询，**不在**本波任何路由上 ⇒ 本文件**不**实现它。
//!
//! | 上游查询 | 本文件的入口 | 语义 |
//! | --- | --- | --- |
//! | `GetNotificationPreference` | [`NotificationPreferenceRepo::get`] | **无行** ⇒ `Ok(None)` |
//! | `UpsertNotificationPreference` | [`NotificationPreferenceRepo::upsert`] | `preferences = $3`（**整体替换**） |
//! | `PatchNotificationPreference` | [`NotificationPreferenceRepo::patch`] | `preferences = preferences \|\| EXCLUDED.preferences`（**原子合并**） |
//!
//! # 三条纪律
//!
//! 1. **没有行 ≠ 全 `all`**：未设置过偏好 ⇒ 上游返回 `preferences: {}`
//!    （**空对象**），不是默认表 ⇒ 本 Repo 的 [`get`] 用 `Option<`[`PreferenceRow`]`>` 区分
//!    「无行」与「有行但空 map」，**不** `unwrap_or_default()` 把两者抹平；
//! 2. **`GET` 不写行**：读面**不得**为了「顺手初始化」插一行（那会让「从没设过」变成
//!    「设过但全默认」，两者的客户端行为不同）⇒ 本文件**只有** [`get`](Self::get) 一个读口，
//!    里面**没有**任何 `INSERT`；
//! 3. **`workspace_id` 来自中间件**：客户端走私的 `workspace_id` 必须被覆盖（`docs/62` §2.7 第 7 条）
//!    ⇒ 两个写口都要求调用方**显式**传 `(workspace_id, user_id)`，Repo 不从别的途径取。
//!
//! # `PUT` 与 `PATCH` 的差别是**这一波的核心**（不是同一份 SQL）
//!
//! 上游两处注释逐字：`UpdateNotificationPreferences`（`PUT`）「preserves the original
//! replace-all PUT contract」；`PatchNotificationPreferences`（`PATCH`）「atomically merges only
//! the supplied keys. This prevents stale tabs or devices from replacing unrelated mute settings.」
//! ⇒ 同一个客户端同时开着两个标签页时，`PATCH` 只改传来的那几个键；把它错写成 upsert
//! 会静默把别的静音设置清掉 ⇒ **两条 SQL 必须分开**，本文件因此给出两个入口。
//!
//! # 词表与校验**不在本文件**
//!
//! 三处（`GROUP` / `VALUE` / 错误文本）都在 [`mc_core::notification`]，本 Repo 只负责读写
//! 那一列 JSONB ⇒ **不**在这里重写一份 7×2 词表（重写就是第二处会漂移的判据）。

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use mc_core::Id;
use sqlx::FromRow;
use uuid::Uuid;

use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

/// `preferences` 那列的 JSONB 解析。
///
/// ⚠️ 上游两处都逐字写了「解析失败 ⇒ 空 map」而不是 500：
/// `GetNotificationPreferences` 的 `if err := json.Unmarshal(pref.Preferences, &prefs); err != nil { prefs = map[string]string{} }`
/// 与 `writeNotificationPreferenceResponse` 的同一段。⇒ 本行形状用
/// `serde_json::from_value(..).unwrap_or_default()` 复刻那一档（**不**把脏数据变成 500，
/// 那会让一个只读端点因为一行历史脏数据整体挂掉）。
fn preferences_of(value: serde_json::Value) -> BTreeMap<String, String> {
    serde_json::from_value(value).unwrap_or_default()
}

/// `notification_preference` 行（`preferences` 已从 JSONB 解成 map）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferenceRow {
    /// 主键。
    pub id: Uuid,
    /// 工作区（与 `user_id` 一起是唯一键）。
    pub workspace_id: Uuid,
    /// 用户。
    pub user_id: Uuid,
    /// 分组 → 取值（**只含设过的**）。
    pub preferences: BTreeMap<String, String>,
    /// 最后一次写入的时刻。
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct RawPreferenceRow {
    id: Uuid,
    workspace_id: Uuid,
    user_id: Uuid,
    preferences: serde_json::Value,
    updated_at: DateTime<Utc>,
}

impl From<RawPreferenceRow> for PreferenceRow {
    fn from(row: RawPreferenceRow) -> Self {
        Self {
            id: row.id,
            workspace_id: row.workspace_id,
            user_id: row.user_id,
            preferences: preferences_of(row.preferences),
            updated_at: row.updated_at,
        }
    }
}

/// 上游 `GetNotificationPreference`：`WHERE workspace_id = $1 AND user_id = $2`。
const SQL_GET: &str = "SELECT id, workspace_id, user_id, preferences, updated_at \
                       FROM notification_preference WHERE workspace_id = $1 AND user_id = $2";

/// 上游 `UpsertNotificationPreference`：`preferences = $3`（**整体替换**，`PUT` 用）。
const SQL_UPSERT: &str = "INSERT INTO notification_preference (workspace_id, user_id, preferences) \
                          VALUES ($1, $2, $3) \
                          ON CONFLICT (workspace_id, user_id) \
                          DO UPDATE SET preferences = $3, updated_at = now() \
                          RETURNING id, workspace_id, user_id, preferences, updated_at";

/// 上游 `PatchNotificationPreference`：`||` 合并（`PATCH` 用，**不**整体替换）。
const SQL_PATCH: &str = "INSERT INTO notification_preference (workspace_id, user_id, preferences) \
                         VALUES ($1, $2, $3) \
                         ON CONFLICT (workspace_id, user_id) \
                         DO UPDATE SET \
                             preferences = notification_preference.preferences || EXCLUDED.preferences, \
                             updated_at = now() \
                         RETURNING id, workspace_id, user_id, preferences, updated_at";

/// `notification_preference` 表访问（**M9-5**）。
#[derive(Clone)]
pub struct NotificationPreferenceRepo {
    db: Db,
}

impl NotificationPreferenceRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `GetNotificationPreference`。
    ///
    /// **无行 ⇒ `Ok(None)`**（纪律 1：不是空 map，也不是 `RepoError::NotFound`）——
    /// 读面要区分「从没设过」与「设过但恰好是空的」，所以这里**不**把无行折成错误。
    /// **本方法不写库**（纪律 2）。
    pub async fn get(&self, workspace_id: Id, user_id: Id) -> Result<Option<PreferenceRow>> {
        let row = sqlx::query_as::<_, RawPreferenceRow>(SQL_GET)
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(row.map(PreferenceRow::from))
    }

    /// 上游 `UpsertNotificationPreference`（`PUT`）：**整体替换** `preferences`。
    pub async fn upsert(
        &self,
        workspace_id: Id,
        user_id: Id,
        preferences: &BTreeMap<String, String>,
    ) -> Result<PreferenceRow> {
        let row = sqlx::query_as::<_, RawPreferenceRow>(SQL_UPSERT)
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .bind(serde_json::to_value(preferences).map_err(|e| RepoError::Db(e.to_string()))?)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(PreferenceRow::from(row))
    }

    /// 上游 `PatchNotificationPreference`（`PATCH`）：**只**合并传来的键。
    ///
    /// 冲突时走 `preferences || EXCLUDED.preferences`（Postgres 的 `jsonb ||` 是**右胜**的浅合并）
    /// ⇒ 一个陈旧标签页只能改自己那几个键，**碰不到**别的键。
    pub async fn patch(
        &self,
        workspace_id: Id,
        user_id: Id,
        preferences: &BTreeMap<String, String>,
    ) -> Result<PreferenceRow> {
        let row = sqlx::query_as::<_, RawPreferenceRow>(SQL_PATCH)
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .bind(serde_json::to_value(preferences).map_err(|e| RepoError::Db(e.to_string()))?)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(PreferenceRow::from(row))
    }
}

impl RepoWithDb for NotificationPreferenceRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn map_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// 纪律 1：脏 JSONB 折成**空 map** 而不是错误（上游 `unmarshal` 失败那一档逐字）。
    #[test]
    fn dirty_jsonb_degrades_to_an_empty_map_not_an_error() {
        // 上游的 `map[string]string` 解码失败 ⇒ `prefs = map[string]string{}`，仍然 200。
        assert_eq!(preferences_of(serde_json::json!({"a": 1})), BTreeMap::new());
        assert_eq!(preferences_of(serde_json::json!(["not", "an", "object"])), BTreeMap::new());
        assert_eq!(preferences_of(serde_json::json!("nope")), BTreeMap::new());
        // 正常形状逐字解析。
        assert_eq!(
            preferences_of(serde_json::json!({"comments": "all"})),
            map_of(&[("comments", "all")])
        );
        // 空对象是**合法**值，不是「解析失败」—— 两者在响应上同形，但本测试钉住区别。
        assert_eq!(preferences_of(serde_json::json!({})), BTreeMap::new());
    }

    /// 三条 SQL 的形状钉住：`GET` 无 `INSERT`（纪律 2），`PUT` 是替换、`PATCH` 是 `||` 合并。
    ///
    /// 这是**静态**断言（`sqlx` 未开 `query!` 宏编译 ⇒ 读不出 SQL 语义），
    /// 真正的落库语义在 `routes/notification_preferences.rs` 的真库用例里直读那一列。
    #[test]
    fn the_three_statements_keep_put_replace_and_patch_merge_apart() {
        // 读面**不**含任何写语句。
        assert!(!SQL_GET.to_ascii_uppercase().contains("INSERT"));
        assert!(!SQL_GET.to_ascii_uppercase().contains("UPDATE"));
        // 两条 upsert 都吃 `(workspace_id, user_id)` 唯一约束。
        for sql in [SQL_UPSERT, SQL_PATCH] {
            assert!(sql.contains("ON CONFLICT (workspace_id, user_id)"), "{sql}");
            assert!(sql.contains("updated_at = now()"), "{sql}");
        }
        // `PUT` = 整体替换：**不**含 `||`。
        assert!(!SQL_UPSERT.contains("||"));
        assert!(SQL_UPSERT.contains("DO UPDATE SET preferences = $3"));
        // `PATCH` = 浅合并，且**带表名限定**（否则 `||` 的左值歧义）。
        assert!(SQL_PATCH.contains("notification_preference.preferences || EXCLUDED.preferences"));
        // 三条都 `RETURNING` 整行（handler 拿它当响应体，不回读）。
        for sql in [SQL_GET, SQL_UPSERT, SQL_PATCH] {
            assert!(sql.contains("id, workspace_id, user_id, preferences, updated_at"), "{sql}");
        }
    }

    // -----------------------------------------------------------------------
    // 真库（门 ⑥）：`#[ignore]` + `MULTICA_TEST_DATABASE_URL`。
    //
    // 判据纪律：🔴 **直读 `notification_preference.preferences` 那一列**，
    // **不**拿 handler 响应体当「写进去了」的证据（响应体是本 Repo 自己
    // `RETURNING` 出来的，同源 ⇒ 假绿风险）。
    // -----------------------------------------------------------------------

    /// 一个 workspace + owner（真库）。
    async fn seed(db: &Db) -> (Id, Id) {
        let tag = Uuid::new_v4().simple().to_string();
        let workspace: Uuid =
            sqlx::query_scalar("INSERT INTO workspace(name, slug) VALUES ($1, $2) RETURNING id")
                .bind(format!("itest-m95-{tag}"))
                .bind(format!("itest-m95-{tag}"))
                .fetch_one(db.pool())
                .await
                .expect("insert workspace");
        let user: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#,
        )
        .bind(format!("itest-m95-{tag}"))
        .bind(format!("itest-m95-{tag}@example.com"))
        .fetch_one(db.pool())
        .await
        .expect("insert user");
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'owner')")
            .bind(workspace)
            .bind(user)
            .execute(db.pool())
            .await
            .expect("insert member");
        (Id::from(workspace), Id::from(user))
    }

    async fn test_db() -> Option<Db> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        Some(Db::connect(&url, 4, 1).await.expect("connect"))
    }

    /// **直读**那一列（`preferences` 的 JSONB 原文）。
    async fn read_column(db: &Db, workspace_id: Id, user_id: Id) -> Option<serde_json::Value> {
        sqlx::query_scalar(
            "SELECT preferences FROM notification_preference \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .fetch_optional(db.pool())
        .await
        .expect("read preferences column")
    }

    /// 纪律 1 + 2：没设过 ⇒ `get` 返 `Ok(None)`，且 **`SELECT` 查不到行**（`GET` 不写行）。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn a_user_with_no_row_gets_none_and_get_does_not_create_one() {
        let Some(db) = test_db().await else {
            println!("skip a_user_with_no_row_gets_none_and_get_does_not_create_one: no env");
            return;
        };
        let (workspace_id, user_id) = seed(&db).await;
        let repo = NotificationPreferenceRepo::new(db.clone());

        assert!(
            repo.get(workspace_id, user_id).await.expect("get") .is_none(),
            "never-set preferences must read as None, not an empty map"
        );
        // `GET` **不写行**：直读那一列仍然是「没有行」。
        assert!(
            read_column(&db, workspace_id, user_id).await.is_none(),
            "GET must not insert a row"
        );
    }

    /// `PUT` = **整体替换**（逐字钉住两条 SQL 的差别）。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn put_replaces_the_whole_map() {
        let Some(db) = test_db().await else {
            println!("skip put_replaces_the_whole_map: no env");
            return;
        };
        let (workspace_id, user_id) = seed(&db).await;
        let repo = NotificationPreferenceRepo::new(db.clone());

        let first = map_of(&[("assignments", "all"), ("mentions", "muted")]);
        repo.upsert(workspace_id, user_id, &first).await.expect("upsert");
        // 只写一个键 ⇒ 另一个键**被抹掉**（这就是 replace-all 契约）。
        let second = map_of(&[("comments", "all")]);
        repo.upsert(workspace_id, user_id, &second).await.expect("upsert again");

        // 🔴 直读那一列：只剩 `comments`。
        assert_eq!(
            read_column(&db, workspace_id, user_id).await.expect("row"),
            serde_json::json!({"comments": "all"}),
            "PUT must replace the whole map, not merge"
        );
        assert_eq!(
            repo.get(workspace_id, user_id).await.expect("get").expect("row").preferences,
            second
        );
    }

    /// `PATCH` = **只合并**传来的键（上游防「陈旧标签页改掉别的静音设置」的那一半）。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn patch_merges_only_the_supplied_keys() {
        let Some(db) = test_db().await else {
            println!("skip patch_merges_only_the_supplied_keys: no env");
            return;
        };
        let (workspace_id, user_id) = seed(&db).await;
        let repo = NotificationPreferenceRepo::new(db.clone());

        repo.upsert(workspace_id, user_id, &map_of(&[("assignments", "all"), ("mentions", "muted")]))
            .await
            .expect("upsert");
        // 只改 `mentions` ⇒ `assignments` **必须**还在。
        repo.patch(workspace_id, user_id, &map_of(&[("mentions", "all")]))
            .await
            .expect("patch");

        // 🔴 直读那一列。
        assert_eq!(
            read_column(&db, workspace_id, user_id).await.expect("row"),
            serde_json::json!({"assignments": "all", "mentions": "all"}),
            "PATCH must merge, leaving untouched keys alone"
        );
    }

    /// 键 = `(workspace_id, user_id)`：两个用户互不干扰（直读两行）。
    #[tokio::test]
    #[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
    async fn preferences_are_keyed_by_workspace_and_user() {
        let Some(db) = test_db().await else {
            println!("skip preferences_are_keyed_by_workspace_and_user: no env");
            return;
        };
        let (workspace_id, user_a) = seed(&db).await;
        let user_b: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ($1, $2) RETURNING id"#,
        )
        .bind("itest-m95-other")
        .bind(format!("itest-m95-other-{}@example.com", Uuid::new_v4().simple()))
        .fetch_one(db.pool())
        .await
        .expect("insert second user");
        sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, 'member')")
            .bind(workspace_id.as_uuid())
            .bind(user_b)
            .execute(db.pool())
            .await
            .expect("insert member");
        let user_b = Id::from(user_b);

        let repo = NotificationPreferenceRepo::new(db.clone());
        repo.upsert(workspace_id, user_a, &map_of(&[("comments", "muted")]))
            .await
            .expect("upsert a");
        repo.upsert(workspace_id, user_b, &map_of(&[("comments", "all")]))
            .await
            .expect("upsert b");

        assert_eq!(
            read_column(&db, workspace_id, user_a).await.expect("row a"),
            serde_json::json!({"comments": "muted"})
        );
        assert_eq!(
            read_column(&db, workspace_id, user_b).await.expect("row b"),
            serde_json::json!({"comments": "all"})
        );
    }
}

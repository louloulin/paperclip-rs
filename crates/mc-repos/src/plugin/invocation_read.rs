//! `plugin_invocation` 的列表/详情读面（调用历史 → 「这个 hook 为什么在失败」）。
//!
//! - **写者**：M6-6（**W**；`docs/57` §3.2）。写侧在 `hook.rs`（M6-8）—— 本文件**只读**
//!   （没有 `INSERT` / `UPDATE` / `DELETE`）。
//! - **上游**：`internal/handler/plugin_hook.go:121` `ListPluginInvocations`
//!   （+ `pkg/db/queries/plugin.sql` 的 `ListPluginInvocations`）。
//! - **13 列**（`362` + `399`）：`id, installation_id, workspace_id, hook_key, trigger, status,
//!   event_type, attempt, latency_ms, error, created_at, delivery_id, planned_at`。
//! - **三条硬语义**：
//!   1. `trigger` 五态（`399` 补 `schedule`，`402` 才 `VALIDATE` 约束）、`status` 四态
//!      （`ok/failed/timeout/refused`）——**行结构里读成 `String`**，折算成封闭枚举是
//!      route 层的事（`plugin/mod.rs` 的约定；本模块不引 `mc_core::plugin` 的枚举）；
//!   2. 表里**没有**请求/响应体（`362` 的注释就是这条）：读面能给的是「发生了什么、几次、
//!      多慢」，不是「发了什么」——所以本文件一个 `content` 字段都没有；
//!   3. 排序是 `created_at DESC`（上游逐字），本文件在它后面**追加 `id DESC` 作次键**：
//!      本片加了 `OFFSET`（见下），而 `created_at` 默认 `now()`、同事务插入的行**时间戳完全
//!      相同**，只按它排会让分页在并列行上跳过或重复（上游没有分页所以看不见这个问题）。
//!      已登记 `docs/32` §9。
//! - **偏离（已登记 `docs/32` §9）**：上游 `LIMIT 100` 硬编码、**没有 offset、也没有查询参数**；
//!   本片的 `DoD` 明确要求分页边界（空页 / 越界 offset）可测，故暴露 `limit` / `offset`
//!   两个参数（缺省 `100` / `0` 与上游行为逐字相同，上限由 route 层钳到 500）。多出来的
//!   只是**能力**，不是改默认行为。
//! - **本仓约定**：裸 `Uuid` + `#[derive(sqlx::FromRow)]`、`map_sqlx_err`（`plugin/mod.rs`）。
//! - **不做什么**：不做保留期清理（上游是 TTL sweep，不在本片）、不做详情路由
//!   （上游 4 条路由里没有 `invocations/:invocationId`，所以这里也不留取单行的死代码）。
//!
//! **状态：M6-6 已落地（LUM-1671）**。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::Result;

/// 上游硬编码的页大小（`ListPluginInvocationsParams.Limit: 100`）——本片保留为缺省值。
pub const DEFAULT_LIMIT: i64 = 100;

/// route 层允许的最大页大小。上游没有这个参数，也就没有上限；本仓加了参数就必须有上限，
/// 否则 `?limit=1000000` 能把一次请求变成全表扫描。
pub const MAX_LIMIT: i64 = 500;

/// `plugin_invocation` 一行的**读**投影（13 列，列序与 `362` + `399` 一致）。
///
/// `trigger` / `status` 保持 `String`：折算成封闭枚举由 route 层做（`plugin/mod.rs`）。
#[derive(Debug, Clone, FromRow)]
pub struct PluginInvocationRow {
    pub id: Uuid,
    pub installation_id: Uuid,
    pub workspace_id: Uuid,
    pub hook_key: String,
    /// `trigger IN ('ui','manual','event','agent','schedule')`。
    pub trigger: String,
    /// `status IN ('ok','failed','timeout','refused')`。
    pub status: String,
    /// 只有 `trigger='event'` 才有值（其余为 `NULL`）。
    pub event_type: Option<String>,
    /// `CHECK attempt BETWEEN 1 AND 10`。
    pub attempt: i32,
    /// `CHECK latency_ms >= 0`；**不是**时长类型，就是整数毫秒。
    pub latency_ms: i32,
    /// 宿主自己写的失败描述（≤500 字符，**永不是响应体**）。
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    /// 幂等/对账键（`399` 加的）。
    pub delivery_id: Option<String>,
    /// 计划投递时间（`399` 加的），只有 `trigger='schedule'` 才填。
    pub planned_at: Option<DateTime<Utc>>,
}

impl PluginInvocationRow {
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    pub fn installation_id(&self) -> Id {
        Id::from(self.installation_id)
    }

    pub fn workspace_id(&self) -> Id {
        Id::from(self.workspace_id)
    }
}

/// `plugin_invocation` 的只读仓储（`plugin/mod.rs` 的约定：`new(&state.db)` 现构造）。
pub struct PluginInvocationRepo {
    db: Db,
}

impl PluginInvocationRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `ListPluginInvocations`：按安装收窄 + `created_at DESC`，页大小 100。
    ///
    /// # Errors
    ///
    /// 库错折成 [`crate::RepoError::Db`]。
    pub async fn list(
        &self,
        installation_id: Id,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PluginInvocationRow>> {
        let rows = sqlx::query_as::<_, PluginInvocationRow>(
            "SELECT id, installation_id, workspace_id, hook_key, trigger, status, event_type, \
             attempt, latency_ms, error, created_at, delivery_id, planned_at \
             FROM plugin_invocation \
             WHERE installation_id = $1 \
             ORDER BY created_at DESC, id DESC \
             LIMIT $2 OFFSET $3",
        )
        .bind(installation_id.0)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// PG 集成测试（需要真库）
//
// `plugin_invocation` 在迁移里**没有任何外键**（`362` 纯建表），所以夹具不需要 workspace /
// installation 行 —— 这也让「空页 / 越界 offset」两条边界能只靠插入本表验证。
// ---------------------------------------------------------------------------
#[cfg(test)]
mod db_tests {
    use super::*;

    async fn setup() -> Option<(Db, Id)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        Some((db, Id::from(Uuid::new_v4())))
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

    /// 插一行调用记录；`created_at` 显式给值，好让排序断言不依赖插入速度。
    async fn insert(db: &Db, installation_id: Id, hook_key: &str, created_at: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO plugin_invocation \
             (installation_id, workspace_id, hook_key, trigger, status, attempt, latency_ms, created_at) \
             VALUES ($1, $2, $3, 'manual', 'failed', 1, 12, $4) RETURNING id",
        )
        .bind(installation_id.0)
        .bind(Uuid::new_v4())
        .bind(hook_key)
        .bind(created_at)
        .fetch_one(db.pool())
        .await
        .expect("insert invocation")
    }

    async fn cleanup(db: &Db, installation_id: Id) {
        let _ = sqlx::query("DELETE FROM plugin_invocation WHERE installation_id = $1")
            .bind(installation_id.0)
            .execute(db.pool())
            .await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_list_is_newest_first_and_scoped_to_the_installation() {
        let (db, installation) = fixture!();
        let repo = PluginInvocationRepo::new(db.clone());
        let other = Id::from(Uuid::new_v4());

        let older = insert(&db, installation, "sync", "2024-01-01T00:00:00Z").await;
        let newer = insert(&db, installation, "notify", "2024-01-02T00:00:00Z").await;
        // 另一个安装的行绝不能出现（跨安装泄露调用历史就是泄露别的租户的行为）。
        insert(&db, other, "sync", "2024-01-03T00:00:00Z").await;

        let rows = repo.list(installation, 100, 0).await.expect("list");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, newer);
        assert_eq!(rows[1].id, older);
        assert_eq!(rows[0].hook_key, "notify");
        assert_eq!(rows[0].status, "failed");
        assert_eq!(rows[0].attempt, 1);
        assert_eq!(rows[0].latency_ms, 12);
        assert_eq!(rows[0].trigger, "manual");
        assert_eq!(rows[0].event_type, None);
        assert_eq!(rows[0].delivery_id, None);
        assert_ne!(rows[0].workspace_id(), rows[1].workspace_id());

        cleanup(&db, installation).await;
        cleanup(&db, other).await;
    }

    /// `DoD`：**空页** —— 安装没有任何调用时返回空表（不是 404、不是 `null`）。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_list_empty_page_for_an_installation_without_invocations() {
        let (db, installation) = fixture!();
        let repo = PluginInvocationRepo::new(db.clone());

        assert!(repo
            .list(installation, 100, 0)
            .await
            .expect("list")
            .is_empty());

        cleanup(&db, installation).await;
    }

    /// `DoD`：**越界 offset** —— offset 超出总数返回空页（`OFFSET` 语义，不报错）。
    ///
    /// 同时钉住「offset 真的透传到 SQL」：`offset=1` 必须丢掉最新那一行 —— 否则
    /// 「越界返空」可以靠「offset 被忽略」蒙对。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_list_skips_by_offset_and_returns_empty_past_the_end() {
        let (db, installation) = fixture!();
        let repo = PluginInvocationRepo::new(db.clone());

        let oldest = insert(&db, installation, "sync", "2024-01-01T00:00:00Z").await;
        insert(&db, installation, "sync", "2024-01-02T00:00:00Z").await;
        let newest = insert(&db, installation, "sync", "2024-01-03T00:00:00Z").await;

        let page = repo.list(installation, 1, 0).await.expect("first page");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, newest);

        let page = repo.list(installation, 10, 1).await.expect("second page");
        assert_eq!(page.len(), 2);
        assert_eq!(page[1].id, oldest);

        let page = repo.list(installation, 10, 3).await.expect("offset at end");
        assert!(page.is_empty());
        let page = repo
            .list(installation, 10, 999)
            .await
            .expect("offset past end");
        assert!(page.is_empty());

        cleanup(&db, installation).await;
    }
}

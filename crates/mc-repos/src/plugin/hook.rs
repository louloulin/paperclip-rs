//! hook 引擎与 job 的**写**侧：`plugin_hook_schedule` + `plugin_invocation`。
//!
//! - **写者**：M6-8（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/service/plugin_hook*` + `internal/service/plugin_schedule.go`
//!   （`reconcilePluginHookSchedules` / `setPluginHookSchedulesEnabled`）+ `internal/handler/plugin.go`
//!   的 hook 段 + `scheduler/jobs_plugin_hook.go` 的读取面。
//! - **12 列**（`399`）：`id, installation_id, workspace_id, hook_key, cron_expression, timezone,
//!   generation, activated_at, next_run_at, enabled, created_at, updated_at`。
//! - **三条硬语义**：
//!   1. `generation`（UUID）是**换代令牌**：改 cron / 停用 / 重装都要换一代 —— 老一代排出来的
//!      调用落到 `plugin_invocation` 时必须被判无效（不能打到新配置上）；
//!   2. `plugin_invocation.trigger='schedule'` 是 `399` 补的第五态（`362` 只允许四态；
//!      `402` 才 `VALIDATE` 约束）—— 写这个取值是合法的，别按老 CHECK 去绕；
//!   3. `delivery_id`（可空，1..128）与 `planned_at` 是幂等/对账用的：同一次计划投递重复执行时
//!      用 `delivery_id` 去重，**不要**靠 `created_at` 猜。
//! - **并发**：`attempt 1..10`，重试预算写在列上；租约/去重不要自创，走
//!   `mc_scheduler` + `mc_repos::scheduler` 的 `sys_cron_executions`（M5-7 已落地）。
//! - **不做什么**：不做 cron 解析（`mc-scheduler` 的 spec）、不做 HTTP 签名（`mc-plugin-host`）。
//!
//! # SQL 逐条对应上游（`pkg/db/queries/plugin.sql`）
//!
//! | 本文件 | 上游 query | 备注 |
//! | --- | --- | --- |
//! | [`HookScheduleRepo::list_enabled`] | `ListEnabledPluginHookSchedules` | **不按** `next_run_at` 过滤（它是展示列，见迁移 `399` 注释），`ORDER BY id ASC` |
//! | [`HookScheduleRepo::get`] | `GetPluginHookSchedule` | 未命中 ⇒ `Ok(None)` |
//! | [`HookScheduleRepo::list_by_installation`] | `ListPluginHookSchedulesByInstallation` | `ORDER BY hook_key ASC` |
//! | [`HookScheduleRepo::advance_next_run`] | `UpdatePluginHookScheduleNextRun` | `WHERE id AND generation AND enabled` 三连守卫 ⇒ 换代/停用后写 0 行 |
//! | [`HookScheduleRepo::record`] | `CreatePluginInvocation` | `id` 由调用方**在出站请求之前**分配（上游同） |
//! | [`HookScheduleRepo::count_recent`] | `CountRecentPluginInvocations` / `CountRecentPluginFailures` | 限流（120/分）与熔断（5 次/5 分）共用一个查询 |
//! | [`reconcile_tx`] | `Create/UpdatePluginHookScheduleDefinition/DeletePluginHookSchedule` | 安装/升级时把投影对齐到 manifest |
//! | [`set_enabled_tx`] | `DisablePluginHookSchedules` / `ReactivatePluginHookSchedule` | 启停时换一代（停用期间的发生**永不**补发） |
//!
//! **两条口径与上游逐字对齐、别自作聪明**：
//!
//! 1. **`delivery_id` 不是行级唯一键**：上游一个计划投递重试三次就写三行（`attempt` 递增），
//!    `delivery_id` 让**接收方**能把它们认成同一次投递。行级唯一会吃掉重试行 ⇒ 限流/熔断的
//!    计数与「这个端点为什么在失败」都会失真。幂等由 `sys_cron_executions` 的
//!    `(job_name, scope_kind, scope_id, plan_time)` 唯一键保证（M5-7 的内核），不在本文件。
//! 2. **`plugin_hook_schedule` 的写者只有本文件**：M6-5 的 `install/**` 只**读**日程（DTO 的
//!    `hooks[].schedule`），安装/升级/启停三处的对齐由它调用本文件的 [`reconcile_tx`] /
//!    [`set_enabled_tx`]（M6-5 登记在 `docs/32` §9.6 的跨片缺口，M6-8 回填）。
//!
//! **状态：M6-8 已落地**。
//!
//! 行预算（门 ⑩）：预计 400 行以内。

use chrono::{DateTime, Utc};
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use crate::plugin::installation::{InstallationRow, Tx};
use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `plugin_hook_schedule` 的列投影（12 列，顺序与迁移 `399` 一致）。
pub const SCHEDULE_COLUMNS: &str = "id, installation_id, workspace_id, hook_key, cron_expression, \
                                    timezone, generation, activated_at, next_run_at, enabled, \
                                    created_at, updated_at";

/// 一行 `plugin_hook_schedule`。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct HookScheduleRow {
    pub id: Uuid,
    pub installation_id: Uuid,
    pub workspace_id: Uuid,
    /// `CHECK char_length BETWEEN 1 AND 128`。
    pub hook_key: String,
    /// 五字段标准 cron（`CHECK 1..255`）。
    pub cron_expression: String,
    /// IANA 时区名（`CHECK 1..255`）。
    pub timezone: String,
    /// 换代令牌：作用域 id 的一半（见 [`HookScheduleRow::scope_id`]）。
    pub generation: Uuid,
    pub activated_at: DateTime<Utc>,
    /// **展示用**投影。派发正确性由 `cron + activated_at + sys_cron_executions` 恢复，
    /// 所以它是陈旧或 `NULL` 都不影响调度。
    pub next_run_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl HookScheduleRow {
    #[must_use]
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    #[must_use]
    pub fn installation_id(&self) -> Id {
        Id::from(self.installation_id)
    }

    #[must_use]
    pub fn workspace_id(&self) -> Id {
        Id::from(self.workspace_id)
    }

    #[must_use]
    pub fn generation(&self) -> Id {
        Id::from(self.generation)
    }

    /// 调度作用域 id：`<schedule_id>:<generation>`（上游 `pluginHookScheduleScopeID`）。
    ///
    /// **换代即换作用域**：改 cron / 停用重开都会换 `generation`，于是上一代的
    /// `sys_cron_executions` 行成为不可变历史，新时间线从 `activated_at` 重新起算 ——
    /// 「停机期间错过的格子」因此**永不**被补发。
    #[must_use]
    pub fn scope_id(&self) -> String {
        format!("{}:{}", self.id, self.generation)
    }
}

/// 一次调用的待写行（上游 `CreatePluginInvocationParams`）。
#[derive(Debug, Clone)]
pub struct NewInvocation<'a> {
    /// **出站请求之前**分配的 id（上游同）：接收方在调用失败时也能对账到这一行。
    pub id: Uuid,
    pub installation_id: Id,
    pub workspace_id: Id,
    /// `CHECK char_length BETWEEN 1 AND 128`。
    pub hook_key: &'a str,
    /// `ui` / `manual` / `event` / `agent` / `schedule`。
    pub trigger: &'a str,
    /// `ok` / `failed` / `timeout` / `refused`。
    pub status: &'a str,
    /// 仅 `event` 触发时有值。
    pub event_type: Option<&'a str>,
    /// `CHECK BETWEEN 1 AND 10`。
    pub attempt: i32,
    /// `CHECK >= 0`。
    pub latency_ms: i32,
    /// 宿主自己的失败描述（≤500 字符，**永不是响应体**）。
    pub error: Option<&'a str>,
    /// 同一次计划投递重试间稳定（`None` = 非计划触发）。
    pub delivery_id: Option<&'a str>,
    /// cron 的**计划发生时刻**（不是真实尝试时刻）。
    pub planned_at: Option<DateTime<Utc>>,
}

/// 一个要落到 `plugin_hook_schedule` 的日程（manifest 的投影，上游 `CreatePluginHookSchedule`）。
///
/// `next_run_at` 由**调用方**算好（cron 解析在 `mc-autopilot::cron`，本 crate 不引它 ——
/// 与 `plugin/skill.rs::PluginSkillInput` 同样的分工）。
#[derive(Debug, Clone)]
pub struct HookScheduleInput {
    pub hook_key: String,
    pub cron_expression: String,
    pub timezone: String,
    /// 安装已停用时调用方传 `None`（上游：停用的安装没有有意义的「下一次」）。
    pub next_run_at: Option<DateTime<Utc>>,
}

/// `plugin_hook_schedule` + `plugin_invocation` 的仓储。
pub struct HookScheduleRepo {
    db: Db,
}

impl HookScheduleRepo {
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `ListEnabledPluginHookSchedules`：**启用的**日程（不按 `next_run_at` 过滤）。
    ///
    /// # Errors
    ///
    /// 库错折成 [`crate::RepoError::Db`]。
    pub async fn list_enabled(&self) -> Result<Vec<HookScheduleRow>> {
        sqlx::query_as::<_, HookScheduleRow>(&format!(
            "SELECT {SCHEDULE_COLUMNS} FROM plugin_hook_schedule WHERE enabled ORDER BY id ASC"
        ))
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `GetPluginHookSchedule`：按 id 取一行，未命中 ⇒ `Ok(None)`。
    ///
    /// # Errors
    ///
    /// 库错折成 [`crate::RepoError::Db`]。
    pub async fn get(&self, id: Id) -> Result<Option<HookScheduleRow>> {
        sqlx::query_as::<_, HookScheduleRow>(&format!(
            "SELECT {SCHEDULE_COLUMNS} FROM plugin_hook_schedule WHERE id = $1"
        ))
        .bind(id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `ListPluginHookSchedulesByInstallation`（`ORDER BY hook_key ASC`）。
    ///
    /// # Errors
    ///
    /// 库错折成 [`crate::RepoError::Db`]。
    pub async fn list_by_installation(&self, installation_id: Id) -> Result<Vec<HookScheduleRow>> {
        sqlx::query_as::<_, HookScheduleRow>(&format!(
            "SELECT {SCHEDULE_COLUMNS} FROM plugin_hook_schedule \
             WHERE installation_id = $1 ORDER BY hook_key ASC"
        ))
        .bind(installation_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 上游 `UpdatePluginHookScheduleNextRun`：推进**展示用**的 `next_run_at`。
    ///
    /// 守卫是 `id AND generation AND enabled` 三连：换代或停用之后旧持有者写 0 行 ——
    /// 这正是「老一代的收尾不会碰到新配置」的实现点。返回影响行数。
    ///
    /// # Errors
    ///
    /// 库错折成 [`crate::RepoError::Db`]。
    pub async fn advance_next_run(
        &self,
        id: Id,
        generation: Id,
        next_run_at: Option<DateTime<Utc>>,
    ) -> Result<u64> {
        sqlx::query(
            "UPDATE plugin_hook_schedule SET next_run_at = $3, updated_at = now() \
             WHERE id = $1 AND generation = $2 AND enabled",
        )
        .bind(id.0)
        .bind(generation.0)
        .bind(next_run_at)
        .execute(self.db.pool())
        .await
        .map(|done| done.rows_affected())
        .map_err(map_sqlx_err)
    }

    /// 上游 `CreatePluginInvocation`：落一行调用记录（**best effort** —— 上游把它的错误丢掉，
    /// 「描述调用的遥测不得让调用本身失败」）。
    ///
    /// # Errors
    ///
    /// 库错折成 [`crate::RepoError::Db`]；调用方按上游口径**忽略**它。
    pub async fn record(&self, new: &NewInvocation<'_>) -> Result<()> {
        sqlx::query(
            "INSERT INTO plugin_invocation \
               (id, installation_id, workspace_id, hook_key, trigger, status, event_type, \
                delivery_id, planned_at, attempt, latency_ms, error) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(new.id)
        .bind(new.installation_id.0)
        .bind(new.workspace_id.0)
        .bind(new.hook_key)
        .bind(new.trigger)
        .bind(new.status)
        .bind(new.event_type)
        .bind(new.delivery_id)
        .bind(new.planned_at)
        .bind(new.attempt)
        .bind(new.latency_ms)
        .bind(new.error)
        .execute(self.db.pool())
        .await
        .map(|_| ())
        .map_err(map_sqlx_err)
    }

    /// 上游 `CountRecentPluginInvocations`（`only_failures=false`）与
    /// `CountRecentPluginFailures`（`true`）共用的一个查询。
    ///
    /// 两个调用点：**限流**（120 次/分/钩子）与**熔断**（5 次失败/5 分/钩子）。
    /// 「次数」按**尝试**计，不按不同调用计 —— 一个往死端点重试的钩子正是限流要拦的流量。
    ///
    /// # Errors
    ///
    /// 库错折成 [`crate::RepoError::Db`]；两个调用点都按上游口径**忽略**错误
    /// （「遥测读失败不得把功能一起带下来」）。
    pub async fn count_recent(
        &self,
        installation_id: Id,
        hook_key: &str,
        since: DateTime<Utc>,
        only_failures: bool,
    ) -> Result<i64> {
        let status_clause = if only_failures {
            "AND status <> 'ok'"
        } else {
            ""
        };
        let row: (i64,) = sqlx::query_as(&format!(
            "SELECT count(*) FROM plugin_invocation \
             WHERE installation_id = $1 AND hook_key = $2 AND created_at > $3 {status_clause}"
        ))
        .bind(installation_id.0)
        .bind(hook_key)
        .bind(since)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.0)
    }

    /// 上游 `DeleteExpiredPluginInvocations`：TTL 清扫（表是运行期遥测，不是历史）。
    ///
    /// 返回删除行数。调用方（dispatcher 的计时器）按小时跑；本片**登记**为「无调度器宿主」
    /// 的缺口（见 `docs/32` §9.10），查询本身落地是为了让 M6-INT 只接线不写 SQL。
    ///
    /// # Errors
    ///
    /// 库错折成 [`crate::RepoError::Db`]。
    pub async fn delete_expired(&self, before: DateTime<Utc>) -> Result<u64> {
        sqlx::query("DELETE FROM plugin_invocation WHERE created_at < $1")
            .bind(before)
            .execute(self.db.pool())
            .await
            .map(|done| done.rows_affected())
            .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for HookScheduleRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

// ---------------------------------------------------------------------------
// 事务内写口（安装/升级/启停的调用方在自己的事务里调）
// ---------------------------------------------------------------------------

/// 上游 `reconcilePluginHookSchedules`：把持久投影对齐到**管理员同意过的** manifest。
///
/// 三条语义逐字照抄：
///
/// 1. **cron/timezone 没变就保留 `generation`** —— 纯代码升级不得重置日程时钟、
///    也不得孤立一条可重试的执行（换代会让在途的那一格失效）；
/// 2. cron/timezone 变了 ⇒ `generation = gen_random_uuid()` + `activated_at = now()`（新纪元）；
/// 3. manifest 里**没有**的日程行 ⇒ 删除。
///
/// `inputs` 为空 = 该版本不再贡献任何日程 ⇒ 全部删掉（与上游同义）。
///
/// # Errors
///
/// 库错折成 [`crate::RepoError::Db`]。
pub async fn reconcile_tx(
    tx: &mut Tx<'_>,
    installation: &InstallationRow,
    inputs: &[HookScheduleInput],
) -> Result<()> {
    let existing = sqlx::query_as::<_, HookScheduleRow>(&format!(
        "SELECT {SCHEDULE_COLUMNS} FROM plugin_hook_schedule \
         WHERE installation_id = $1 ORDER BY hook_key ASC"
    ))
    .bind(installation.id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx_err)?;

    let mut stale: Vec<(Uuid, String)> = existing
        .iter()
        .map(|row| (row.id, row.hook_key.clone()))
        .collect();

    for input in inputs {
        let found = existing.iter().find(|row| row.hook_key == input.hook_key);
        match found {
            None => {
                sqlx::query(
                    "INSERT INTO plugin_hook_schedule \
                       (installation_id, workspace_id, hook_key, cron_expression, timezone, \
                        next_run_at, enabled) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                )
                .bind(installation.id)
                .bind(installation.workspace_id)
                .bind(&input.hook_key)
                .bind(&input.cron_expression)
                .bind(&input.timezone)
                .bind(input.next_run_at)
                .bind(installation.enabled)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx_err)?;
            }
            Some(row) => {
                stale.retain(|(_, key)| key != &input.hook_key);
                if row.cron_expression == input.cron_expression && row.timezone == input.timezone {
                    // 定义没变 ⇒ 保留 generation（第 1 条语义）。
                    continue;
                }
                sqlx::query(
                    "UPDATE plugin_hook_schedule SET cron_expression = $2, timezone = $3, \
                        generation = gen_random_uuid(), activated_at = now(), \
                        next_run_at = $4, enabled = $5, updated_at = now() \
                     WHERE id = $1",
                )
                .bind(row.id)
                .bind(&input.cron_expression)
                .bind(&input.timezone)
                .bind(input.next_run_at)
                .bind(installation.enabled)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx_err)?;
            }
        }
    }

    for (id, _) in stale {
        sqlx::query("DELETE FROM plugin_hook_schedule WHERE id = $1")
            .bind(id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx_err)?;
    }
    Ok(())
}

/// 上游 `setPluginHookSchedulesEnabled`：停用 ⇒ 全部关掉并清 `next_run_at`；
/// 启用 ⇒ 逐行**换一代**再开（`ReactivatePluginHookSchedule`）。
///
/// ⚠️ 换一代是**故意的**：停用期间错过的格子永不补发。已经发出去的上一代请求可以让它跑完，
/// 但没启动的旧计划过不了 handler 的 generation 检查。
///
/// `next_run_at_of` 由调用方给（cron 解析在 `mc-http` / `mc-autopilot`）：入参是 `(hook_key,
/// cron, timezone)`，返回算好的下一次。
///
/// # Errors
///
/// 库错折成 [`crate::RepoError::Db`]。
pub async fn set_enabled_tx(
    tx: &mut Tx<'_>,
    installation_id: Id,
    enabled: bool,
    next_run_at_of: impl Fn(&HookScheduleRow) -> Option<DateTime<Utc>>,
) -> Result<()> {
    if !enabled {
        sqlx::query(
            "UPDATE plugin_hook_schedule SET enabled = FALSE, next_run_at = NULL, updated_at = now() \
             WHERE installation_id = $1",
        )
        .bind(installation_id.0)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;
        return Ok(());
    }

    let rows = sqlx::query_as::<_, HookScheduleRow>(&format!(
        "SELECT {SCHEDULE_COLUMNS} FROM plugin_hook_schedule \
         WHERE installation_id = $1 ORDER BY hook_key ASC"
    ))
    .bind(installation_id.0)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx_err)?;

    for row in rows {
        let next_run_at = next_run_at_of(&row);
        sqlx::query(
            "UPDATE plugin_hook_schedule SET enabled = TRUE, generation = gen_random_uuid(), \
                activated_at = now(), next_run_at = $2, updated_at = now() \
             WHERE id = $1",
        )
        .bind(row.id)
        .bind(next_run_at)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 真库用例（门 ⑥：`cargo test -p mc-repos -- --ignored`）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod db_tests {
    use super::*;

    /// `plugin_hook_schedule` 与 `plugin_invocation` 在迁移里**没有任何外键**
    /// （`399` 的注释写明关系由应用拥有），所以夹具不需要 workspace / installation 行。
    async fn setup() -> Option<(Db, Id, Id)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        Some((db, Id::from(Uuid::new_v4()), Id::from(Uuid::new_v4())))
    }

    fn input(hook_key: &str, cron: &str) -> HookScheduleInput {
        HookScheduleInput {
            hook_key: hook_key.to_owned(),
            cron_expression: cron.to_owned(),
            timezone: "UTC".to_owned(),
            next_run_at: None,
        }
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    #[allow(clippy::too_many_lines)] // 四段语义（保留代 / 换代 / 守卫 / 启停）是一条时间线，拆开就断
    async fn reconcile_keeps_generation_until_the_definition_changes() {
        let Some((db, installation_id, workspace_id)) = setup().await else {
            println!("skipping: MULTICA_TEST_DATABASE_URL not set");
            return;
        };
        let repo = HookScheduleRepo::new(db.clone());
        let installation = InstallationRow {
            id: installation_id.0,
            workspace_id: workspace_id.0,
            plugin_key: "com.example.hook".into(),
            version: "1.0.0".into(),
            manifest: sqlx::types::Json(serde_json::json!({})),
            granted_scopes: sqlx::types::Json(serde_json::json!([])),
            config: sqlx::types::Json(serde_json::json!({})),
            enabled: true,
            installed_by: None,
            token_hash: None,
            token_rotated_at: None,
            mcp_approvals: sqlx::types::Json(serde_json::json!({})),
            package_version_id: Uuid::new_v4(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let mut tx = db.pool().begin().await.expect("begin");
        reconcile_tx(
            &mut tx,
            &installation,
            &[input("sync", "0 3 * * *"), input("digest", "30 4 * * *")],
        )
        .await
        .expect("first reconcile");
        tx.commit().await.expect("commit");

        let first = repo
            .list_by_installation(installation_id)
            .await
            .expect("list");
        assert_eq!(first.len(), 2);
        let sync_before = first.iter().find(|r| r.hook_key == "sync").expect("sync");

        // 纯代码升级（cron/timezone 未变）⇒ 同一代：日程时钟不被重置。
        let mut tx = db.pool().begin().await.expect("begin");
        reconcile_tx(
            &mut tx,
            &installation,
            &[input("sync", "0 3 * * *"), input("digest", "30 4 * * *")],
        )
        .await
        .expect("idempotent reconcile");
        tx.commit().await.expect("commit");
        let again = repo
            .list_by_installation(installation_id)
            .await
            .expect("list again");
        let sync_after = again.iter().find(|r| r.hook_key == "sync").expect("sync");
        assert_eq!(
            sync_before.generation, sync_after.generation,
            "定义没变 ⇒ generation 必须保留（否则在途投递会被换代判无效）"
        );

        // cron 变了 ⇒ 换代，且旧一代的 `advance_next_run` 写 0 行（三连守卫）。
        assert_eq!(
            repo.advance_next_run(sync_before.id(), sync_before.generation(), Some(Utc::now()))
                .await
                .expect("advance on live generation"),
            1
        );

        let mut tx = db.pool().begin().await.expect("begin");
        reconcile_tx(&mut tx, &installation, &[input("sync", "0 5 * * *")])
            .await
            .expect("reconcile with new cron");
        tx.commit().await.expect("commit");
        let rotated = repo
            .list_by_installation(installation_id)
            .await
            .expect("list");
        assert_eq!(rotated.len(), 1, "manifest 里没有的日程行必须被删掉");
        assert_ne!(
            rotated[0].generation, sync_before.generation,
            "cron 变了 ⇒ 必须换代"
        );
        assert_eq!(
            repo.advance_next_run(sync_before.id(), sync_before.generation(), Some(Utc::now()))
                .await
                .expect("advance on stale generation"),
            0,
            "换代之后旧持有者写 next_run_at 必须影响 0 行"
        );

        // 启停：停用清 next_run_at，启用换一代。
        let generation_before = rotated[0].generation;
        let mut tx = db.pool().begin().await.expect("begin");
        set_enabled_tx(&mut tx, installation_id, false, |_| None)
            .await
            .expect("disable");
        tx.commit().await.expect("commit");
        assert!(repo
            .list_enabled()
            .await
            .expect("list")
            .iter()
            .all(|r| r.installation_id != installation_id.0));

        let mut tx = db.pool().begin().await.expect("begin");
        set_enabled_tx(&mut tx, installation_id, true, |_| Some(Utc::now()))
            .await
            .expect("enable");
        tx.commit().await.expect("commit");
        let enabled = repo
            .list_by_installation(installation_id)
            .await
            .expect("list");
        assert_eq!(enabled.len(), 1);
        assert!(enabled[0].enabled);
        assert_ne!(
            enabled[0].generation, generation_before,
            "重开必须换一代（停机期间的格子永不补发）"
        );

        // 清理：两张表都没有外键，按 id 前缀删即可。
        sqlx::query("DELETE FROM plugin_hook_schedule WHERE installation_id = $1")
            .bind(installation_id.0)
            .execute(db.pool())
            .await
            .expect("cleanup schedules");
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn record_writes_every_retry_with_the_same_delivery_id() {
        let Some((db, installation_id, workspace_id)) = setup().await else {
            println!("skipping: MULTICA_TEST_DATABASE_URL not set");
            return;
        };
        let repo = HookScheduleRepo::new(db.clone());
        let window = Utc::now() - chrono::Duration::minutes(1);

        // 同一次计划投递的两次尝试 = 两行（`attempt` 递增），`delivery_id` 相同。
        for attempt in 1..=2 {
            repo.record(&NewInvocation {
                id: Uuid::new_v4(),
                installation_id,
                workspace_id,
                hook_key: "sync",
                trigger: "schedule",
                status: if attempt == 1 { "failed" } else { "ok" },
                event_type: None,
                attempt,
                latency_ms: 12,
                error: (attempt == 1).then_some("hook endpoint did not answer"),
                delivery_id: Some("psd_deadbeef"),
                planned_at: Some(Utc::now()),
            })
            .await
            .expect("record");
        }
        let rows: Vec<(String, i32, Option<String>)> = sqlx::query_as(
            "SELECT status, attempt, delivery_id FROM plugin_invocation \
             WHERE installation_id = $1 ORDER BY attempt ASC",
        )
        .bind(installation_id.0)
        .fetch_all(db.pool())
        .await
        .expect("read back");
        assert_eq!(rows.len(), 2, "重试必须各留一行（行级去重会吃掉重试）");
        assert_eq!(rows[0].1, 1);
        assert_eq!(rows[1].1, 2);
        assert_eq!(rows[0].2.as_deref(), Some("psd_deadbeef"));

        // 限流按**尝试**计；熔断只数非 ok。
        assert_eq!(
            repo.count_recent(installation_id, "sync", window, false)
                .await
                .expect("count all"),
            2
        );
        assert_eq!(
            repo.count_recent(installation_id, "sync", window, true)
                .await
                .expect("count failures"),
            1
        );
        assert_eq!(
            repo.count_recent(installation_id, "digest", window, false)
                .await
                .expect("count other hook"),
            0
        );

        let removed = repo
            .delete_expired(Utc::now() + chrono::Duration::minutes(1))
            .await
            .expect("sweep");
        assert!(removed >= 2, "TTL 清扫必须删掉刚写的行");
    }
}

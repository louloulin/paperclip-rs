//! `AutopilotSchedulePort` 的**生产实现**（M5-9 / LUM-1659）。
//!
//! 本文件是 `docs/55` §3.4「缺口 1」的落地：trait 只写了 5 条 SQL 的口径，实现放在
//! `apps/mc-server`（`mc-scheduler` 的依赖表里没有 `sqlx`，M5-0 冻结）。
//!
//! # 与上游 SQL 的逐条对照
//!
//! | trait 方法 | 上游 | 差异 |
//! | --- | --- | --- |
//! | [`AutopilotSchedulePort::list_schedulable_triggers`] | `ListSchedulableAutopilotTriggers`（`autopilot.sql:541`） | 上游 `:many` 只 SELECT 6 列（`id, autopilot_id, cron_expression, timezone, created_at, last_fired_at`），而 trait 要求**完整行** ⇒ 本地取全部 19 列。**WHERE / ORDER BY 逐字相同**，是上游的超集（多出来的列不参与筛选） |
//! | [`AutopilotSchedulePort::load_trigger`] | `GetAutopilotTrigger` | 逐字相同（`SELECT *`） |
//! | [`AutopilotSchedulePort::load_autopilot`] | `GetAutopilot` | 逐字相同（`SELECT *`） |
//! | [`AutopilotSchedulePort::advance_next_run`] | `AdvanceTriggerNextRun`（`autopilot.sql:238`） | 逐字相同：`next_run_at = $2, last_fired_at = now(), updated_at = now()`（**三个**列，不是只 `next_run_at`） |
//! | [`AutopilotSchedulePort::touch_fired_at`] | `TouchAutopilotTriggerFiredAt`（`autopilot.sql:257`） | 上游只有 `last_fired_at = now()`；本地**多推 `updated_at`**，与同文件的 `AdvanceTriggerNextRun` 口径一致（登记为偏差，见 PR 说明） |
//!
//! 「查不到 ⇒ `Ok(None)`」是上游 `pgx.ErrNoRows` 的等价物，**不是**错误。

use chrono::{DateTime, Utc};
use uuid::Uuid;

use mc_repos::autopilot::{AutopilotRow, AutopilotTriggerRow};
use mc_scheduler::error::SchedulerResult;
use mc_scheduler::jobs::autopilot::AutopilotSchedulePort;
use mc_scheduler::jobs::PortFuture;

use super::repo_err;

/// 可调度 trigger 的全列（19 列，逐列 `t.` 限定 —— 本查询带 `JOIN autopilot a`，
/// `id` 等列不加限定会歧义）。
///
/// WHERE / ORDER BY 与上游 `ListSchedulableAutopilotTriggers` 逐字相同。
const LIST_SCHEDULABLE_TRIGGERS: &str = "\
SELECT t.id, t.autopilot_id, t.kind, t.enabled, t.cron_expression, t.timezone, t.next_run_at, \
       t.webhook_token, t.label, t.last_fired_at, t.created_at, t.updated_at, t.provider, \
       t.signing_secret, t.event_filters, t.published_by_type, t.published_by_id, \
       t.created_by_type, t.created_by_id \
  FROM autopilot_trigger t \
  JOIN autopilot a ON a.id = t.autopilot_id \
 WHERE t.kind = 'schedule' \
   AND t.enabled = TRUE \
   AND a.status = 'active' \
   AND t.cron_expression IS NOT NULL \
   AND t.cron_expression <> '' \
 ORDER BY t.id";

/// `GetAutopilotTrigger`。
const LOAD_TRIGGER: &str = "SELECT * FROM autopilot_trigger WHERE id = $1";

/// `GetAutopilot`。
const LOAD_AUTOPILOT: &str = "SELECT * FROM autopilot WHERE id = $1";

/// `AdvanceTriggerNextRun`（`next_run_at` + `last_fired_at` + `updated_at`）。
const ADVANCE_NEXT_RUN: &str = "\
UPDATE autopilot_trigger \
   SET next_run_at = $2, last_fired_at = now(), updated_at = now() \
 WHERE id = $1";

/// `TouchAutopilotTriggerFiredAt`（只推 `last_fired_at`）。
const TOUCH_FIRED_AT: &str = "\
UPDATE autopilot_trigger \
   SET last_fired_at = now(), updated_at = now() \
 WHERE id = $1";

/// 生产实现的 `AutopilotSchedulePort`：autopilot job 的 trigger 读面 + 展示列写面。
///
/// 只持有池的克隆 —— 两种构造入口都在下面（`new` 给 `main.rs`，`from_pool` 给真库测试）。
#[derive(Debug, Clone)]
pub struct McAutopilotSchedulePort {
    pool: sqlx::PgPool,
}

impl McAutopilotSchedulePort {
    /// 从 `main.rs` 手上的 `Db` 构造（复用同一个池）。
    #[must_use]
    pub fn new(db: &mc_db::Db) -> Self {
        Self {
            pool: db.pool().clone(),
        }
    }

    /// 从裸池构造（真库用例用；`mc-db` 的 `from_pool` 需要 `test-util` feature，端口不想依赖它）。
    #[must_use]
    #[cfg(test)]
    pub fn from_pool(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

impl AutopilotSchedulePort for McAutopilotSchedulePort {
    fn list_schedulable_triggers(
        &self,
    ) -> PortFuture<'_, SchedulerResult<Vec<AutopilotTriggerRow>>> {
        Box::pin(async move {
            sqlx::query_as::<_, AutopilotTriggerRow>(LIST_SCHEDULABLE_TRIGGERS)
                .fetch_all(&self.pool)
                .await
                .map_err(repo_err)
        })
    }

    fn load_trigger(
        &self,
        trigger_id: Uuid,
    ) -> PortFuture<'_, SchedulerResult<Option<AutopilotTriggerRow>>> {
        Box::pin(async move {
            sqlx::query_as::<_, AutopilotTriggerRow>(LOAD_TRIGGER)
                .bind(trigger_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(repo_err)
        })
    }

    fn load_autopilot(
        &self,
        autopilot_id: Uuid,
    ) -> PortFuture<'_, SchedulerResult<Option<AutopilotRow>>> {
        Box::pin(async move {
            sqlx::query_as::<_, AutopilotRow>(LOAD_AUTOPILOT)
                .bind(autopilot_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(repo_err)
        })
    }

    fn advance_next_run(
        &self,
        trigger_id: Uuid,
        next_run_at: Option<DateTime<Utc>>,
    ) -> PortFuture<'_, SchedulerResult<u64>> {
        Box::pin(async move {
            sqlx::query(ADVANCE_NEXT_RUN)
                .bind(trigger_id)
                .bind(next_run_at)
                .execute(&self.pool)
                .await
                .map(|outcome| outcome.rows_affected())
                .map_err(repo_err)
        })
    }

    fn touch_fired_at(&self, trigger_id: Uuid) -> PortFuture<'_, SchedulerResult<u64>> {
        Box::pin(async move {
            sqlx::query(TOUCH_FIRED_AT)
                .bind(trigger_id)
                .execute(&self.pool)
                .await
                .map(|outcome| outcome.rows_affected())
                .map_err(repo_err)
        })
    }
}

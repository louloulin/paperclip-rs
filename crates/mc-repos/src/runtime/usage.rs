//! runtime 用量 / 活动读数（M3-4 / LUM-1427）。
//!
//! 对应 upstream `server/pkg/db/queries/runtime_usage.sql` 的 4 条查询：
//! `ListRuntimeUsage`（按日×provider×model）、`ListRuntimeUsageByAgent`、
//! `GetRuntimeUsageByHour`、`GetRuntimeTaskHourlyActivity`。
//!
//! 共同口径（照抄上游注释，别自己「优化」）：
//! - `provider` 一律 `LOWER()` 归一，把历史上大小写混写的行合并；
//! - `model` 维度保留，因为价格表在客户端，服务端折叠掉就没法算钱了；
//! - 成本分两半：`cost_usd_ticks` 是 provider 真实计费（1e-10 USD），
//!   `uncosted_*` 是 provider 没定价的那些 token —— 客户端报「真实 + 估算」，
//!   混着两种行的窗口才不会缺一块（迁移 213）；
//! - 时间维度都按调用方给的 IANA 时区投影（`@tz`），「下午很忙」指的是**看报表的人**
//!   的下午；`by-agent` / `by-hour` 不按日分桶，tz 只用来定 `since` 边界。
//!
//! 行类型只做字节级读取与聚合，不做任何业务判断 —— `since` 的计算（含 `days` 参数
//! 钳制与 N+1 桶的 headroom）留在路由层，因为那是 HTTP 语义。

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use mc_core::Id;

use crate::workspace::map_sqlx_err;
use crate::Result;

/// `GET /api/runtimes/{id}/usage` 的一行：某天 × provider × model 的 token 聚合。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeUsageRow {
    /// 按调用方时区投影出来的日历日（`DATE(bucket_hour AT TIME ZONE tz)`）。
    pub date: NaiveDate,
    pub provider: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost_usd_ticks: i64,
    pub uncosted_input_tokens: i64,
    pub uncosted_output_tokens: i64,
    pub uncosted_cache_read_tokens: i64,
    pub uncosted_cache_write_tokens: i64,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for RuntimeUsageRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        Ok(Self {
            date: row.try_get("date")?,
            provider: row.try_get("provider")?,
            model: row.try_get("model")?,
            input_tokens: row.try_get("input_tokens")?,
            output_tokens: row.try_get("output_tokens")?,
            cache_read_tokens: row.try_get("cache_read_tokens")?,
            cache_write_tokens: row.try_get("cache_write_tokens")?,
            cost_usd_ticks: row.try_get("cost_usd_ticks")?,
            uncosted_input_tokens: row.try_get("uncosted_input_tokens")?,
            uncosted_output_tokens: row.try_get("uncosted_output_tokens")?,
            uncosted_cache_read_tokens: row.try_get("uncosted_cache_read_tokens")?,
            uncosted_cache_write_tokens: row.try_get("uncosted_cache_write_tokens")?,
        })
    }
}

/// `usage/by-agent` 的一行：某 agent × provider × model 的 token 聚合。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeUsageByAgentRow {
    pub agent_id: Id,
    pub provider: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost_usd_ticks: i64,
    pub uncosted_input_tokens: i64,
    pub uncosted_output_tokens: i64,
    pub uncosted_cache_read_tokens: i64,
    pub uncosted_cache_write_tokens: i64,
    pub task_count: i64,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for RuntimeUsageByAgentRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        Ok(Self {
            agent_id: Id::from(row.try_get::<Uuid, _>("agent_id")?),
            provider: row.try_get("provider")?,
            model: row.try_get("model")?,
            input_tokens: row.try_get("input_tokens")?,
            output_tokens: row.try_get("output_tokens")?,
            cache_read_tokens: row.try_get("cache_read_tokens")?,
            cache_write_tokens: row.try_get("cache_write_tokens")?,
            cost_usd_ticks: row.try_get("cost_usd_ticks")?,
            uncosted_input_tokens: row.try_get("uncosted_input_tokens")?,
            uncosted_output_tokens: row.try_get("uncosted_output_tokens")?,
            uncosted_cache_read_tokens: row.try_get("uncosted_cache_read_tokens")?,
            uncosted_cache_write_tokens: row.try_get("uncosted_cache_write_tokens")?,
            task_count: row.try_get("task_count")?,
        })
    }
}

/// `usage/by-hour` 的一行：一天里的某个小时 × model。零活动的桶不返回，
/// 客户端自己补齐 0..23（upstream 同款）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeUsageByHourRow {
    pub hour: i64,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost_usd_ticks: i64,
    pub uncosted_input_tokens: i64,
    pub uncosted_output_tokens: i64,
    pub uncosted_cache_read_tokens: i64,
    pub uncosted_cache_write_tokens: i64,
    pub task_count: i64,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for RuntimeUsageByHourRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        Ok(Self {
            hour: row.try_get("hour")?,
            model: row.try_get("model")?,
            input_tokens: row.try_get("input_tokens")?,
            output_tokens: row.try_get("output_tokens")?,
            cache_read_tokens: row.try_get("cache_read_tokens")?,
            cache_write_tokens: row.try_get("cache_write_tokens")?,
            cost_usd_ticks: row.try_get("cost_usd_ticks")?,
            uncosted_input_tokens: row.try_get("uncosted_input_tokens")?,
            uncosted_output_tokens: row.try_get("uncosted_output_tokens")?,
            uncosted_cache_read_tokens: row.try_get("uncosted_cache_read_tokens")?,
            uncosted_cache_write_tokens: row.try_get("uncosted_cache_write_tokens")?,
            task_count: row.try_get("task_count")?,
        })
    }
}

/// `GET /api/runtimes/{id}/activity` 的一行：某小时的启动任务数（按调用方时区分桶）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityRow {
    pub hour: i64,
    pub count: i64,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for ActivityRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        Ok(Self {
            hour: row.try_get("hour")?,
            count: row.try_get("count")?,
        })
    }
}

/// 读 UTC 桶表 `task_usage_hourly`，按调用方时区投影成日历日再聚合。
///
/// `since` 由路由层按 `days`（默认 90）算好；`tz` 必传 —— 即使调用方想用 UTC，
/// 也让 `bucket_hour` 的投影无歧义。
const LIST_RUNTIME_USAGE: &str = "SELECT \
       DATE(bucket_hour AT TIME ZONE $3::text) AS date, \
       LOWER(provider) AS provider, \
       model, \
       SUM(input_tokens)::bigint AS input_tokens, \
       SUM(output_tokens)::bigint AS output_tokens, \
       SUM(cache_read_tokens)::bigint AS cache_read_tokens, \
       SUM(cache_write_tokens)::bigint AS cache_write_tokens, \
       SUM(cost_usd_ticks)::bigint AS cost_usd_ticks, \
       SUM(COALESCE(uncosted_input_tokens, input_tokens))::bigint AS uncosted_input_tokens, \
       SUM(COALESCE(uncosted_output_tokens, output_tokens))::bigint AS uncosted_output_tokens, \
       SUM(COALESCE(uncosted_cache_read_tokens, cache_read_tokens))::bigint \
           AS uncosted_cache_read_tokens, \
       SUM(COALESCE(uncosted_cache_write_tokens, cache_write_tokens))::bigint \
           AS uncosted_cache_write_tokens \
     FROM task_usage_hourly \
     WHERE runtime_id = $1 AND bucket_hour >= $2::timestamptz \
     GROUP BY DATE(bucket_hour AT TIME ZONE $3::text), LOWER(provider), model \
     ORDER BY DATE(bucket_hour AT TIME ZONE $3::text) DESC, LOWER(provider), model";

/// 明细表 `task_usage` 只有 `task_id`，所以 join 队列表才能拿到 `agent_id`。
const LIST_RUNTIME_USAGE_BY_AGENT: &str = "SELECT \
       atq.agent_id, \
       LOWER(tu.provider) AS provider, \
       tu.model, \
       SUM(tu.input_tokens)::bigint AS input_tokens, \
       SUM(tu.output_tokens)::bigint AS output_tokens, \
       SUM(tu.cache_read_tokens)::bigint AS cache_read_tokens, \
       SUM(tu.cache_write_tokens)::bigint AS cache_write_tokens, \
       COALESCE(SUM(tu.cost_usd_ticks), 0)::bigint AS cost_usd_ticks, \
       COALESCE(SUM(tu.input_tokens) FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint \
           AS uncosted_input_tokens, \
       COALESCE(SUM(tu.output_tokens) FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint \
           AS uncosted_output_tokens, \
       COALESCE(SUM(tu.cache_read_tokens) FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint \
           AS uncosted_cache_read_tokens, \
       COALESCE(SUM(tu.cache_write_tokens) FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint \
           AS uncosted_cache_write_tokens, \
       COUNT(DISTINCT tu.task_id)::bigint AS task_count \
     FROM task_usage tu \
     JOIN agent_task_queue atq ON atq.id = tu.task_id \
     WHERE atq.runtime_id = $1 AND tu.created_at >= $2::timestamptz \
     GROUP BY atq.agent_id, LOWER(tu.provider), tu.model \
     ORDER BY atq.agent_id, LOWER(tu.provider), tu.model";

const GET_RUNTIME_USAGE_BY_HOUR: &str = "SELECT \
       EXTRACT(HOUR FROM tu.created_at AT TIME ZONE $3::text)::bigint AS hour, \
       tu.model, \
       SUM(tu.input_tokens)::bigint AS input_tokens, \
       SUM(tu.output_tokens)::bigint AS output_tokens, \
       SUM(tu.cache_read_tokens)::bigint AS cache_read_tokens, \
       SUM(tu.cache_write_tokens)::bigint AS cache_write_tokens, \
       COALESCE(SUM(tu.cost_usd_ticks), 0)::bigint AS cost_usd_ticks, \
       COALESCE(SUM(tu.input_tokens) FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint \
           AS uncosted_input_tokens, \
       COALESCE(SUM(tu.output_tokens) FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint \
           AS uncosted_output_tokens, \
       COALESCE(SUM(tu.cache_read_tokens) FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint \
           AS uncosted_cache_read_tokens, \
       COALESCE(SUM(tu.cache_write_tokens) FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint \
           AS uncosted_cache_write_tokens, \
       COUNT(DISTINCT tu.task_id)::bigint AS task_count \
     FROM task_usage tu \
     JOIN agent_task_queue atq ON atq.id = tu.task_id \
     WHERE atq.runtime_id = $1 AND tu.created_at >= $2::timestamptz \
     GROUP BY EXTRACT(HOUR FROM tu.created_at AT TIME ZONE $3::text), tu.model \
     ORDER BY hour, tu.model";

const GET_RUNTIME_TASK_HOURLY_ACTIVITY: &str = "SELECT \
       EXTRACT(HOUR FROM started_at AT TIME ZONE $2::text)::bigint AS hour, \
       COUNT(*)::bigint AS count \
     FROM agent_task_queue \
     WHERE runtime_id = $1 AND started_at IS NOT NULL \
     GROUP BY hour \
     ORDER BY hour";

impl super::AgentRuntimeRepo {
    /// 按日 × provider × model 的 token 趋势（`usage`，默认 90 天）。
    ///
    /// # Errors
    /// 数据库错误统一映射为 [`crate::RepoError::Db`]。
    pub async fn list_runtime_usage(
        &self,
        runtime_id: Id,
        since: DateTime<Utc>,
        tz: &str,
    ) -> Result<Vec<RuntimeUsageRow>> {
        let rows = sqlx::query_as::<_, RuntimeUsageRow>(LIST_RUNTIME_USAGE)
            .bind(runtime_id.as_uuid())
            .bind(since)
            .bind(tz)
            .fetch_all(self.conn())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows)
    }

    /// 按 agent × provider × model 的 token 聚合（`usage/by-agent`，默认 30 天）。
    ///
    /// # Errors
    /// 数据库错误统一映射为 [`crate::RepoError::Db`]。
    pub async fn list_runtime_usage_by_agent(
        &self,
        runtime_id: Id,
        since: DateTime<Utc>,
    ) -> Result<Vec<RuntimeUsageByAgentRow>> {
        let rows = sqlx::query_as::<_, RuntimeUsageByAgentRow>(LIST_RUNTIME_USAGE_BY_AGENT)
            .bind(runtime_id.as_uuid())
            .bind(since)
            .fetch_all(self.conn())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows)
    }

    /// 按小时（0..23）× model 的 token 聚合（`usage/by-hour`，默认 30 天）。
    ///
    /// # Errors
    /// 数据库错误统一映射为 [`crate::RepoError::Db`]。
    pub async fn get_runtime_usage_by_hour(
        &self,
        runtime_id: Id,
        since: DateTime<Utc>,
        tz: &str,
    ) -> Result<Vec<RuntimeUsageByHourRow>> {
        let rows = sqlx::query_as::<_, RuntimeUsageByHourRow>(GET_RUNTIME_USAGE_BY_HOUR)
            .bind(runtime_id.as_uuid())
            .bind(since)
            .bind(tz)
            .fetch_all(self.conn())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows)
    }

    /// 任务启动的时刻分布（`activity`；无 `days` 窗口，上游就是全量）。
    ///
    /// # Errors
    /// 数据库错误统一映射为 [`crate::RepoError::Db`]。
    pub async fn get_runtime_task_activity(
        &self,
        runtime_id: Id,
        tz: &str,
    ) -> Result<Vec<ActivityRow>> {
        let rows = sqlx::query_as::<_, ActivityRow>(GET_RUNTIME_TASK_HOURLY_ACTIVITY)
            .bind(runtime_id.as_uuid())
            .bind(tz)
            .fetch_all(self.conn())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows)
    }

    /// 供 `usage` 之外的地方复用连接池（测试里建 fixture 用）。
    pub fn pool_ref(&self) -> &PgPool {
        self.conn()
    }
}

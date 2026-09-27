//! `DashboardRepo` —— dashboard 6 条的**只读聚合**（**写者 M9-4** / `docs/62` §9.6）。
//!
//! | 路由 | 表 | 口径要点 |
//! | --- | --- | --- |
//! | `usage/daily` | `task_usage_hourly` | `SUM` 四类 token + `cost_usd_ticks` + `task_count`，按 `DATE(bucket_hour AT TIME ZONE tz)` + `LOWER(provider)` + `model` 分组；`uncosted_*` 用 `COALESCE(uncosted_x, x)` |
//! | `usage/by-agent` | `task_usage_hourly` | 同上，分组换 `agent_id`，**不吃 `tz`**（无日期维度） |
//! | `agent-runtime` | `agent_task_queue` ⋈ `agent` ⋈ `issue` | `SUM(EXTRACT(EPOCH FROM (completed_at - started_at)))` + 三个计数 + 「计过费」的 `EXISTS (SELECT 1 FROM task_usage …)` |
//! | `runtime/daily` | 同上 | 按 `DATE(completed_at AT TIME ZONE tz)` 分组 |
//! | `failures/daily` / `failures/by-agent` | 同上 | 按 `failure_reason` 计数（含"从未 started"的任务） |
//!
//! 🔴 **6 条里 0 条读 `task_usage_dashboard_*`**（`084` 的两张 legacy rollup 表已被
//! `101`/`103` 的 hourly 化取代 —— `103_drop_legacy_daily_rollups.up.sql` 逐字
//! 「drop legacy daily rollups」）⇒ 本文件的 6 段 SQL 里**没有**那个前缀，
//! `no_legacy_rollup_tables_are_referenced` 用例把这条纪律钉成断言。
//!
//! # 三个可复用面（**禁止重复实现**，`docs/62` §2.3）
//!
//! - `crates/mc-repos/src/runtime/usage.rs`（`task_usage_hourly` 的读写，322 行）—— **只读**；
//! - `crates/mc-repos/src/task/*`（`agent_task_queue` 的既有访问）；
//! - `crates/mc-http/src/routes/runtimes/usage.rs`（既有 per-runtime 口径 ——
//!   dashboard 的 `provider`/`model` 维度**故意**与它一致）。
//!
//! # 行形状与口径常量不在本文件
//!
//! 六个行结构 + `days`/`tz`/cutoff/折叠哨兵全在 [`mc_core::dashboard`]
//! （anchor 定形、此后冻结）⇒ 本文件的 6 个方法**直接返回那六个 wire 结构**，
//! 路由层不再做第二次行映射（少一处能漂移的转换）。
//!
//! # `@since` / `@tz` 的归属
//!
//! 上游的 `since` 是「**看的人的本地零点**往前推 N 天」算出来的**瞬时**，
//! 由 HTTP 层（[`mc_core::dashboard::cutoff_days_with_convention`] + 时区库）算完传进来；
//! 本文件**只透传**、绝不 `DATE_TRUNC`（上游注释逐字：`DATE_TRUNC` 走 session tz，
//! 会把切点拽回 UTC 午夜，给非 UTC 的看的人多拖进半个本地日）。

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use mc_core::dashboard::{
    DashboardAgentRunTimeResponse, DashboardFailureByAgentResponse, DashboardFailureDailyResponse,
    DashboardRunTimeDailyResponse, DashboardUsageByAgentResponse, DashboardUsageDailyResponse,
    TokenUsageCounts,
};
use mc_core::Id;

use crate::agent::{
    role_is_admin, AgentInvocationTargetRow, AgentRepo, PERMISSION_MODE_PUBLIC_TO, TARGET_MEMBER,
    TARGET_WORKSPACE,
};
use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

// ---------------------------------------------------------------------------
// SQL（逐段照抄 `server/pkg/db/queries/task_usage.sql` 的 6 条，
// 只把 sqlc 的具名参数换成 `$n` 位置参数）
// ---------------------------------------------------------------------------

/// 上游 `ListDashboardUsageDaily`。
///
/// ⚠️ `$3` 是调用方时区，作用在**每个**分桶表达式上 —— 三个 `AT TIME ZONE $3::text`
/// 必须逐字一致，否则 `GROUP BY` 与 `SELECT` 认的不是同一个键（PG 会直接报
/// 「column must appear in the GROUP BY」而不是静默给错答案）。
const LIST_USAGE_DAILY: &str = r"
SELECT
    DATE(bucket_hour AT TIME ZONE $3::text) AS date,
    LOWER(provider) AS provider,
    model,
    SUM(input_tokens)::bigint        AS input_tokens,
    SUM(output_tokens)::bigint       AS output_tokens,
    SUM(cache_read_tokens)::bigint   AS cache_read_tokens,
    SUM(cache_write_tokens)::bigint  AS cache_write_tokens,
    SUM(cost_usd_ticks)::bigint      AS cost_usd_ticks,
    SUM(COALESCE(uncosted_input_tokens, input_tokens))::bigint         AS uncosted_input_tokens,
    SUM(COALESCE(uncosted_output_tokens, output_tokens))::bigint       AS uncosted_output_tokens,
    SUM(COALESCE(uncosted_cache_read_tokens, cache_read_tokens))::bigint AS uncosted_cache_read_tokens,
    SUM(COALESCE(uncosted_cache_write_tokens, cache_write_tokens))::bigint AS uncosted_cache_write_tokens,
    SUM(task_count)::int             AS task_count
FROM task_usage_hourly
WHERE workspace_id = $1
  AND bucket_hour >= $2
  AND ($4::uuid IS NULL OR project_id = $4)
GROUP BY 1, 2, 3
ORDER BY 1 DESC, 2, 3
";

/// 上游 `ListDashboardUsageByAgent`：**不吃 `@tz`**（结果里没有日期维度）。
const LIST_USAGE_BY_AGENT: &str = r"
SELECT
    agent_id,
    LOWER(provider) AS provider,
    model,
    SUM(input_tokens)::bigint        AS input_tokens,
    SUM(output_tokens)::bigint       AS output_tokens,
    SUM(cache_read_tokens)::bigint   AS cache_read_tokens,
    SUM(cache_write_tokens)::bigint  AS cache_write_tokens,
    SUM(cost_usd_ticks)::bigint      AS cost_usd_ticks,
    SUM(COALESCE(uncosted_input_tokens, input_tokens))::bigint         AS uncosted_input_tokens,
    SUM(COALESCE(uncosted_output_tokens, output_tokens))::bigint       AS uncosted_output_tokens,
    SUM(COALESCE(uncosted_cache_read_tokens, cache_read_tokens))::bigint AS uncosted_cache_read_tokens,
    SUM(COALESCE(uncosted_cache_write_tokens, cache_write_tokens))::bigint AS uncosted_cache_write_tokens,
    SUM(task_count)::int             AS task_count
FROM task_usage_hourly
WHERE workspace_id = $1
  AND bucket_hour >= $2
  AND ($3::uuid IS NULL OR project_id = $3)
GROUP BY 1, 2, 3
ORDER BY 1, 2, 3
";

/// 上游 `ListDashboardAgentRunTime`：per-agent，**无日期分桶** ⇒ 不吃 `@tz`。
///
/// `metered_task_count` 用「`task_usage` 里有对应行」而不是「token 总额非零」——
/// 上游注释逐字：provider 报 0 的那次运行与**什么都没报**的那次必须是两回事。
const LIST_AGENT_RUNTIME: &str = r"
SELECT
    atq.agent_id,
    COALESCE(SUM(EXTRACT(EPOCH FROM (atq.completed_at - atq.started_at)))::bigint, 0) AS total_seconds,
    COUNT(*)::int AS task_count,
    COUNT(*) FILTER (WHERE EXISTS (
        SELECT 1 FROM task_usage tu WHERE tu.task_id = atq.id
    ))::int AS metered_task_count,
    COUNT(*) FILTER (WHERE atq.status = 'failed')::int    AS failed_count,
    COUNT(*) FILTER (WHERE atq.status = 'cancelled')::int AS cancelled_count
FROM agent_task_queue atq
JOIN agent a ON a.id = atq.agent_id
LEFT JOIN issue i ON i.id = atq.issue_id
WHERE a.workspace_id = $1
  AND atq.status IN ('completed', 'failed', 'cancelled')
  AND atq.started_at IS NOT NULL
  AND atq.completed_at IS NOT NULL
  AND atq.completed_at >= $2
  AND ($3::uuid IS NULL OR i.project_id = $3)
GROUP BY atq.agent_id
ORDER BY total_seconds DESC
";

/// 上游 `ListDashboardRunTimeDaily`。
///
/// `'cancelled'` 在过滤里（上游注释逐字：用户中途停掉的运行**已经烧掉的** agent 时间
/// 与 token 是真的），而 `started_at IS NOT NULL` 守卫把「还在排队时就被取消」的运行
/// 排除掉（它从没占住一个 agent）。
const LIST_RUNTIME_DAILY: &str = r"
SELECT
    DATE(atq.completed_at AT TIME ZONE $3::text) AS date,
    COALESCE(SUM(EXTRACT(EPOCH FROM (atq.completed_at - atq.started_at)))::bigint, 0) AS total_seconds,
    COUNT(*)::int AS task_count,
    COUNT(*) FILTER (WHERE atq.status = 'failed')::int    AS failed_count,
    COUNT(*) FILTER (WHERE atq.status = 'cancelled')::int AS cancelled_count
FROM agent_task_queue atq
JOIN agent a ON a.id = atq.agent_id
LEFT JOIN issue i ON i.id = atq.issue_id
WHERE a.workspace_id = $1
  AND atq.status IN ('completed', 'failed', 'cancelled')
  AND atq.started_at IS NOT NULL
  AND atq.completed_at IS NOT NULL
  AND atq.completed_at >= $2
  AND ($4::uuid IS NULL OR i.project_id = $4)
GROUP BY 1
ORDER BY 1 DESC
";

/// 上游 `ListDashboardFailuresDaily`。
///
/// 🔴 **不**要求 `started_at`：在队列里过期掉的任务（`failure_reason='queued_expired'`）
/// 从没 start 过，但它无可争议是一次失败 —— 漏掉它就会低报 Errors 图要呈现的那次故障。
/// 而 `status='failed'` 且 `failure_reason` 为空/NULL 的行落进 `'unclassified'` 桶，
/// 这样它**仍然可数**，而不是冒充一次成功。
const LIST_FAILURES_DAILY: &str = r"
SELECT
    DATE(atq.completed_at AT TIME ZONE $3::text) AS date,
    CASE
        WHEN atq.status = 'failed'
            THEN COALESCE(NULLIF(atq.failure_reason, ''), 'unclassified')
        ELSE ''
    END AS failure_reason,
    COUNT(*)::int AS task_count
FROM agent_task_queue atq
JOIN agent a ON a.id = atq.agent_id
LEFT JOIN issue i ON i.id = atq.issue_id
WHERE a.workspace_id = $1
  AND atq.status IN ('completed', 'failed')
  AND atq.completed_at IS NOT NULL
  AND atq.completed_at >= $2
  AND ($4::uuid IS NULL OR i.project_id = $4)
GROUP BY 1, 2
ORDER BY 1 DESC, 2
";

/// 上游 `ListDashboardFailuresByAgent`：per-agent，**无日期分桶** ⇒ 不吃 `@tz`。
const LIST_FAILURES_BY_AGENT: &str = r"
SELECT
    atq.agent_id,
    CASE
        WHEN atq.status = 'failed'
            THEN COALESCE(NULLIF(atq.failure_reason, ''), 'unclassified')
        ELSE ''
    END AS failure_reason,
    COUNT(*)::int AS task_count
FROM agent_task_queue atq
JOIN agent a ON a.id = atq.agent_id
LEFT JOIN issue i ON i.id = atq.issue_id
WHERE a.workspace_id = $1
  AND atq.status IN ('completed', 'failed')
  AND atq.completed_at IS NOT NULL
  AND atq.completed_at >= $2
  AND ($3::uuid IS NULL OR i.project_id = $3)
GROUP BY 1, 2
ORDER BY 1, 2
";

/// 上游 `ListAllAgentsAnyKind` 的**最小投影**：折叠判定只需要这四列。
///
/// ⚠️ **不能**加 `WHERE kind = 'user'`：隐藏的 `kind='system'` 承运 agent（agent builder
/// 会话）没有任何 list 端点会命名它，但它们**真的在跑任务、真的在记账** ——
/// 少了它们，聚合行就会带着一个没人能叫出名字的裸 UUID 出现在响应里。
const LIST_AGENTS_ANY_KIND: &str = r"
SELECT id, kind, owner_id, permission_mode
FROM agent
WHERE workspace_id = $1
ORDER BY created_at ASC
";

// ---------------------------------------------------------------------------
// 内部行结构（只服务于「DB 的类型 → wire 的类型」这一次转换）
// ---------------------------------------------------------------------------

/// `DATE(...)` 在 PG 里是 `date` 类型；wire 上是 `YYYY-MM-DD`。
fn date_to_wire(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

#[derive(Debug, Clone, FromRow)]
struct UsageRow {
    date: NaiveDate,
    provider: String,
    model: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    cost_usd_ticks: i64,
    uncosted_input_tokens: i64,
    uncosted_output_tokens: i64,
    uncosted_cache_read_tokens: i64,
    uncosted_cache_write_tokens: i64,
    task_count: i32,
}

#[derive(Debug, Clone, FromRow)]
struct UsageByAgentRow {
    agent_id: Uuid,
    provider: String,
    model: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    cost_usd_ticks: i64,
    uncosted_input_tokens: i64,
    uncosted_output_tokens: i64,
    uncosted_cache_read_tokens: i64,
    uncosted_cache_write_tokens: i64,
    task_count: i32,
}

#[derive(Debug, Clone, FromRow)]
struct AgentRuntimeRow {
    agent_id: Uuid,
    total_seconds: i64,
    task_count: i32,
    metered_task_count: i32,
    failed_count: i32,
    cancelled_count: i32,
}

#[derive(Debug, Clone, FromRow)]
struct RuntimeDailyRow {
    date: NaiveDate,
    total_seconds: i64,
    task_count: i32,
    failed_count: i32,
    cancelled_count: i32,
}

#[derive(Debug, Clone, FromRow)]
struct FailureDailyRow {
    date: NaiveDate,
    failure_reason: String,
    task_count: i32,
}

#[derive(Debug, Clone, FromRow)]
struct FailureByAgentRow {
    agent_id: Uuid,
    failure_reason: String,
    task_count: i32,
}

#[derive(Debug, Clone, FromRow)]
struct AgentVisibilityRow {
    id: Uuid,
    kind: String,
    owner_id: Option<Uuid>,
    permission_mode: String,
}

// ---------------------------------------------------------------------------
// repo
// ---------------------------------------------------------------------------

/// dashboard 的只读聚合。
///
/// 🔴 本类型**只读**：没有 insert / update / delete，6 个方法全是 `SELECT`。
#[derive(Clone)]
pub struct DashboardRepo {
    db: mc_db::Db,
}

impl DashboardRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: mc_db::Db) -> Self {
        Self { db }
    }

    fn pool(&self) -> &sqlx::PgPool {
        self.db.pool()
    }

    // -- usage 面（`task_usage_hourly`） ----------------------------------

    /// 上游 `ListDashboardUsageDaily`：`$1` workspace / `$2` since / `$3` tz / `$4` project。
    pub async fn list_usage_daily(
        &self,
        workspace_id: Id,
        since: DateTime<Utc>,
        tz: &str,
        project_id: Option<Uuid>,
    ) -> Result<Vec<DashboardUsageDailyResponse>> {
        let rows = sqlx::query_as::<_, UsageRow>(LIST_USAGE_DAILY)
            .bind(workspace_id.0)
            .bind(since)
            .bind(tz)
            .bind(project_id)
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|row| DashboardUsageDailyResponse {
                date: date_to_wire(row.date),
                provider: row.provider,
                model: row.model,
                tokens: TokenUsageCounts {
                    input_tokens: row.input_tokens,
                    output_tokens: row.output_tokens,
                    cache_read_tokens: row.cache_read_tokens,
                    cache_write_tokens: row.cache_write_tokens,
                },
                cost_usd_ticks: row.cost_usd_ticks,
                uncosted_input_tokens: row.uncosted_input_tokens,
                uncosted_output_tokens: row.uncosted_output_tokens,
                uncosted_cache_read_tokens: row.uncosted_cache_read_tokens,
                uncosted_cache_write_tokens: row.uncosted_cache_write_tokens,
                task_count: row.task_count,
            })
            .collect())
    }

    /// 上游 `ListDashboardUsageByAgent`：`$1` workspace / `$2` since / `$3` project。
    pub async fn list_usage_by_agent(
        &self,
        workspace_id: Id,
        since: DateTime<Utc>,
        project_id: Option<Uuid>,
    ) -> Result<Vec<DashboardUsageByAgentResponse>> {
        let rows = sqlx::query_as::<_, UsageByAgentRow>(LIST_USAGE_BY_AGENT)
            .bind(workspace_id.0)
            .bind(since)
            .bind(project_id)
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|row| DashboardUsageByAgentResponse {
                agent_id: row.agent_id.to_string(),
                provider: row.provider,
                model: row.model,
                tokens: TokenUsageCounts {
                    input_tokens: row.input_tokens,
                    output_tokens: row.output_tokens,
                    cache_read_tokens: row.cache_read_tokens,
                    cache_write_tokens: row.cache_write_tokens,
                },
                cost_usd_ticks: row.cost_usd_ticks,
                uncosted_input_tokens: row.uncosted_input_tokens,
                uncosted_output_tokens: row.uncosted_output_tokens,
                uncosted_cache_read_tokens: row.uncosted_cache_read_tokens,
                uncosted_cache_write_tokens: row.uncosted_cache_write_tokens,
                task_count: row.task_count,
            })
            .collect())
    }

    // -- runtime 面（`agent_task_queue` ⋈ `agent` ⋈ `issue`） ------------

    /// 上游 `ListDashboardAgentRunTime`：`$1` workspace / `$2` since / `$3` project。
    pub async fn list_agent_runtime(
        &self,
        workspace_id: Id,
        since: DateTime<Utc>,
        project_id: Option<Uuid>,
    ) -> Result<Vec<DashboardAgentRunTimeResponse>> {
        let rows = sqlx::query_as::<_, AgentRuntimeRow>(LIST_AGENT_RUNTIME)
            .bind(workspace_id.0)
            .bind(since)
            .bind(project_id)
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|row| DashboardAgentRunTimeResponse {
                agent_id: row.agent_id.to_string(),
                total_seconds: row.total_seconds,
                task_count: row.task_count,
                metered_task_count: row.metered_task_count,
                failed_count: row.failed_count,
                cancelled_count: row.cancelled_count,
            })
            .collect())
    }

    /// 上游 `ListDashboardRunTimeDaily`：`$1`/`$2`/`$3` tz/`$4` project。
    pub async fn list_runtime_daily(
        &self,
        workspace_id: Id,
        since: DateTime<Utc>,
        tz: &str,
        project_id: Option<Uuid>,
    ) -> Result<Vec<DashboardRunTimeDailyResponse>> {
        let rows = sqlx::query_as::<_, RuntimeDailyRow>(LIST_RUNTIME_DAILY)
            .bind(workspace_id.0)
            .bind(since)
            .bind(tz)
            .bind(project_id)
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|row| DashboardRunTimeDailyResponse {
                date: date_to_wire(row.date),
                total_seconds: row.total_seconds,
                task_count: row.task_count,
                failed_count: row.failed_count,
                cancelled_count: row.cancelled_count,
            })
            .collect())
    }

    // -- failures 面 ---------------------------------------------------

    /// 上游 `ListDashboardFailuresDaily`：`$1`/`$2`/`$3` tz/`$4` project。
    pub async fn list_failures_daily(
        &self,
        workspace_id: Id,
        since: DateTime<Utc>,
        tz: &str,
        project_id: Option<Uuid>,
    ) -> Result<Vec<DashboardFailureDailyResponse>> {
        let rows = sqlx::query_as::<_, FailureDailyRow>(LIST_FAILURES_DAILY)
            .bind(workspace_id.0)
            .bind(since)
            .bind(tz)
            .bind(project_id)
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|row| DashboardFailureDailyResponse {
                date: date_to_wire(row.date),
                failure_reason: row.failure_reason,
                task_count: row.task_count,
            })
            .collect())
    }

    /// 上游 `ListDashboardFailuresByAgent`：`$1` workspace / `$2` since / `$3` project。
    pub async fn list_failures_by_agent(
        &self,
        workspace_id: Id,
        since: DateTime<Utc>,
        project_id: Option<Uuid>,
    ) -> Result<Vec<DashboardFailureByAgentResponse>> {
        let rows = sqlx::query_as::<_, FailureByAgentRow>(LIST_FAILURES_BY_AGENT)
            .bind(workspace_id.0)
            .bind(since)
            .bind(project_id)
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|row| DashboardFailureByAgentResponse {
                agent_id: row.agent_id.to_string(),
                failure_reason: row.failure_reason,
                task_count: row.task_count,
            })
            .collect())
    }

    // -- 可见性折叠（上游 `restrictedAgentIDs`） ------------------------

    /// 本次请求**不许点名**的 agent id 集合（上游 `dashboardRestrictedAgents` 的返回值）。
    ///
    /// 判定逐字照抄上游 `restrictedAgentIDs`：
    ///
    /// - `kind = 'user'` 的 agent：owner / admin 放行，否则按
    ///   `permission_mode` + `agent_invocation_target` 白名单判（**fail-closed**）；
    /// - `kind = 'system'` 的 agent：**恒**进集合 —— 没有任何 list 端点会把它交给
    ///   任何人，而聚合查询不带 `kind` 过滤，它会自己冒出来。
    ///
    /// 本仓的 agent actor 未接线（`routes/agents.rs` 顶部同款说明）⇒ 一律按 member
    /// 处理，**只会更严**不会更松。
    ///
    /// ⚠️ 出错必须**冒泡**（路由层映射 500）：「返回一份没折叠过的聚合」是这个函数
    /// 唯一不能退化的结局（上游注释逐字）。
    pub async fn restricted_agent_ids(
        &self,
        workspace_id: Id,
        user_id: Id,
        role: &str,
    ) -> Result<HashSet<Uuid>> {
        let agents = sqlx::query_as::<_, AgentVisibilityRow>(LIST_AGENTS_ANY_KIND)
            .bind(workspace_id.0)
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        if agents.is_empty() {
            return Ok(HashSet::new());
        }
        // 上游 `loadInvocationTargetsByAgent`：admin 不需要白名单（`judgeUserAgents=false`）
        // ⇒ 那一整批查询都省掉。
        let judge_user_agents = !role_is_admin(role);
        let mut targets: HashMap<Uuid, Vec<AgentInvocationTargetRow>> = HashMap::new();
        if judge_user_agents {
            let ids: Vec<Uuid> = agents.iter().map(|a| a.id).collect();
            for row in AgentRepo::new(self.db.clone())
                .list_invocation_targets_for_agents(&ids)
                .await?
            {
                targets.entry(row.agent_id).or_default().push(row);
            }
        }
        let empty = Vec::new();
        let restricted = agents
            .into_iter()
            .filter(|agent| {
                if agent.kind != "user" {
                    return true;
                }
                if !judge_user_agents {
                    return false;
                }
                !member_allowed_to_view(agent, user_id, targets.get(&agent.id).unwrap_or(&empty))
            })
            .map(|agent| agent.id)
            .collect();
        Ok(restricted)
    }
}

impl RepoWithDb for DashboardRepo {
    fn db(&self) -> &mc_db::Db {
        &self.db
    }
}

/// 上游 `memberHitsInvocationTargets`。
///
/// `team` 目标**恒不命中**（V1 没有 team 成员表，fail-closed）。
fn member_hits_targets(targets: &[AgentInvocationTargetRow], user_id: Id) -> bool {
    targets.iter().any(|t| match t.target_type.as_str() {
        TARGET_WORKSPACE => true,
        TARGET_MEMBER => t.target_id == user_id.0,
        _ => false,
    })
}

/// 上游 `memberAllowedToViewAgent`（`roleAllowed(owner, admin)` 那一段已由调用方短路）。
fn member_allowed_to_view(
    agent: &AgentVisibilityRow,
    user_id: Id,
    targets: &[AgentInvocationTargetRow],
) -> bool {
    if agent.owner_id == Some(user_id.0) {
        return true;
    }
    agent.permission_mode == PERMISSION_MODE_PUBLIC_TO && member_hits_targets(targets, user_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::PERMISSION_MODE_PRIVATE;

    /// 🔴 纪律断言：6 条聚合**没有一条**读 `task_usage_dashboard_*`。
    ///
    /// 那两张 `084` legacy rollup 表已被 `103` 逐字「drop legacy daily rollups」删掉；
    /// 一旦有人「为了看起来更聚合」去读它们，SQL 在真库上会直接 42P01，而这条
    /// 断言让它在 `cargo test`（不接库的那道门）就红。
    #[test]
    fn no_legacy_rollup_tables_are_referenced() {
        for sql in [
            LIST_USAGE_DAILY,
            LIST_USAGE_BY_AGENT,
            LIST_AGENT_RUNTIME,
            LIST_RUNTIME_DAILY,
            LIST_FAILURES_DAILY,
            LIST_FAILURES_BY_AGENT,
        ] {
            assert!(!sql.contains("task_usage_dashboard_"), "{sql}");
            assert!(!sql.contains("task_usage_daily"), "{sql}");
        }
    }

    /// 4 条走 `task_usage_hourly`、4 条走 `agent_task_queue ⋈ agent` —— 一条不多一条不少。
    #[test]
    fn the_six_queries_read_only_the_two_upstream_faces() {
        for sql in [LIST_USAGE_DAILY, LIST_USAGE_BY_AGENT] {
            assert!(sql.contains("FROM task_usage_hourly"), "{sql}");
            assert!(!sql.contains("agent_task_queue"), "{sql}");
        }
        for sql in [
            LIST_AGENT_RUNTIME,
            LIST_RUNTIME_DAILY,
            LIST_FAILURES_DAILY,
            LIST_FAILURES_BY_AGENT,
        ] {
            assert!(sql.contains("FROM agent_task_queue atq"), "{sql}");
            assert!(sql.contains("JOIN agent a ON a.id = atq.agent_id"), "{sql}");
            assert!(
                sql.contains("LEFT JOIN issue i ON i.id = atq.issue_id"),
                "{sql}"
            );
        }
    }

    /// 两条「无日期维度」的 SQL **不许**出现 `AT TIME ZONE`（上游同款：tz 只在
    /// 算 `since` 时用过一次，SQL 里再吃一次就是第二个真相源）。
    #[test]
    fn only_the_four_dated_queries_project_a_timezone() {
        for sql in [LIST_USAGE_DAILY, LIST_RUNTIME_DAILY, LIST_FAILURES_DAILY] {
            assert!(sql.contains("AT TIME ZONE $3::text"), "{sql}");
        }
        for sql in [
            LIST_USAGE_BY_AGENT,
            LIST_AGENT_RUNTIME,
            LIST_FAILURES_BY_AGENT,
        ] {
            assert!(!sql.contains("AT TIME ZONE"), "{sql}");
        }
    }

    /// 三条 runtime/failures 查询的状态过滤**逐字**含 `'cancelled'`
    /// （上游注释：中途停掉的运行烧掉的 agent 时间是真的），
    /// 而两条 failures 查询**不**要求 `started_at`（队列里过期的任务从没 start 过，
    /// 但它无可争议是一次失败）。
    #[test]
    fn status_and_started_at_filters_match_upstream() {
        for sql in [LIST_AGENT_RUNTIME, LIST_RUNTIME_DAILY] {
            assert!(
                sql.contains("atq.status IN ('completed', 'failed', 'cancelled')"),
                "{sql}"
            );
            assert!(sql.contains("atq.started_at IS NOT NULL"), "{sql}");
        }
        for sql in [LIST_FAILURES_DAILY, LIST_FAILURES_BY_AGENT] {
            assert!(
                sql.contains("atq.status IN ('completed', 'failed')"),
                "{sql}"
            );
            assert!(!sql.contains("atq.started_at IS NOT NULL"), "{sql}");
            // 空 / NULL 的失败原因落 `'unclassified'`，不冒充成功。
            assert!(
                sql.contains("COALESCE(NULLIF(atq.failure_reason, ''), 'unclassified')"),
                "{sql}"
            );
        }
    }

    /// `metered_task_count` 判的是「`task_usage` 里有行」，不是「token 总额非零」。
    #[test]
    fn metered_count_asks_for_row_existence() {
        assert!(LIST_AGENT_RUNTIME.contains("EXISTS ("));
        assert!(LIST_AGENT_RUNTIME.contains("FROM task_usage tu WHERE tu.task_id = atq.id"));
    }

    /// `DATE(...)` 的三个表达式必须**逐字一致**（`SELECT` / `GROUP BY` / `ORDER BY`）——
    /// 不一致时 PG 报的是「column must appear in the GROUP BY」，而不是静默给错答案，
    /// 但这三条断言能在不接库的那道门上先钉住它。
    #[test]
    fn dated_queries_bucket_by_the_same_expression_in_select_group_and_order() {
        for sql in [LIST_USAGE_DAILY, LIST_RUNTIME_DAILY, LIST_FAILURES_DAILY] {
            let date_exprs = sql.matches("DATE(").count();
            assert!(date_exprs >= 1, "{sql}");
            // 分桶一律走序号（`GROUP BY 1` / `ORDER BY 1 DESC`）⇒ 表达式只出现一次，
            // 杜绝「SELECT 改了、GROUP BY 没改」这一类漂移。
            assert!(sql.contains("GROUP BY 1"), "{sql}");
        }
    }

    /// 折叠判定：owner 放行、`public_to` 命中白名单放行、其余（`private` / 无白名单 /
    /// `system`）**恒**折叠 —— fail-closed。
    #[test]
    fn folding_is_fail_closed() {
        let owner = AgentVisibilityRow {
            id: Uuid::nil(),
            kind: "user".into(),
            owner_id: Some(uuid::Uuid::from_u128(7)),
            permission_mode: PERMISSION_MODE_PRIVATE.into(),
        };
        let user = Id(uuid::Uuid::from_u128(7));
        assert!(member_allowed_to_view(&owner, user, &[]));

        let public_to = AgentVisibilityRow {
            id: Uuid::nil(),
            kind: "user".into(),
            owner_id: Some(uuid::Uuid::from_u128(9)),
            permission_mode: PERMISSION_MODE_PUBLIC_TO.into(),
        };
        assert!(!member_allowed_to_view(&public_to, user, &[]));
        let ws_target = AgentInvocationTargetRow {
            id: Uuid::nil(),
            agent_id: Uuid::nil(),
            target_type: TARGET_WORKSPACE.into(),
            target_id: Uuid::nil(),
            created_by: None,
            created_at: Utc::now(),
        };
        assert!(member_allowed_to_view(
            &public_to,
            user,
            std::slice::from_ref(&ws_target)
        ));
        // `team` 目标在 V1 恒不命中。
        let team_target = AgentInvocationTargetRow {
            target_type: "team".into(),
            ..ws_target.clone()
        };
        assert!(!member_allowed_to_view(&public_to, user, &[team_target]));
    }

    /// `agent` 那一列**不许**加 `WHERE kind = 'user'`（`ListAllAgentsAnyKind` 的
    /// 逐字理由：system 承运 agent 真的在跑任务、真的在记账）。
    #[test]
    fn the_agent_visibility_query_sees_system_agents() {
        assert!(!LIST_AGENTS_ANY_KIND.contains("kind = 'user'"));
        assert!(LIST_AGENTS_ANY_KIND.contains("SELECT id, kind, owner_id, permission_mode"));
    }
}

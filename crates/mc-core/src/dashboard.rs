//! dashboard 面的**行形状与查询口径**（上游 `internal/handler/dashboard.go`，655 行；
//! `docs/62` §4.1 的 `M9-4`）。
//!
//! # 6 条路由的**真实数据源**（`docs/62` §9.6 逐条实测）
//!
//! | 路由 | 表 | 本模块对应的行结构 |
//! | --- | --- | --- |
//! | `GET /api/dashboard/usage/daily` | `task_usage_hourly` | [`DashboardUsageDailyResponse`] |
//! | `GET /api/dashboard/usage/by-agent` | `task_usage_hourly` | [`DashboardUsageByAgentResponse`] |
//! | `GET /api/dashboard/agent-runtime` | `agent_task_queue` ⋈ `agent` ⋈ `issue` | [`DashboardAgentRunTimeResponse`] |
//! | `GET /api/dashboard/runtime/daily` | 同上 | [`DashboardRunTimeDailyResponse`] |
//! | `GET /api/dashboard/failures/daily` | 同上 | [`DashboardFailureDailyResponse`] |
//! | `GET /api/dashboard/failures/by-agent` | 同上 | [`DashboardFailureByAgentResponse`] |
//!
//! 🔴 **6 条里 0 条读 `task_usage_dashboard_*`** —— 那两张 rollup 表（`084`）的管道已被
//! `101`/`103` 的 hourly 化取代（`103_drop_legacy_daily_rollups.up.sql` 逐字
//! 「drop legacy daily rollups」）。**不许**为了"看起来更聚合"去读它们。
//!
//! # 四条口径（逐条可测，写进 `M9-4` 的 `DoD`）
//!
//! 1. **`?days=N`：默认 30、接受 `1..=365`，非法值静默回落到默认**（上游
//!    `parseDaysCutoff`：`if err == nil && parsed > 0 && parsed <= 365 { days = parsed }`
//!    —— 没有 400 分支）。⚠️ 计划文本（`docs/62` §6.5 的 M9-4 行）写的是「非法值 400」，
//!    与上游不符；本模块按**上游**实现，差异登记 `docs/32` §9.13。
//! 2. **`?tz=`：先看查询参数、再回落用户存储的 tz、最后 `UTC`**；非法 tz **不报错**
//!    （上游逐字：「invalid values fall through rather than erroring — tz is a display
//!    concern」）。
//! 3. **两半 cutoff**：日期分桶的 4 条用 **N+1 天**（多一天 headroom，客户端用
//!    `-(days-1)` 裁掉）；**没有日期维度**的聚合（`agent-runtime`）用**恰好 N 天**
//!    —— 否则它会比旁边的图多算一天（上游 `parseExactSinceParamInTZ` 的注释逐字）。
//! 4. **可见性折叠**：3 条 per-agent 路由必须把**私有** agent 折叠到哨兵
//!    [`RESTRICTED_AGENTS_ROW_ID`] 且**合并不丢总额**（上游：「client-side filtering is
//!    decoration: one curl bypasses it」⇒ 必须在**服务端**折叠）。

use serde::{Deserialize, Serialize};

/// `?days=` 的默认值（上游 6 条路由 모두 30）。
pub const DEFAULT_DAYS: u32 = 30;
/// `?days=` 的上界（上游逐字 `parsed <= 365`）。
pub const MAX_DAYS: u32 = 365;
/// `?days=` 的下界（上游逐字 `parsed > 0`）。
pub const MIN_DAYS: u32 = 1;

/// 私有 agent 折叠后的哨兵行 id（上游 `restrictedAgentsRowID`）。
pub const RESTRICTED_AGENTS_ROW_ID: &str = "__restricted_agents__";

/// `?tz` 缺失且用户没存 tz 时的兜底（上游 `time.UTC`）。
pub const DEFAULT_TIMEZONE: &str = "UTC";

/// 上游 `parseDaysCutoff` 的 `days` 解析：**非法值静默回落**（不报错）。
///
/// - 未给参数 ⇒ [`DEFAULT_DAYS`]；
/// - 给了但解析失败 / `<= 0` / `> 365` ⇒ [`DEFAULT_DAYS`]（上游逐字，没有 400 分支）。
#[must_use]
pub fn resolve_days(raw: Option<&str>) -> u32 {
    let Some(raw) = raw else {
        return DEFAULT_DAYS;
    };
    if raw.is_empty() {
        return DEFAULT_DAYS;
    }
    match raw.parse::<i64>() {
        Ok(parsed) if parsed >= i64::from(MIN_DAYS) && parsed <= i64::from(MAX_DAYS) => {
            u32::try_from(parsed).unwrap_or(DEFAULT_DAYS)
        }
        _ => DEFAULT_DAYS,
    }
}

/// 某条路由要用哪一半 cutoff（`docs/62` §9.6 的表格）。
///
/// 日期分桶的 4 条 ⇒ [`CutoffConvention::HeadroomDay`]（N+1 天）；
/// 无日期维度的聚合（`agent-runtime`）⇒ [`CutoffConvention::ExactDays`]（恰好 N 天）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutoffConvention {
    /// N+1 个日历日（多一天 headroom；客户端 `-(days-1)` 裁掉）。
    HeadroomDay,
    /// 恰好 N 个日历日（没有日期维度可用来自裁的聚合只能用这个）。
    ExactDays,
}

impl CutoffConvention {
    /// 上游 `parseDaysCutoff` 的 `trimDays` 实参：headroom 是 `0`，exact 是 `1`。
    #[must_use]
    pub const fn trim_days(self) -> u32 {
        match self {
            Self::HeadroomDay => 0,
            Self::ExactDays => 1,
        }
    }

    /// 每个 dashboard 路由的约定（上游逐条点名的 4 + 2 分布）。
    #[must_use]
    pub const fn for_route(route: DashboardRoute) -> Self {
        match route {
            DashboardRoute::UsageDaily
            | DashboardRoute::UsageByAgent
            | DashboardRoute::RuntimeDaily
            | DashboardRoute::FailuresDaily
            | DashboardRoute::FailuresByAgent => Self::HeadroomDay,
            DashboardRoute::AgentRunTime => Self::ExactDays,
        }
    }
}

/// dashboard 的 6 条路由（顺序与 `docs/fixtures/upstream-routes.tsv` 的 M9 行一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DashboardRoute {
    /// `GET /api/dashboard/agent-runtime`
    AgentRunTime,
    /// `GET /api/dashboard/failures/by-agent`
    FailuresByAgent,
    /// `GET /api/dashboard/failures/daily`
    FailuresDaily,
    /// `GET /api/dashboard/runtime/daily`
    RuntimeDaily,
    /// `GET /api/dashboard/usage/by-agent`
    UsageByAgent,
    /// `GET /api/dashboard/usage/daily`
    UsageDaily,
}

impl DashboardRoute {
    /// 全部 6 条。
    pub const ALL: [Self; 6] = [
        Self::AgentRunTime,
        Self::FailuresByAgent,
        Self::FailuresDaily,
        Self::RuntimeDaily,
        Self::UsageByAgent,
        Self::UsageDaily,
    ];

    /// 本路由是否 per-agent（⇒ 必须做 [`RESTRICTED_AGENTS_ROW_ID`] 折叠）。
    #[must_use]
    pub const fn is_per_agent(self) -> bool {
        matches!(
            self,
            Self::UsageByAgent | Self::AgentRunTime | Self::FailuresByAgent
        )
    }
}

/// 一组「四类 token」计数（三条 usage 行形状共用）。
///
/// 上游把「计过价的」与「没计过价的」**分成两列组**（`cost_usd_ticks` 是提供方真收的钱，
/// `uncosted_*` 是它**没有**定价的那些行）⇒ 客户端报「权威值 + 估算（uncosted）」，
/// 一个混合窗口因此仍然完整。本结构把两组都带上，**不许**在服务端合并它们。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsageCounts {
    /// 输入 token。
    pub input_tokens: i64,
    /// 输出 token。
    pub output_tokens: i64,
    /// cache 读 token。
    pub cache_read_tokens: i64,
    /// cache 写 token。
    pub cache_write_tokens: i64,
}

/// `GET /api/dashboard/usage/daily` 的一行（上游 `DashboardUsageDailyResponse`）。
///
/// 粒度 = `(日期, provider, model)`。provider 与 model 都留在 wire 上，因为**不同 provider
/// 的裸 model id 会撞名**（上游逐字点名 Cursor 的 `auto`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardUsageDailyResponse {
    /// `YYYY-MM-DD`（在**看的人的时区**里分桶）。
    pub date: String,
    /// provider（小写，上游 `LOWER(provider)`）。
    pub provider: String,
    /// model。
    pub model: String,
    /// 计过价的四类 token。
    #[serde(flatten)]
    pub tokens: TokenUsageCounts,
    /// 提供方**真收**的钱（1e-10 USD 为单位的整数 tick）。
    pub cost_usd_ticks: i64,
    /// 没被定价的输入 token。
    pub uncosted_input_tokens: i64,
    /// 没被定价的输出 token。
    pub uncosted_output_tokens: i64,
    /// 没被定价的 cache 读 token。
    pub uncosted_cache_read_tokens: i64,
    /// 没被定价的 cache 写 token。
    pub uncosted_cache_write_tokens: i64,
    /// 落进这一桶的任务数。
    pub task_count: i32,
}

/// `GET /api/dashboard/usage/by-agent` 的一行（上游 `DashboardUsageByAgentResponse`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardUsageByAgentResponse {
    /// agent id（**服务端折叠后**可能是 [`RESTRICTED_AGENTS_ROW_ID`]）。
    pub agent_id: String,
    /// provider。
    pub provider: String,
    /// model。
    pub model: String,
    /// 计过价的四类 token。
    #[serde(flatten)]
    pub tokens: TokenUsageCounts,
    /// 提供方真收的钱。
    pub cost_usd_ticks: i64,
    /// 没被定价的输入 token。
    pub uncosted_input_tokens: i64,
    /// 没被定价的输出 token。
    pub uncosted_output_tokens: i64,
    /// 没被定价的 cache 读 token。
    pub uncosted_cache_read_tokens: i64,
    /// 没被定价的 cache 写 token。
    pub uncosted_cache_write_tokens: i64,
    /// 任务数。
    pub task_count: i32,
}

/// `GET /api/dashboard/agent-runtime` 的一行（上游 `DashboardAgentRunTimeResponse`）。
///
/// ⚠️ `failed_count` 与 `cancelled_count` 是 `task_count` 的**不相交子集** ⇒ 成功数由客户端
/// 用**余数**推（不要在这里再给一个 `succeeded_count` 字段，那会造出第二个真相源）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardAgentRunTimeResponse {
    /// agent id（**服务端折叠后**可能是 [`RESTRICTED_AGENTS_ROW_ID`]）。
    pub agent_id: String,
    /// 终态任务的运行秒数之和（`completed_at - started_at`）。
    pub total_seconds: i64,
    /// 任务数。
    pub task_count: i32,
    /// **计过费**的任务数（`EXISTS (SELECT 1 FROM task_usage …)`）。
    pub metered_task_count: i32,
    /// 失败数（`task_count` 的子集）。
    pub failed_count: i32,
    /// 取消数（`task_count` 的子集）。
    pub cancelled_count: i32,
}

/// `GET /api/dashboard/runtime/daily` 的一行（上游 `DashboardRunTimeDailyResponse`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardRunTimeDailyResponse {
    /// `YYYY-MM-DD`。
    pub date: String,
    /// 终态任务运行秒数之和。
    pub total_seconds: i64,
    /// 任务数。
    pub task_count: i32,
    /// 失败数。
    pub failed_count: i32,
    /// 取消数。
    pub cancelled_count: i32,
}

/// `GET /api/dashboard/failures/daily` 的一行（上游 `DashboardFailureDailyResponse`）。
///
/// ⚠️ `failure_reason == ""` 是**成功**桶（不是"未知原因"）—— 上游注释逐字。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardFailureDailyResponse {
    /// `YYYY-MM-DD`。
    pub date: String,
    /// 失败原因（空串 = 成功桶）。
    pub failure_reason: String,
    /// 任务数。
    pub task_count: i32,
}

/// `GET /api/dashboard/failures/by-agent` 的一行（上游 `DashboardFailureByAgentResponse`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardFailureByAgentResponse {
    /// agent id（**服务端折叠后**可能是 [`RESTRICTED_AGENTS_ROW_ID`]）。
    pub agent_id: String,
    /// 失败原因（空串 = 成功桶）。
    pub failure_reason: String,
    /// 任务数。
    pub task_count: i32,
}

/// 上游 `sinceFromDays` 的口径提示：cutoff = 「**今天**（看的人的时区）往前 `days` 个日历日
/// 的起点」，`days` 已经是「含今天在内」的日历天数。
///
/// 本函数存在的理由是把**一处**算术钉住（`M9-4` 的 6 条 SQL 都用它算 cutoff），
/// 而不是让 6 条 SQL 各自拼 `INTERVAL`。
///
/// ⚠️ 它只回答「日历天数的口径」，**不**做时区换算（那是 SQL 的
/// `AT TIME ZONE $tz` 与 `chrono_tz` 的事）。
#[must_use]
pub fn cutoff_days_with_convention(days: u32, convention: CutoffConvention) -> u32 {
    days.saturating_sub(convention.trim_days()).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_param_matches_upstream_silent_fallback() {
        assert_eq!(resolve_days(None), DEFAULT_DAYS);
        assert_eq!(resolve_days(Some("")), DEFAULT_DAYS);
        assert_eq!(resolve_days(Some("1")), 1);
        assert_eq!(resolve_days(Some("30")), 30);
        assert_eq!(resolve_days(Some("365")), 365);
        // 越界 / 非法 ⇒ **回落**到默认（上游没有 400 分支 —— 计划文本写 400 是错的）。
        for invalid in ["0", "-1", "366", "1000", "abc", "1.5", " 30", "30 ", "+30"] {
            assert_eq!(resolve_days(Some(invalid)), DEFAULT_DAYS, "{invalid}");
        }
    }

    #[test]
    fn the_six_routes_use_the_two_cutoff_halves() {
        assert_eq!(DashboardRoute::ALL.len(), 6);
        let exact: Vec<_> = DashboardRoute::ALL
            .into_iter()
            .filter(|route| CutoffConvention::for_route(*route) == CutoffConvention::ExactDays)
            .collect();
        assert_eq!(exact, vec![DashboardRoute::AgentRunTime]);
        assert_eq!(CutoffConvention::HeadroomDay.trim_days(), 0);
        assert_eq!(CutoffConvention::ExactDays.trim_days(), 1);
        // days=1 时降级到「只有今天」而不是 0（上游逐字注释）。
        assert_eq!(
            cutoff_days_with_convention(1, CutoffConvention::ExactDays),
            1
        );
        assert_eq!(
            cutoff_days_with_convention(1, CutoffConvention::HeadroomDay),
            1
        );
        assert_eq!(
            cutoff_days_with_convention(30, CutoffConvention::ExactDays),
            29
        );
        assert_eq!(
            cutoff_days_with_convention(30, CutoffConvention::HeadroomDay),
            30
        );
    }

    #[test]
    fn exactly_three_routes_are_per_agent_and_need_folding() {
        let per_agent: Vec<_> = DashboardRoute::ALL
            .into_iter()
            .filter(|route| route.is_per_agent())
            .collect();
        assert_eq!(
            per_agent,
            vec![
                DashboardRoute::AgentRunTime,
                DashboardRoute::FailuresByAgent,
                DashboardRoute::UsageByAgent
            ]
        );
        assert_eq!(RESTRICTED_AGENTS_ROW_ID, "__restricted_agents__");
    }

    #[test]
    fn row_shapes_serialize_with_the_upstream_field_names() {
        let row = DashboardUsageDailyResponse {
            date: "2026-09-26".into(),
            provider: "anthropic".into(),
            model: "claude".into(),
            tokens: TokenUsageCounts {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_tokens: 3,
                cache_write_tokens: 4,
            },
            cost_usd_ticks: 123,
            uncosted_input_tokens: 1,
            uncosted_output_tokens: 2,
            uncosted_cache_read_tokens: 0,
            uncosted_cache_write_tokens: 0,
            task_count: 7,
        };
        let json = serde_json::to_value(&row).expect("serialize");
        // `#[serde(flatten)]` ⇒ 四类 token 是**平铺**字段（不是嵌套对象）。
        assert_eq!(json["input_tokens"], serde_json::json!(10));
        assert_eq!(json["cache_write_tokens"], serde_json::json!(4));
        assert_eq!(json["cost_usd_ticks"], serde_json::json!(123));
        assert_eq!(json["uncosted_output_tokens"], serde_json::json!(2));
        assert!(json.get("tokens").is_none());
        assert_eq!(json["task_count"], serde_json::json!(7));

        let runtime = DashboardAgentRunTimeResponse {
            agent_id: RESTRICTED_AGENTS_ROW_ID.into(),
            total_seconds: 90,
            task_count: 3,
            metered_task_count: 2,
            failed_count: 1,
            cancelled_count: 1,
        };
        let json = serde_json::to_value(&runtime).expect("serialize");
        assert_eq!(json["agent_id"], serde_json::json!("__restricted_agents__"));
        // `failed_count` / `cancelled_count` 是 `task_count` 的子集（这里 1+1 ≤ 3）。
        assert!(json["failed_count"].as_i64().unwrap() <= 3);
    }

    #[test]
    fn failure_rows_use_the_empty_string_as_the_success_bucket() {
        let success = DashboardFailureDailyResponse {
            date: "2026-09-26".into(),
            failure_reason: String::new(),
            task_count: 5,
        };
        assert_eq!(success.failure_reason, "");
        let failure = DashboardFailureByAgentResponse {
            agent_id: "a".into(),
            failure_reason: "timeout".into(),
            task_count: 1,
        };
        assert_ne!(failure.failure_reason, "");
        assert_eq!(DEFAULT_TIMEZONE, "UTC");
        assert_eq!(MAX_DAYS, 365);
        assert_eq!(MIN_DAYS, 1);
    }
}

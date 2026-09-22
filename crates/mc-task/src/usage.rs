//! usage 结算：token / 时长 → `task_usage` 行与汇总（纯计算）。
//!
//! # 上游真值
//!
//! | 事实 | 真值 |
//! |---|---|
//! | 上报载荷 | `handler/daemon.go:4791` `TaskUsagePayload`（`{"usage":[...]}`） |
//! | provider 规范化 | `daemon.go:294` `normalizeProvider` = `ToLower(TrimSpace(s))` |
//! | 空 provider 回填 | `daemon.go:4837`：老 daemon 不带 provider ⇒ 从 task 的 runtime 取 |
//! | 成本是否权威 | `daemon.go:4804` `authoritativeCostTicks`：`<= 0` ⇒ `NULL` |
//! | 落库语义 | `task_usage.sql:1` `UpsertTaskUsage`（**覆盖**，不是累加） |
//! | 行形状 | `task_usage`（`032` + `213`）：11 列，`UNIQUE (task_id, provider, model)` |
//! | 汇总 | `task_usage.sql:80` `GetIssueUsageSummary`、`ListDashboardRunTimeDaily` |
//! | 小时汇总键/公式 | `migrations/102` + `213`（`task_usage_hour_bucket` = UTC 整点） |
//! | 失败分桶 | `ListDashboardFailuresDaily` 的 `CASE` |
//!
//! # 三个容易搞反的点
//!
//! 1. **token 是覆盖不是累加**：同一个 `(task_id, provider, model)` 再报一次就是
//!    `DO UPDATE SET input_tokens = EXCLUDED.input_tokens` —— 修正后的报告要替换
//!    旧数字，而不是叠加。`updated_at` 在 INSERT 与冲突两侧都刷新，因为小时汇总
//!    靠它认「脏行」。
//! 2. **`cost_usd_ticks = 0` 不是「花了 0 美元」**：单位是 `1e-10 USD`，且
//!    「不知道成本」的 daemon 也发 0。存 0 会冒充真实零花费并**压掉**按价目表
//!    估算，所以 `<= 0` 一律落 `NULL`；读侧看到 `NULL` 才知道要用估算。
//! 3. **`uncosted_*` 是「没被 provider 定价的那部分 token」**，与
//!    `cost_usd_ticks`（只有 provider 定价过的行才累加）互补：两者相加才是全部
//!    token。小时汇总把两者分开存，读侧 `COALESCE(uncosted_*, *)` 把历史
//!    `NULL` 行保守地当作全未定价。
//!
//! # 范围
//!
//! 只做纯计算：行形状、覆盖语义、汇总公式、小时聚合公式、失败分桶。**不写 SQL**：
//! 脏队列（`task_usage_hourly_dirty`）、plpgsql 汇总函数、`272` 的事务锁都是
//! M3-6 适配层的事；成本**定价**（按 model 的价目表）在客户端做，本 crate 不猜价格。

use serde::{Deserialize, Serialize};

use crate::retry::FailureReason;
use crate::status::TaskStatus;
use mc_core::{Id, Timestamp};

/// `cost_usd_ticks` 的单位：1 tick = `1e-10` USD（`daemon.go:4797`）。
///
/// 用整数 tick 而不是浮点，是为了让 provider 报来的成本在库里是精确值。
pub const COST_TICKS_PER_USD: i64 = 10_000_000_000;

/// 一小时多少秒（`task_usage_hour_bucket` 的桶宽）。
pub const SECS_PER_HOUR: i64 = 3_600;

/// `task_usage_hour_bucket(ts)`（`migrations/102`）：**UTC** 整点截断。
///
/// 上游函数体是 `date_trunc('hour', ts AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'`
/// —— 显式 UTC，不跟会话时区走（读侧才按观众时区切日历天）。
#[must_use]
pub fn hour_bucket(ts: Timestamp) -> Timestamp {
    Timestamp::from_unix(ts.as_unix() - ts.as_unix().rem_euclid(SECS_PER_HOUR))
}

/// provider 规范化（`daemon.go:294`）：trim + 小写。
///
/// 写的时候统一，是为了让客户端的按 model 定价表不再被大小写漂移分裂成两个桶。
#[must_use]
pub fn normalize_provider(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// provider 最终取值的来源（只是信息，不影响落库）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSource {
    /// 上报里带了 provider。
    Reported,
    /// 上报没带，从 task 的 runtime 回填。
    RuntimeFallback,
    /// 两边都没有 ⇒ 落 `''`（上游也一样会写这一行，只 warn）。
    Missing,
}

/// 决定这一行 usage 的 provider。
///
/// 空 provider 必须回填：泛化 model id（例如 `auto`）落成 `''` 就永远定价 $0。
#[must_use]
pub fn resolve_provider(reported: &str, runtime_provider: &str) -> (String, ProviderSource) {
    let reported = normalize_provider(reported);
    if !reported.is_empty() {
        return (reported, ProviderSource::Reported);
    }
    let fallback = normalize_provider(runtime_provider);
    if !fallback.is_empty() {
        return (fallback, ProviderSource::RuntimeFallback);
    }
    (String::new(), ProviderSource::Missing)
}

/// 把上报的成本换算成可落库的值（`daemon.go:4804`）。
///
/// `<= 0` ⇒ `None`（`NULL`）：0 是「daemon 不知道成本」，负数是无意义的上报。
#[must_use]
pub const fn authoritative_cost_ticks(reported: i64) -> Option<i64> {
    if reported > 0 {
        Some(reported)
    } else {
        None
    }
}

/// 1 tick = `1e-10` USD 的定点格式化（避免浮点误差）。
///
/// 不用于定价，只用于日志/展示；定价在客户端按 model 价目表做。
#[must_use]
pub fn cost_ticks_to_usd_string(ticks: i64) -> String {
    let per = COST_TICKS_PER_USD.unsigned_abs();
    let magnitude = ticks.unsigned_abs();
    let sign = if ticks < 0 { "-" } else { "" };
    format!("{sign}{}.{:010}", magnitude / per, magnitude % per)
}

/// 提示词缓存命中率（`daemon.go:4866` 的日志指标）。
///
/// 分母是「输入侧总量」= `input + cache_read + cache_write`；总量为 0 时返回
/// `None`（上游同样不打印）。只用于观测，所以用浮点。
#[must_use]
#[allow(clippy::cast_precision_loss)] // 日志指标，不需要精确
pub fn prompt_cache_read_ratio(
    input_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
) -> Option<f64> {
    let total = input_tokens
        .saturating_add(cache_read_tokens)
        .saturating_add(cache_write_tokens);
    if total <= 0 {
        return None;
    }
    Some(cache_read_tokens as f64 / total as f64)
}

/// daemon 上报的一条 usage（`TaskUsagePayload`）。
///
/// 所有字段都 `#[serde(default)]`：Go 的零值语义就是「缺字段 = 0 / `""`」，
/// 缺 `cost_usd_ticks` 的老 daemon 必须解析成 0（再被
/// [`authoritative_cost_ticks`] 变成 `NULL`），而不是解析失败。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageReport {
    /// `provider`（会经过 [`resolve_provider`]）。
    pub provider: String,
    /// `model`（保留原样，不做大小写规范化）。
    pub model: String,
    /// `input_tokens`。
    pub input_tokens: i64,
    /// `output_tokens`。
    pub output_tokens: i64,
    /// `cache_read_tokens`。
    pub cache_read_tokens: i64,
    /// `cache_write_tokens`。
    pub cache_write_tokens: i64,
    /// `cost_usd_ticks`：provider 自己的定价（`1e-10 USD`），`<= 0` 视为没报。
    pub cost_usd_ticks: i64,
}

/// 一条 usage 的自然键（`UNIQUE (task_id, provider, model)`）。
///
/// `task_usage.id` 是 `gen_random_uuid()` 代理键，`UpsertTaskUsage` 从不提供它，
/// 所以领域层不建模它 —— 行的身份就是这个三元组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageKey {
    /// `task_id`。
    pub task_id: Id,
    /// `provider`（已规范化）。
    pub provider: String,
    /// `model`。
    pub model: String,
}

impl UsageKey {
    /// 构造（`provider` 会被规范化）。
    #[must_use]
    pub fn new(task_id: Id, provider: &str, model: &str) -> Self {
        Self {
            task_id,
            provider: normalize_provider(provider),
            model: model.to_owned(),
        }
    }
}

/// `task_usage` 一行（11 列，`032` + `213`；不含代理键 `id`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskUsageRow {
    /// `task_id`（FK → `agent_task_queue.id`，`ON DELETE CASCADE`）。
    pub task_id: Id,
    /// `provider`。
    pub provider: String,
    /// `model`。
    pub model: String,
    /// `input_tokens`。
    pub input_tokens: i64,
    /// `output_tokens`。
    pub output_tokens: i64,
    /// `cache_read_tokens`。
    pub cache_read_tokens: i64,
    /// `cache_write_tokens`。
    pub cache_write_tokens: i64,
    /// `cost_usd_ticks`（`NULL` = provider 没定价 ⇒ 读侧要估算）。
    pub cost_usd_ticks: Option<i64>,
    /// `created_at`（`DO UPDATE` **不动**它）。
    pub created_at: Timestamp,
    /// `updated_at`（可空列；INSERT 与冲突两侧都刷新，小时汇总靠它认脏行）。
    pub updated_at: Option<Timestamp>,
}

impl TaskUsageRow {
    /// 自然键。
    #[must_use]
    pub fn key(&self) -> UsageKey {
        UsageKey {
            task_id: self.task_id,
            provider: self.provider.clone(),
            model: self.model.clone(),
        }
    }

    /// `UpsertTaskUsage` 的 INSERT 侧。
    #[must_use]
    pub fn from_report(task_id: Id, report: &UsageReport, provider: &str, now: Timestamp) -> Self {
        Self {
            task_id,
            provider: normalize_provider(provider),
            model: report.model.clone(),
            input_tokens: report.input_tokens,
            output_tokens: report.output_tokens,
            cache_read_tokens: report.cache_read_tokens,
            cache_write_tokens: report.cache_write_tokens,
            cost_usd_ticks: authoritative_cost_ticks(report.cost_usd_ticks),
            created_at: now,
            updated_at: Some(now),
        }
    }

    /// `UpsertTaskUsage` 的 `DO UPDATE` 侧：**整体覆盖**计数与成本。
    ///
    /// `created_at` 保持不变（上游 `DO UPDATE` 没碰它），`updated_at` 刷新。
    pub fn overwrite_with(&mut self, report: &UsageReport, provider: &str, now: Timestamp) {
        self.provider = normalize_provider(provider);
        self.model.clone_from(&report.model);
        self.input_tokens = report.input_tokens;
        self.output_tokens = report.output_tokens;
        self.cache_read_tokens = report.cache_read_tokens;
        self.cache_write_tokens = report.cache_write_tokens;
        self.cost_usd_ticks = authoritative_cost_ticks(report.cost_usd_ticks);
        self.updated_at = Some(now);
    }

    /// 是否被 provider 定了价。
    #[must_use]
    pub const fn is_costed(&self) -> bool {
        self.cost_usd_ticks.is_some()
    }
}

/// token / 成本汇总块（`GetIssueUsageSummary` 的 `usage` CTE）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenTotals {
    /// `total_input_tokens`。
    pub input_tokens: i64,
    /// `total_output_tokens`。
    pub output_tokens: i64,
    /// `total_cache_read_tokens`。
    pub cache_read_tokens: i64,
    /// `total_cache_write_tokens`。
    pub cache_write_tokens: i64,
    /// `total_cost_usd_ticks`：**只有** provider 定价过的行贡献。
    pub cost_usd_ticks: i64,
    /// `uncosted_input_tokens`。
    pub uncosted_input_tokens: i64,
    /// `uncosted_output_tokens`。
    pub uncosted_output_tokens: i64,
    /// `uncosted_cache_read_tokens`。
    pub uncosted_cache_read_tokens: i64,
    /// `uncosted_cache_write_tokens`。
    pub uncosted_cache_write_tokens: i64,
}

impl TokenTotals {
    /// 累加一行。
    pub fn add(&mut self, row: &TaskUsageRow) {
        self.input_tokens += row.input_tokens;
        self.output_tokens += row.output_tokens;
        self.cache_read_tokens += row.cache_read_tokens;
        self.cache_write_tokens += row.cache_write_tokens;
        if let Some(ticks) = row.cost_usd_ticks {
            self.cost_usd_ticks += ticks;
        } else {
            // 未定价的行只进 uncosted_*（上游 FILTER 子句）。
            self.uncosted_input_tokens += row.input_tokens;
            self.uncosted_output_tokens += row.output_tokens;
            self.uncosted_cache_read_tokens += row.cache_read_tokens;
            self.uncosted_cache_write_tokens += row.cache_write_tokens;
        }
    }
}

/// 一次运行的「是否计入统计」的输入（`agent_task_queue` 侧）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCoverage {
    /// `id`。
    pub task_id: Id,
    /// `status`。
    pub status: TaskStatus,
    /// `started_at`。
    pub started_at: Option<Timestamp>,
    /// `completed_at`。
    pub completed_at: Option<Timestamp>,
}

impl RunCoverage {
    /// 是否算「一次真实跑过的终态运行」。
    ///
    /// `status IN ('completed','failed','cancelled') AND started_at IS NOT NULL
    /// AND completed_at IS NOT NULL` —— `queued` 里被取消的任务从未占用 agent，
    /// 所以 `started_at` 这一条把它挡掉。
    #[must_use]
    pub const fn counts_as_run(&self) -> bool {
        self.status.is_terminal() && self.started_at.is_some() && self.completed_at.is_some()
    }

    /// 运行时长（秒）：`completed_at - started_at`。
    ///
    /// 不 clamp 到 0：上游直接用 `EXTRACT(EPOCH FROM (completed - started))`，
    /// 负数只可能来自坏数据，这里也不假装它是 0。
    #[must_use]
    pub fn duration_secs(&self) -> Option<i64> {
        match (self.started_at, self.completed_at) {
            (Some(started), Some(completed)) => {
                Some(completed.as_unix().saturating_sub(started.as_unix()))
            }
            _ => None,
        }
    }
}

/// issue 级 usage 汇总（`GetIssueUsageSummary`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueUsageSummary {
    /// token / 成本合计。
    pub totals: TokenTotals,
    /// `task_count` = `COUNT(DISTINCT task_id)`（有 usage 行的任务数）。
    pub task_count: i64,
    /// `terminal_task_count`：真实跑过的终态运行数。
    pub terminal_task_count: i64,
    /// `metered_task_count`：其中**有 usage 行**的（哪怕计数器全是 0，
    /// 「有行」本身就是上报过的证据）。
    pub metered_task_count: i64,
    /// `unreported_task_count` = 终态运行 − 已计量（跑过但一条 usage 都没报）。
    pub unreported_task_count: i64,
}

/// 汇总一个 issue 的 usage。
#[must_use]
pub fn summarize_issue_usage(rows: &[TaskUsageRow], runs: &[RunCoverage]) -> IssueUsageSummary {
    let mut totals = TokenTotals::default();
    let mut seen: Vec<Id> = Vec::new();
    for row in rows {
        totals.add(row);
        if !seen.contains(&row.task_id) {
            seen.push(row.task_id);
        }
    }

    let mut terminal = 0;
    let mut metered = 0;
    for run in runs {
        if !run.counts_as_run() {
            continue;
        }
        terminal += 1;
        if rows.iter().any(|row| row.task_id == run.task_id) {
            metered += 1;
        }
    }

    IssueUsageSummary {
        totals,
        task_count: i64::try_from(seen.len()).unwrap_or(i64::MAX),
        terminal_task_count: terminal,
        metered_task_count: metered,
        unreported_task_count: terminal - metered,
    }
}

/// 运行时长 / 任务数汇总（`ListDashboardRunTimeDaily` + `ListDashboardAgentRunTime`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTimeSummary {
    /// `total_seconds`：终态运行时长之和。
    pub total_seconds: i64,
    /// `task_count`。
    pub task_count: i64,
    /// `metered_task_count`：有 usage 行的运行数（与 token 卡对齐用）。
    pub metered_task_count: i64,
    /// `failed_count`。
    pub failed_count: i64,
    /// `cancelled_count`（`cancelled` 要计入：跑到一半被停也真实烧了时间与 token，
    /// 漏掉它会让 Time 卡与 Cost 卡统计的不是同一批任务）。
    pub cancelled_count: i64,
}

/// 汇总运行时长与任务数。
#[must_use]
pub fn summarize_run_time(runs: &[RunCoverage], rows: &[TaskUsageRow]) -> RunTimeSummary {
    let mut summary = RunTimeSummary::default();
    for run in runs {
        if !run.counts_as_run() {
            continue;
        }
        summary.task_count += 1;
        summary.total_seconds += run.duration_secs().unwrap_or(0);
        match run.status {
            TaskStatus::Failed => summary.failed_count += 1,
            TaskStatus::Cancelled => summary.cancelled_count += 1,
            _ => {}
        }
        if rows.iter().any(|row| row.task_id == run.task_id) {
            summary.metered_task_count += 1;
        }
    }
    summary
}

/// 失败分桶（`ListDashboardFailuresDaily` 的 `CASE`）。
///
/// **不 derive `Serialize`**：它的「线上形态」就是 SQL 出来的字符串
/// （`''` / `'unclassified'` / 原始 `failure_reason` 文本），用 `as_str()` 取；
/// 派生 enum serde 会得出 `non_failure` 这种不一样的形态，正是要避免的静默漂移。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureBucket {
    /// 非失败（`''` 桶）：图表用它当成功计数，也是错误率的分母。
    NonFailure,
    /// 失败但 `failure_reason` 是 `NULL` 或空串 ⇒ `'unclassified'`。
    ///
    /// 上游特意不把它并进成功桶：老行（`MUL-1949` 之前）或忘了分类的失败路径
    /// 必须仍然可计数，而不是冒充成功。
    Unclassified,
    /// 有 `failure_reason`：按**原始文本**分桶（不校验是否在枚举里，上游亦然）。
    Reason(String),
}

impl FailureBucket {
    /// SQL 形态（`''` / `'unclassified'` / 原因文本）。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::NonFailure => "",
            Self::Unclassified => "unclassified",
            Self::Reason(reason) => reason,
        }
    }

    /// 若原因是我们认识的规范值，给出强类型形态。
    #[must_use]
    pub fn canonical(&self) -> Option<FailureReason> {
        match self {
            Self::Reason(reason) => FailureReason::parse(reason).ok(),
            _ => None,
        }
    }
}

/// 分桶：`CASE WHEN status = 'failed' THEN COALESCE(NULLIF(failure_reason,''),'unclassified')
/// ELSE '' END`。
///
/// 注意 `cancelled` 走 `ELSE`（该查询的 `WHERE status IN ('completed','failed')`
/// 本来也不含它），所以这里返回 [`FailureBucket::NonFailure`]。
#[must_use]
pub fn classify_failure_bucket(status: TaskStatus, failure_reason: Option<&str>) -> FailureBucket {
    if status != TaskStatus::Failed {
        return FailureBucket::NonFailure;
    }
    match failure_reason.map(str::trim) {
        Some(reason) if !reason.is_empty() => FailureBucket::Reason(reason.to_owned()),
        _ => FailureBucket::Unclassified,
    }
}

/// 小时汇总的桶键（`uq_task_usage_hourly_key`）。
///
/// `UNIQUE NULLS NOT DISTINCT (bucket_hour, workspace_id, runtime_id, agent_id,
/// project_id, provider, model)` —— 「NULLS NOT DISTINCT」在这里正好等价于
/// `Option<Id>` 的相等语义：两个 `project_id IS NULL` 的桶是同一个桶。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HourlyBucketKey {
    /// `bucket_hour`（UTC 整点，见 [`hour_bucket`]）。
    pub bucket_hour: Timestamp,
    /// `workspace_id`。
    pub workspace_id: Id,
    /// `runtime_id`（汇总**只**吃 `runtime_id IS NOT NULL` 的行）。
    pub runtime_id: Id,
    /// `agent_id`。
    pub agent_id: Id,
    /// `project_id`（可空；`NULL` 自成一桶）。
    pub project_id: Option<Id>,
    /// `provider`。
    pub provider: String,
    /// `model`。
    pub model: String,
}

/// 参与小时汇总的一条明细（`task_usage` 行 + 它所属运行的键维度）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HourlySourceRow {
    /// 桶键（`bucket_hour` 由 `task_usage.created_at` 推出）。
    pub key: HourlyBucketKey,
    /// 明细的 `task_id`（用来数 `COUNT(DISTINCT task_id)`）。
    pub task_id: Id,
    /// `input_tokens`。
    pub input_tokens: i64,
    /// `output_tokens`。
    pub output_tokens: i64,
    /// `cache_read_tokens`。
    pub cache_read_tokens: i64,
    /// `cache_write_tokens`。
    pub cache_write_tokens: i64,
    /// `cost_usd_ticks`（`None` = 未定价）。
    pub cost_usd_ticks: Option<i64>,
}

/// `task_usage_hourly` 一行的聚合值（`213` 的 `recomputed` CTE）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HourlyAggregate {
    /// 桶键。
    pub key: HourlyBucketKey,
    /// `input_tokens`。
    pub input_tokens: i64,
    /// `output_tokens`。
    pub output_tokens: i64,
    /// `cache_read_tokens`。
    pub cache_read_tokens: i64,
    /// `cache_write_tokens`。
    pub cache_write_tokens: i64,
    /// `cost_usd_ticks` = `COALESCE(SUM(cost_usd_ticks), 0)`（只含已定价行）。
    pub cost_usd_ticks: i64,
    /// `uncosted_input_tokens`（未定价行的 input 之和）。
    pub uncosted_input_tokens: i64,
    /// `uncosted_output_tokens`。
    pub uncosted_output_tokens: i64,
    /// `uncosted_cache_read_tokens`。
    pub uncosted_cache_read_tokens: i64,
    /// `uncosted_cache_write_tokens`。
    pub uncosted_cache_write_tokens: i64,
    /// `task_count` = `COUNT(DISTINCT task_id)`。
    pub task_count: i64,
    /// `event_count` = `COUNT(*)`（多少条明细折进这个桶，即
    /// `(task, provider, model)` 三元组的个数）。
    pub event_count: i64,
}

/// 按桶键聚合成 `task_usage_hourly` 行（纯 fold）。
///
/// 输出顺序 = 首次出现的顺序（仓储层落库无所谓顺序，但测试与文档需要确定性）。
/// **重算为空的桶要删掉**（`deleted_empty`）：脏键在明细被修正/删除后可能已经没有
/// 对应行，聚合结果里自然就不会出现它 —— 这个「不出现」正是删除信号。
#[must_use]
pub fn fold_hourly(rows: &[HourlySourceRow]) -> Vec<HourlyAggregate> {
    let mut out: Vec<HourlyAggregate> = Vec::new();
    for row in rows {
        let idx = if let Some(idx) = out.iter().position(|agg| agg.key == row.key) {
            idx
        } else {
            out.push(HourlyAggregate {
                key: row.key.clone(),
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd_ticks: 0,
                uncosted_input_tokens: 0,
                uncosted_output_tokens: 0,
                uncosted_cache_read_tokens: 0,
                uncosted_cache_write_tokens: 0,
                task_count: 0,
                event_count: 0,
            });
            out.len() - 1
        };
        let slot = &mut out[idx];
        slot.input_tokens += row.input_tokens;
        slot.output_tokens += row.output_tokens;
        slot.cache_read_tokens += row.cache_read_tokens;
        slot.cache_write_tokens += row.cache_write_tokens;
        slot.event_count += 1;
        if let Some(ticks) = row.cost_usd_ticks {
            slot.cost_usd_ticks += ticks;
        } else {
            slot.uncosted_input_tokens += row.input_tokens;
            slot.uncosted_output_tokens += row.output_tokens;
            slot.uncosted_cache_read_tokens += row.cache_read_tokens;
            slot.uncosted_cache_write_tokens += row.cache_write_tokens;
        }
    }

    // task_count 需要在收齐后再数（同一 task 的多条明细算一次）。
    for agg in &mut out {
        let mut seen: Vec<Id> = Vec::new();
        for row in rows.iter().filter(|row| row.key == agg.key) {
            if !seen.contains(&row.task_id) {
                seen.push(row.task_id);
            }
        }
        agg.task_count = i64::try_from(seen.len()).unwrap_or(i64::MAX);
    }

    out
}

#[cfg(test)]
mod tests;

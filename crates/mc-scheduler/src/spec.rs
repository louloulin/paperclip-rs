//! 调度规格（job 名 / 计划时间 / 重试间隔）的格式化与解析。
//!
//! - **写者**：M5-7。
//! - **上游**：`scheduler/spec.go`261（262）—— 两个 `String()`（24 + 132 行的格式化/解析）、
//!   `validate`34、`retryDelay`16、`FloorPlan`8。
//! - **`FloorPlan`(8) 是 `plan_time` 的取整契约**：同一 tick 内的计划必须落到同一个 `plan_time`，
//!   否则 `sys_cron_executions` 的租约键会漂移、重复执行。
//! - **`String()` 是持久化格式**：它出现在租约表里 ⇒ 改格式等于迁移数据（本波**不改**格式）。
//!
//! **状态：M5-7 已落地**（`LUM-1566`）。与上游的差异（都在下面各自的位置标了）：
//!
//! 1. `CatchUpMode` 多了 `FromStr`：上游只 `String()`（`unknown(%d)` 兜底），本地要解析
//!    就必须让非法输入**在 `register` 时报错**，而不是等到 tick 才发现。
//! 2. `JobSpec` 多了 builder：上游是裸结构体（零值合法），本地把「必填」推进类型
//!    （`scopes` / `handler` 不是 `Option`），时间预算故意留零值 ⇒ 漏填会被
//!    [`JobSpec::validate`] 拒掉，而不会静默取一个我发明的默认值。
//! 3. 回调是 `Arc<dyn Fn(..) -> BoxFuture<..>>`（不是 async trait）：可以 `Clone` 进
//!    `HandlerInput` / 注册表，且不需要 `async_trait` 依赖（M5-0 的依赖表已冻结）。

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};

use mc_repos::scheduler::LatestPlanInfo;

use crate::db_ops::Heartbeat;
use crate::error::SchedulerError;

/// 装箱的 future：所有回调（scope provider / plan hook / handler）的统一返回形态。
///
/// `Send + 'static` 是硬要求：回调会被 `tokio::spawn` 到别的 worker 上（handler 隔离），
/// 生命周期也必须能超出 `run_once` 的一次 tick。
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// `global` 作用域的规范字面量（kind 与 id 都用它，让唯一键**没有 NULL 列**）。
pub const GLOBAL: &str = "global";

/// 追赶模式：tick 迟到 / 长时间停机之后，调度器该认哪些 `plan_time`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CatchUpMode {
    /// 只认最近一个到期计划。适合「handler 自带水位」的 job
    /// （上游例子：`task_usage` 小时汇总，漏掉的桶由它自己的 watermark 补）。
    #[default]
    LatestOnly,
    /// 把错过的计划桶**从旧到新**逐个补，受 `catch_up_window` 与 `max_plans_per_tick` 双上界。
    /// 适合「每个桶都有独立业务含义」的 job。
    EveryPlan,
}

impl CatchUpMode {
    /// 落配置 / 日志的字面量（上游 `spec.go:24` 的 `String()`）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LatestOnly => "latest_only",
            Self::EveryPlan => "every_plan",
        }
    }
}

impl fmt::Display for CatchUpMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CatchUpMode {
    type Err = SchedulerError;

    /// 上游没有解析器（`String()` 的兜底分支给 `unknown(%d)`）。本地必须有，
    /// 因为「不认识的模式」的正确处置是**注册时拒绝**，而不是 tick 时静默跳过。
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "latest_only" => Ok(Self::LatestOnly),
            "every_plan" => Ok(Self::EveryPlan),
            other => Err(SchedulerError::InvalidSpec(format!(
                "unknown catch_up_mode {other:?}; expected latest_only / every_plan"
            ))),
        }
    }
}

/// 锁定维度：一个 `(job, scope, plan_time)` 三元组就是一把租约（迁移 `113` 的唯一键）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scope {
    /// 作用域种类（`global` / `workspace` / `agent` …）。
    pub kind: String,
    /// 作用域 id（`global` 时是字面量 `global`）。
    pub id: String,
}

impl Scope {
    /// 构造。
    #[must_use]
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }

    /// 全局单例作用域（`global/global`）。
    #[must_use]
    pub fn global() -> Self {
        Self::new(GLOBAL, GLOBAL)
    }
}

impl fmt::Display for Scope {
    /// `kind/id`。上游 `spec.go:132` 的 `String()`；**这个格式会进日志与指标标签**。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind, self.id)
    }
}

/// 作用域提供者：每个 tick 给出本 job 要跑哪些 scope（上游 `spec.go:96`）。
///
/// 上游签名带 `ctx`；本地不传 —— 取消靠 future 被 drop 传播（`run_once` 的 future 被
/// `CancellationToken` 掉时，正在 await 的这个回调也一起被 drop）。
pub type ScopeProvider =
    Arc<dyn Fn(DateTime<Utc>) -> BoxFuture<Result<Vec<Scope>, SchedulerError>> + Send + Sync>;

/// 固定作用域集合（上游 `StaticScopes`）。集合在构造时**冻结**一份，之后与调用方无关。
#[must_use]
pub fn static_scopes(scopes: Vec<Scope>) -> ScopeProvider {
    Arc::new(move |_now| {
        let scopes = scopes.clone();
        Box::pin(async move { Ok(scopes) })
    })
}

/// `global/global` 单例作用域提供者（占大多数 job 的形态）。
#[must_use]
pub fn global_scopes() -> ScopeProvider {
    static_scopes(vec![Scope::global()])
}

/// 自定义计划器：一旦设置就**替代** `cadence` 网格（上游 `PlansForScope`，为 autopilot
/// 的任意 cron 表达式准备的）。入参是 `(scope, now, 该 (job,scope) 的最新历史行)`。
///
/// 返回的 `plan_time` 必须已经是**规范 UTC**（调度器原样交给 `try_claim`）；返回空表 =
/// 「本 tick 无事」。重复返回已终态的 `plan_time` 是安全的（`try_claim` 会当冲突）。
pub type PlansHook = Arc<
    dyn Fn(
            Scope,
            DateTime<Utc>,
            LatestPlanInfo,
        ) -> BoxFuture<Result<Vec<DateTime<Utc>>, SchedulerError>>
        + Send
        + Sync,
>;

/// 业务 handler：一次租约调用**恰好一次**（上游 `spec.go:118`）。
///
/// 长任务**必须**定期调 [`HandlerInput::heartbeat`]，否则 `stale_after` 过期后租约会被
/// 回收 / 被别的实例偷走；`beat()` 返回 `Err(LeaseLost)` 表示「你已经不是持有者了」，
/// 应尽快收尾并返回（不要在租约丢失后继续写业务数据）。
pub type Handler =
    Arc<dyn Fn(HandlerInput) -> BoxFuture<Result<HandlerResult, SchedulerError>> + Send + Sync>;

/// 递给 handler 的上下文。
pub struct HandlerInput {
    /// 本 job 的规格（handler 常要读 `name` 做日志/审计）。
    pub job: Arc<JobSpec>,
    /// 本次锁定的作用域。
    pub scope: Scope,
    /// 本次的计划时间桶（`FloorPlan` 取整过的）。
    pub plan_time: DateTime<Utc>,
    /// 第几次尝试（首次为 1）。
    pub attempt: i32,
    /// 本进程的 runner 标识（审计用）。
    pub runner_id: String,
    /// 租约续期句柄。
    pub heartbeat: Heartbeat,
}

/// handler 的返回值：喂给审计行（上游 `HandlerResult`）。
#[derive(Debug, Clone, Default)]
pub struct HandlerResult {
    /// 业务影响行数（落 `rows_affected` 列，纯审计）。
    pub rows_affected: i64,
    /// 结构化结果（JSON 文本，落 `result::jsonb`）。`None` / 空白 ⇒ `{}`；
    /// 上限 16KB，超了会报错（大对象请进结构化日志）。
    pub result_json: Option<String>,
}

impl HandlerResult {
    /// 只记影响行数、不带结果载荷。
    #[must_use]
    pub const fn rows(rows_affected: i64) -> Self {
        Self {
            rows_affected,
            result_json: None,
        }
    }
}

/// 一条注册的 job 规格。
///
/// **`name` 是持久化键**（`sys_cron_executions.job_name`），跨版本必须稳定 ——
/// 改名等于把历史审计行孤立出去。
#[derive(Clone)]
pub struct JobSpec {
    /// 业务 job 名（`snake_case` ASCII；**不是** `TaskKind`）。
    pub name: String,
    /// 计划桶大小（`plan_time` 是它的整数倍，见 [`floor_plan`]）。
    pub cadence: Duration,
    /// 资格地平线回移量：12:00 的桶在 `db_now >= 12:00 + schedule_delay` 时才可认领
    /// （避免 handler 用 `now() - 5min` 当上界时漏掉刚到的数据）。
    pub schedule_delay: Duration,
    /// 追赶模式。
    pub catch_up_mode: CatchUpMode,
    /// 追赶地平线：早于 `now - catch_up_window` 的计划一律不再补。
    /// `<= 0` 表示「只认最新桶」（与上游一致）。
    pub catch_up_window: Duration,
    /// 单 tick 最多认几个计划（`EveryPlan` 用；`0` 在 hook 模式下表示不截断）。
    pub max_plans_per_tick: usize,
    /// handler 的运行上限；必须**小于** `stale_timeout`。
    pub run_timeout: StdDuration,
    /// 心跳静默多久算陈旧。陈旧 + `allow_stale_reentry` ⇒ 别的实例可以偷。
    pub stale_timeout: StdDuration,
    /// 心跳间隔；必须 `> 0` 且 `< stale_timeout`。
    pub heartbeat_interval: StdDuration,
    /// 是否允许「偷陈旧租约」。非幂等 job 设 `false`：陈旧租约只会被收成
    /// `FAILED(error_code='stale_timeout')`，需要人工修复。
    pub allow_stale_reentry: bool,
    /// 同一个 `plan_time` 最多尝试几次（含首次）。
    pub max_attempts: i32,
    /// 第 `i+1` 次尝试前的等待；`RetryBackoff[i]` 是「第 `i+1` 次失败后等多久」。
    /// 超出长度就复用最后一项；空表 = 不重试（`retry_delay` 返回 0）。
    pub retry_backoff: Vec<Duration>,
    /// 作用域提供者。
    pub scopes: ScopeProvider,
    /// 自定义计划器（设了它 ⇒ `cadence` / `catch_up_mode` / `catch_up_window` 只管审计）。
    pub plans_for_scope: Option<PlansHook>,
    /// 业务逻辑。
    pub handler: Handler,
}

impl JobSpec {
    /// 建一个 job。**只**给安全零值：`scopes` / `handler` 是必填参数，
    /// 时间预算（`run_timeout` / `stale_timeout` / `heartbeat_interval` / `max_attempts`）
    /// 故意留零 ⇒ 必须用 builder 显式给，漏了会被 [`Self::validate`] 拒掉。
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        cadence: Duration,
        scopes: ScopeProvider,
        handler: Handler,
    ) -> Self {
        Self {
            name: name.into(),
            cadence,
            schedule_delay: Duration::zero(),
            catch_up_mode: CatchUpMode::LatestOnly,
            catch_up_window: Duration::zero(),
            max_plans_per_tick: 0,
            run_timeout: StdDuration::ZERO,
            stale_timeout: StdDuration::ZERO,
            heartbeat_interval: StdDuration::ZERO,
            allow_stale_reentry: false,
            max_attempts: 0,
            retry_backoff: Vec::new(),
            scopes,
            plans_for_scope: None,
            handler,
        }
    }

    /// 资格地平线回移。
    #[must_use]
    pub const fn with_schedule_delay(mut self, delay: Duration) -> Self {
        self.schedule_delay = delay;
        self
    }

    /// 追赶模式 + 地平线 + 单 tick 上限（三者总是一起定，合成一个 setter 免得漏）。
    #[must_use]
    pub const fn with_catch_up(
        mut self,
        mode: CatchUpMode,
        window: Duration,
        max_plans_per_tick: usize,
    ) -> Self {
        self.catch_up_mode = mode;
        self.catch_up_window = window;
        self.max_plans_per_tick = max_plans_per_tick;
        self
    }

    /// 时间预算三件套（约束由 [`Self::validate`] 检查：`0 < run < stale`、`0 < hb < stale`）。
    #[must_use]
    pub const fn with_timing(
        mut self,
        run_timeout: StdDuration,
        stale_timeout: StdDuration,
        heartbeat_interval: StdDuration,
    ) -> Self {
        self.run_timeout = run_timeout;
        self.stale_timeout = stale_timeout;
        self.heartbeat_interval = heartbeat_interval;
        self
    }

    /// 重试预算 + 退避表。
    #[must_use]
    pub fn with_retry(mut self, max_attempts: i32, retry_backoff: Vec<Duration>) -> Self {
        self.max_attempts = max_attempts;
        self.retry_backoff = retry_backoff;
        self
    }

    /// 是否允许偷陈旧租约（默认 `false`）。
    #[must_use]
    pub const fn with_allow_stale_reentry(mut self, allow: bool) -> Self {
        self.allow_stale_reentry = allow;
        self
    }

    /// 换成自定义计划器（会绕过 `cadence` 网格）。
    #[must_use]
    pub fn with_plans_for_scope(mut self, hook: PlansHook) -> Self {
        self.plans_for_scope = Some(hook);
        self
    }

    /// 检查 SQL 原语依赖的不变量（上游 `spec.go:174` 的 `validate`）。
    ///
    /// 检查顺序与上游逐条对应，错误文案里的字段名是**给人看的**（谁漏填谁尴尬）。
    pub fn validate(&self) -> Result<(), SchedulerError> {
        let name = &self.name;
        if name.trim().is_empty() {
            return Err(SchedulerError::InvalidSpec(
                "job name is required".to_owned(),
            ));
        }
        if self.plans_for_scope.is_none() && self.cadence <= Duration::zero() {
            return Err(SchedulerError::InvalidSpec(format!(
                "job {name:?}: cadence must be > 0 (or set plans_for_scope)"
            )));
        }
        if self.run_timeout.is_zero() {
            return Err(SchedulerError::InvalidSpec(format!(
                "job {name:?}: run_timeout must be > 0"
            )));
        }
        if self.stale_timeout <= self.run_timeout {
            let (stale, run) = (self.stale_timeout, self.run_timeout);
            return Err(SchedulerError::InvalidSpec(format!(
                "job {name:?}: stale_timeout ({stale:?}) must be greater than run_timeout ({run:?})"
            )));
        }
        if self.heartbeat_interval.is_zero() || self.heartbeat_interval >= self.stale_timeout {
            return Err(SchedulerError::InvalidSpec(format!(
                "job {name:?}: heartbeat_interval must be > 0 and < stale_timeout"
            )));
        }
        if self.max_attempts < 1 {
            return Err(SchedulerError::InvalidSpec(format!(
                "job {name:?}: max_attempts must be >= 1"
            )));
        }
        if self.plans_for_scope.is_none()
            && self.catch_up_mode == CatchUpMode::EveryPlan
            && self.max_plans_per_tick == 0
        {
            return Err(SchedulerError::InvalidSpec(format!(
                "job {name:?}: max_plans_per_tick must be > 0 for every_plan catch-up"
            )));
        }
        Ok(())
    }

    /// 第 `attempt` 次尝试（1 起）失败后要等多久（上游 `spec.go:246` 的 `retryDelay`）。
    ///
    /// 索引 = `attempt - 1`，两边都夹住（`attempt <= 0` 取首项，越界复用末项）；空表 = `0`。
    #[must_use]
    pub fn retry_delay(&self, attempt: i32) -> Duration {
        let Some(last) = self.retry_backoff.len().checked_sub(1) else {
            return Duration::zero();
        };
        let idx = usize::try_from(attempt.saturating_sub(1))
            .unwrap_or(0)
            .min(last);
        self.retry_backoff[idx]
    }

    /// 落库用的「陈旧窗口秒数」：上游 `db_ops.go` 的 `int64(StaleTimeout / time.Second)`
    /// —— **整数截断**（不是四舍五入），并且下限 1 秒（0 会让租约立刻可被偷）。
    #[must_use]
    pub fn stale_secs(&self) -> f64 {
        stale_secs_f64(self.stale_timeout)
    }
}

/// Unix 纪元距 Go `time.Time` 零值（`0001-01-01T00:00:00Z`）的秒数取负。
///
/// 关键：Go 的 `time.Time.Truncate` 是「从**零值**起算的整数倍」，而 chrono 的
/// `timestamp` 是从 **Unix 纪元**起算的 —— 两者对 5m/15m/1h 这类整除一天的 cadence
/// 等价（62135596800 能被 300/900/3600/86400 整除），对 7h/90m 之类**不等价**。
/// 所以取整必须显式补上这个偏移，否则 `plan_time` 会在两实例时钟/实现之间漂移。
const GO_ZERO_UNIX_NANOS: i128 = -62_135_596_800_i128 * 1_000_000_000;

/// 规范 `plan_time` 桶：`eligible` 向下取整到 `cadence` 的整数倍（上游 `spec.go:253`
/// 的 `FloorPlan`）。
///
/// * `cadence <= 0` ⇒ 原样返回 `eligible`（上游同：`c <= 0` 直接返回 `eligible.UTC()`）。
/// * 取整原点 = Go 的零值（见 [`GO_ZERO_UNIX_NANOS`]），用 `i128` 纳秒算，避免
///   `i64` 纳秒在 2262 年溢出。
/// * 结果超出 chrono 表示范围（理论上不会）⇒ 退回 `eligible`，保证函数**永不 panic**。
#[must_use]
pub fn floor_plan(eligible: DateTime<Utc>, cadence: Duration) -> DateTime<Utc> {
    let step = cadence_nanos(cadence);
    if step <= 0 {
        return eligible;
    }
    let since_zero = unix_nanos(eligible) - GO_ZERO_UNIX_NANOS;
    let bucket = since_zero - since_zero.rem_euclid(step);
    from_unix_nanos(bucket + GO_ZERO_UNIX_NANOS).unwrap_or(eligible)
}

/// 相对于 Unix 纪元的纳秒数（`i128`，不溢出）。
fn unix_nanos(t: DateTime<Utc>) -> i128 {
    i128::from(t.timestamp()) * 1_000_000_000 + i128::from(t.timestamp_subsec_nanos())
}

/// `cadence` 的纳秒数（负值保持负号：chrono 的 `subsec_nanos()` 是有符号的）。
fn cadence_nanos(d: Duration) -> i128 {
    i128::from(d.num_seconds()) * 1_000_000_000 + i128::from(d.subsec_nanos())
}

/// `i128` 纳秒 → `DateTime<Utc>`；超出范围返回 `None`（不用会 panic 的 `from_timestamp_nanos`）。
fn from_unix_nanos(total: i128) -> Option<DateTime<Utc>> {
    let secs = total.div_euclid(1_000_000_000);
    let nanos = total.rem_euclid(1_000_000_000);
    DateTime::from_timestamp(i64::try_from(secs).ok()?, u32::try_from(nanos).ok()?)
}

/// 「陈旧窗口」的落库值（**共享**给 `db_ops` / `manager`，避免两处各截一次）。
#[must_use]
pub fn stale_secs_f64(stale_timeout: StdDuration) -> f64 {
    // `.max(1)` = 上游的 `if staleSecs <= 0 { staleSecs = 1 }`。
    let secs = stale_timeout.as_secs().max(1);
    // > 136 年的陈旧窗口没有意义；用 u32 封顶而不是 `as f64` 触发精度 lint。
    f64::from(u32::try_from(secs).unwrap_or(u32::MAX))
}

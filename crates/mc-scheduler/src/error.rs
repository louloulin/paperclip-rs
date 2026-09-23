//! crate 级错误类型与「错误分类 → 重试决策」的映射。
//!
//! - **写者**：M5-7（§3.2 矩阵没有本文件的对应行 ⇒ 本 anchor 判给 M5-7）。
//! - **上游**：`manager.go` 的 `classifyError`23 —— 它决定「重试 / 终态失败 / 放弃租约」。
//! - **纪律**：分类结果必须能区分三类（可重试 / 不可重试 / 陈旧租约），因为 `db_ops.go` 的
//!   `finishFailure`46 与 `RetryEligible`18 按它分叉；用一个 `bool` 表达会导致重试风暴。
//!
//! **状态：M5-7 已落地**（`LUM-1566`）—— 上游 `classifyError` 的 5 个码逐字保留
//! （`run_timeout` / `canceled` / `lease_lost` / `handler_panic` / `handler_error`），
//! 另加本地两处**加法**：
//!
//! 1. `code()` 还覆盖「规格 / 重名 / 仓储」三类错误（上游这些错误在 `Register` / `dbNow`
//!    就返回了，落不到审计行；本地把它们也映射成稳定码，免得日志里出现空 `error_code`）。
//! 2. [`ErrorClass::Permanent`]（不可重试）上游**没有**：上游只有「按 `attempt` 重试」一条路。
//!    本地加它是为了让「配置已经错了 / 数据不可能自愈」这类错误不要白白烧满 `max_attempts`
//!    次网络往返；落地方式见 [`crate::manager`]（`attempt_override = max_attempts`）——
//!    **没有引入新列**，用的还是迁移 `113` 已有的 `attempt`。

use mc_repos::RepoError;
use thiserror::Error;

/// crate 级结果别名。
pub type SchedulerResult<T> = Result<T, SchedulerError>;

/// 调度器错误。
#[derive(Debug, Error)]
pub enum SchedulerError {
    /// 租约已不是我们的（被偷 / 已终态）：终态写入影响 0 行。
    ///
    /// **不是** handler 的错，所以 `code()` 与「重试预算」都不能当成业务失败处理。
    #[error("scheduler: lease lost ({0})")]
    LeaseLost(&'static str),
    /// handler 超过 `run_timeout`（上游 `context.DeadlineExceeded`）。
    #[error("scheduler: run timeout")]
    RunTimeout,
    /// 任务被取消（进程关闭 / runtime 停机；上游 `context.Canceled`）。
    #[error("scheduler: canceled")]
    Canceled,
    /// handler panic（被调度器收住 —— 上游 `ErrHandlerPanic`）。
    ///
    /// 与上游同样处理：**不做**「panic 就当成功」的降级，而是写成 `FAILED` 审计行。
    #[error("scheduler: handler panic: {0}")]
    HandlerPanic(String),
    /// handler 返回的业务错误（默认分类）。
    #[error("scheduler: handler error: {0}")]
    Handler(String),
    /// 不可重试的错误；`code` 是 handler 自己给的稳定审计码。
    #[error("scheduler: permanent ({code}): {message}")]
    Permanent {
        /// 审计码（落 `error_code` 列）。
        code: String,
        /// 人类可读详情（落 `error_msg` 列，超长会被截断）。
        message: String,
    },
    /// job 规格不合法；`register` 时就会拒掉（不会进 tick 循环）。
    #[error("scheduler: invalid spec: {0}")]
    InvalidSpec(String),
    /// 同名 job 重复注册。
    #[error("scheduler: duplicate job: {0}")]
    DuplicateJob(String),
    /// 仓储 / 数据库错误。
    #[error(transparent)]
    Repo(#[from] RepoError),
}

/// 「这个错误该怎么处置」—— 上游 `classifyError` 的返回值再往上抽一层。
///
/// 三类对应三条**不同**的处置路径：
///
/// | 分类 | 是否写终态 | `next_retry_at` | `attempt` |
/// | --- | --- | --- | --- |
/// | [`ErrorClass::Retryable`] | 写 `FAILED` | `attempt < max_attempts` 时给退避时间 | 原值 |
/// | [`ErrorClass::Permanent`] | 写 `FAILED` | `NULL` | 烧到 `max_attempts` |
/// | [`ErrorClass::LeaseLost`] | 写但预期 0 行 | 无关（写不进） | 原值 |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// 可重试：走 `attempt < max_attempts` + `retryDelay` 退避。
    Retryable,
    /// 不可重试：烧掉重试预算（`attempt_override = max_attempts` + `next_retry_at = NULL`）。
    Permanent,
    /// 租约已丢：仍然尝试写终态（上游也写，好让「0 行」成为可见证据），但影响 0 行是预期结果。
    LeaseLost,
}

impl SchedulerError {
    /// 落 `error_code` 列的稳定短码（= 上游 `classifyError`）。
    ///
    /// `Permanent` 透传 handler 自己的码：那个码比 `permanent` 有信息量得多
    /// （例如 `invalid_cron`），且它由 handler 保证稳定。
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::LeaseLost(_) => "lease_lost",
            Self::RunTimeout => "run_timeout",
            Self::Canceled => "canceled",
            Self::HandlerPanic(_) => "handler_panic",
            Self::Handler(_) => "handler_error",
            Self::Permanent { code, .. } => code.as_str(),
            Self::InvalidSpec(_) => "invalid_spec",
            Self::DuplicateJob(_) => "duplicate_job",
            Self::Repo(_) => "db_error",
        }
    }

    /// 处置分类（重试决策的唯一开关）。
    ///
    /// 注意 `Repo(_)` 归 **`Retryable`**：数据库错误在上游看来就是 handler 错误
    /// （`classifyError` 的 `default` 分支），按 `attempt` 重试；把它当 `Permanent`
    /// 会让一次网络抖动永久废掉一个计划桶。
    #[must_use]
    pub const fn class(&self) -> ErrorClass {
        match self {
            Self::LeaseLost(_) => ErrorClass::LeaseLost,
            Self::InvalidSpec(_) | Self::DuplicateJob(_) | Self::Permanent { .. } => {
                ErrorClass::Permanent
            }
            Self::RunTimeout
            | Self::Canceled
            | Self::HandlerPanic(_)
            | Self::Handler(_)
            | Self::Repo(_) => ErrorClass::Retryable,
        }
    }
}

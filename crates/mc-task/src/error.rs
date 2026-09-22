//! `mc-task` 的领域错误 —— 全部是**纯计算**错误，不含 I/O、不含 SQL 错误。
//!
//! 为什么不复用 `mc-errors`：`mc-errors` 是**协议层**错误（映射到 HTTP 状态码与
//! `not_implemented` 之类的响应体），而本 crate 是领域层 —— 它不认识 HTTP。
//! 仓储实现（M3-6 的 `TaskRepo`）负责把 `TaskError` 映射成 `mc-errors`。
//!
//! 所有变体都 `Clone + PartialEq + Eq`：这样状态机矩阵用例可以直接
//! `assert_eq!(err, TaskError::IllegalTransition { .. })`，而不是只能 `matches!`。

use mc_core::Timestamp;

use crate::state::TaskEvent;

/// 领域层统一错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskError {
    /// `status` 字符串不认识（上游最终的 CHECK 只有 8 个值，见 `TaskStatus::ALL`）。
    #[error("unknown task status: {got:?}")]
    UnknownStatus {
        /// 原始输入。
        got: String,
    },

    /// `failure_reason` 字符串不认识（上游 `pkg/taskfailure` 是唯一真值来源）。
    #[error("unknown failure_reason: {got:?}")]
    UnknownFailureReason {
        /// 原始输入。
        got: String,
    },

    /// 事件名不认识（上游 `pkg/protocol/events.go` 的 `task:` 前缀常量）。
    #[error("unknown task event: {got:?}")]
    UnknownEvent {
        /// 原始输入。
        got: String,
    },

    /// 状态机拒绝这条边：`from` 既不是该事件的合法源状态，也不是终态。
    #[error("illegal transition: {from} --{event:?}--> (edge not in the state machine)")]
    IllegalTransition {
        /// 源状态。
        from: crate::status::TaskStatus,
        /// 被拒绝的事件。
        event: TaskEvent,
    },

    /// 终态是吸收态：`completed` / `failed` / `cancelled` 之后任何事件都必须 `Err`。
    #[error("task is already terminal ({status}); no event may leave a terminal state")]
    TerminalState {
        /// 当前终态。
        status: crate::status::TaskStatus,
        /// 被拒绝的事件。
        event: TaskEvent,
    },

    /// 事件没有落在任何已知形态上（例如 `waiting_local_directory` 缺 `wait_reason`）。
    #[error("event payload is incomplete: {detail}")]
    MalformedEvent {
        /// 人类可读的说明（`'static`：不引入分配，且错误可直接比较）。
        detail: &'static str,
    },

    /// `prepare-lease` 还活着，`ReclaimStale` 必须拒绝（LUM 实测：
    /// `TestClaimTaskByRuntime_DoesNotReclaimActivePrepareLease`）。
    #[error("prepare lease is still active until {expires_at:?}")]
    LeaseStillActive {
        /// 当前租约到期时刻。
        expires_at: Timestamp,
    },

    /// 已经 `StartTask` 过的行不能被「重投递」回收 —— 回收只针对 `started_at IS NULL`。
    #[error("task already started at {started_at:?}; only never-started rows can be reclaimed")]
    AlreadyStarted {
        /// `started_at`。
        started_at: Timestamp,
    },

    /// `ReclaimStaleDispatchedTaskForRuntime` 的 `claim_recovery_secs` 窗口没到。
    #[error(
        "reclaim recovery window still open: dispatched_at={dispatched_at:?}, need {claim_recovery_secs}s"
    )]
    ReclaimWindowOpen {
        /// 上次派遣时刻。
        dispatched_at: Timestamp,
        /// 上游 `claim_recovery_secs`（默认 90s，`claimResponseRecoveryWindow`）。
        claim_recovery_secs: u64,
    },

    /// 目标 runtime 不满足该操作的前置条件（未 `online`，或心跳超过 `runtime_stale_secs`）。
    #[error("runtime is not eligible for this operation: {detail}")]
    RuntimeNotEligible {
        /// 人类可读的说明。
        detail: &'static str,
    },

    /// 结算时长时两端的顺序非法（`completed_at < started_at`）。
    #[error("cannot settle duration: {detail}")]
    DurationUnavailable {
        /// 人类可读的说明。
        detail: &'static str,
    },

    /// usage 载荷非法（负 token、空 `model` 等）。
    #[error("invalid usage payload: {detail}")]
    InvalidUsage {
        /// 人类可读的说明。
        detail: &'static str,
    },

    /// 细分原因的粗分类不认识（例如仓储里存了本仓自造的值）。
    #[error("unknown failure class: {got:?}")]
    UnknownFailureClass {
        /// 原始输入。
        got: String,
    },

    /// `TaskStore` 端口上的「目标行不存在」。
    #[error("task not found: {id:?}")]
    NotFound {
        /// 任务 id。
        id: mc_core::Id,
    },

    /// 唯一约束冲突（例如 `idx_one_pending_task_per_issue`：同一 issue 已有未决任务）。
    ///
    /// 与 [`Self::Backend`] 分开：这不是「数据库坏了」，而是**契约按设计生效**，
    /// M3-6 要把它映射成 409 而不是 500。
    #[error("conflict: {detail}")]
    Conflict {
        /// 人类可读的说明（哪条约束）。
        detail: &'static str,
    },

    /// 端口实现内部的仓储错误（Pg 实现在 M3-6 填入；领域层不解释它的细节）。
    #[error("store backend error: {message}")]
    Backend {
        /// 底层错误文本。
        message: String,
    },
}

impl TaskError {
    /// 便捷构造：`TaskError::FailedReason` 的粗分类缺失。
    #[must_use]
    pub fn unknown_failure_class(got: impl Into<String>) -> Self {
        Self::UnknownFailureClass { got: got.into() }
    }
}

/// `FailureReason` 的解析失败是 `TaskError::UnknownFailureReason` —— 这里只做
/// 一个命名别名，避免调用方在 `retry` 模块里再定义一个同义错误类型。
pub type RetryError = TaskError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_comparable_and_printable() {
        let a = TaskError::unknown_failure_class("agent_error.wat");
        let b = TaskError::unknown_failure_class("agent_error.wat");
        assert_eq!(a, b);
        assert!(a.to_string().contains("agent_error.wat"));
    }

    #[test]
    fn unknown_reason_alias_is_the_same_type() {
        let e: RetryError = TaskError::UnknownFailureReason {
            got: "nope".to_owned(),
        };
        assert!(matches!(e, TaskError::UnknownFailureReason { .. }));
    }
}

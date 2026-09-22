//! 取消与 cancel-ack：纯计算。
//!
//! # 上游真值
//!
//! | 行为 | 真值 |
//! |---|---|
//! | 系统取消（无理由） | `agent.sql:1564` `CancelAgentTask` |
//! | 系统取消（带理由） | `agent.sql:1676` `CancelAgentTaskWithReason` |
//! | 人工取消 | `agent.sql:1573` `CancelAgentTaskByUser`（迁移 `458` 的三列） |
//! | 队列编辑式取消（按 session CAS） | `agent.sql` `CancelQueuedAgentTask*` |
//! | `delivered_comment_ids` 重算分支 | `CancelAgentTaskByUser` 里的 `CASE` |
//! | cancel-ack 三张写单 | `agent.sql:1640` `SetAgentTaskBranchName`、`:1650` `SetAgentTaskDurableWorkDir`、`:1662` `SetAgentTaskErrorIfEmpty` |
//! | ack 处理流程 | `handler/daemon.go:5203` `AckTaskCancelled` |
//!
//! # 三种取消**不是**同一种
//!
//! - **系统取消**（自动修复：claim 失败、worktree 声明门拒绝、陈旧 runtime）：
//!   带不带理由分两条语句。带理由的那条会写 `error` + `failure_reason`
//!   ——「用户没要求的取消」必须留下解释，否则界面上只剩一个没有原因的
//!   `cancelled`（`CancelAgentTaskWithReason` 的注释原话）。它**不碰**
//!   `delivered_comment_ids`，因为自动修复需要让 delegated-failure 恢复信号
//!   保持可重放。
//! - **人工取消**（用户在 issue/API 上点取消）：`error` / `failure_reason`
//!   保持 `NULL`（用户自己知道为什么），但要把 delegated-failure 恢复信号
//!   终态确认掉（`delivered_comment_ids` 的重算分支）。
//! - 两者都写 `cancelled_by_type ∈ {'system','user'}` + `cancelled_by_id/name`。
//!   **上游从不往 `failure_reason` 写 `manual`**（`manual` 只是本仓的分类标签，
//!   见 [`crate::retry::FailureReason::Manual`]）。
//!
//! # 范围
//!
//! 语句里的 `WHERE status IN (...5 个非终态...)` 是 CAS：命中不到行就是
//! **故意的不写**（`Exec` 不报错），不是失败。所以终态行上的取消在领域层
//! 是 [`CancelOutcome::AlreadyTerminal`]，由调用方决定把它当幂等成功
//! （重复取消）还是冲突（已完成后取消）。
//!
//! `AckTaskCancelled` 还会调用 `FinalizeDeferredCancelledChat`（聊天草稿恢复，
//! `service/task.go:3305`，涉及 `chat_session` 锁与 `chat_draft_restore` 表）——
//! 那是 chat 领域的事，M3-3 只做任务列的写单与重播，**不建模**它。

use serde::{Deserialize, Serialize};

use crate::error::TaskError;
use crate::retry::FailureReason;
#[cfg(test)]
use crate::retry::RetryBudget;
use crate::state::{CancelledBy, ColumnWrite, TaskEvent, TaskState, TaskTransition};
use crate::status::TaskStatus;

/// 取消请求（三种取消共用一个载荷）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cancellation {
    /// 谁取消的（`cancelled_by_type` / `_id` / `_name`）。
    pub by: CancelledBy,
    /// 是否用户显式点取消：
    /// `true` ⇒ 终态确认 delegated-failure 恢复信号；
    /// `false` ⇒ 自动修复，恢复信号保持可重放。
    pub user_initiated: bool,
    /// 「用户没要求的取消」的解释（`error` + `failure_reason`）。
    ///
    /// 用户取消时**必须**是 `None`：上游人工取消不写这两列。
    pub explanation: Option<CancelExplanation>,
}

/// 系统取消的解释（对应 `CancelAgentTaskWithReason` 的 `error` / `failure_reason`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelExplanation {
    /// `error` 列（人类可读，例如「daemon 版本过旧，worktree 声明被拒」）。
    pub message: String,
    /// `failure_reason` 列（规范值）。
    pub reason: FailureReason,
}

impl Cancellation {
    /// 人工取消（`CancelAgentTaskByUser`）。
    #[must_use]
    pub const fn by_user(id: Option<mc_core::Id>, name: Option<String>) -> Self {
        Self {
            by: CancelledBy::User { id, name },
            user_initiated: true,
            explanation: None,
        }
    }

    /// 系统自动取消、不给解释（`CancelAgentTask`）。
    #[must_use]
    pub const fn by_system() -> Self {
        Self {
            by: CancelledBy::System,
            user_initiated: false,
            explanation: None,
        }
    }

    /// 系统自动取消并给出解释（`CancelAgentTaskWithReason`）。
    ///
    /// # Errors
    ///
    /// [`TaskError::MalformedEvent`]：`message` 去空白后为空（空 `error` 等于
    /// 没解释，但走的是「带理由」那条语句 ⇒ 静默丢解释）。
    pub fn by_system_with_reason(message: &str, reason: FailureReason) -> Result<Self, TaskError> {
        let message = message.trim();
        if message.is_empty() {
            return Err(TaskError::MalformedEvent {
                detail: "系统取消的解释不能是空白（CancelAgentTaskWithReason 的 error 列）",
            });
        }
        Ok(Self {
            by: CancelledBy::System,
            user_initiated: false,
            explanation: Some(CancelExplanation {
                message: message.to_owned(),
                reason,
            }),
        })
    }
}

/// `delivered_comment_ids` 这一列怎么处理（`CancelAgentTaskByUser` 的 `CASE`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveredCommentsPlan {
    /// 系统取消：语句根本不写这一列。
    Untouched,
    /// 人工取消但无需 join：保持原值（高频路径）。
    KeepUnchanged,
    /// 人工取消且可能存在 delegated-failure 恢复信号：仓储层按 join 重算。
    ///
    /// 领域层只做**分支判定**：具体集合要靠 `comment` / `agent_task_queue`
    /// 的 join 才能算出来，那是 M3-6 的活。
    RecomputeRecoverySignalReceipts,
}

/// 判定 `delivered_comment_ids` 该走哪条分支。
///
/// `recovery_signal_present` 是**廉价形状探测**的结果（上游先看
/// `trigger_comment_id` / `coalesced_comment_ids` 的形态，只有可能命中时才做
/// join）。传 `true` 只是「可能有」，不是「一定有」。
#[must_use]
pub fn delivered_comments_plan(
    user_initiated: bool,
    trigger_comment_id: Option<mc_core::Id>,
    coalesced_comment_ids: &[mc_core::Id],
    recovery_signal_present: bool,
) -> DeliveredCommentsPlan {
    if !user_initiated {
        return DeliveredCommentsPlan::Untouched;
    }
    if trigger_comment_id.is_none() && coalesced_comment_ids.is_empty() {
        // 高频路径：聊天任务与普通 issue 任务几乎不带恢复信号，先看形状再决定
        // 要不要为这一列付出一次 join。
        return DeliveredCommentsPlan::KeepUnchanged;
    }
    if !recovery_signal_present {
        return DeliveredCommentsPlan::KeepUnchanged;
    }
    DeliveredCommentsPlan::RecomputeRecoverySignalReceipts
}

/// 取消的领域结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    /// 取消生效（CAS 命中，状态列被改写）。
    Applied {
        /// 状态转移（`from → cancelled`，写集 = 语句的 `SET` 列表）。
        transition: TaskTransition,
        /// `delivered_comment_ids` 的分支判定。
        delivered_comments: DeliveredCommentsPlan,
        /// 是否终态确认 delegated-failure 恢复信号（= 人工取消）。
        acknowledges_delegated_failure: bool,
    },
    /// 行已经是终态：CAS 未命中，**没有**任何列被写。
    ///
    /// 重复取消（`status == cancelled`）可以当幂等成功；
    /// 已 `completed` / `failed` 的行则更适合当冲突。
    AlreadyTerminal {
        /// 行当前的状态。
        status: TaskStatus,
    },
}

impl CancelOutcome {
    /// 是否真的取消了（写了列）。
    #[must_use]
    pub const fn is_applied(&self) -> bool {
        matches!(self, Self::Applied { .. })
    }

    /// 幂等重复取消（行本来就已经 `cancelled`）。
    #[must_use]
    pub const fn is_idempotent_replay(&self) -> bool {
        matches!(
            self,
            Self::AlreadyTerminal {
                status: TaskStatus::Cancelled
            }
        )
    }

    /// 写单（`AlreadyTerminal` 时为空）。
    #[must_use]
    pub fn writes(&self) -> &[ColumnWrite] {
        match self {
            Self::Applied { transition, .. } => &transition.writes,
            Self::AlreadyTerminal { .. } => &[],
        }
    }
}

/// 取消一个任务（纯函数，不改入参）。
///
/// # Errors
///
/// - [`TaskError::MalformedEvent`]：`explanation` 与 `user_initiated` 组合非法
///   （人工取消携带解释，或系统取消的解释是空白）。
/// - [`TaskError::IllegalTransition`]：状态**非终态但也不可取消**（当前状态面下
///   不可能出现，留给未来加状态时兜底，而不是静默当成成功）。
pub fn plan_cancellation(
    state: &TaskState,
    cancellation: &Cancellation,
    delivered_comments: DeliveredCommentsPlan,
    at: mc_core::Timestamp,
) -> Result<CancelOutcome, TaskError> {
    if cancellation.user_initiated && cancellation.explanation.is_some() {
        return Err(TaskError::MalformedEvent {
            detail: "人工取消不写 error/failure_reason（CancelAgentTaskByUser 的注释）",
        });
    }
    if let Some(explanation) = &cancellation.explanation {
        if explanation.message.trim().is_empty() {
            return Err(TaskError::MalformedEvent {
                detail: "系统取消的解释不能是空白（CancelAgentTaskWithReason 的 error 列）",
            });
        }
    }
    if state.status.is_terminal() {
        return Ok(CancelOutcome::AlreadyTerminal {
            status: state.status,
        });
    }
    if !state.status.is_cancellable() {
        return Err(TaskError::IllegalTransition {
            from: state.status,
            event: TaskEvent::Cancelled {
                by: cancellation.by.clone(),
            },
        });
    }

    let mut next = state.clone();
    let transition = next.apply(
        TaskEvent::Cancelled {
            by: cancellation.by.clone(),
        },
        at,
    )?;
    let mut transition = transition;
    if let Some(explanation) = &cancellation.explanation {
        transition
            .writes
            .push(ColumnWrite::ErrorMessage(Some(explanation.message.clone())));
        transition
            .writes
            .push(ColumnWrite::FailureReason(explanation.reason));
    }

    Ok(CancelOutcome::Applied {
        transition,
        delivered_comments,
        acknowledges_delegated_failure: cancellation.user_initiated,
    })
}

// ---------------------------------------------------------------------------
// cancel-ack
// ---------------------------------------------------------------------------

/// daemon 的取消确认体（`TaskCancelAckRequest`，`handler/daemon.go:5187`）。
///
/// 四个字段都可空：老 daemon 发 `{}`，`body` 解码失败也不能破坏取消契约
/// （上游对 body 解码错误**故意忽略**）。所有字段在落库前 trim。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelAck {
    /// `branch_name`：被取消的 worktree 任务已经提交了 agent 的产出
    /// （daemon 在得知取消**之前**就 finalize 了 worktree），这是唯一能报出
    /// 产出在哪儿的通道。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    /// `durable_work_dir`：仅当 Finalize 确认一次性 worktree 已删除、
    /// 项目目录成为权威时才带。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_work_dir: Option<String>,
    /// `error_message`：worktree Finalize **中止**时才有 —— 没有分支，
    /// 那段指明保留目录的错误文本就是 agent 产出的唯一线索。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// `failure_reason`：与 `error_message` 成对（仅非空时落库）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

impl CancelAck {
    /// 空 ack（老 daemon 的 `{}`）。
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// 四个字段逐个 trim，空串视为「没带」（上游用的是 `strings.TrimSpace != ""`）。
    #[must_use]
    pub fn has_payload(&self) -> bool {
        self.branch_name
            .as_deref()
            .map(str::trim)
            .is_some_and(|v| !v.is_empty())
            || self
                .durable_work_dir
                .as_deref()
                .map(str::trim)
                .is_some_and(|v| !v.is_empty())
            || self
                .error_message
                .as_deref()
                .map(str::trim)
                .is_some_and(|v| !v.is_empty())
    }
}

/// 行在 ack 到达时的现状（ack 的三条语句都是「不覆盖已写值 + `status='cancelled'` CAS」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelAckTarget {
    /// 行当前状态；只有 `cancelled` 才允许写。
    pub status: TaskStatus,
    /// `branch_name` 现值。
    pub branch_name: Option<String>,
    /// `durable_work_dir` 现值。
    pub durable_work_dir: Option<String>,
    /// `error` 现值（`NULL` 或空串都算「没写过」）。
    pub error: Option<String>,
}

impl CancelAckTarget {
    /// 从任务状态 + 非状态列现值构造。
    #[must_use]
    pub fn from_state(
        state: &TaskState,
        branch_name: Option<String>,
        durable_work_dir: Option<String>,
    ) -> Self {
        Self {
            status: state.status,
            branch_name,
            durable_work_dir,
            error: state.failure.as_ref().and_then(|f| f.message.clone()),
        }
    }

    /// `error` 列是否还是空的（`NULL` 或空串）。
    #[must_use]
    pub fn error_is_unset(&self) -> bool {
        match &self.error {
            None => true,
            Some(value) => value.trim().is_empty(),
        }
    }

    /// 是否还是被取消的行（ack 的 CAS 条件）。
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self.status, TaskStatus::Cancelled)
    }
}

/// ack 要写的一列（或一组列）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelAckWrite {
    /// `durable_work_dir = COALESCE(durable_work_dir, <v>)`。
    DurableWorkDir(String),
    /// `branch_name = COALESCE(branch_name, <v>)`。
    BranchName(String),
    /// `error = <v>, failure_reason = COALESCE(failure_reason, <v>)`。
    Error {
        /// `error` 列。
        message: String,
        /// `failure_reason` 列（ack 没带就是 `NULL`）。
        failure_reason: Option<String>,
    },
}

impl CancelAckWrite {
    /// 写的是哪一列（测试与仓储层断言用）。
    #[must_use]
    pub const fn column(&self) -> CancelAckColumn {
        match self {
            Self::DurableWorkDir(_) => CancelAckColumn::DurableWorkDir,
            Self::BranchName(_) => CancelAckColumn::BranchName,
            Self::Error { .. } => CancelAckColumn::Error,
        }
    }
}

/// [`CancelAckWrite`] 对应的列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelAckColumn {
    /// `durable_work_dir`。
    DurableWorkDir,
    /// `branch_name`。
    BranchName,
    /// `error`（伴随 `failure_reason`）。
    Error,
}

/// ack 的结算结果。
///
/// `writes` 的顺序**就是上游 handler 的执行顺序**：durable work dir → branch
/// name → error（前两条任一失败就 500 返回，第三条失败也 500）——
/// 仓储层应保持同序，这样部分失败时的落库形状与上游一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelAckPlan {
    /// 实际要写的列（空 = 全部被 CAS 或 COALESCE 拒掉）。
    pub writes: Vec<CancelAckWrite>,
    /// 是否需要重播 `task:cancelled`。
    ///
    /// `task:cancelled` 在取消那一刻就播过了，而 ack 可能后到 —— 客户端手里
    /// 可能已经是一份没有 branch/error 的快照，且不会自己再拉。所以**只要有
    /// 一列真的写了**就要重播，一列都没写就不用（上游 `delivered` 标志）。
    pub rebroadcast: bool,
}

impl CancelAckPlan {
    /// 什么都不写（重放的 ack / 非 `cancelled` 行）。
    #[must_use]
    pub const fn noop() -> Self {
        Self {
            writes: Vec::new(),
            rebroadcast: false,
        }
    }

    /// 是否无事可做。
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.writes.is_empty()
    }

    /// 是否要写某列。
    #[must_use]
    pub fn writes_column(&self, probe: CancelAckColumn) -> bool {
        self.writes.iter().any(|w| w.column() == probe)
    }
}

/// 结算 cancel-ack（纯函数）。
///
/// 规则逐条对应上游：
///
/// 1. 行不是 `cancelled` ⇒ 一条都不写（三条语句都带 `AND status = 'cancelled'`
///    CAS；daemon 对**每个**终态都会 ack，迟到的 ack 不能把 branch/error 盖到
///    `completed` / `failed` 行上）。
/// 2. 字段 trim 后为空 ⇒ 跳过（`strings.TrimSpace(...) != ""`）。
/// 3. `durable_work_dir` / `branch_name` 现值非空 ⇒ 跳过（`COALESCE` 不覆盖）。
/// 4. `error` 现值非空 ⇒ 整条 `Error` 写单跳过（`(error IS NULL OR error = '')`
///    是整条 UPDATE 的条件，连 `failure_reason` 一起不写）。
/// 5. 只要有一列写了 ⇒ `rebroadcast = true`。
///
/// 注意 ack **永远**返回 200（CAS 拒绝是故意的不写，不是错误），所以调用方
/// 不该因为 [`CancelAckPlan::is_noop`] 而报错。
#[must_use]
pub fn plan_cancel_ack(ack: &CancelAck, target: &CancelAckTarget) -> CancelAckPlan {
    if !target.is_cancelled() {
        return CancelAckPlan::noop();
    }

    let mut writes = Vec::new();

    if let Some(dir) = trimmed(ack.durable_work_dir.as_deref()) {
        if !has_text(target.durable_work_dir.as_deref()) {
            writes.push(CancelAckWrite::DurableWorkDir(dir.to_owned()));
        }
    }
    if let Some(branch) = trimmed(ack.branch_name.as_deref()) {
        if !has_text(target.branch_name.as_deref()) {
            writes.push(CancelAckWrite::BranchName(branch.to_owned()));
        }
    }
    if let Some(message) = trimmed(ack.error_message.as_deref()) {
        if target.error_is_unset() {
            writes.push(CancelAckWrite::Error {
                message: message.to_owned(),
                failure_reason: trimmed(ack.failure_reason.as_deref()).map(str::to_owned),
            });
        }
    }

    let rebroadcast = !writes.is_empty();
    CancelAckPlan {
        writes,
        rebroadcast,
    }
}

/// 空白的值不算「写了」（上游对 `branch_name` 也只看非空）。
fn has_text(value: Option<&str>) -> bool {
    match value {
        Some(value) => !value.trim().is_empty(),
        None => false,
    }
}

/// trim 后仍非空才返回。
fn trimmed(value: Option<&str>) -> Option<&str> {
    match value.map(str::trim) {
        Some(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

/// ack 里 `failure_reason` 的规范化（保留原始字符串：ack 是 daemon 传来的自由文本，
/// 上游也没做枚举校验，只 trim + 空则 `NULL`）。
#[must_use]
pub fn normalize_ack_failure_reason(ack: &CancelAck) -> Option<&str> {
    trimmed(ack.failure_reason.as_deref())
}

/// 构造一个可取消的 `queued` 行（测试用；生产路径由 store 构造）。
#[cfg(test)]
fn queued() -> TaskState {
    let (state, _) = TaskState::enqueue(RetryBudget::FIRST_RUN);
    state
}

/// 把行推进到 `running`（测试用）。
#[cfg(test)]
fn running(at: mc_core::Timestamp) -> TaskState {
    let mut state = queued();
    state.apply(TaskEvent::Dispatch, at).unwrap();
    state.apply(TaskEvent::Running, at).unwrap();
    state
}

#[cfg(test)]
mod tests;

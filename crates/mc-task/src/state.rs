//! 任务状态机 —— `agent_task_queue` 行级生命周期的**纯**模型。
//!
//! 本模块不认识 SQL、不认识 HTTP、不读时钟（时间一律由调用方以 [`mc_core::Timestamp`]
//! 传入），所以每条边都可以被逐条断言。
//!
//! # 真值来源：事件表 + 真实 UPDATE 语句，两者都要
//!
//! `docs/15` §2.3 要求以 `pkg/protocol/events.go:32-42` 的事件表为**唯一**真值来源。
//! 该文件恰好 9 个 `task:` 常量，其中：
//!
//! ```text
//! task:queued                   ∅ → queued                        （enqueue / retry create）
//! task:dispatch                 queued → dispatched               （daemon claim）
//! task:running                  dispatched → running              （daemon started）
//! task:waiting_local_directory  dispatched → waiting_local_directory
//! task:completed                running → completed
//! task:failed                   running → failed
//! task:cancelled                * → cancelled
//! task:progress / task:message  运行期附加信息（**不改变状态**）
//! ```
//!
//! （`docs/15` §2.3 把最后两条合写成一行，所以原文说「8 行」；上游实为 9 个常量。
//! 本 crate 的 [`TaskEventKind::WIRE_EVENTS`] 逐字等于这 9 个。）
//!
//! 但事件表只标注了「用户想看到什么变化」，**不是**全部 DB 边。真实 UPDATE 语句里
//! 还存在 4 条没有对应 `task:` 常量的边，[`TaskEventKind`] 一并建模（并在
//! `docs/19` 与 PR 里标明「本仓从 SQL 实测补入，非 events.go 所载」）：
//!
//! - `dispatched → queued`：`RequeueAgentTaskAfterClaimFailure`
//!   （claim 收尾失败、响应未写出，把该次 claim 代际放回队列；`started_at IS NULL` +
//!   `dispatched_at` CAS 守卫）。
//! - `dispatched → dispatched`：`ReclaimStaleDispatchedTaskForRuntime`
//!   （服务端其实已发出、响应没到 daemon；刷新 `dispatched_at` + `prepare_lease_expires_at`）。
//! - `deferred → queued`：`PromoteDeferredChannelIssueTask`
//!   （停泊行 `fire_at` 到期/媒体就绪后的提升）。
//! - `∅ → deferred`：插入时即停泊（`fire_at` 在未来 / `channel_issue_media_pending`）。
//!
//! 另有两条**事件表没写、但 SQL 明确允许**的入边，同样建模：
//!
//! - `waiting_local_directory → running`：`StartAgentTask` 的
//!   `WHERE status IN ('dispatched', 'waiting_local_directory')`
//!   —— daemon 拿到路径锁后直接翻 `running`（事件表注释只提 dispatched）；
//! - `queued | dispatched | waiting_local_directory → failed`：`ExpireStaleQueuedTasks` /
//!   `FailStaleTasks` / `FailAgentTask` 的 WHERE 子句都比事件表注释宽。

use serde::{Deserialize, Serialize};

use crate::error::TaskError;
use crate::retry::{FailureReason, RetryBudget, RetryChild};
use crate::status::TaskStatus;

/// `TaskEventKind` 的字符串形态与其 serde 实现放在子模块里（本文件贴住 800 行尺寸门）。
pub mod wire;

/// 上游 `task:` 事件名前缀（前端按此前缀订阅并失效 workspace 任务快照）。
pub const TASK_EVENT_PREFIX: &str = "task:";

/// 触发这一次转移的事件（线上事件 or 服务端内部生命周期操作）。
///
/// 带载荷的变体：`WaitingLocalDirectory` 必须带 `wait_reason`、`Failed` 必须带
/// 规范的 `failure_reason`、`Cancelled` 必须知道是谁取消的 —— 这三样都是**列写入**，
/// 缺了就没法落库，所以不允许匿名事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskEvent {
    /// `task:queued`：入队 / 重试创建 / `deferred` 提升。
    Queued,
    /// `task:dispatch`：daemon claim。
    Dispatch,
    /// `task:running`：daemon 已开工。
    Running,
    /// `task:waiting_local_directory`：等 `local_directory` 路径锁。
    WaitingLocalDirectory {
        /// 写进 `wait_reason`（UI 用来显示争用的路径）。
        wait_reason: String,
    },
    /// `task:progress`：附加信息，**不改变状态**。
    Progress,
    /// `task:message`：附加信息，**不改变状态**。
    Message,
    /// `task:completed`。
    Completed,
    /// `task:failed`。
    Failed {
        /// 规范 `failure_reason`（上游 `pkg/taskfailure` 的值）。
        reason: FailureReason,
        /// 落 `error` 列的人类可读信息。
        message: Option<String>,
    },
    /// `task:cancelled`。
    Cancelled {
        /// 写 `cancelled_by_type` / `cancelled_by_id` / `cancelled_by_name`（迁移 `458`）。
        by: CancelledBy,
    },
    /// 仅创建：`∅ → deferred`（`fire_at` 在未来 / channel 媒体未就绪）。
    Deferred,
    /// `dispatched → dispatched`：重投递并刷新租约。
    Reclaim,
    /// `dispatched → queued`：claim 收尾失败，放回队列。
    RequeueAfterClaimFailure,
}

/// [`TaskEvent`] 的类型标签（无载荷），用于转移表、矩阵测试与文档核对。
///
/// 字符串形态由 [`TaskEventKind::as_str`] / [`TaskEventKind::parse`] 定义
/// （线上事件 = `task:<name>`，与 `events.go` 逐字一致），serde 直接复用它们 ——
/// 这里**不能**用 `#[derive(Serialize)] + rename_all`，那会得出 `"queued"`
/// 这种丢掉 `task:` 前缀的形态，线上订阅就错了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskEventKind {
    /// `task:queued`。
    Queued,
    /// `task:dispatch`。
    Dispatch,
    /// `task:running`。
    Running,
    /// `task:waiting_local_directory`。
    WaitingLocalDirectory,
    /// `task:progress`。
    Progress,
    /// `task:message`。
    Message,
    /// `task:completed`。
    Completed,
    /// `task:failed`。
    Failed,
    /// `task:cancelled`。
    Cancelled,
    /// 仅创建：停泊为 `deferred`。
    Deferred,
    /// 服务端内部：重投递（刷新租约）。
    Reclaim,
    /// 服务端内部：claim 收尾失败放回队列。
    RequeueAfterClaimFailure,
}

impl TaskEventKind {
    /// 上游 `events.go:32-42` 的 9 个 `task:` 常量，**顺序逐字一致**。
    pub const WIRE_EVENTS: [Self; 9] = [
        Self::Queued,
        Self::Dispatch,
        Self::Running,
        Self::WaitingLocalDirectory,
        Self::Progress,
        Self::Completed,
        Self::Failed,
        Self::Message,
        Self::Cancelled,
    ];

    /// 全部 12 种事件。
    pub const ALL: [Self; 12] = [
        Self::Queued,
        Self::Dispatch,
        Self::Running,
        Self::WaitingLocalDirectory,
        Self::Progress,
        Self::Message,
        Self::Completed,
        Self::Failed,
        Self::Cancelled,
        Self::Deferred,
        Self::Reclaim,
        Self::RequeueAfterClaimFailure,
    ];

    /// 线上事件名；服务端内部操作（`Deferred` / `Reclaim` / `RequeueAfterClaimFailure`）
    /// 返回 `None` —— 上游**没有**给它们 `task:` 常量，本仓也不许自造线上事件名。
    #[must_use]
    pub const fn wire_name(self) -> Option<&'static str> {
        match self {
            Self::Queued => Some("task:queued"),
            Self::Dispatch => Some("task:dispatch"),
            Self::Running => Some("task:running"),
            Self::WaitingLocalDirectory => Some("task:waiting_local_directory"),
            Self::Progress => Some("task:progress"),
            Self::Message => Some("task:message"),
            Self::Completed => Some("task:completed"),
            Self::Failed => Some("task:failed"),
            Self::Cancelled => Some("task:cancelled"),
            Self::Deferred | Self::Reclaim | Self::RequeueAfterClaimFailure => None,
        }
    }

    /// 是否是上游线上事件（有 `task:` 常量）。
    #[must_use]
    pub const fn is_wire(self) -> bool {
        self.wire_name().is_some()
    }

    /// 该事件是否会**改变**状态列。
    ///
    /// `task:progress` / `task:message` 不改（它们是通知）；
    /// `Reclaim` 是 `dispatched → dispatched` 自转，也不改。
    #[must_use]
    pub const fn changes_status(self) -> bool {
        !matches!(self, Self::Progress | Self::Message | Self::Reclaim)
    }

    /// 是否只允许在创建时发生（在既有行上 `apply` 必须 `Err`）。
    #[must_use]
    pub const fn is_creation_only(self) -> bool {
        matches!(self, Self::Deferred)
    }

    /// 该事件在转移表里允许的**源状态**（`None` 表示创建路径）。
    ///
    /// 分臂是有意的：即使两个事件今天允许同一组源状态（`waiting_local_directory`
    /// 与两个 reclaim 事件都只从 `dispatched` 出发），它们来源不同、上游语句不同，
    /// 合并会把「来源不同」这件事藏起来。
    #[must_use]
    #[allow(clippy::match_same_arms)]
    pub const fn source_statuses(self) -> &'static [TaskStatus] {
        match self {
            Self::Queued => &[TaskStatus::Deferred],
            Self::Dispatch => &[TaskStatus::Queued],
            // StartAgentTask: WHERE status IN ('dispatched','waiting_local_directory')
            Self::Running => &[TaskStatus::Dispatched, TaskStatus::WaitingLocalDirectory],
            Self::WaitingLocalDirectory => &[TaskStatus::Dispatched],
            // CompleteAgentTask: WHERE status = 'running'
            Self::Completed => &[TaskStatus::Running],
            // ExpireStaleQueuedTasks / FailStaleTasks / FailAgentTask /
            // FailExpiredRuntimeReconnectRetries 的并集
            Self::Failed => &[
                TaskStatus::Queued,
                TaskStatus::Dispatched,
                TaskStatus::Running,
                TaskStatus::WaitingLocalDirectory,
                TaskStatus::Deferred,
            ],
            // 上游取消语句的 WHERE 集合 = 全部非终态（见 status::is_cancellable 用例）
            Self::Cancelled => &[
                TaskStatus::Queued,
                TaskStatus::Dispatched,
                TaskStatus::Running,
                TaskStatus::WaitingLocalDirectory,
                TaskStatus::Deferred,
            ],
            // 通知类：任何状态都接受（终态也接受，因为它们不改状态）
            Self::Progress | Self::Message => &[],
            Self::Deferred => &[],
            Self::Reclaim | Self::RequeueAfterClaimFailure => &[TaskStatus::Dispatched],
        }
    }
}

impl TaskEvent {
    /// 类型标签。
    #[must_use]
    pub const fn kind(&self) -> TaskEventKind {
        match self {
            Self::Queued => TaskEventKind::Queued,
            Self::Dispatch => TaskEventKind::Dispatch,
            Self::Running => TaskEventKind::Running,
            Self::WaitingLocalDirectory { .. } => TaskEventKind::WaitingLocalDirectory,
            Self::Progress => TaskEventKind::Progress,
            Self::Message => TaskEventKind::Message,
            Self::Completed => TaskEventKind::Completed,
            Self::Failed { .. } => TaskEventKind::Failed,
            Self::Cancelled { .. } => TaskEventKind::Cancelled,
            Self::Deferred => TaskEventKind::Deferred,
            Self::Reclaim => TaskEventKind::Reclaim,
            Self::RequeueAfterClaimFailure => TaskEventKind::RequeueAfterClaimFailure,
        }
    }
}

/// 取消发起方 —— 迁移 `458` 的 `cancelled_by_type` / `cancelled_by_id` / `cancelled_by_name`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelledBy {
    /// 人工取消（`cancelled_by_type = 'user'`）。
    ///
    /// 上游用**这一对列**表达「人工终止」，而不是往 `failure_reason` 里写 `manual`
    /// —— 这就是 [`FailureReason::Manual`] 不落库的原因。
    User {
        /// `cancelled_by_id`（系统触发时为空）。
        id: Option<mc_core::Id>,
        /// `cancelled_by_name`（快照，用户改名后历史仍可读）。
        name: Option<String>,
    },
    /// 系统取消（`cancelled_by_type = 'system'`）。
    System,
}

impl CancelledBy {
    /// 写 `cancelled_by_type` 的值。
    #[must_use]
    pub const fn type_str(&self) -> &'static str {
        match self {
            Self::User { .. } => "user",
            Self::System => "system",
        }
    }

    /// `cancelled_by_id`。
    #[must_use]
    pub const fn id(&self) -> Option<mc_core::Id> {
        match self {
            Self::User { id, .. } => *id,
            Self::System => None,
        }
    }

    /// `cancelled_by_name`。
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::User { name, .. } => name.as_deref(),
            Self::System => None,
        }
    }

    /// 是否人工发起（自动重试必须对它恒为 false）。
    #[must_use]
    pub const fn is_user(&self) -> bool {
        matches!(self, Self::User { .. })
    }
}

/// 失败载荷（只在 `status = failed` 时存在）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// `failure_reason` 列（规范值）。
    pub reason: FailureReason,
    /// `error` 列。
    pub message: Option<String>,
}

/// 一条转移会写哪些列（顺序即建议写库顺序）。
///
/// 用 `Vec` 而不是一串 `bool`：既躲开 `clippy::struct_excessive_bools`，
/// 也让仓储层可以直接把它当写集遍历，而不是再解一遍 `match`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnWrite {
    /// `dispatched_at = <t>`。
    DispatchedAt(mc_core::Timestamp),
    /// `dispatched_at = NULL`。
    DispatchedAtCleared,
    /// `started_at = <t>`。
    StartedAt(mc_core::Timestamp),
    /// `completed_at = <t>`。
    CompletedAt(mc_core::Timestamp),
    /// `wait_reason = <v>` / `NULL`。
    WaitReason(Option<String>),
    /// `prepare_lease_expires_at = <v>` / `NULL`。
    PrepareLeaseExpiresAt(Option<mc_core::Timestamp>),
    /// `fire_at = <v>`。
    FireAt(Option<mc_core::Timestamp>),
    /// `failure_reason = <v>`。
    FailureReason(FailureReason),
    /// `error = <v>` / `NULL`。
    ErrorMessage(Option<String>),
    /// `cancelled_by_*` 三列。
    CancelledBy(CancelledBy),
}

/// 写入的是**哪一列**（无载荷），用于测试与仓储层断言写集。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    /// `dispatched_at`。
    DispatchedAt,
    /// `started_at`。
    StartedAt,
    /// `completed_at`。
    CompletedAt,
    /// `wait_reason`。
    WaitReason,
    /// `prepare_lease_expires_at`。
    PrepareLeaseExpiresAt,
    /// `fire_at`。
    FireAt,
    /// `failure_reason`。
    FailureReason,
    /// `error`。
    ErrorMessage,
    /// `cancelled_by_type` / `cancelled_by_id` / `cancelled_by_name`。
    CancelledBy,
}

impl ColumnWrite {
    /// 本写项对应的列。
    #[must_use]
    pub const fn column(&self) -> Column {
        match self {
            Self::DispatchedAt(_) | Self::DispatchedAtCleared => Column::DispatchedAt,
            Self::StartedAt(_) => Column::StartedAt,
            Self::CompletedAt(_) => Column::CompletedAt,
            Self::WaitReason(_) => Column::WaitReason,
            Self::PrepareLeaseExpiresAt(_) => Column::PrepareLeaseExpiresAt,
            Self::FireAt(_) => Column::FireAt,
            Self::FailureReason(_) => Column::FailureReason,
            Self::ErrorMessage(_) => Column::ErrorMessage,
            Self::CancelledBy(_) => Column::CancelledBy,
        }
    }
}

/// 一次转移的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskTransition {
    /// 源状态；`None` = `∅`（创建）。
    pub from: Option<TaskStatus>,
    /// 目标状态。
    pub to: TaskStatus,
    /// 触发事件。
    pub event: TaskEventKind,
    /// 状态列是否真的变了（`Progress`/`Message`/`Reclaim` 为 `false`）。
    pub status_changed: bool,
    /// 要写的列。
    pub writes: Vec<ColumnWrite>,
}

impl TaskTransition {
    /// `∅ → to` 的创建转移（构造器专用）。
    fn creation(to: TaskStatus, event: TaskEventKind, writes: Vec<ColumnWrite>) -> Self {
        Self {
            from: None,
            to,
            event,
            status_changed: true,
            writes,
        }
    }

    /// 在既有状态之间转移。
    fn between(
        from: TaskStatus,
        to: TaskStatus,
        event: TaskEventKind,
        writes: Vec<ColumnWrite>,
    ) -> Self {
        Self {
            from: Some(from),
            to,
            event,
            status_changed: from != to,
            writes,
        }
    }

    /// 写集里是否包含某个列的写。
    #[must_use]
    pub fn writes_column(&self, probe: Column) -> bool {
        self.writes.iter().any(|w| w.column() == probe)
    }
}

/// 任务行的领域状态 —— [`crate::store::TaskRecord`] 的 `state` 字段。
///
/// 字段与上游列一一对应，**没有任何本仓自造列**（`docs/15` §2.2 列出的
/// `retry_count` / `source_task_id` / `session_id` / `retired_session_id` /
/// `lease_expires_at` / `terminal_completed_at` / `delegated_failure_evidence` /
/// `initiator_user_id` 在这里一个都不存在）：
///
/// | 本字段 | 上游列 |
/// |---|---|
/// | `status` | `status` |
/// | `budget` | `attempt` / `max_attempts` |
/// | `parent_task_id` | `parent_task_id`（055） |
/// | `failure` | `failure_reason` / `error`（055） |
/// | `wait_reason` | `wait_reason`（109） |
/// | `fire_at` | `fire_at` |
/// | `dispatched_at` / `started_at` / `completed_at` | 同名列 |
/// | `prepare_lease_expires_at` | **`prepare_lease_expires_at`**（124） |
/// | `runtime_id` | `runtime_id` |
/// | `delegated_from_task_id` | `delegated_from_task_id` |
/// | `escalation_for_task_id` | `escalation_for_task_id` |
/// | `cancelled_by` | `cancelled_by_type` / `_id` / `_name`（458） |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskState {
    /// `status`。
    pub status: TaskStatus,
    /// `attempt` / `max_attempts`。
    pub budget: RetryBudget,
    /// `parent_task_id`。
    pub parent_task_id: Option<mc_core::Id>,
    /// `failure_reason` / `error`（仅 `failed`）。
    pub failure: Option<Failure>,
    /// `wait_reason`（仅 `waiting_local_directory` 期间有意义）。
    pub wait_reason: Option<String>,
    /// `fire_at`（`deferred` 的提升时刻）。
    pub fire_at: Option<mc_core::Timestamp>,
    /// `dispatched_at`。
    pub dispatched_at: Option<mc_core::Timestamp>,
    /// `started_at`。
    pub started_at: Option<mc_core::Timestamp>,
    /// `completed_at`（三个终态都会写）。
    pub completed_at: Option<mc_core::Timestamp>,
    /// `prepare_lease_expires_at`（124 引入；**不是** `lease_expires_at`）。
    pub prepare_lease_expires_at: Option<mc_core::Timestamp>,
    /// `runtime_id`。
    pub runtime_id: Option<mc_core::Id>,
    /// `delegated_from_task_id`：非空 + `failed` 就是上游的
    /// 「delegated failure」子类（本地 CHECK 里的 `delegated_failure` **不是**状态）。
    pub delegated_from_task_id: Option<mc_core::Id>,
    /// `escalation_for_task_id`。
    pub escalation_for_task_id: Option<mc_core::Id>,
    /// `cancelled_by_*`（仅 `cancelled`）。
    pub cancelled_by: Option<CancelledBy>,
}

impl TaskState {
    /// `∅ → queued`（入队 / 重试创建）。
    #[must_use]
    pub fn enqueue(budget: RetryBudget) -> (Self, TaskTransition) {
        let state = Self {
            status: TaskStatus::Queued,
            budget,
            parent_task_id: None,
            failure: None,
            wait_reason: None,
            fire_at: None,
            dispatched_at: None,
            started_at: None,
            completed_at: None,
            prepare_lease_expires_at: None,
            runtime_id: None,
            delegated_from_task_id: None,
            escalation_for_task_id: None,
            cancelled_by: None,
        };
        let transition =
            TaskTransition::creation(TaskStatus::Queued, TaskEventKind::Queued, vec![]);
        (state, transition)
    }

    /// `∅ → deferred`：插入时即停泊（`fire_at` 在未来，或 channel 媒体未就绪）。
    #[must_use]
    pub fn defer(
        budget: RetryBudget,
        fire_at: Option<mc_core::Timestamp>,
    ) -> (Self, TaskTransition) {
        let mut state = Self::blank(TaskStatus::Deferred, budget);
        state.fire_at = fire_at;
        let transition = TaskTransition::creation(
            TaskStatus::Deferred,
            TaskEventKind::Deferred,
            vec![ColumnWrite::FireAt(fire_at)],
        );
        (state, transition)
    }

    /// 依 [`RetryChild`] 创建自动重试子行（父指针指向失败的那一行）。
    ///
    /// `child.status` 已由 [`crate::retry::decide_retry`] 决定：有延迟 ⇒ `deferred`
    /// （等调度器按 `fire_at` 提升），否则 `queued`。
    #[must_use]
    pub fn from_retry_child(
        child: RetryChild,
        parent_task_id: mc_core::Id,
        fire_at: Option<mc_core::Timestamp>,
    ) -> (Self, TaskTransition) {
        let budget = RetryBudget::new(child.attempt, child.max_attempts);
        let (mut state, transition) = if child.status == TaskStatus::Deferred {
            Self::defer(budget, fire_at)
        } else {
            Self::enqueue(budget)
        };
        state.parent_task_id = Some(parent_task_id);
        (state, transition)
    }

    /// 空行骨架（构造器内部用）。
    fn blank(status: TaskStatus, budget: RetryBudget) -> Self {
        Self {
            status,
            budget,
            parent_task_id: None,
            failure: None,
            wait_reason: None,
            fire_at: None,
            dispatched_at: None,
            started_at: None,
            completed_at: None,
            prepare_lease_expires_at: None,
            runtime_id: None,
            delegated_from_task_id: None,
            escalation_for_task_id: None,
            cancelled_by: None,
        }
    }

    /// 上游的「delegated failure」子类：`status = 'failed' AND delegated_from_task_id IS NOT NULL`。
    ///
    /// 本地 `0001_init` 的 CHECK 把它当一个**独立状态** `delegated_failure` —— 那是
    /// 本仓的臆造，上游没有这个状态值（见 `contracts/upstream-schema.sql:1056`）。
    #[must_use]
    pub const fn is_delegated_failure(&self) -> bool {
        matches!(self.status, TaskStatus::Failed) && self.delegated_from_task_id.is_some()
    }

    /// `status = 'failed' AND escalation_for_task_id IS NOT NULL`。
    #[must_use]
    pub const fn is_escalation_failure(&self) -> bool {
        matches!(self.status, TaskStatus::Failed) && self.escalation_for_task_id.is_some()
    }

    /// 是否处于「占着 per-(issue, agent) 唯一槽位」的状态
    /// （`022` 的部分唯一索引 `idx_one_pending_task_per_issue` 家族）。
    #[must_use]
    pub const fn occupies_serialization_slot(&self) -> bool {
        matches!(self.status, TaskStatus::Queued | TaskStatus::Dispatched)
    }

    /// 该行是否已经开工（`started_at` 存在）。
    #[must_use]
    pub const fn has_started(&self) -> bool {
        self.started_at.is_some()
    }

    /// `deferred` 行是否可被提升为 `queued`（`fire_at` 已到）。
    #[must_use]
    pub fn is_promotable(&self, now: mc_core::Timestamp) -> bool {
        self.status.is_promotable(self.fire_at, now)
    }

    /// 施加一个事件（**纯函数**：时间由 `at` 传入，不读时钟、不做 I/O）。
    ///
    /// # Errors
    ///
    /// - [`TaskError::TerminalState`]：终态是吸收态，任何**改变状态**的事件都被拒。
    /// - [`TaskError::IllegalTransition`]：该事件在转移表里不接受当前源状态。
    /// - [`TaskError::MalformedEvent`]：载荷不合法（空 `wait_reason`；把
    ///   `manual` 当 `failure_reason` 写 —— 人工终止必须走 `Cancelled`）。
    /// - [`TaskError::AlreadyStarted`]：已经开工的行不能被重投递回收。
    pub fn apply(
        &mut self,
        event: TaskEvent,
        at: mc_core::Timestamp,
    ) -> Result<TaskTransition, TaskError> {
        let kind = event.kind();
        let from = self.status;
        self.validate(&event)?;

        let writes = match event {
            TaskEvent::Queued => {
                // 只有 deferred 提升能走到这里（入队由构造器完成）。
                let mut writes = vec![ColumnWrite::FireAt(None)];
                self.fire_at = None;
                self.status = TaskStatus::Queued;
                writes.push(ColumnWrite::PrepareLeaseExpiresAt(None));
                self.prepare_lease_expires_at = None;
                writes
            }
            TaskEvent::Dispatch => {
                self.dispatched_at = Some(at);
                self.status = TaskStatus::Dispatched;
                vec![ColumnWrite::DispatchedAt(at)]
            }
            TaskEvent::Running => {
                self.started_at = Some(at);
                self.wait_reason = None;
                // StartAgentTask 同时清 prepare lease（开工后租约不再有守护意义）。
                self.prepare_lease_expires_at = None;
                self.status = TaskStatus::Running;
                vec![
                    ColumnWrite::StartedAt(at),
                    ColumnWrite::WaitReason(None),
                    ColumnWrite::PrepareLeaseExpiresAt(None),
                ]
            }
            TaskEvent::WaitingLocalDirectory { wait_reason } => {
                self.wait_reason = Some(wait_reason.clone());
                self.status = TaskStatus::WaitingLocalDirectory;
                vec![ColumnWrite::WaitReason(Some(wait_reason))]
            }
            TaskEvent::Progress | TaskEvent::Message => vec![],
            TaskEvent::Completed => {
                self.completed_at = Some(at);
                self.status = TaskStatus::Completed;
                vec![ColumnWrite::CompletedAt(at)]
            }
            TaskEvent::Failed { reason, message } => {
                self.completed_at = Some(at);
                self.failure = Some(Failure {
                    reason,
                    message: message.clone(),
                });
                self.wait_reason = None;
                self.prepare_lease_expires_at = None;
                self.status = TaskStatus::Failed;
                vec![
                    ColumnWrite::CompletedAt(at),
                    ColumnWrite::FailureReason(reason),
                    ColumnWrite::ErrorMessage(message),
                    ColumnWrite::WaitReason(None),
                    ColumnWrite::PrepareLeaseExpiresAt(None),
                ]
            }
            TaskEvent::Cancelled { by } => {
                self.completed_at = Some(at);
                self.prepare_lease_expires_at = None;
                self.cancelled_by = Some(by.clone());
                self.status = TaskStatus::Cancelled;
                vec![
                    ColumnWrite::CompletedAt(at),
                    ColumnWrite::CancelledBy(by),
                    ColumnWrite::PrepareLeaseExpiresAt(None),
                ]
            }
            TaskEvent::Reclaim => {
                // dispatched → dispatched：刷新 dispatched_at 让服务端的派遣超时
                // 从这次「重投递」重新计时。
                self.dispatched_at = Some(at);
                vec![ColumnWrite::DispatchedAt(at)]
            }
            TaskEvent::RequeueAfterClaimFailure => {
                self.dispatched_at = None;
                self.prepare_lease_expires_at = None;
                self.status = TaskStatus::Queued;
                vec![
                    ColumnWrite::DispatchedAtCleared,
                    ColumnWrite::PrepareLeaseExpiresAt(None),
                ]
            }
            TaskEvent::Deferred => {
                // 仅创建路径；在既有行上已被 validate 拒绝。
                unreachable!("Deferred 是仅创建事件，validate 已拒绝")
            }
        };

        Ok(TaskTransition::between(from, self.status, kind, writes))
    }

    /// `apply` 的前置校验（与转移表一一对应）。
    fn validate(&self, event: &TaskEvent) -> Result<(), TaskError> {
        let kind = event.kind();

        if kind.changes_status() && self.status.is_terminal() {
            return Err(TaskError::TerminalState {
                status: self.status,
                event: event.clone(),
            });
        }

        if kind.is_creation_only() {
            return Err(TaskError::IllegalTransition {
                from: self.status,
                event: event.clone(),
            });
        }

        if matches!(kind, TaskEventKind::Progress | TaskEventKind::Message) {
            return Ok(());
        }

        if !kind.source_statuses().contains(&self.status) {
            return Err(TaskError::IllegalTransition {
                from: self.status,
                event: event.clone(),
            });
        }

        match event {
            TaskEvent::WaitingLocalDirectory { wait_reason } if wait_reason.trim().is_empty() => {
                Err(TaskError::MalformedEvent {
                    detail: "waiting_local_directory 必须带非空 wait_reason（109 的 wait_reason 列语义）",
                })
            }
            TaskEvent::Failed { reason, .. } if !reason.is_persisted_in_failure_reason_column() => {
                Err(TaskError::MalformedEvent {
                    detail: "manual 不是 failure_reason 列值：人工终止走 Cancelled + cancelled_by_type='user'",
                })
            }
            TaskEvent::Reclaim if self.has_started() => Err(TaskError::AlreadyStarted {
                started_at: self.started_at.expect("has_started() 已保证 Some"),
            }),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;

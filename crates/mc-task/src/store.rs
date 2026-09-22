//! `TaskStore` 端口：M3-3 定义的存储契约（**只有接口，没有生产实现**）。
//!
//! # 这个端口是干什么的
//!
//! M3-3 把 task 领域的**判定**写成了纯函数（状态机、租约扫描、重试决策、取消、
//! 结算），M3-6 负责把这些判定接到 PostgreSQL 上。这个 trait 就是那条缝：
//! 适配层只做「读一行 → 调纯函数 → 按 `TaskTransition` 写回」，不再自己判断业务。
//!
//! # 与上游 SQL 的对应
//!
//! | 方法 | 上游 |
//! |---|---|
//! | [`TaskStore::insert`] | `CreateAgentTask`（含 `idx_one_pending_task_per_issue`） |
//! | [`TaskStore::get`] | `GetAgentTask` |
//! | [`TaskStore::commit`] | 各状态迁移语句（`StartAgentTask` / `CompleteAgentTask` / …） |
//! | [`TaskStore::claim_next`] | `agent.sql:743` `ClaimAgentTask` |
//! | [`TaskStore::cancel`] | `CancelAgentTask{,WithReason,ByUser}`（`agent.sql:1564/1573/1676`） |
//! | [`TaskStore::apply_cancel_ack`] | `SetAgentTaskBranchName` / `SetAgentTaskDurableWorkDir` / `SetAgentTaskErrorIfEmpty` |
//! | [`TaskStore::list_usage`] / [`TaskStore::upsert_usage`] | `task_usage.sql` `GetTaskUsage` / `UpsertTaskUsage` |
//!
//! # 契约里的三条硬约束（适配层必须自己保证，纯函数管不了）
//!
//! 1. **CAS 语义**：`commit` 必须带 `WHERE id = $1 AND status = <from>`，
//!    `from` 为 `None` 时是插入。返回 [`CommitOutcome::LostRace`] 而不是报错 ——
//!    并发下「别人先改了」是正常结果，调用方重读一次再决定。
//! 2. **认领的串行化栅栏**：`claim_next` 必须在 SQL 里做（`FOR UPDATE SKIP LOCKED`
//!    或 `ClaimAgentTask` 的 `NOT EXISTS`），因为它要的是「同一 (`issue`, `agent`)
//!    上同时只有一个 dispatched/running」这种**跨行**约束。领域层的
//!    [`TaskState::occupies_serialization_slot`](crate::state::TaskState::occupies_serialization_slot)
//!    只是这条栅栏的读侧表达。
//! 3. **策略值一律由调用方传入**：`mc-task` 不依赖 `mc-config`，扫描阈值、
//!    租期、上限都从 [`StalePolicy`] / [`RetryBudget`] 进来，端口不自取配置。
//!
//! # 为什么没有生产用的内存实现
//!
//! 上游只有 PostgreSQL 一种存储，内存实现会让「重启即丢失」的任务队列看起来能跑。
//! 这里只放一个 `#[cfg(test)]` 的内存实现，用途是**证明端口够用且可实现**，
//! 不作为运行时选项。

use async_trait::async_trait;
use mc_core::{Id, Timestamp};

use crate::cancel::{CancelAck, CancelAckPlan, CancelOutcome, Cancellation};
use crate::error::TaskError;
use crate::lease::StalePolicy;
use crate::state::{TaskState, TaskTransition};
use crate::usage::TaskUsageRow;

/// `commit` 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    /// CAS 命中：这一行确实从 `from` 迁到了 `to`。
    Applied,
    /// CAS 落空：行的状态已经不是 `from` 了（并发写或被扫描器抢先）。
    ///
    /// 不是错误：调用方重读后重新决策。
    LostRace,
}

impl CommitOutcome {
    /// 是否写成功。
    #[must_use]
    pub const fn is_applied(self) -> bool {
        matches!(self, Self::Applied)
    }
}

/// 认领请求（`ClaimAgentTask` 的**参数面**）。
///
/// 阈值走 [`StalePolicy`]，因为 `ClaimAgentTask` 要用
/// `runtime_stale_secs`（心跳新鲜度）与 `prepare_lease_secs`（租期）两个值：
/// 认领成功的同时就把租约写下去。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimRequest {
    /// `agent_id`：串行化栅栏的键之一。
    pub agent_id: Id,
    /// `runtime_id`：必须有这一行、且 `status = 'online'`、心跳新鲜。
    pub runtime_id: Id,
    /// 「现在」（由调用方给，端口不读时钟）。
    pub now: Timestamp,
    /// 阈值集。
    pub policy: StalePolicy,
}

impl ClaimRequest {
    /// 构造。
    #[must_use]
    pub const fn new(agent_id: Id, runtime_id: Id, now: Timestamp, policy: StalePolicy) -> Self {
        Self {
            agent_id,
            runtime_id,
            now,
            policy,
        }
    }
}

/// 认领结果：认领后的行 + 这次迁移的写单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskClaim {
    /// 被认领的任务 id。
    pub task_id: Id,
    /// 已变为 `dispatched` 的行（`dispatched_at` / `prepare_lease_expires_at` 已写）。
    pub state: TaskState,
    /// `queued → dispatched` 的迁移。
    pub transition: TaskTransition,
}

/// task 存储端口。
///
/// 所有方法都返回 [`TaskError`]：端口把「业务上不可能」编码成错误
/// （例如 CAS 之外的非法迁移），把「并发/缺行」编码成 `Ok` 里的 `Option`/枚举。
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// 插入新行（`∅ → queued` / `∅ → deferred`）。
    ///
    /// `id` 由**调用方**铸造：上游在应用侧生成 `UUIDv7`（`pkg/dbid`，让连续入队落在
    /// 主键 B-tree 的相邻区间），仓储只兜底 `gen_random_uuid()`。端口收 `id` 而不是
    /// 让数据库生成，是因为紧接着的取消 / 认领 / 日志都要立刻用到它。
    ///
    /// 状态以外只吃 [`TaskState`]：创建迁移的写集就是这些字段的初值，所以不需要
    /// 单独的写单。**必须**让 `idx_one_pending_task_per_issue`（`022`/`037` 的部分
    /// 唯一索引）真的生效：同一 issue 的重复未决任务要靠数据库拒绝，不能靠先查后插。
    ///
    /// # Errors
    ///
    /// - [`TaskError::Conflict`]：违反部分唯一索引（已有未决任务）。
    async fn insert(&self, id: Id, state: &TaskState) -> Result<(), TaskError>;

    /// 读一行（`GetAgentTask`）。
    ///
    /// # Errors
    ///
    /// - [`TaskError::Backend`]：底层查询失败。
    async fn get(&self, id: Id) -> Result<Option<TaskState>, TaskError>;

    /// 落一次迁移（状态列 + [`TaskTransition::writes`]）。
    ///
    /// `transition.from` 为 `None` 的创建迁移不该走这里（用 [`TaskStore::insert`]）：
    /// CAS 需要一个源状态才能写 `WHERE status = <from>`。
    ///
    /// # Errors
    ///
    /// - [`TaskError::IllegalTransition`]：`from` 为 `None`（创建态）却被当作更新提交。
    /// - [`TaskError::NotFound`]：行不存在。
    /// - [`TaskError::Backend`]：底层写失败。
    async fn commit(&self, id: Id, transition: &TaskTransition)
        -> Result<CommitOutcome, TaskError>;

    /// 为 `(agent, runtime)` 认领下一个可认领任务（`ClaimAgentTask`）。
    ///
    /// SQL 侧必须同时满足：`status = 'queued'`；agent 的 `runtime_id` 与
    /// `atq.runtime_id` 一致（agent 重绑后持久化的 runtime 不再是权威）；
    /// runtime 在线且心跳在 `runtime_stale_secs` 内；wakeup 门未关闭
    /// （`context->>'wakeup_id'` 为空或指向未禁用且 revision 匹配的 `issue_wakeup`）；
    /// 同一 agent 在**同一串行化键**上没有 `dispatched` / `running` /
    /// `waiting_local_directory` 行（`issue` / `chat_session` / quick-create 形状三选一）。
    ///
    /// 顺序是 `priority DESC, created_at ASC, id ASC`。
    ///
    /// # Errors
    ///
    /// - [`TaskError::Backend`]：底层查询失败。
    async fn claim_next(&self, request: &ClaimRequest) -> Result<Option<TaskClaim>, TaskError>;

    /// 取消一行（三个 `CancelAgentTask*` 语句的合并语义）。
    ///
    /// 适配层负责：读行 → 用 [`crate::cancel::delivered_comments_plan`] 判定
    /// `delivered_comment_ids` 要不要重算（`CancelAgentTaskByUser` 的 CASE）→
    /// 调 [`crate::cancel::plan_cancellation`] → 带 `WHERE status IN (5 个非终态)`
    /// 的 CAS 写上 [`CancelOutcome::writes`]。
    ///
    /// 终态行不是错误：返回 [`CancelOutcome::AlreadyTerminal`]（重复取消 =
    /// 幂等成功，完成后再取消 = 冲突，由调用方按状态区分）。
    ///
    /// # Errors
    ///
    /// - [`TaskError::NotFound`]：行不存在。
    /// - [`TaskError::MalformedEvent`]：`explanation` 与 `user_initiated` 组合非法。
    async fn cancel(
        &self,
        id: Id,
        cancellation: &Cancellation,
        at: Timestamp,
    ) -> Result<CancelOutcome, TaskError>;

    /// 应用一次取消确认（`AckTaskCancelled`，`daemon.go:5182`）。
    ///
    /// 适配层：读行构造 [`crate::cancel::CancelAckTarget`] →
    /// [`crate::cancel::plan_cancel_ack`] → 按 `plan.writes` 写（三条 CAS 语句）。
    /// 返回 plan 是为了让调用方知道 `rebroadcast`：有列被写才重播。
    ///
    /// 这个接口**永远不该返回错误**来表示「没东西可写」：空载荷、状态不对、
    /// 值已存在都是 `Ok(plan)`，plan 里 `is_noop()` 为真。
    ///
    /// # Errors
    ///
    /// - [`TaskError::NotFound`]：行不存在。
    /// - [`TaskError::Backend`]：底层读写失败（上游对写失败是 500，不是静默）。
    async fn apply_cancel_ack(&self, id: Id, ack: &CancelAck) -> Result<CancelAckPlan, TaskError>;

    /// 读一个任务的 usage 行（`GetTaskUsage`）。
    ///
    /// 顺序：`ORDER BY model`（上游如此，读侧不重排）。
    ///
    /// # Errors
    ///
    /// - [`TaskError::Backend`]：底层查询失败。
    async fn list_usage(&self, task_id: Id) -> Result<Vec<TaskUsageRow>, TaskError>;

    /// 写一条 usage（`UpsertTaskUsage`）：**冲突即覆盖**，不累加。
    ///
    /// 实现要保证：`ON CONFLICT (task_id, provider, model) DO UPDATE`，
    /// 刷新 `updated_at`（小时汇总的脏标记），不碰 `created_at`。
    ///
    /// # Errors
    ///
    /// - [`TaskError::Backend`]：底层写失败（上游只 warn 并继续下一条）。
    async fn upsert_usage(&self, row: &TaskUsageRow) -> Result<(), TaskError>;
}

#[cfg(test)]
mod tests;

//! task 领域层（M3-3）：任务状态机、租约、重试、取消、usage 结算与 `TaskStore` 端口。
//!
//! 本 crate 是 **W3b（M3-6）/ W3c（M3-7）的共享底座**：它只做纯 Rust 计算，
//! **不写 SQL、不写路由、不写 daemon 侧代码**，因此不被 W0-B2 阻塞。
//! SQL 实现（`TaskRepo`）在 M3-6 的 `mc-repos`，路由在 M3-7 的 `mc-http`。
//!
//! # 真值来源（§8.4 仲裁：上游事实 > 本仓既有实现）
//!
//! | 领域事实 | 上游真值 |
//! |---|---|
//! | 8 个 `status` 值 | `contracts/upstream-schema.sql:1056` 的最终 CHECK |
//! | 9 个 `task:` 线上事件 | `server/pkg/protocol/events.go:32-42` |
//! | 状态迁移的**实际**边集 | `server/pkg/db/queries/agent.sql` 里每条 `UPDATE ... WHERE` |
//! | 5 个粗分类 `failure_reason` | `server/migrations/055_task_lease_and_retry.up.sql` |
//! | 27 个细分失败原因 | `server/pkg/taskfailure/failure.go` |
//! | 可重试集合 | `server/internal/service/task.go:5172` `retryableReasons` |
//! | 重试上限抬升规则 | `server/internal/service/task.go:5210` `retryAttemptCeiling` |
//! | 租约/清扫阈值 | `service/task.go:189/194/195`、`cmd/server/runtime_sweeper.go:43/74/89` |
//! | usage 行形状 | `task_usage`（`032`+`213`）、`task_usage_hourly`（`101`） |
//!
//! **本仓 `migrations/0001_init.up.sql:230` 的 `agent_task_queue` CHECK 是错的**
//! （缺 `dispatched` / `waiting_local_directory`，且把 `delegated_failure` 当状态），
//! 不能当契约用；见 [`status`] 的模块文档。
//!
//! # 范围限制
//!
//! 不引入自造列（`retry_count` / `source_task_id` / `session_id` /
//! `retired_session_id` / `lease_expires_at` / `terminal_completed_at` /
//! `delegated_failure_evidence` / `initiator_user_id`，`docs/15` §2.2）；
//! 不碰 `agent_task_queue` 的 DDL；内存实现只允许出现在测试里（plan1 §3.4）。
//!
//! # 模块
//!
//! - [`status`]：8 个状态值 + 取值面（终态/在飞/待处理/可认领/可取消/可提升）。
//! - [`retry`]：失败原因分类、可重试判定、重试上限与延迟、子行创建。
//! - [`state`]：事件与状态机（`apply` 是纯函数，时间由调用方传入），
//!   以及每次转移的列写集。
//! - [`lease`]：`prepare_lease_expires_at` 租约与四条「僵尸任务」清扫的纯判定。
//! - [`cancel`]：取消载荷与 `cancel-ack` 的纯结算。
//! - [`usage`]：`task_usage` / `task_usage_hourly` 的规范化与结算。
//! - [`store`]：`TaskStore` 端口（trait），由 M3-6 的 `mc-repos` 用 SQL 实现。
//! - [`error`]：领域错误（不含 I/O 与 HTTP 语义）。
//!
//! ```
//! use mc_task::{status::TaskStatus, retry::RetryBudget, state::{TaskEvent, TaskState}};
//! use mc_core::Timestamp;
//!
//! let now = Timestamp::from_unix(1_700_000_000);
//! let (mut task, _) = TaskState::enqueue(RetryBudget::FIRST_RUN);
//! assert_eq!(task.status, TaskStatus::Queued);
//!
//! task.apply(TaskEvent::Dispatch, now).unwrap();
//! task.apply(TaskEvent::Running, now).unwrap();
//! task.apply(TaskEvent::Completed, now).unwrap();
//! assert_eq!(task.status, TaskStatus::Completed);
//!
//! // 终态是吸收态：任何改变状态的事件都必须被拒绝。
//! assert!(task.apply(TaskEvent::Failed { reason: mc_task::retry::FailureReason::Timeout, message: None }, now).is_err());
//! ```

pub mod cancel;
pub mod error;
pub mod lease;
pub mod retry;
pub mod state;
pub mod status;
pub mod store;
pub mod usage;

pub use error::TaskError;
pub use retry::{FailureClass, FailureReason, RetryBudget, RetryDecision, RetrySkip};
pub use state::{CancelledBy, TaskEvent, TaskEventKind, TaskState, TaskTransition};
pub use status::TaskStatus;
pub use store::{ClaimRequest, CommitOutcome, TaskClaim, TaskStore};
pub use usage::{IssueUsageSummary, TaskUsageRow, UsageKey, UsageReport};

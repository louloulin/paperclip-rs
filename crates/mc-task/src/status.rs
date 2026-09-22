//! `agent_task_queue.status` —— 任务状态枚举。
//!
//! # 真值来源（为什么不是本仓 `migrations/0001_init.up.sql` 的 CHECK）
//!
//! 本仓 `0001_init.up.sql:234` 的 CHECK 是
//! `(queued, running, terminal_completed, terminal_cancelled, terminal_failed, delegated_failure)`
//! —— 它**缺** `dispatched` / `waiting_local_directory`，而且把
//! `delegated_failure` 错当成一个**独立状态**。上游真值来自
//! `contracts/upstream-schema.sql:1056`（`agent_task_queue_status_check`，由上游迁移
//! `109` 建立、后续迁移追加）：
//!
//! ```text
//! queued | dispatched | running | completed | failed | cancelled
//!        | waiting_local_directory | deferred
//! ```
//!
//! 逐条对应：
//!
//! - `dispatched`：daemon claim 之后的「已投递、未开工」态（上游 `ClaimAgentTask` 写
//!   `status='dispatched'` + `dispatched_at=now()` + `prepare_lease_expires_at`）。
//! - `waiting_local_directory`：上游迁移 `109`。daemon claim 成功但目标
//!   `project_resource(local_directory)` 被另一个在飞任务占着 —— 先落在本态等路径锁，
//!   拿到锁才翻 `running`。
//! - `deferred`：上游迁移 `109` 之后的 `deferred` 状态（本仓 `contracts/upstream-schema.sql`
//!   的最终 CHECK 里确实在）。它是**调度器侧停泊态**：`fire_at` 尚未到期的延后任务、
//!   以及 `runtime_offline` 重试子任务（等 runtime 心跳回来后被
//!   `PromoteDeferred…` / `fire_at` sweeper 提升为 `queued`）。注意
//!   `pkg/protocol/events.go` 的 `task:` 事件表**没有**给它一条边
//!   （daemon 侧观察不到「停泊」这件事，它是服务端调度决定），所以它不计入
//!   `docs/15` §2.3 那 8 行事件表；但它是**真状态**，必须能被解析与持久化。
//!
//! `delegated_failure` **不是**状态：上游建模为
//! `status='failed' AND delegated_from_task_id IS NOT NULL`
//! （见 `agent.sql` 的 `GetAgentTaskForDelegatedFailureUpdate` /
//! `HasRetryTaskForParent`，以及 `trigger_evidence_kind='delegated_failure'`）。
//! 所以本 crate 不定义该状态，而是把它做成 `TaskState::failed` 的一个**子类标记**
//! （`crate::state::Failure::is_delegated`）。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::TaskError;

/// `agent_task_queue.status` 的 8 个取值（上游最终 CHECK，见模块文档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// `∅ → queued`：入队 / 重试创建。
    Queued,
    /// `queued → dispatched`：daemon claim（`ClaimAgentTask`）。
    Dispatched,
    /// `dispatched → running`：daemon 已 `StartTask`。
    Running,
    /// `dispatched → waiting_local_directory`：等 `local_directory` 路径锁（迁移 `109`）。
    WaitingLocalDirectory,
    /// 终态：正常完成。
    Completed,
    /// 终态：失败（`delegated_failure` 是本态的子类，见模块文档）。
    Failed,
    /// 终态：取消（`* → cancelled`）。
    Cancelled,
    /// 调度器停泊态（迁移 `109` 之后的 CHECK；无 `task:` 事件边）。
    Deferred,
}

impl TaskStatus {
    /// 全部 8 个值，顺序与上游 CHECK 的 `status = ANY (ARRAY[...])` 一致。
    ///
    /// 顺序**有意**与 `contracts/upstream-schema.sql:1056` 逐字一致，便于对照核对。
    pub const ALL: [Self; 8] = [
        Self::Queued,
        Self::Dispatched,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::Cancelled,
        Self::WaitingLocalDirectory,
        Self::Deferred,
    ];

    /// 终态（吸收态）。
    pub const TERMINAL: [Self; 3] = [Self::Completed, Self::Failed, Self::Cancelled];

    /// 「在飞」：占用 `idx_one_pending_task_per_issue_agent_v2` 的
    /// per-(issue, agent) 序列化槽位，也是 `FailStaleTasks` / 取消集合的成员来源。
    pub const IN_FLIGHT: [Self; 3] = [Self::Dispatched, Self::Running, Self::WaitingLocalDirectory];

    /// 尚未开工：可被 claim，或被取消而无需打断执行。
    pub const PENDING: [Self; 2] = [Self::Queued, Self::Deferred];

    /// 线上字符串形式（写库 / 上 ws 的 `task:` 事件名后缀）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatched => "dispatched",
            Self::Running => "running",
            Self::WaitingLocalDirectory => "waiting_local_directory",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Deferred => "deferred",
        }
    }

    /// 解析线上字符串；不认识的输入一律 `Err`（**不**静默回落到默认值）。
    ///
    /// # Errors
    ///
    /// 输入不是 [`TaskStatus::ALL`] 里任何一个字符串时返回
    /// [`TaskError::UnknownStatus`]。
    pub fn parse(s: &str) -> Result<Self, TaskError> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == s)
            .ok_or_else(|| TaskError::UnknownStatus { got: s.to_owned() })
    }

    /// 终态是吸收态：`completed` / `failed` / `cancelled`。
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// 在飞（占序列化槽位）。
    #[must_use]
    pub const fn is_in_flight(self) -> bool {
        matches!(
            self,
            Self::Dispatched | Self::Running | Self::WaitingLocalDirectory
        )
    }

    /// 未开工（`queued` / `deferred`）。
    #[must_use]
    pub const fn is_pending(self) -> bool {
        matches!(self, Self::Queued | Self::Deferred)
    }

    /// 是否可被 claim（只有 `queued`；`deferred` 必须先被提升）。
    #[must_use]
    pub const fn can_be_claimed(self) -> bool {
        matches!(self, Self::Queued)
    }

    /// 是否可取消。上游取消路径的集合是
    /// `queued|dispatched|running|waiting_local_directory|deferred`
    /// —— 恰好等于「非终态」，所以这里用 `!is_terminal()` 表达，
    /// 并断言它与上游字面集合等价（见测试 `cancel_set_equals_upstream_literal`）。
    #[must_use]
    pub const fn is_cancellable(self) -> bool {
        !self.is_terminal()
    }

    /// 已完成过 `StartTask`（`started_at` 应当存在）的状态集合。
    #[must_use]
    pub const fn implies_started_at(self) -> bool {
        matches!(self, Self::Running | Self::Completed | Self::Failed)
    }

    /// `Deferred` 的 `fire_at` 是否已到（到达即可提升为 `queued`）。
    ///
    /// 只有 `deferred` 行才有意义；其余状态返回 `false`（无停泊态可提升）。
    #[must_use]
    pub fn is_promotable(
        self,
        fire_at: Option<mc_core::Timestamp>,
        now: mc_core::Timestamp,
    ) -> bool {
        if !matches!(self, Self::Deferred) {
            return false;
        }
        match fire_at {
            // 没有 `fire_at` 的停泊行（如 media-gated 的 channel 任务）由显式提升路径处理。
            None => true,
            Some(at) => at <= now,
        }
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TaskStatus {
    type Err = TaskError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_statuses_round_trip_through_wire_form() {
        for status in TaskStatus::ALL {
            assert_eq!(TaskStatus::parse(status.as_str()).unwrap(), status);
            assert_eq!(status.to_string(), status.as_str());
            assert_eq!(status.as_str().parse::<TaskStatus>().unwrap(), status);
        }
    }

    #[test]
    fn all_is_exactly_the_upstream_final_check() {
        // contracts/upstream-schema.sql:1056 的 8 个字面量，逐字比对。
        let upstream = [
            "queued",
            "dispatched",
            "running",
            "completed",
            "failed",
            "cancelled",
            "waiting_local_directory",
            "deferred",
        ];
        let ours: Vec<&str> = TaskStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(ours, upstream.to_vec());
    }

    #[test]
    fn local_check_values_are_rejected_not_silently_mapped() {
        // 本仓 0001_init 的私有状态名必须被拒绝：它们不是上游契约。
        for bogus in ["terminal_completed", "terminal_failed", "delegated_failure"] {
            let err = TaskStatus::parse(bogus).unwrap_err();
            assert_eq!(err, TaskError::UnknownStatus { got: bogus.into() });
        }
        assert!(TaskStatus::parse("").is_err());
        assert!(TaskStatus::parse("Queued").is_err(), "大小写敏感");
    }

    #[test]
    fn terminal_in_flight_pending_partition_is_disjoint_and_total() {
        let mut seen = Vec::new();
        for group in [
            TaskStatus::TERMINAL.as_slice(),
            TaskStatus::IN_FLIGHT.as_slice(),
            TaskStatus::PENDING.as_slice(),
        ] {
            for status in group {
                assert!(!seen.contains(&status), "{status} 出现在两个分组里");
                seen.push(status);
            }
        }
        assert_eq!(seen.len(), TaskStatus::ALL.len(), "分组未覆盖全部 8 个状态");
    }

    #[test]
    fn cancel_set_equals_upstream_literal() {
        // 上游取消查询的 WHERE 子句字面集合（agent.sql CancelAgentTask 等 7 处）。
        let upstream = [
            "queued",
            "dispatched",
            "running",
            "waiting_local_directory",
            "deferred",
        ];
        let ours: Vec<&str> = TaskStatus::ALL
            .into_iter()
            .filter(|s| s.is_cancellable())
            .map(TaskStatus::as_str)
            .collect();
        assert_eq!(ours, upstream.to_vec());
    }

    #[test]
    fn started_at_implied_states() {
        assert!(TaskStatus::Running.implies_started_at());
        assert!(TaskStatus::Completed.implies_started_at());
        assert!(TaskStatus::Failed.implies_started_at());
        // queued_expired 的 failed 行没有 started_at（见 usage 结算的时长口径），
        // 所以 implies_started_at 只用于反向校验，不当作充要条件。
        assert!(!TaskStatus::Dispatched.implies_started_at());
        assert!(!TaskStatus::Queued.implies_started_at());
    }

    #[test]
    fn serialized_form_is_the_wire_form() {
        for status in TaskStatus::ALL {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{}\"", status.as_str()));
        }
    }

    #[test]
    fn deferred_promotion_needs_fire_at_to_have_arrived() {
        use mc_core::Timestamp;
        let now = Timestamp::now();
        let past = Timestamp::from_unix(now.as_unix() - 10);
        let future = Timestamp::from_unix(now.as_unix() + 10);
        assert!(TaskStatus::Deferred.is_promotable(None, now));
        assert!(TaskStatus::Deferred.is_promotable(Some(past), now));
        assert!(!TaskStatus::Deferred.is_promotable(Some(future), now));
        assert!(!TaskStatus::Queued.is_promotable(Some(past), now));
    }
}

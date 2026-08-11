//! Service 实现 —— IssueAssignmentWakeupService。
//!
//! 设计：
//! - 接收 caller 注入的 heartbeat 依赖（DIP）
//! - 触发对应 hook
//! - 把核心逻辑委托给 `pc_repos::issue_assignment_wakeup`

use std::sync::Arc;

use pc_repos::issue_assignment_wakeup::{
    queue_issue_assignment_wakeup as core_queue,
    IssueAssignmentSnapshot, IssueAssignmentWakeupDeps,
    QueueIssueAssignmentWakeupInput, QueueIssueAssignmentWakeupOutcome,
    WakeupRequestedByActorType,
};

use super::hook::{IssueAssignmentWakeupHook, NoopIssueAssignmentWakeupHook};

/// 业务层的 queue request（独立于 pc-repos 的 input）。
///
/// 设计：
/// - 不暴露 `heartbeat` 字段（由 service 持有）
/// - 提供 builder 风格的 helper
#[derive(Debug, Clone)]
pub struct QueueRequest {
    pub issue: IssueAssignmentSnapshot,
    pub reason: String,
    pub mutation: String,
    pub context_source: String,
    pub requested_by_actor_type: Option<WakeupRequestedByActorType>,
    pub requested_by_actor_id: Option<String>,
    pub task_key: Option<String>,
    pub rethrow_on_error: bool,
}

impl Default for QueueRequest {
    fn default() -> Self {
        Self {
            issue: IssueAssignmentSnapshot::default(),
            reason: String::new(),
            mutation: String::new(),
            context_source: String::new(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        }
    }
}

/// Issue assignment wakeup service —— 封装 `queue_issue_assignment_wakeup` + Hook。
pub struct IssueAssignmentWakeupService {
    hook: Arc<dyn IssueAssignmentWakeupHook>,
}

impl std::fmt::Debug for IssueAssignmentWakeupService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueAssignmentWakeupService").finish()
    }
}

impl Default for IssueAssignmentWakeupService {
    fn default() -> Self {
        Self::new()
    }
}

impl IssueAssignmentWakeupService {
    pub fn new() -> Self {
        Self {
            hook: Arc::new(NoopIssueAssignmentWakeupHook),
        }
    }

    pub fn with_hook(hook: Arc<dyn IssueAssignmentWakeupHook>) -> Self {
        Self { hook }
    }

    pub fn hook(&self) -> Arc<dyn IssueAssignmentWakeupHook> {
        self.hook.clone()
    }

    /// Queue assignment wakeup（与 Node `queueIssueAssignmentWakeup` 1:1 对齐）。
    ///
    /// 行为：
    /// 1. 提前返回（无 assignee 或 status="backlog"）：返回 `Skipped`
    /// 2. 触发 `before_queue` hook
    /// 3. 调用 `heartbeat.wakeup(agentId, opts)`
    /// 4. 根据 outcome 触发 `after_queue` 或 `on_swallowed` hook
    /// 5. 返回 outcome
    pub async fn queue(
        &self,
        heartbeat: &dyn IssueAssignmentWakeupDeps,
        request: QueueRequest,
    ) -> QueueIssueAssignmentWakeupOutcome {
        self.hook.before_queue(&request.issue);

        let input = QueueIssueAssignmentWakeupInput {
            heartbeat,
            issue: request.issue.clone(),
            reason: request.reason.clone(),
            mutation: request.mutation.clone(),
            context_source: request.context_source.clone(),
            requested_by_actor_type: request.requested_by_actor_type,
            requested_by_actor_id: request.requested_by_actor_id.clone(),
            task_key: request.task_key.clone(),
            rethrow_on_error: request.rethrow_on_error,
        };

        let outcome = core_queue(input).await;

        match &outcome {
            QueueIssueAssignmentWakeupOutcome::Skipped => {
                self.hook.on_skipped(&request.issue.id, &request.issue.status);
            }
            QueueIssueAssignmentWakeupOutcome::Succeeded => {
                let agent_id = request.issue.assignee_agent_id.as_deref().unwrap_or("");
                self.hook.after_queue(&request.issue.id, agent_id);
            }
            QueueIssueAssignmentWakeupOutcome::Swallowed(err) => {
                self.hook.on_swallowed(&request.issue.id, err);
            }
        }

        outcome
    }
}

/// 顶层公开函数：直接调用 service（单次 queue）。
///
/// 业务层通过 `IssueAssignmentWakeupService::new()` 或 `with_hook()` 创建 service，
/// 然后调用 `service.queue(heartbeat, request)`。
pub async fn queue_issue_assignment_wakeup(
    heartbeat: &dyn IssueAssignmentWakeupDeps,
    request: QueueRequest,
) -> QueueIssueAssignmentWakeupOutcome {
    IssueAssignmentWakeupService::new()
        .queue(heartbeat, request)
        .await
}

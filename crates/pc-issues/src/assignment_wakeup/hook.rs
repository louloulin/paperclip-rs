//! Hook 抽象层 —— IssueAssignmentWakeupService 在 wakeup 前/后调用。
//!
//! 设计：
//! - 4 个回调：`BeforeQueue` / `AfterQueue` / `OnSkipped` / `OnSwallowed`
//! - 默认 `NoopIssueAssignmentWakeupHook`：空实现
//! - `RecordingIssueAssignmentWakeupHook`：记录所有事件

use std::sync::Mutex;

use pc_repos::issue_assignment_wakeup::IssueAssignmentSnapshot;

/// Issue assignment wakeup hook 事件。
#[derive(Debug, Clone)]
pub enum IssueAssignmentWakeupHookEvent {
    /// Queue 之前调用。
    BeforeQueue {
        issue_id: String,
        has_assignee: bool,
        status: String,
    },
    /// Queue 成功完成调用。
    AfterQueue { issue_id: String, agent_id: String },
    /// 因无 assignee 或 status == "backlog" 提前跳过调用。
    OnSkipped { issue_id: String, status: String },
    /// Wakeup 失败被吞咽调用（rethrow_on_error=false）。
    OnSwallowed { issue_id: String, error: String },
}

/// Issue assignment wakeup hook trait。
pub trait IssueAssignmentWakeupHook: Send + Sync {
    fn before_queue(&self, _issue: &IssueAssignmentSnapshot) {}
    fn after_queue(&self, _issue_id: &str, _agent_id: &str) {}
    fn on_skipped(&self, _issue_id: &str, _status: &str) {}
    fn on_swallowed(&self, _issue_id: &str, _error: &str) {}
}

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopIssueAssignmentWakeupHook;

impl IssueAssignmentWakeupHook for NoopIssueAssignmentWakeupHook {}

/// 记录所有 hook 事件，方便测试断言。
#[derive(Debug, Default)]
pub struct RecordingIssueAssignmentWakeupHook {
    events: Mutex<Vec<IssueAssignmentWakeupHookEvent>>,
}

impl RecordingIssueAssignmentWakeupHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<IssueAssignmentWakeupHookEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }

    pub fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.lock().unwrap().is_empty()
    }
}

impl IssueAssignmentWakeupHook for RecordingIssueAssignmentWakeupHook {
    fn before_queue(&self, issue: &IssueAssignmentSnapshot) {
        self.events.lock().unwrap().push(
            IssueAssignmentWakeupHookEvent::BeforeQueue {
                issue_id: issue.id.clone(),
                has_assignee: issue.assignee_agent_id.is_some(),
                status: issue.status.clone(),
            },
        );
    }

    fn after_queue(&self, issue_id: &str, agent_id: &str) {
        self.events.lock().unwrap().push(
            IssueAssignmentWakeupHookEvent::AfterQueue {
                issue_id: issue_id.to_string(),
                agent_id: agent_id.to_string(),
            },
        );
    }

    fn on_skipped(&self, issue_id: &str, status: &str) {
        self.events
            .lock()
            .unwrap()
            .push(IssueAssignmentWakeupHookEvent::OnSkipped {
                issue_id: issue_id.to_string(),
                status: status.to_string(),
            });
    }

    fn on_swallowed(&self, issue_id: &str, error: &str) {
        self.events
            .lock()
            .unwrap()
            .push(IssueAssignmentWakeupHookEvent::OnSwallowed {
                issue_id: issue_id.to_string(),
                error: error.to_string(),
            });
    }
}

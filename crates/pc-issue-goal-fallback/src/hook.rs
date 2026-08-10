//! Hook 抽象层 —— IssueGoalFallbackService 在解析前/后调用。
//!
//! 设计：
//! - 6 个回调：BeforeResolve / AfterResolve / OnNullSingle /
//!   BeforeResolveNext / AfterResolveNext / OnNullNext
//! - 默认 `NoopIssueGoalFallbackHook`：空实现
//! - `RecordingIssueGoalFallbackHook`：记录所有事件

use std::sync::Mutex;

use pc_repos::issue_goal_fallback::{ResolveIssueGoalIdInput, ResolveNextIssueGoalIdInput};

/// Issue goal fallback hook 事件。
#[derive(Debug, Clone)]
pub enum IssueGoalFallbackHookEvent {
    /// 单点解析前调用。
    BeforeResolve {
        has_goal_id: bool,
        has_project_id: bool,
    },
    /// 单点解析后调用，且结果非 None。
    AfterResolve { resolved: String },
    /// 单点解析后调用，结果为 None。
    OnNullSingle,
    /// 状态迁移解析前调用。
    BeforeResolveNext {
        has_explicit_goal_id: bool,
        has_project_id: bool,
    },
    /// 状态迁移解析后调用，且结果非 None。
    AfterResolveNext { resolved: String },
    /// 状态迁移解析后调用，结果为 None。
    OnNullNext,
}

/// Issue goal fallback hook trait。
pub trait IssueGoalFallbackHook: Send + Sync {
    fn before_resolve(&self, _input: &ResolveIssueGoalIdInput) {}
    fn after_resolve(&self, _resolved: &str) {}
    fn on_null_single(&self) {}

    fn before_resolve_next(&self, _input: &ResolveNextIssueGoalIdInput) {}
    fn after_resolve_next(&self, _resolved: &str) {}
    fn on_null_next(&self) {}
}

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopIssueGoalFallbackHook;

impl IssueGoalFallbackHook for NoopIssueGoalFallbackHook {}

/// 记录所有 hook 事件，方便测试断言。
#[derive(Debug, Default)]
pub struct RecordingIssueGoalFallbackHook {
    events: Mutex<Vec<IssueGoalFallbackHookEvent>>,
}

impl RecordingIssueGoalFallbackHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<IssueGoalFallbackHookEvent> {
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

impl IssueGoalFallbackHook for RecordingIssueGoalFallbackHook {
    fn before_resolve(&self, input: &ResolveIssueGoalIdInput) {
        self.events.lock().unwrap().push(IssueGoalFallbackHookEvent::BeforeResolve {
            has_goal_id: input.goal_id.is_some(),
            has_project_id: input.project_id.is_some(),
        });
    }

    fn after_resolve(&self, resolved: &str) {
        self.events.lock().unwrap().push(IssueGoalFallbackHookEvent::AfterResolve {
            resolved: resolved.to_string(),
        });
    }

    fn on_null_single(&self) {
        self.events.lock().unwrap().push(IssueGoalFallbackHookEvent::OnNullSingle);
    }

    fn before_resolve_next(&self, input: &ResolveNextIssueGoalIdInput) {
        self.events.lock().unwrap().push(IssueGoalFallbackHookEvent::BeforeResolveNext {
            has_explicit_goal_id: input.goal_id.is_some(),
            has_project_id: input.project_id.is_some(),
        });
    }

    fn after_resolve_next(&self, resolved: &str) {
        self.events.lock().unwrap().push(IssueGoalFallbackHookEvent::AfterResolveNext {
            resolved: resolved.to_string(),
        });
    }

    fn on_null_next(&self) {
        self.events.lock().unwrap().push(IssueGoalFallbackHookEvent::OnNullNext);
    }
}

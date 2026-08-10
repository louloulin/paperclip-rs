//! Hook 抽象层 —— IssueDependencyWakeupService 在关键点调用。
//!
//! 设计：
//! - 4 个回调：`BeforeFind` / `AfterFindHit` / `AfterFindMiss` / `BeforeBuildKey`
//! - 默认 `NoopIssueDependencyWakeupHook`：空实现
//! - `RecordingIssueDependencyWakeupHook`：记录所有事件

use std::sync::Mutex;

use crate::types::{BuildIdempotencyKeyInput, FindExistingWakeForAnyKeyInput, FindExistingWakeInput};
use crate::ExistingIssueBlockersResolvedWake;

/// Issue dependency wakeup hook 事件。
#[derive(Debug, Clone)]
pub enum IssueDependencyWakeupHookEvent {
    /// Build idempotency key 之前。
    BeforeBuildKey {
        dependent_issue_id: uuid::Uuid,
        blocker_issue_id: uuid::Uuid,
    },
    /// After build idempotency key。
    AfterBuildKey { key: String },
    /// Find existing wake 之前。
    BeforeFind { company_id: uuid::Uuid, key_count: usize },
    /// Find existing wake 之后且命中。
    AfterFindHit { wake: ExistingIssueBlockersResolvedWake },
    /// Find existing wake 之后且未命中。
    AfterFindMiss { key_count: usize },
}

/// Issue dependency wakeup hook trait。
pub trait IssueDependencyWakeupHook: Send + Sync {
    fn before_build_key(&self, _input: &BuildIdempotencyKeyInput) {}
    fn after_build_key(&self, _key: &str) {}
    fn before_find(&self, _key_count: usize) {}
    fn after_find_hit(&self, _wake: &ExistingIssueBlockersResolvedWake) {}
    fn after_find_miss(&self, _key_count: usize) {}
}

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopIssueDependencyWakeupHook;

impl IssueDependencyWakeupHook for NoopIssueDependencyWakeupHook {}

/// 记录所有 hook 事件，方便测试断言。
#[derive(Debug, Default)]
pub struct RecordingIssueDependencyWakeupHook {
    events: Mutex<Vec<IssueDependencyWakeupHookEvent>>,
}

impl RecordingIssueDependencyWakeupHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<IssueDependencyWakeupHookEvent> {
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

impl IssueDependencyWakeupHook for RecordingIssueDependencyWakeupHook {
    fn before_build_key(&self, input: &BuildIdempotencyKeyInput) {
        self.events.lock().unwrap().push(
            IssueDependencyWakeupHookEvent::BeforeBuildKey {
                dependent_issue_id: input.dependent_issue_id,
                blocker_issue_id: input.resolved_blocker_issue_id,
            },
        );
    }

    fn after_build_key(&self, key: &str) {
        self.events.lock().unwrap().push(
            IssueDependencyWakeupHookEvent::AfterBuildKey { key: key.to_string() },
        );
    }

    fn before_find(&self, key_count: usize) {
        self.events.lock().unwrap().push(
            IssueDependencyWakeupHookEvent::BeforeFind { company_id: uuid::Uuid::nil(), key_count },
        );
    }

    fn after_find_hit(&self, wake: &ExistingIssueBlockersResolvedWake) {
        self.events.lock().unwrap().push(
            IssueDependencyWakeupHookEvent::AfterFindHit { wake: wake.clone() },
        );
    }

    fn after_find_miss(&self, key_count: usize) {
        self.events.lock().unwrap().push(
            IssueDependencyWakeupHookEvent::AfterFindMiss { key_count },
        );
    }
}

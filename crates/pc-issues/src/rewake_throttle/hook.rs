//! Hook 抽象层 —— IssueRewakeThrottleService 在关键点调用。
//!
//! 设计：
//! - 4 个回调：`BeforeEvaluate` / `AfterAllowed` / `AfterBlocked` / `OnNotCandidate`
//! - 默认 `NoopIssueRewakeThrottleHook`：空实现
//! - `RecordingIssueRewakeThrottleHook`：记录所有事件

use std::sync::Mutex;

use super::types::{IssueRewakeCandidateInput, IssueRewakeThrottleDecision};

/// Issue rewake throttle hook 事件。
#[derive(Debug, Clone)]
pub enum IssueRewakeThrottleHookEvent {
    /// Throttle 评估之前。
    BeforeEvaluate {
        reason: Option<String>,
        has_wake_comment: bool,
        force_fresh_session: bool,
    },
    /// Wake 不在候选集（已 pass）。
    OnNotCandidate { reason: Option<String> },
    /// Throttle 评估后允许。
    AfterAllowed { no_progress_streak: usize },
    /// Throttle 评估后阻断。
    AfterBlocked {
        no_progress_streak: usize,
        cooldown_ms: u64,
    },
}

/// Issue rewake throttle hook trait。
pub trait IssueRewakeThrottleHook: Send + Sync {
    fn before_evaluate(&self, _candidate: &IssueRewakeCandidateInput) {}
    fn on_not_candidate(&self, _reason: &Option<String>) {}
    fn after_allowed(&self, _no_progress_streak: usize) {}
    fn after_blocked(&self, _no_progress_streak: usize, _cooldown_ms: u64) {}
}

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopIssueRewakeThrottleHook;

impl IssueRewakeThrottleHook for NoopIssueRewakeThrottleHook {}

/// 记录所有 hook 事件，方便测试断言。
#[derive(Debug, Default)]
pub struct RecordingIssueRewakeThrottleHook {
    events: Mutex<Vec<IssueRewakeThrottleHookEvent>>,
}

impl RecordingIssueRewakeThrottleHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<IssueRewakeThrottleHookEvent> {
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

impl IssueRewakeThrottleHook for RecordingIssueRewakeThrottleHook {
    fn before_evaluate(&self, candidate: &IssueRewakeCandidateInput) {
        self.events.lock().unwrap().push(
            IssueRewakeThrottleHookEvent::BeforeEvaluate {
                reason: candidate.reason.clone(),
                has_wake_comment: candidate.wake_comment_id.is_some(),
                force_fresh_session: candidate.force_fresh_session,
            },
        );
    }

    fn on_not_candidate(&self, reason: &Option<String>) {
        self.events.lock().unwrap().push(
            IssueRewakeThrottleHookEvent::OnNotCandidate { reason: reason.clone() },
        );
    }

    fn after_allowed(&self, no_progress_streak: usize) {
        self.events.lock().unwrap().push(
            IssueRewakeThrottleHookEvent::AfterAllowed { no_progress_streak },
        );
    }

    fn after_blocked(&self, no_progress_streak: usize, cooldown_ms: u64) {
        self.events.lock().unwrap().push(
            IssueRewakeThrottleHookEvent::AfterBlocked {
                no_progress_streak,
                cooldown_ms,
            },
        );
    }
}

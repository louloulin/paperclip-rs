//! Hook 抽象 — 让上层（HTTP / CLI / 其它 service）在 transition 前后注入副作用。
//!
//! 设计：
//! - `IssueExecutionPolicyHook`：async trait，4 个生命周期回调
//!   - `before_transition`：transition 计算之前（可拒绝 / 改 input）
//!   - `after_transition`：transition 计算之后（可记录事件 / 通知）
//!   - `before_monitor_change`：monitor patch 计算之前
//!   - `after_monitor_change`：monitor patch 计算之后
//! - 默认实现 `NoopIssueExecutionPolicyHook`：全部空操作
//! - 测试用 `RecordingIssueExecutionPolicyHook`：记录所有调用

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::types::{ApplyTransitionOutcome, ApplyTransitionRequest, MonitorPatchOutcome};

// -----------------------------------------------------------------------------
// Hook event
// -----------------------------------------------------------------------------

/// Hook 调用事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueExecutionPolicyHookEvent {
    BeforeTransition {
        issue_id: uuid::Uuid,
    },
    AfterTransition {
        issue_id: uuid::Uuid,
        has_decision: bool,
        patch_size: usize,
    },
    BeforeMonitorChange {
        issue_id: uuid::Uuid,
        kind: &'static str, // "initial" | "trigger" | "clear"
    },
    AfterMonitorChange {
        issue_id: uuid::Uuid,
        kind: &'static str,
        patch_size: usize,
    },
}

// -----------------------------------------------------------------------------
// Hook trait
// -----------------------------------------------------------------------------

#[async_trait]
pub trait IssueExecutionPolicyHook: Send + Sync {
    /// 默认 noop；可在 hook 内改 input / 拒绝（返回 Err）
    async fn before_transition(&self, _request: &ApplyTransitionRequest) -> Result<(), String> {
        Ok(())
    }
    async fn after_transition(
        &self,
        _request: &ApplyTransitionRequest,
        _outcome: &ApplyTransitionOutcome,
    ) {
    }
    async fn before_monitor_change(
        &self,
        _kind: &'static str,
        _issue_id: uuid::Uuid,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn after_monitor_change(
        &self,
        _kind: &'static str,
        _issue_id: uuid::Uuid,
        _outcome: &MonitorPatchOutcome,
    ) {
    }
}

// -----------------------------------------------------------------------------
// Noop / Recording implementations
// -----------------------------------------------------------------------------

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopIssueExecutionPolicyHook;

#[async_trait]
impl IssueExecutionPolicyHook for NoopIssueExecutionPolicyHook {}

/// 记录所有 hook 调用（用于测试 / debug）。
#[derive(Debug, Default, Clone)]
pub struct RecordingIssueExecutionPolicyHook {
    pub events: Arc<Mutex<Vec<IssueExecutionPolicyHookEvent>>>,
}

impl RecordingIssueExecutionPolicyHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<IssueExecutionPolicyHookEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }
}

#[async_trait]
impl IssueExecutionPolicyHook for RecordingIssueExecutionPolicyHook {
    async fn before_transition(&self, request: &ApplyTransitionRequest) -> Result<(), String> {
        self.events
            .lock()
            .unwrap()
            .push(IssueExecutionPolicyHookEvent::BeforeTransition {
                issue_id: request.issue.id,
            });
        Ok(())
    }
    async fn after_transition(
        &self,
        request: &ApplyTransitionRequest,
        outcome: &ApplyTransitionOutcome,
    ) {
        self.events
            .lock()
            .unwrap()
            .push(IssueExecutionPolicyHookEvent::AfterTransition {
                issue_id: request.issue.id,
                has_decision: outcome.decision.is_some(),
                patch_size: outcome.patch.len(),
            });
    }
    async fn before_monitor_change(
        &self,
        kind: &'static str,
        issue_id: uuid::Uuid,
    ) -> Result<(), String> {
        self.events
            .lock()
            .unwrap()
            .push(IssueExecutionPolicyHookEvent::BeforeMonitorChange { issue_id, kind });
        Ok(())
    }
    async fn after_monitor_change(
        &self,
        kind: &'static str,
        issue_id: uuid::Uuid,
        outcome: &MonitorPatchOutcome,
    ) {
        self.events
            .lock()
            .unwrap()
            .push(IssueExecutionPolicyHookEvent::AfterMonitorChange {
                issue_id,
                kind,
                patch_size: outcome.patch.len(),
            });
    }
}

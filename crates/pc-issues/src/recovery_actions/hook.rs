//! Hook 抽象 — 让上层在 recovery action upsert / resolve 时注入副作用。
//!
//! 设计：
//! - `IssueRecoveryActionHook` trait：async，4 个生命周期回调
//!   - `before_upsert`：upsert 前（可拒绝 / 改 input）
//!   - `after_upsert`：upsert 后（可记录事件 / 通知 owner）
//!   - `before_resolve`：resolve 前
//!   - `after_resolve`：resolve 后
//! - 默认 `NoopIssueRecoveryActionHook`：全部空操作
//! - 测试用 `RecordingIssueRecoveryActionHook`：记录所有调用

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::types::{
    IssueRecoveryActionInfo, ResolveIssueRecoveryActionRequest, UpsertIssueRecoveryActionRequest,
};

/// Hook 调用事件。
#[derive(Debug, Clone)]
pub enum IssueRecoveryActionHookEvent {
    BeforeUpsert {
        source_issue_id: uuid::Uuid,
        fingerprint: String,
    },
    AfterUpsert {
        action_id: uuid::Uuid,
        source_issue_id: uuid::Uuid,
        is_new: bool,
    },
    BeforeResolve {
        source_issue_id: uuid::Uuid,
        action_id: Option<uuid::Uuid>,
    },
    AfterResolve {
        action_id: uuid::Uuid,
        source_issue_id: uuid::Uuid,
        status: String,
    },
}

#[async_trait]
pub trait IssueRecoveryActionHook: Send + Sync {
    async fn before_upsert(
        &self,
        _request: &UpsertIssueRecoveryActionRequest,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn after_upsert(&self, _action: &IssueRecoveryActionInfo, _is_new: bool) {}
    async fn before_resolve(
        &self,
        _request: &ResolveIssueRecoveryActionRequest,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn after_resolve(&self, _action: &IssueRecoveryActionInfo) {}
}

/// Noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopIssueRecoveryActionHook;

#[async_trait]
impl IssueRecoveryActionHook for NoopIssueRecoveryActionHook {}

/// 记录所有 hook 调用。
#[derive(Debug, Default, Clone)]
pub struct RecordingIssueRecoveryActionHook {
    pub events: Arc<Mutex<Vec<IssueRecoveryActionHookEvent>>>,
}

impl RecordingIssueRecoveryActionHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<IssueRecoveryActionHookEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }
}

#[async_trait]
impl IssueRecoveryActionHook for RecordingIssueRecoveryActionHook {
    async fn before_upsert(
        &self,
        request: &UpsertIssueRecoveryActionRequest,
    ) -> Result<(), String> {
        self.events
            .lock()
            .unwrap()
            .push(IssueRecoveryActionHookEvent::BeforeUpsert {
                source_issue_id: request.source_issue_id,
                fingerprint: request.fingerprint.clone(),
            });
        Ok(())
    }
    async fn after_upsert(&self, action: &IssueRecoveryActionInfo, is_new: bool) {
        self.events
            .lock()
            .unwrap()
            .push(IssueRecoveryActionHookEvent::AfterUpsert {
                action_id: action.id,
                source_issue_id: action.source_issue_id,
                is_new,
            });
    }
    async fn before_resolve(
        &self,
        request: &ResolveIssueRecoveryActionRequest,
    ) -> Result<(), String> {
        self.events
            .lock()
            .unwrap()
            .push(IssueRecoveryActionHookEvent::BeforeResolve {
                source_issue_id: request.source_issue_id,
                action_id: request.action_id,
            });
        Ok(())
    }
    async fn after_resolve(&self, action: &IssueRecoveryActionInfo) {
        self.events
            .lock()
            .unwrap()
            .push(IssueRecoveryActionHookEvent::AfterResolve {
                action_id: action.id,
                source_issue_id: action.source_issue_id,
                status: action.status.clone(),
            });
    }
}

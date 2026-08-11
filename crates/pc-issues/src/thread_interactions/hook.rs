//! Hook 抽象层 —— IssueThreadInteractionService 在关键点调用。
//!
//! 设计：
//! - 5 个回调：`BeforeCreate` / `AfterCreate` / `BeforeResolve` / `AfterResolve` / `OnConflict`
//! - 默认 `NoopIssueThreadInteractionHook`：空实现
//! - `RecordingIssueThreadInteractionHook`：记录所有事件

use std::sync::Mutex;

use serde_json::Value;
use uuid::Uuid;

use super::types::{
    CreateIssueThreadInteractionInput, InteractionActor, InteractionResolution,
    ResolveInteractionInput,
};

/// Issue thread interaction hook 事件。
#[derive(Debug, Clone)]
pub enum IssueThreadInteractionHookEvent {
    /// Create 之前调用。
    BeforeCreate {
        issue_id: Uuid,
        kind: String,
        idempotency_key: Option<String>,
    },
    /// Create 之后调用。
    AfterCreate {
        interaction_id: Uuid,
        kind: String,
    },
    /// Resolve 之前调用。
    BeforeResolve {
        interaction_id: Uuid,
        new_status: String,
        actor: InteractionActor,
    },
    /// Resolve 之后调用。
    AfterResolve { resolution: InteractionResolution },
    /// Conflict / 重复 idempotency_key 时调用。
    OnConflict {
        issue_id: Uuid,
        kind: String,
        idempotency_key: String,
    },
}

/// Issue thread interaction hook trait。
pub trait IssueThreadInteractionHook: Send + Sync {
    fn before_create(&self, _input: &CreateIssueThreadInteractionInput) {}
    fn after_create(&self, _interaction_id: Uuid, _kind: &str) {}
    fn before_resolve(&self, _input: &ResolveInteractionInput) {}
    fn after_resolve(&self, _resolution: &InteractionResolution) {}
    fn on_conflict(&self, _issue_id: Uuid, _kind: &str, _idempotency_key: &str) {}
}

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopIssueThreadInteractionHook;

impl IssueThreadInteractionHook for NoopIssueThreadInteractionHook {}

/// 记录所有 hook 事件，方便测试断言。
#[derive(Debug, Default)]
pub struct RecordingIssueThreadInteractionHook {
    events: Mutex<Vec<IssueThreadInteractionHookEvent>>,
}

impl RecordingIssueThreadInteractionHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<IssueThreadInteractionHookEvent> {
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

impl IssueThreadInteractionHook for RecordingIssueThreadInteractionHook {
    fn before_create(&self, input: &CreateIssueThreadInteractionInput) {
        self.events.lock().unwrap().push(
            IssueThreadInteractionHookEvent::BeforeCreate {
                issue_id: input.issue_id,
                kind: input.kind.clone(),
                idempotency_key: input.idempotency_key.clone(),
            },
        );
    }

    fn after_create(&self, interaction_id: Uuid, kind: &str) {
        self.events.lock().unwrap().push(
            IssueThreadInteractionHookEvent::AfterCreate {
                interaction_id,
                kind: kind.to_string(),
            },
        );
    }

    fn before_resolve(&self, input: &ResolveInteractionInput) {
        self.events.lock().unwrap().push(
            IssueThreadInteractionHookEvent::BeforeResolve {
                interaction_id: input.interaction_id,
                new_status: input.new_status.as_str().to_string(),
                actor: input.resolved_by_actor.clone(),
            },
        );
    }

    fn after_resolve(&self, resolution: &InteractionResolution) {
        self.events.lock().unwrap().push(
            IssueThreadInteractionHookEvent::AfterResolve {
                resolution: resolution.clone(),
            },
        );
    }

    fn on_conflict(&self, issue_id: Uuid, kind: &str, idempotency_key: &str) {
        self.events.lock().unwrap().push(
            IssueThreadInteractionHookEvent::OnConflict {
                issue_id,
                kind: kind.to_string(),
                idempotency_key: idempotency_key.to_string(),
            },
        );
    }
}

// Suppress unused
#[allow(dead_code)]
fn _unused(_v: Value) {}

//! Hook 抽象层 —— InboxAgentPolicyService 在 `get` / `update` 关键点调用。
//!
//! 设计：
//! - 3 个回调：`BeforeUpdate` / `AfterUpdate` / `AfterGet`
//! - 默认 `NoopInboxAgentPolicyHook`：空实现，方便 caller 不传 hook 时使用
//! - `RecordingInboxAgentPolicyHook`：记录所有事件，方便测试断言

use std::sync::Mutex;

use uuid::Uuid;

use pc_repos::inbox_agent_policy::{InboxAgentPolicy, InboxAgentPolicyMode};

/// Inbox agent policy hook 事件。
///
/// 注意：`InboxAgentPolicy` 不实现 `PartialEq`，所以本枚举整体也不实现 `PartialEq`。
/// 测试断言时按 `format!("{:?}", event)` 比较；或用 [`crate::RecordingInboxAgentPolicyHook::events`]
/// 配合 [`event_summary`](InboxAgentPolicyHookEvent::event_summary) 取可比较 summary。
#[derive(Debug, Clone)]
pub enum InboxAgentPolicyHookEvent {
    /// Update 前调用。caller 可读取/校验 input（一般只读）。
    BeforeUpdate {
        company_id: Uuid,
        user_id: String,
        mode: InboxAgentPolicyMode,
        allowed_agent_ids: Vec<Uuid>,
    },
    /// Update 成功后调用，附带返回的 policy 视图。
    AfterUpdate { policy: Box<InboxAgentPolicy> },
    /// Get 后调用，附带返回的 policy 视图（无论是否 materialized）。
    AfterGet { policy: Box<InboxAgentPolicy> },
}

impl InboxAgentPolicyHookEvent {
    /// 事件变体名（用于测试断言）。
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::BeforeUpdate { .. } => "BeforeUpdate",
            Self::AfterUpdate { .. } => "AfterUpdate",
            Self::AfterGet { .. } => "AfterGet",
        }
    }
}

/// Inbox agent policy hook trait。
///
/// 所有方法默认 noop 实现，便于 caller 选择性 override。
pub trait InboxAgentPolicyHook: Send + Sync {
    fn before_update(
        &self,
        _company_id: Uuid,
        _user_id: &str,
        _mode: InboxAgentPolicyMode,
        _allowed_agent_ids: &[Uuid],
    ) {
    }
    fn after_update(&self, _policy: &InboxAgentPolicy) {}
    fn after_get(&self, _policy: &InboxAgentPolicy) {}
}

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopInboxAgentPolicyHook;

impl InboxAgentPolicyHook for NoopInboxAgentPolicyHook {}

/// 记录所有 hook 事件，方便测试断言。
#[derive(Debug, Default)]
pub struct RecordingInboxAgentPolicyHook {
    events: Mutex<Vec<InboxAgentPolicyHookEvent>>,
}

impl RecordingInboxAgentPolicyHook {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取所有事件。
    pub fn events(&self) -> Vec<InboxAgentPolicyHookEvent> {
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

    pub fn before_update_count(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, InboxAgentPolicyHookEvent::BeforeUpdate { .. }))
            .count()
    }

    pub fn after_update_count(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, InboxAgentPolicyHookEvent::AfterUpdate { .. }))
            .count()
    }

    pub fn after_get_count(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, InboxAgentPolicyHookEvent::AfterGet { .. }))
            .count()
    }
}

impl InboxAgentPolicyHook for RecordingInboxAgentPolicyHook {
    fn before_update(
        &self,
        company_id: Uuid,
        user_id: &str,
        mode: InboxAgentPolicyMode,
        allowed_agent_ids: &[Uuid],
    ) {
        self.events.lock().unwrap().push(InboxAgentPolicyHookEvent::BeforeUpdate {
            company_id,
            user_id: user_id.to_string(),
            mode,
            allowed_agent_ids: allowed_agent_ids.to_vec(),
        });
    }

    fn after_update(&self, policy: &InboxAgentPolicy) {
        self.events.lock().unwrap().push(InboxAgentPolicyHookEvent::AfterUpdate {
            policy: Box::new(policy.clone()),
        });
    }

    fn after_get(&self, policy: &InboxAgentPolicy) {
        self.events.lock().unwrap().push(InboxAgentPolicyHookEvent::AfterGet {
            policy: Box::new(policy.clone()),
        });
    }
}

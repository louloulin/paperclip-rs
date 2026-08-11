//! Hook 抽象层 —— AgentActionAuditService 在 list 前后调用。
//!
//! 设计：
//! - 2 个回调：`BeforeList` / `AfterList`
//! - 默认 `NoopAgentActionAuditHook`：空实现
//! - `RecordingAgentActionAuditHook`：记录所有事件，方便测试断言

use std::sync::Mutex;

use pc_repos::agent_action_audit::AgentActionAuditFilters;

use super::AgentActionAuditPage;

/// Agent action audit hook 事件。
#[derive(Debug, Clone)]
pub enum AgentActionAuditHookEvent {
    /// List 前调用。
    BeforeList {
        filters: Box<AgentActionAuditFilters>,
    },
    /// List 成功后调用，附带返回的 page。
    AfterList {
        page: Box<AgentActionAuditPage>,
        /// 该次查询的过滤条件（caller 无法反查）
        filters: Box<AgentActionAuditFilters>,
    },
}

impl AgentActionAuditHookEvent {
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::BeforeList { .. } => "BeforeList",
            Self::AfterList { .. } => "AfterList",
        }
    }
}

/// Agent action audit hook trait。
///
/// 所有方法默认 noop 实现，便于 caller 选择性 override。
pub trait AgentActionAuditHook: Send + Sync {
    fn before_list(&self, _filters: &AgentActionAuditFilters) {}
    fn after_list(&self, _page: &AgentActionAuditPage, _filters: &AgentActionAuditFilters) {}
}

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopAgentActionAuditHook;

impl AgentActionAuditHook for NoopAgentActionAuditHook {}

/// 记录所有 hook 事件，方便测试断言。
#[derive(Debug, Default)]
pub struct RecordingAgentActionAuditHook {
    events: Mutex<Vec<AgentActionAuditHookEvent>>,
}

impl RecordingAgentActionAuditHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<AgentActionAuditHookEvent> {
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

    pub fn before_count(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, AgentActionAuditHookEvent::BeforeList { .. }))
            .count()
    }

    pub fn after_count(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, AgentActionAuditHookEvent::AfterList { .. }))
            .count()
    }
}

impl AgentActionAuditHook for RecordingAgentActionAuditHook {
    fn before_list(&self, filters: &AgentActionAuditFilters) {
        self.events
            .lock()
            .unwrap()
            .push(AgentActionAuditHookEvent::BeforeList {
                filters: Box::new(filters.clone()),
            });
    }

    fn after_list(&self, page: &AgentActionAuditPage, filters: &AgentActionAuditFilters) {
        self.events
            .lock()
            .unwrap()
            .push(AgentActionAuditHookEvent::AfterList {
                page: Box::new(page.clone()),
                filters: Box::new(filters.clone()),
            });
    }
}

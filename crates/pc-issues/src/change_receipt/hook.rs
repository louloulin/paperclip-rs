//! Hook 抽象层 —— IssueChangeReceiptService 在 diff 前/后调用。
//!
//! 设计：
//! - 3 个回调：`BeforeDiff` / `AfterDiff` / `OnNoChanges`
//! - 默认 `NoopIssueChangeReceiptHook`：空实现，方便 caller 不传 hook 时使用
//! - `RecordingIssueChangeReceiptHook`：记录所有事件，方便测试

use std::sync::Mutex;

use serde_json::Map;

use crate::IssueChanges;

/// Issue change receipt hook 事件。
#[derive(Debug, Clone)]
pub enum IssueChangeReceiptHookEvent {
    /// Diff 前调用。caller 可改 existing/updated（一般只读）。
    BeforeDiff {
        existing_keys: Vec<String>,
        updated_keys: Vec<String>,
    },
    /// Diff 后调用，且有 changes 时。
    AfterDiff { changes: IssueChanges },
    /// Diff 后调用，但无 changes 时。
    OnNoChanges,
}

/// Issue change receipt hook trait。
///
/// 所有方法默认 noop 实现，便于 caller 选择性 override。
pub trait IssueChangeReceiptHook: Send + Sync {
    fn before_diff(&self, _existing: &Map<String, serde_json::Value>, _updated: &Map<String, serde_json::Value>) {}
    fn after_diff(&self, _changes: &IssueChanges) {}
    fn on_no_changes(&self) {}
}

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopIssueChangeReceiptHook;

impl IssueChangeReceiptHook for NoopIssueChangeReceiptHook {}

/// 记录所有 hook 事件，方便测试断言。
#[derive(Debug, Default)]
pub struct RecordingIssueChangeReceiptHook {
    events: Mutex<Vec<IssueChangeReceiptHookEvent>>,
}

impl RecordingIssueChangeReceiptHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<IssueChangeReceiptHookEvent> {
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

impl IssueChangeReceiptHook for RecordingIssueChangeReceiptHook {
    fn before_diff(&self, existing: &Map<String, serde_json::Value>, updated: &Map<String, serde_json::Value>) {
        let mut events = self.events.lock().unwrap();
        events.push(IssueChangeReceiptHookEvent::BeforeDiff {
            existing_keys: existing.keys().cloned().collect(),
            updated_keys: updated.keys().cloned().collect(),
        });
    }

    fn after_diff(&self, changes: &IssueChanges) {
        self.events.lock().unwrap().push(IssueChangeReceiptHookEvent::AfterDiff {
            changes: changes.clone(),
        });
    }

    fn on_no_changes(&self) {
        self.events.lock().unwrap().push(IssueChangeReceiptHookEvent::OnNoChanges);
    }
}

//! Hook 抽象层 —— InboxDismissalService 在关键点调用。
//!
//! 设计：
//! - 6 个回调：`BeforeDismiss` / `AfterDismiss` / `BeforeSnooze` / `AfterSnooze` / `BeforeRestore` / `AfterRestore`
//! - 默认 `NoopInboxDismissalHook`：空实现
//! - `RecordingInboxDismissalHook`：记录所有事件，方便测试断言

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use pc_repos::inbox::{DismissKind, InboxDismissalRow};
use uuid::Uuid;

/// Inbox dismissal hook 事件。
#[derive(Debug, Clone)]
pub enum InboxDismissalHookEvent {
    /// Dismiss 前调用。
    BeforeDismiss {
        company_id: Uuid,
        user_id: String,
        item_key: String,
    },
    /// Dismiss 成功后调用，附带返回的行。
    AfterDismiss { row: Box<InboxDismissalRow> },
    /// Snooze 前调用。
    BeforeSnooze {
        company_id: Uuid,
        user_id: String,
        item_key: String,
        snoozed_until: DateTime<Utc>,
    },
    /// Snooze 成功后调用，附带返回的行。
    AfterSnooze { row: Box<InboxDismissalRow> },
    /// Restore 前调用。
    BeforeRestore {
        company_id: Uuid,
        user_id: String,
        item_key: String,
    },
    /// Restore 成功后调用，附带返回是否真删除了行。
    AfterRestore {
        company_id: Uuid,
        user_id: String,
        item_key: String,
        removed: bool,
    },
}

impl InboxDismissalHookEvent {
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::BeforeDismiss { .. } => "BeforeDismiss",
            Self::AfterDismiss { .. } => "AfterDismiss",
            Self::BeforeSnooze { .. } => "BeforeSnooze",
            Self::AfterSnooze { .. } => "AfterSnooze",
            Self::BeforeRestore { .. } => "BeforeRestore",
            Self::AfterRestore { .. } => "AfterRestore",
        }
    }
}

/// Inbox dismissal hook trait。
///
/// 所有方法默认 noop 实现，便于 caller 选择性 override。
pub trait InboxDismissalHook: Send + Sync {
    fn before_dismiss(&self, _company_id: Uuid, _user_id: &str, _item_key: &str) {}
    fn after_dismiss(&self, _row: &InboxDismissalRow) {}
    fn before_snooze(
        &self,
        _company_id: Uuid,
        _user_id: &str,
        _item_key: &str,
        _snoozed_until: DateTime<Utc>,
    ) {
    }
    fn after_snooze(&self, _row: &InboxDismissalRow) {}
    fn before_restore(&self, _company_id: Uuid, _user_id: &str, _item_key: &str) {}
    fn after_restore(&self, _company_id: Uuid, _user_id: &str, _item_key: &str, _removed: bool) {}
}

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopInboxDismissalHook;

impl InboxDismissalHook for NoopInboxDismissalHook {}

/// 记录所有 hook 事件，方便测试断言。
#[derive(Debug, Default)]
pub struct RecordingInboxDismissalHook {
    events: Mutex<Vec<InboxDismissalHookEvent>>,
}

impl RecordingInboxDismissalHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<InboxDismissalHookEvent> {
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

    pub fn count_for(&self, kind: DismissKind) -> (usize, usize) {
        let events = self.events.lock().unwrap();
        let (mut before, mut after) = (0, 0);
        let variant_before = match kind {
            DismissKind::Dismiss => "BeforeDismiss",
            DismissKind::Snooze => "BeforeSnooze",
        };
        let variant_after = match kind {
            DismissKind::Dismiss => "AfterDismiss",
            DismissKind::Snooze => "AfterSnooze",
        };
        for e in events.iter() {
            match e {
                InboxDismissalHookEvent::BeforeRestore { .. } => {}
                InboxDismissalHookEvent::AfterRestore { .. } => {}
                _ => {}
            }
            if e.variant_name() == variant_before {
                before += 1;
            } else if e.variant_name() == variant_after {
                after += 1;
            }
        }
        (before, after)
    }

    pub fn restore_count(&self) -> (usize, usize) {
        let events = self.events.lock().unwrap();
        let mut before = 0;
        let mut after = 0;
        for e in events.iter() {
            match e {
                InboxDismissalHookEvent::BeforeRestore { .. } => before += 1,
                InboxDismissalHookEvent::AfterRestore { .. } => after += 1,
                _ => {}
            }
        }
        (before, after)
    }
}

impl InboxDismissalHook for RecordingInboxDismissalHook {
    fn before_dismiss(&self, company_id: Uuid, user_id: &str, item_key: &str) {
        self.events.lock().unwrap().push(InboxDismissalHookEvent::BeforeDismiss {
            company_id,
            user_id: user_id.to_string(),
            item_key: item_key.to_string(),
        });
    }

    fn after_dismiss(&self, row: &InboxDismissalRow) {
        self.events.lock().unwrap().push(InboxDismissalHookEvent::AfterDismiss {
            row: Box::new(row.clone()),
        });
    }

    fn before_snooze(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
        snoozed_until: DateTime<Utc>,
    ) {
        self.events.lock().unwrap().push(InboxDismissalHookEvent::BeforeSnooze {
            company_id,
            user_id: user_id.to_string(),
            item_key: item_key.to_string(),
            snoozed_until,
        });
    }

    fn after_snooze(&self, row: &InboxDismissalRow) {
        self.events.lock().unwrap().push(InboxDismissalHookEvent::AfterSnooze {
            row: Box::new(row.clone()),
        });
    }

    fn before_restore(&self, company_id: Uuid, user_id: &str, item_key: &str) {
        self.events.lock().unwrap().push(InboxDismissalHookEvent::BeforeRestore {
            company_id,
            user_id: user_id.to_string(),
            item_key: item_key.to_string(),
        });
    }

    fn after_restore(&self, company_id: Uuid, user_id: &str, item_key: &str, removed: bool) {
        self.events.lock().unwrap().push(InboxDismissalHookEvent::AfterRestore {
            company_id,
            user_id: user_id.to_string(),
            item_key: item_key.to_string(),
            removed,
        });
    }
}

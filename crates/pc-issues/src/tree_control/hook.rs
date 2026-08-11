//! Hooks for issue tree control lifecycle events.
//!
//! 与 Node `issueTreeControlService` 的副作用层（创建时 dispatch agent wakeup,
//! release 时 dispatch resume 等）对齐。Rust 侧用 async trait 抽象，
//! 让上游 HTTP / 调度器在 hook 中实现具体副作用。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_core::Timestamp;
use pc_errors::Result as PcResult;

/// Tree control 生命周期事件 — 与 Node `issueTreeControlSvc.emit` 对齐。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum IssueTreeControlHookEvent {
    /// 预览阶段（不修改状态，只通知 UI）。
    Previewed {
        company_id: Uuid,
        root_issue_id: Uuid,
        hold_id: Option<Uuid>,
        mode: String,
        member_count: i64,
    },
    /// apply 阶段创建了 hold + 写入了 members。
    Applied {
        company_id: Uuid,
        root_issue_id: Uuid,
        hold_id: Uuid,
        mode: String,
        member_count: i64,
    },
    /// 释放阶段更新了 hold 元数据。
    Released {
        company_id: Uuid,
        root_issue_id: Uuid,
        hold_id: Uuid,
        mode: String,
        released_at: Timestamp,
    },
}

#[async_trait]
pub trait IssueTreeControlHook: Send + Sync {
    async fn on_issue_tree_control_event(&self, _event: IssueTreeControlHookEvent) -> PcResult<()> {
        Ok(())
    }
}

/// 默认空实现。
pub struct NoopIssueTreeControlHook;
#[async_trait]
impl IssueTreeControlHook for NoopIssueTreeControlHook {}

/// 测试 / 调试用：记录所有触发的 hook 事件。
#[derive(Default)]
pub struct RecordingIssueTreeControlHook {
    pub events: std::sync::Mutex<Vec<IssueTreeControlHookEvent>>,
}

impl RecordingIssueTreeControlHook {
    pub fn events_snapshot(&self) -> Vec<IssueTreeControlHookEvent> {
        self.events.lock().expect("mutex").clone()
    }
    pub fn clear(&self) {
        self.events.lock().expect("mutex").clear()
    }
    pub fn len(&self) -> usize {
        self.events.lock().expect("mutex").len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl IssueTreeControlHook for RecordingIssueTreeControlHook {
    async fn on_issue_tree_control_event(&self, e: IssueTreeControlHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

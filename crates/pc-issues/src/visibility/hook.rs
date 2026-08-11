//! Hook 抽象 — 让上层在 visibility check 时注入副作用。
//!
//! 设计：
//! - `IssueVisibilityHook`：async trait，2 个生命周期回调
//!   - `before_classify`：classification 前（可拒绝 / 改 row）
//!   - `after_classify`：classification 后（可记录事件 / 通知）
//! - 默认 `NoopIssueVisibilityHook`
//! - 测试用 `RecordingIssueVisibilityHook`

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use pc_repos::issue::IssueRow;

use super::types::{IssueVisibilityClassification, VisibilityFilterConfig};

/// Hook 调用事件。
#[derive(Debug, Clone)]
pub enum IssueVisibilityHookEvent {
    BeforeClassify {
        issue_id: uuid::Uuid,
    },
    AfterClassify {
        issue_id: uuid::Uuid,
        is_visible: bool,
        reason: String,
    },
    BeforeFilter {
        filter_config: String,
    },
    AfterFilter {
        filter_config: String,
        accepted: usize,
        rejected: usize,
    },
}

#[async_trait]
pub trait IssueVisibilityHook: Send + Sync {
    async fn before_classify(
        &self,
        _row: &IssueRow,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn after_classify(
        &self,
        _row: &IssueRow,
        _classification: &IssueVisibilityClassification,
    ) {
    }
    async fn before_filter(
        &self,
        _config: &VisibilityFilterConfig,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn after_filter(
        &self,
        _config: &VisibilityFilterConfig,
        _accepted: usize,
        _rejected: usize,
    ) {
    }
}

/// Noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopIssueVisibilityHook;

#[async_trait]
impl IssueVisibilityHook for NoopIssueVisibilityHook {}

/// 记录所有 hook 调用。
#[derive(Debug, Default, Clone)]
pub struct RecordingIssueVisibilityHook {
    pub events: Arc<Mutex<Vec<IssueVisibilityHookEvent>>>,
}

impl RecordingIssueVisibilityHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<IssueVisibilityHookEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }
}

#[async_trait]
impl IssueVisibilityHook for RecordingIssueVisibilityHook {
    async fn before_classify(&self, row: &IssueRow) -> Result<(), String> {
        self.events
            .lock()
            .unwrap()
            .push(IssueVisibilityHookEvent::BeforeClassify {
                issue_id: row.id,
            });
        Ok(())
    }
    async fn after_classify(
        &self,
        row: &IssueRow,
        classification: &IssueVisibilityClassification,
    ) {
        self.events
            .lock()
            .unwrap()
            .push(IssueVisibilityHookEvent::AfterClassify {
                issue_id: row.id,
                is_visible: classification.is_visible,
                reason: classification.reason.as_str().to_string(),
            });
    }
    async fn before_filter(&self, config: &VisibilityFilterConfig) -> Result<(), String> {
        self.events
            .lock()
            .unwrap()
            .push(IssueVisibilityHookEvent::BeforeFilter {
                filter_config: format!("{:?}", config),
            });
        Ok(())
    }
    async fn after_filter(
        &self,
        config: &VisibilityFilterConfig,
        accepted: usize,
        rejected: usize,
    ) {
        self.events
            .lock()
            .unwrap()
            .push(IssueVisibilityHookEvent::AfterFilter {
                filter_config: format!("{:?}", config),
                accepted,
                rejected,
            });
    }
}

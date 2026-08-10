//! Hook 抽象层 —— IssueContinuationSummaryService 在关键点调用。

use std::sync::Mutex;

use uuid::Uuid;

use crate::types::{IssueContinuationSummaryDocument, RefreshContinuationSummaryInput};

/// Issue continuation summary hook 事件。
#[derive(Debug, Clone)]
pub enum IssueContinuationSummaryHookEvent {
    /// Build markdown 之前调用。
    BeforeBuild { issue_id: String, run_id: String },
    /// Build markdown 之后调用。
    AfterBuild { body_len: usize },
    /// Refresh 之前调用。
    BeforeRefresh { issue_id: Uuid, run_id: String },
    /// Refresh 之后调用（成功 upsert）。
    AfterRefresh { document: IssueContinuationSummaryDocument },
}

/// Issue continuation summary hook trait。
pub trait IssueContinuationSummaryHook: Send + Sync {
    fn before_build(&self, _issue_id: &str, _run_id: &str) {}
    fn after_build(&self, _body_len: usize) {}
    fn before_refresh(&self, _input: &RefreshContinuationSummaryInput) {}
    fn after_refresh(&self, _doc: &IssueContinuationSummaryDocument) {}
}

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopIssueContinuationSummaryHook;

impl IssueContinuationSummaryHook for NoopIssueContinuationSummaryHook {}

/// 记录所有 hook 事件。
#[derive(Debug, Default)]
pub struct RecordingIssueContinuationSummaryHook {
    events: Mutex<Vec<IssueContinuationSummaryHookEvent>>,
}

impl RecordingIssueContinuationSummaryHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<IssueContinuationSummaryHookEvent> {
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

impl IssueContinuationSummaryHook for RecordingIssueContinuationSummaryHook {
    fn before_build(&self, issue_id: &str, run_id: &str) {
        self.events.lock().unwrap().push(
            IssueContinuationSummaryHookEvent::BeforeBuild {
                issue_id: issue_id.to_string(),
                run_id: run_id.to_string(),
            },
        );
    }

    fn after_build(&self, body_len: usize) {
        self.events
            .lock()
            .unwrap()
            .push(IssueContinuationSummaryHookEvent::AfterBuild { body_len });
    }

    fn before_refresh(&self, input: &RefreshContinuationSummaryInput) {
        self.events.lock().unwrap().push(
            IssueContinuationSummaryHookEvent::BeforeRefresh {
                issue_id: input.issue_id,
                run_id: input.run.id.clone(),
            },
        );
    }

    fn after_refresh(&self, doc: &IssueContinuationSummaryDocument) {
        self.events.lock().unwrap().push(
            IssueContinuationSummaryHookEvent::AfterRefresh {
                document: doc.clone(),
            },
        );
    }
}

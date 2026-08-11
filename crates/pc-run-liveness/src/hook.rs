//! Hook 抽象层 —— RunLivenessService 在分类前后调用。

use std::sync::Mutex;

use crate::types::{RunLivenessClassification, RunLivenessClassificationInput};

/// Run liveness hook 事件。
#[derive(Debug, Clone)]
pub enum RunLivenessHookEvent {
    /// 分类之前调用。
    BeforeClassify { run_status: String, has_issue: bool },
    /// 分类之后调用。
    AfterClassify {
        classification: RunLivenessClassification,
    },
}

/// Run liveness hook trait。
pub trait RunLivenessHook: Send + Sync {
    fn before_classify(&self, _input: &RunLivenessClassificationInput) {}
    fn after_classify(&self, _classification: &RunLivenessClassification) {}
}

/// 默认 noop hook。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRunLivenessHook;

impl RunLivenessHook for NoopRunLivenessHook {}

/// 记录所有 hook 事件。
#[derive(Debug, Default)]
pub struct RecordingRunLivenessHook {
    events: Mutex<Vec<RunLivenessHookEvent>>,
}

impl RecordingRunLivenessHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<RunLivenessHookEvent> {
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

impl RunLivenessHook for RecordingRunLivenessHook {
    fn before_classify(&self, input: &RunLivenessClassificationInput) {
        self.events
            .lock()
            .unwrap()
            .push(RunLivenessHookEvent::BeforeClassify {
                run_status: input.run_status.clone(),
                has_issue: input.issue.is_some(),
            });
    }

    fn after_classify(&self, classification: &RunLivenessClassification) {
        self.events
            .lock()
            .unwrap()
            .push(RunLivenessHookEvent::AfterClassify {
                classification: classification.clone(),
            });
    }
}

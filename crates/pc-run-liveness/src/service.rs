//! Service 实现 —— RunLivenessService。
//!
//! 纯函数 service（无 DB I/O），封装 classifier + Hook。

use std::sync::Arc;

use crate::classifier::classify_run_liveness;
use crate::hook::{NoopRunLivenessHook, RunLivenessHook};
use crate::types::{RunLivenessClassification, RunLivenessClassificationInput};

/// Run liveness service —— 封装 + Hook。
pub struct RunLivenessService {
    hook: Arc<dyn RunLivenessHook>,
}

impl std::fmt::Debug for RunLivenessService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunLivenessService").finish()
    }
}

impl Default for RunLivenessService {
    fn default() -> Self {
        Self::new()
    }
}

impl RunLivenessService {
    pub fn new() -> Self {
        Self {
            hook: Arc::new(NoopRunLivenessHook),
        }
    }

    pub fn with_hook(hook: Arc<dyn RunLivenessHook>) -> Self {
        Self { hook }
    }

    pub fn hook(&self) -> Arc<dyn RunLivenessHook> {
        self.hook.clone()
    }

    /// 主分类入口（hook 集成）。
    pub fn classify(&self, input: &RunLivenessClassificationInput) -> RunLivenessClassification {
        self.hook.before_classify(input);
        let result = classify_run_liveness(input);
        self.hook.after_classify(&result);
        result
    }
}

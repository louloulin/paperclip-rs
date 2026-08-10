use async_trait::async_trait;
use pc_errors::{Error as PcError, Result as PcResult};
use pc_repos::feedback_redaction as repo;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

pub const DEFAULT_MAX_CHARS: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RedactionHookEvent {
    Redacted {
        patterns: Vec<String>,
        total_redactions: usize,
    },
    Truncated {
        fields: Vec<String>,
    },
    Sanitized {
        patterns: Vec<String>,
        truncated_fields: Vec<String>,
    },
}

#[async_trait]
pub trait RedactionHook: Send + Sync {
    async fn on_redaction_event(&self, _event: RedactionHookEvent) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopRedactionHook;
#[async_trait]
impl RedactionHook for NoopRedactionHook {}

#[derive(Default)]
pub struct RecordingRedactionHook {
    pub events: std::sync::Mutex<Vec<RedactionHookEvent>>,
}
impl RecordingRedactionHook {
    pub fn events_snapshot(&self) -> Vec<RedactionHookEvent> {
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
impl RedactionHook for RecordingRedactionHook {
    async fn on_redaction_event(&self, e: RedactionHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RedactionError {
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Pc(#[from] PcError),
}
pub type RedactionResult<T> = std::result::Result<T, RedactionError>;

#[derive(Default, Clone)]
pub struct RedactionService {
    hooks: Vec<Arc<dyn RedactionHook>>,
}

impl RedactionService {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_hooks(hooks: Vec<Arc<dyn RedactionHook>>) -> Self {
        Self { hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn RedactionHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    async fn dispatch(&self, e: RedactionHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_redaction_event(e.clone()).await {
                tracing::warn!(?err, "redaction hook failed");
            }
        }
    }

    pub fn redact(&self, input: &str) -> (String, repo::RedactionState) {
        let (out, state) = repo::redact_free_text(input, None);
        let total: usize = state.counts.values().sum();
        let patterns: Vec<String> = state.redacted_patterns.iter().cloned().collect();
        if !patterns.is_empty() {
            let ev = RedactionHookEvent::Redacted {
                patterns,
                total_redactions: total,
            };
            // fire-and-forget: dispatch is sync but we cannot await inside sync method;
            // use a small block_on helper would block the runtime, so we mirror Node semantics
            // by only invoking hooks asynchronously in dedicated async methods below.
            let _ = ev;
        }
        (out, state)
    }

    pub async fn redact_async(&self, input: &str) -> (String, repo::RedactionState) {
        let (out, state) = repo::redact_free_text(input, None);
        let total: usize = state.counts.values().sum();
        let patterns: Vec<String> = state.redacted_patterns.iter().cloned().collect();
        if !patterns.is_empty() {
            self.dispatch(RedactionHookEvent::Redacted {
                patterns,
                total_redactions: total,
            })
            .await;
        }
        (out, state)
    }

    pub fn truncate(&self, value: &str, max_chars: usize) -> RedactionResult<(String, bool)> {
        if max_chars == 0 {
            return Err(RedactionError::Validation("max_chars must be > 0".into()));
        }
        Ok(repo::truncate_value(value, max_chars))
    }

    pub async fn truncate_async(
        &self,
        value: &str,
        max_chars: usize,
    ) -> RedactionResult<(String, bool)> {
        let pair = self.truncate(value, max_chars)?;
        if pair.1 {
            self.dispatch(RedactionHookEvent::Truncated {
                fields: vec![format!("len={}", value.len())],
            })
            .await;
        }
        Ok(pair)
    }

    pub fn sanitize_value(
        &self,
        value: &Value,
        max_chars: usize,
    ) -> RedactionResult<(Value, repo::RedactionState)> {
        if max_chars == 0 {
            return Err(RedactionError::Validation("max_chars must be > 0".into()));
        }
        let (out, state) = repo::sanitize_free_text_value(value, max_chars);
        Ok((out, state))
    }

    pub async fn sanitize_value_async(
        &self,
        value: &Value,
        max_chars: usize,
    ) -> RedactionResult<(Value, repo::RedactionState)> {
        let (out, state) = self.sanitize_value(value, max_chars)?;
        let patterns: Vec<String> = state.redacted_patterns.iter().cloned().collect();
        let truncated: Vec<String> = state.truncated_fields.iter().cloned().collect();
        if !patterns.is_empty() || !truncated.is_empty() {
            self.dispatch(RedactionHookEvent::Sanitized {
                patterns,
                truncated_fields: truncated,
            })
            .await;
        }
        Ok((out, state))
    }
}

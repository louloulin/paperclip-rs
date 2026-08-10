use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pc_errors::{Error as PcError, Result as PcResult};
use pc_telemetry::feedback_share::{
    build_feedback_share_object_key, encode_feedback_share_payload, FeedbackTraceBundle,
    FeedbackTraceShareClient, FeedbackTraceShareError, UploadTraceBundleResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FeedbackShareHookEvent {
    ObjectKeyBuilt {
        trace_id: String,
        object_key: String,
    },
    PayloadEncoded {
        encoding: String,
        byte_size: usize,
    },
    Uploaded {
        trace_id: String,
        object_key: String,
    },
    UploadFailed {
        trace_id: String,
        status: Option<u16>,
        message: String,
    },
}

#[async_trait]
pub trait FeedbackShareHook: Send + Sync {
    async fn on_feedback_share_event(&self, _event: FeedbackShareHookEvent) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopFeedbackShareHook;
#[async_trait]
impl FeedbackShareHook for NoopFeedbackShareHook {}

#[derive(Default)]
pub struct RecordingFeedbackShareHook {
    pub events: std::sync::Mutex<Vec<FeedbackShareHookEvent>>,
}
impl RecordingFeedbackShareHook {
    pub fn events_snapshot(&self) -> Vec<FeedbackShareHookEvent> {
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
impl FeedbackShareHook for RecordingFeedbackShareHook {
    async fn on_feedback_share_event(&self, e: FeedbackShareHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FeedbackShareError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("feedback share: {0}")]
    Share(#[from] FeedbackTraceShareError),
    #[error(transparent)]
    Pc(#[from] PcError),
}
pub type FeedbackShareResult<T> = std::result::Result<T, FeedbackShareError>;

/// Service that wraps a [`FeedbackTraceShareClient`] and adds validation + hook events.
#[derive(Clone)]
pub struct FeedbackShareService<C: FeedbackTraceShareClient + Clone + Send + Sync + 'static> {
    client: C,
    hooks: Vec<Arc<dyn FeedbackShareHook>>,
}

impl<C: FeedbackTraceShareClient + Clone + Send + Sync + 'static> FeedbackShareService<C> {
    pub fn new(client: C) -> Self {
        Self {
            client,
            hooks: vec![],
        }
    }
    pub fn with_hooks(client: C, hooks: Vec<Arc<dyn FeedbackShareHook>>) -> Self {
        Self { client, hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn FeedbackShareHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    async fn dispatch(&self, e: FeedbackShareHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_feedback_share_event(e.clone()).await {
                tracing::warn!(?err, "feedback share hook failed");
            }
        }
    }

    pub fn build_object_key(
        &self,
        bundle: &FeedbackTraceBundle,
        exported_at: DateTime<Utc>,
    ) -> FeedbackShareResult<String> {
        if bundle.trace_id.trim().is_empty() {
            return Err(FeedbackShareError::Validation(
                "trace_id must not be empty".into(),
            ));
        }
        if bundle.company_id.trim().is_empty() {
            return Err(FeedbackShareError::Validation(
                "company_id must not be empty".into(),
            ));
        }
        let key = build_feedback_share_object_key(bundle, exported_at);
        // fire-and-forget semantics for sync helper: no await; we still surface the value
        Ok(key)
    }

    pub async fn build_object_key_async(
        &self,
        bundle: &FeedbackTraceBundle,
        exported_at: DateTime<Utc>,
    ) -> FeedbackShareResult<String> {
        let key = self.build_object_key(bundle, exported_at)?;
        self.dispatch(FeedbackShareHookEvent::ObjectKeyBuilt {
            trace_id: bundle.trace_id.clone(),
            object_key: key.clone(),
        })
        .await;
        Ok(key)
    }

    pub fn encode_payload(
        &self,
        object_key: &str,
        exported_at: DateTime<Utc>,
        bundle: &FeedbackTraceBundle,
    ) -> FeedbackShareResult<(String, String)> {
        if object_key.trim().is_empty() {
            return Err(FeedbackShareError::Validation(
                "object_key must not be empty".into(),
            ));
        }
        let result = encode_feedback_share_payload(object_key, exported_at, bundle)?;
        Ok(result)
    }

    pub async fn encode_payload_async(
        &self,
        object_key: &str,
        exported_at: DateTime<Utc>,
        bundle: &FeedbackTraceBundle,
    ) -> FeedbackShareResult<(String, String)> {
        let (encoding, payload) = self.encode_payload(object_key, exported_at, bundle)?;
        self.dispatch(FeedbackShareHookEvent::PayloadEncoded {
            encoding: encoding.clone(),
            byte_size: payload.len(),
        })
        .await;
        Ok((encoding, payload))
    }

    pub async fn upload(
        &self,
        bundle: &FeedbackTraceBundle,
    ) -> FeedbackShareResult<UploadTraceBundleResponse> {
        if bundle.trace_id.trim().is_empty() {
            return Err(FeedbackShareError::Validation(
                "trace_id must not be empty".into(),
            ));
        }
        match self.client.upload_trace_bundle(bundle).await {
            Ok(resp) => {
                self.dispatch(FeedbackShareHookEvent::Uploaded {
                    trace_id: bundle.trace_id.clone(),
                    object_key: resp.object_key.clone(),
                })
                .await;
                Ok(resp)
            }
            Err(err) => {
                let (status, message) = match &err {
                    FeedbackTraceShareError::Http { status, body } => {
                        (Some(*status), format!("HTTP {status}: {body}"))
                    }
                    other => (None, other.to_string()),
                };
                self.dispatch(FeedbackShareHookEvent::UploadFailed {
                    trace_id: bundle.trace_id.clone(),
                    status,
                    message,
                })
                .await;
                Err(FeedbackShareError::Share(err))
            }
        }
    }
}

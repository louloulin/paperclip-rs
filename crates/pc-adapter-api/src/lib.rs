#![forbid(unsafe_code)]

pub mod models_env;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterSource {
    Builtin,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterDescriptor {
    pub adapter_type: String,
    pub label: String,
    pub source: AdapterSource,
    pub supports_local_agent_jwt: bool,
    pub supports_instructions_bundle: bool,
}

impl AdapterDescriptor {
    pub fn builtin(adapter_type: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            adapter_type: adapter_type.into(),
            label: label.into(),
            source: AdapterSource::Builtin,
            supports_local_agent_jwt: false,
            supports_instructions_bundle: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AdapterExecutionContext {
    pub run_id: Uuid,
    pub agent_id: Uuid,
    pub prompt: String,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub session_id: Option<String>,
    pub session_params: Option<serde_json::Value>,
    pub adapter_config: serde_json::Value,
    pub runtime_config: serde_json::Value,
    /// 执行目标（local / ssh / sandbox），由 route 层从 agent config 解析并
    /// 注入。adapter 在需要 ssh/sandbox 行为时通过 `pc_acpx::execution_target`
    /// 下的 `AdapterExecutionTarget` 解码。存为 JSON 以避免 pc-adapter-api
    /// 与 pc-acpx 形成循环依赖。
    pub execution_target: Option<serde_json::Value>,
    pub cancellation: CancellationToken,
}

impl AdapterExecutionContext {
    pub fn new(run_id: Uuid, agent_id: Uuid, prompt: impl Into<String>) -> Self {
        Self {
            run_id,
            agent_id,
            prompt: prompt.into(),
            cwd: None,
            env: BTreeMap::new(),
            session_id: None,
            session_params: None,
            adapter_config: serde_json::Value::Null,
            runtime_config: serde_json::Value::Null,
            execution_target: None,
            cancellation: CancellationToken::new(),
        }
    }

    /// 设置 execution_target（JSON 形式）。调用方通常从 agent config 解析后
    /// 注入。
    pub fn with_execution_target(mut self, target: serde_json::Value) -> Self {
        self.execution_target = Some(target);
        self
    }

    /// 取出 execution_target 的 JSON 形式。
    pub fn execution_target_json(&self) -> Option<&serde_json::Value> {
        self.execution_target.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdapterEvent {
    Output {
        stream: OutputStream,
        text: String,
        at: chrono::DateTime<chrono::Utc>,
    },
    Progress {
        message: String,
        payload: Option<serde_json::Value>,
        at: chrono::DateTime<chrono::Utc>,
    },
    Session {
        session_id: Option<String>,
        session_params: Option<serde_json::Value>,
        display_id: Option<String>,
        at: chrono::DateTime<chrono::Utc>,
    },
}

impl AdapterEvent {
    pub fn stdout(text: impl Into<String>) -> Self {
        Self::Output {
            stream: OutputStream::Stdout,
            text: text.into(),
            at: chrono::Utc::now(),
        }
    }

    pub fn stderr(text: impl Into<String>) -> Self {
        Self::Output {
            stream: OutputStream::Stderr,
            text: text.into(),
            at: chrono::Utc::now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AdapterEventSink {
    sender: mpsc::Sender<AdapterEvent>,
}

impl AdapterEventSink {
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<AdapterEvent>) {
        let (sender, receiver) = mpsc::channel(capacity.max(1));
        (Self { sender }, receiver)
    }

    pub async fn emit(&self, event: AdapterEvent) -> Result<(), AdapterError> {
        self.sender
            .send(event)
            .await
            .map_err(|_| AdapterError::EventConsumerClosed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AdapterExecutionResult {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub timed_out: bool,
    pub error_message: Option<String>,
    pub error_code: Option<String>,
    pub usage: Option<UsageSummary>,
    pub session_id: Option<String>,
    pub session_params: Option<serde_json::Value>,
    pub session_display_id: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub billing_type: Option<String>,
    pub cost_usd: Option<f64>,
    pub result_json: Option<serde_json::Value>,
    pub summary: Option<String>,
    pub clear_session: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("adapter event consumer closed")]
    EventConsumerClosed,
    #[error("adapter execution cancelled")]
    Cancelled,
    #[error("adapter execution timed out")]
    TimedOut,
    #[error("adapter process failed: {0}")]
    Process(String),
    #[error("adapter configuration invalid: {0}")]
    InvalidConfiguration(String),
}

#[async_trait]
pub trait Adapter: Send + Sync + 'static {
    fn descriptor(&self) -> AdapterDescriptor;

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError>;
}

// ============================================================================
// HireApprovedHook — adapter-side hook for "agent hire approved" notifications.
// ============================================================================

/// hire 通知的 source 字段（与 Node 字面量 1:1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HireApprovedSource {
    JoinRequest,
    Approval,
}

impl HireApprovedSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::JoinRequest => "join_request",
            Self::Approval => "approval",
        }
    }
}

/// 与 Node `HireApprovedPayload` 1:1 对齐的 payload DTO。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HireApprovedPayload {
    pub company_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub adapter_type: String,
    pub source: HireApprovedSource,
    pub source_id: String,
    /// RFC3339 timestamp string。
    pub approved_at: String,
    pub message: String,
}

/// adapter `onHireApproved` 调用结果 —— 与 Node `{ ok, error?, detail? }` 1:1。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HireApprovedResult {
    pub ok: bool,
    pub error: Option<String>,
    pub detail: Option<String>,
}

impl HireApprovedResult {
    pub fn ok() -> Self {
        Self { ok: true, error: None, detail: None }
    }

    pub fn failure(error: impl Into<String>, detail: Option<String>) -> Self {
        Self { ok: false, error: Some(error.into()), detail }
    }
}

/// Adapter 实现的 hire-approved 回调 trait。
///
/// Node 端 `adapter.onHireApproved(payload, adapterConfig)` 在 Rust 中被拆为
/// 独立 trait；adapter 自己选择是否实现。
#[async_trait]
pub trait HireApprovedHook: Send + Sync + 'static {
    async fn on_hire_approved(
        &self,
        payload: HireApprovedPayload,
        adapter_config: serde_json::Value,
    ) -> HireApprovedResult;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdapterRegistryError {
    #[error("adapter already registered: {0}")]
    AlreadyRegistered(String),
    #[error("adapter not found: {0}")]
    NotFound(String),
}

#[derive(Clone, Default)]
pub struct AdapterRegistry {
    adapters: Arc<RwLock<HashMap<String, Arc<dyn Adapter>>>>,
}

impl AdapterRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, adapter: Arc<dyn Adapter>) -> Result<(), AdapterRegistryError> {
        let descriptor = adapter.descriptor();
        let mut adapters = self
            .adapters
            .write()
            .expect("adapter registry lock poisoned");
        if adapters.contains_key(&descriptor.adapter_type) {
            return Err(AdapterRegistryError::AlreadyRegistered(
                descriptor.adapter_type,
            ));
        }
        adapters.insert(descriptor.adapter_type, adapter);
        Ok(())
    }

    pub fn descriptors(&self) -> Vec<AdapterDescriptor> {
        let mut descriptors = self
            .adapters
            .read()
            .expect("adapter registry lock poisoned")
            .values()
            .map(|adapter| adapter.descriptor())
            .collect::<Vec<_>>();
        descriptors.sort_by(|left, right| left.adapter_type.cmp(&right.adapter_type));
        descriptors
    }

    pub fn descriptor(&self, adapter_type: &str) -> Option<AdapterDescriptor> {
        self.adapters
            .read()
            .expect("adapter registry lock poisoned")
            .get(adapter_type)
            .map(|adapter| adapter.descriptor())
    }

    pub async fn execute(
        &self,
        adapter_type: &str,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterExecutionError> {
        let adapter = self
            .adapters
            .read()
            .expect("adapter registry lock poisoned")
            .get(adapter_type)
            .cloned()
            .ok_or_else(|| AdapterRegistryError::NotFound(adapter_type.into()))?;
        adapter
            .execute(context, events)
            .await
            .map_err(AdapterExecutionError::Adapter)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterExecutionError {
    #[error(transparent)]
    Registry(#[from] AdapterRegistryError),
    #[error(transparent)]
    Adapter(#[from] AdapterError),
}

// =============================================================================
// Adapter config schema — declarative UI config for adapters
//
// Mirrors Node `@paperclipai/adapter-utils` types:
//   - `AdapterConfigSchema`
//   - `ConfigFieldSchema`
//   - `ConfigFieldOption`
// =============================================================================

/// Option label/value pair for select/combobox fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFieldOption {
    pub label: String,
    pub value: String,
    /// Optional group key for categorizing options.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// Field type discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFieldType {
    Text,
    Select,
    Toggle,
    Number,
    Textarea,
    Combobox,
}

impl ConfigFieldType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ConfigFieldType::Text => "text",
            ConfigFieldType::Select => "select",
            ConfigFieldType::Toggle => "toggle",
            ConfigFieldType::Number => "number",
            ConfigFieldType::Textarea => "textarea",
            ConfigFieldType::Combobox => "combobox",
        }
    }
}

/// Declarative field schema for adapter config UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigFieldSchema {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: ConfigFieldType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ConfigFieldOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Optional metadata — not rendered, but available to custom UI logic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

/// Adapter config schema declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterConfigSchema {
    pub fields: Vec<ConfigFieldSchema>,
}

impl AdapterConfigSchema {
    #[must_use]
    pub fn new(fields: Vec<ConfigFieldSchema>) -> Self {
        Self { fields }
    }
}

/// Field visibility predicate (`visibleWhen: { key, values }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldVisibility {
    pub key: String,
    pub values: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    struct FakeAdapter;

    #[async_trait::async_trait]
    impl Adapter for FakeAdapter {
        fn descriptor(&self) -> AdapterDescriptor {
            AdapterDescriptor::builtin("fake", "Fake")
        }

        async fn execute(
            &self,
            context: AdapterExecutionContext,
            events: AdapterEventSink,
        ) -> Result<AdapterExecutionResult, AdapterError> {
            events.emit(AdapterEvent::stdout("hello")).await?;
            Ok(AdapterExecutionResult {
                exit_code: Some(0),
                session_id: context.session_id,
                usage: Some(UsageSummary {
                    input_tokens: 10,
                    output_tokens: 4,
                    cached_input_tokens: Some(2),
                }),
                ..AdapterExecutionResult::default()
            })
        }
    }

    #[tokio::test]
    async fn registry_executes_adapter_and_streams_events() {
        let registry = AdapterRegistry::new();
        registry.register(Arc::new(FakeAdapter)).unwrap();
        let (sink, mut receiver) = AdapterEventSink::channel(4);
        let context =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "prompt");

        let result = registry.execute("fake", context, sink).await.unwrap();
        let event = receiver.recv().await.unwrap();

        assert!(matches!(
            event,
            AdapterEvent::Output {
                stream: OutputStream::Stdout,
                ..
            }
        ));
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.usage.unwrap().output_tokens, 4);
    }

    #[test]
    fn registry_rejects_duplicate_adapter_type() {
        let registry = AdapterRegistry::new();
        registry.register(Arc::new(FakeAdapter)).unwrap();

        let error = registry.register(Arc::new(FakeAdapter)).unwrap_err();

        assert_eq!(
            error,
            AdapterRegistryError::AlreadyRegistered("fake".into())
        );
    }
}

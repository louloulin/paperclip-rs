//! `pc-acpx` ACP runtime protocol — Rust port of the `AcpRuntime` interface
//! from Node `acpx/runtime` (the upstream acpx package).
//!
//! The interface defines how the engine talks to the agent runtime. The
//! contract is async with optional methods, mirroring the Node API surface.
//! Implementations are usually backed by the acpx subprocess; for tests we
//! provide `MockAcpRuntime`.
//!
//! ## Scope of this round
//!
//! R365 ports the **interface + core value types** plus an in-memory mock
//! for tests. The full agent-runtime spawn lifecycle (the `acpx` JS library
//! re-impl) is a separate port that lands in a later round.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

use crate::normalize::{NormalizedMode, NormalizedPermissionMode};

// ============================================================================
// Public types
// ============================================================================

/// Mirrors Node `AcpRuntimeHandle`. The handle identifies an active ACP
/// session across the network / process boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpRuntimeHandle {
    pub session_key: String,
    pub backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acpx_record_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
}

/// Mirrors Node `AcpRuntimeEnsureInput`.
#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeEnsureInput {
    pub session_key: String,
    pub agent: String,
    pub mode: AcpRuntimeMode,
    pub resume_session_id: Option<String>,
    pub cwd: Option<String>,
    pub session_options: Option<SessionAgentOptions>,
}

/// Mirrors Node `SessionAgentOptions` (the subset the engine uses).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAgentOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<NormalizedPermissionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<McpServerEntry>>,
}

/// Mirrors Node `McpServer$1` — the MCP server payload the engine ships
/// to the runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
}

/// Mirrors Node `AcpRuntimeTurnAttachment`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpRuntimeTurnAttachment {
    pub media_type: String,
    pub data: String,
}

/// Mirrors Node `AcpRuntimeTurnInput`.
#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeTurnInput {
    pub handle: AcpRuntimeHandle,
    pub text: String,
    pub attachments: Vec<AcpRuntimeTurnAttachment>,
    pub mode: AcpRuntimePromptMode,
    pub request_id: String,
    pub timeout_ms: Option<u64>,
}

/// Mirrors Node `AcpRuntimePromptMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcpRuntimePromptMode {
    Prompt,
    Steer,
}

impl Default for AcpRuntimePromptMode {
    fn default() -> Self {
        AcpRuntimePromptMode::Prompt
    }
}

/// Mirrors Node `AcpRuntimeSessionMode`. The Engine view delegates to
/// `NormalizedMode` so callers do not need to import a separate type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcpRuntimeMode {
    Persistent,
    OneShot,
}

impl Default for AcpRuntimeMode {
    fn default() -> Self {
        AcpRuntimeMode::Persistent
    }
}

impl From<NormalizedMode> for AcpRuntimeMode {
    fn from(mode: NormalizedMode) -> Self {
        match mode {
            NormalizedMode::Persistent => AcpRuntimeMode::Persistent,
            NormalizedMode::OneShot => AcpRuntimeMode::OneShot,
        }
    }
}

impl From<AcpRuntimeMode> for NormalizedMode {
    fn from(mode: AcpRuntimeMode) -> Self {
        match mode {
            AcpRuntimeMode::Persistent => NormalizedMode::Persistent,
            AcpRuntimeMode::OneShot => NormalizedMode::OneShot,
        }
    }
}

/// Mirrors Node `AcpRuntimeCapabilities`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpRuntimeCapabilities {
    pub controls: Vec<AcpRuntimeControl>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_option_keys: Option<Vec<String>>,
}

/// Mirrors Node `AcpRuntimeControl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpRuntimeControl {
    SetMode,
    SetConfigOption,
    Status,
}

impl AcpRuntimeControl {
    pub fn as_str(&self) -> &'static str {
        match self {
            AcpRuntimeControl::SetMode => "session/set_mode",
            AcpRuntimeControl::SetConfigOption => "session/set_config_option",
            AcpRuntimeControl::Status => "session/status",
        }
    }
}

/// Mirrors Node `AcpRuntimeEvent` — a tagged union of event variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcpRuntimeEvent {
    TextDelta {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        stream: Option<AcpRuntimeStream>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },
    Status {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        used: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost: Option<AcpRuntimeUsageCost>,
        #[serde(skip_serializing_if = "Option::is_none")]
        breakdown: Option<AcpRuntimeUsageBreakdown>,
        #[serde(skip_serializing_if = "Option::is_none")]
        available_commands: Option<Vec<AcpRuntimeAvailableCommand>>,
    },
    ToolCall {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        locations: Option<Vec<AcpRuntimeToolCallLocation>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw_input: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw_output: Option<serde_json::Value>,
    },
    Done {
        #[serde(skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
    },
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        retryable: Option<bool>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcpRuntimeStream {
    Output,
    Thought,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpRuntimeToolCallLocation {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AcpRuntimeUsageCost {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpRuntimeUsageBreakdown {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_read_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_write_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpRuntimeAvailableCommand {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_input: Option<bool>,
}

/// Mirrors Node `AcpRuntimeTurnResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AcpRuntimeTurnResult {
    Completed {
        #[serde(skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
    },
    Cancelled {
        #[serde(skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
    },
    Failed {
        error: AcpRuntimeTurnResultError,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AcpRuntimeTurnResultError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

/// Mirrors Node `AcpRuntimeSessionUsage`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AcpRuntimeSessionUsage {
    pub cumulative: Option<AcpRuntimeUsageBreakdown>,
    pub cost: Option<AcpRuntimeUsageCost>,
    pub per_request: BTreeMap<String, AcpRuntimeUsageBreakdown>,
}

/// Mirrors Node `AcpRuntimeSessionModels`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpRuntimeSessionModels {
    pub current_model_id: Option<String>,
    pub available_model_ids: Vec<String>,
}

/// Mirrors Node `AcpRuntimeStatus`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AcpRuntimeStatus {
    pub summary: Option<String>,
    pub acpx_record_id: Option<String>,
    pub backend_session_id: Option<String>,
    pub agent_session_id: Option<String>,
    pub models: Option<AcpRuntimeSessionModels>,
    pub usage: Option<AcpRuntimeSessionUsage>,
    pub available_commands: Option<Vec<AcpRuntimeAvailableCommand>>,
    pub details: Option<serde_json::Value>,
}

/// Mirrors Node `AcpRuntimeDoctorReport`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpRuntimeDoctorReport {
    pub ok: bool,
    pub code: Option<String>,
    pub message: String,
    pub install_command: Option<String>,
    pub details: Option<Vec<String>>,
}

// ============================================================================
// Errors
// ============================================================================

/// Errors raised by `AcpRuntime` implementations.
#[derive(Debug, Error)]
pub enum AcpRuntimeError {
    #[error("acpx handshake failed: {message}")]
    HandshakeFailed {
        message: String,
        code: Option<String>,
    },
    #[error("acpx turn failed: {message}")]
    TurnFailed {
        message: String,
        code: Option<String>,
    },
    #[error("acpx session operation failed: {0}")]
    SessionError(String),
    #[error("acpx io error: {0}")]
    Io(String),
}

// ============================================================================
// Optional get/set inputs
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeGetCapabilitiesInput {
    pub handle: Option<AcpRuntimeHandle>,
}

#[derive(Debug, Clone)]
pub struct AcpRuntimeGetStatusInput {
    pub handle: AcpRuntimeHandle,
}

#[derive(Debug, Clone)]
pub struct AcpRuntimeSetModeInput {
    pub handle: AcpRuntimeHandle,
    pub mode: String,
}

#[derive(Debug, Clone)]
pub struct AcpRuntimeSetConfigOptionInput {
    pub handle: AcpRuntimeHandle,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct AcpRuntimeCancelInput {
    pub handle: AcpRuntimeHandle,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AcpRuntimeCloseInput {
    pub handle: AcpRuntimeHandle,
    pub reason: String,
    pub discard_persistent_state: Option<bool>,
}

// ============================================================================
// Trait
// ============================================================================

/// Mirrors Node `AcpRuntime`. The interface is async with optional methods.
#[async_trait]
pub trait AcpRuntime: Send + Sync {
    async fn ensure_session(
        &self,
        input: AcpRuntimeEnsureInput,
    ) -> Result<AcpRuntimeHandle, AcpRuntimeError>;

    fn start_turn(&self, input: AcpRuntimeTurnInput) -> AcpRuntimeTurn;

    async fn run_turn(&self, input: AcpRuntimeTurnInput) -> Vec<AcpRuntimeEvent> {
        let turn = self.start_turn(input);
        let mut events = Vec::new();
        use futures::StreamExt;
        let mut stream = turn.events;
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        events
    }

    async fn get_capabilities(
        &self,
        _input: AcpRuntimeGetCapabilitiesInput,
    ) -> Option<AcpRuntimeCapabilities> {
        None
    }

    async fn get_status(&self, _input: AcpRuntimeGetStatusInput) -> Option<AcpRuntimeStatus> {
        None
    }

    async fn set_mode(&self, _input: AcpRuntimeSetModeInput) -> Result<(), AcpRuntimeError> {
        Err(AcpRuntimeError::SessionError(
            "set_mode not supported".into(),
        ))
    }

    async fn set_config_option(
        &self,
        _input: AcpRuntimeSetConfigOptionInput,
    ) -> Result<(), AcpRuntimeError> {
        Ok(())
    }

    async fn doctor(&self) -> Option<AcpRuntimeDoctorReport> {
        None
    }

    async fn cancel(&self, _input: AcpRuntimeCancelInput) -> Result<(), AcpRuntimeError> {
        Ok(())
    }

    async fn close(&self, _input: AcpRuntimeCloseInput) -> Result<(), AcpRuntimeError> {
        Ok(())
    }
}

// ============================================================================
// Turn handle
// ============================================================================

pub struct AcpRuntimeTurn {
    pub request_id: String,
    pub events: AcpRuntimeEventStream,
    pub result: AcpRuntimeTurnResultResolver,
}

impl std::fmt::Debug for AcpRuntimeTurn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpRuntimeTurn")
            .field("request_id", &self.request_id)
            .finish()
    }
}

pub type AcpRuntimeEventStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = AcpRuntimeEvent> + Send + Sync>>;

pub type AcpRuntimeTurnResultFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = AcpRuntimeTurnResult> + Send + Sync>>;

pub struct AcpRuntimeTurnResultResolver {
    pub future: AcpRuntimeTurnResultFuture,
}

impl std::fmt::Debug for AcpRuntimeTurnResultResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpRuntimeTurnResultResolver")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_default_is_empty() {
        let handle = AcpRuntimeHandle::default();
        assert_eq!(handle.session_key, "");
        assert!(handle.runtime_session_name.is_none());
    }

    #[test]
    fn mode_round_trips_through_normalized_mode() {
        let mode = AcpRuntimeMode::OneShot;
        let normalized: NormalizedMode = mode.into();
        assert_eq!(normalized, NormalizedMode::OneShot);
        let back: AcpRuntimeMode = normalized.into();
        assert_eq!(back, AcpRuntimeMode::OneShot);
    }

    #[test]
    fn event_serializes_with_tag() {
        let event = AcpRuntimeEvent::TextDelta {
            text: "hello".into(),
            stream: Some(AcpRuntimeStream::Output),
            tag: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"text_delta\""));
        assert!(json.contains("\"text\":\"hello\""));
    }

    #[test]
    fn grouped_turn_result_serializes_with_status_tag() {
        let result = AcpRuntimeTurnResult::Completed {
            stop_reason: Some("end_turn".into()),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"status\":\"completed\""));
        assert!(json.contains("\"stop_reason\":\"end_turn\""));
    }

    #[test]
    fn failed_turn_result_carries_error_message() {
        let result = AcpRuntimeTurnResult::Failed {
            error: AcpRuntimeTurnResultError {
                message: "boom".into(),
                code: Some("E_BOOM".into()),
                detail_code: None,
                retryable: Some(false),
            },
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"status\":\"failed\""));
        assert!(json.contains("\"message\":\"boom\""));
    }

    #[test]
    fn control_as_str_matches_node_invariant_strings() {
        assert_eq!(AcpRuntimeControl::SetMode.as_str(), "session/set_mode");
        assert_eq!(
            AcpRuntimeControl::SetConfigOption.as_str(),
            "session/set_config_option"
        );
        assert_eq!(AcpRuntimeControl::Status.as_str(), "session/status");
    }
}

// ============================================================================
// Mock runtime
// ============================================================================

/// In-memory `AcpRuntime` implementation for tests. The mock records each
/// `ensure_session` call and replays a configurable event list per turn.
///
/// # Example
///
/// ```ignore
/// use pc_acpx::acp_runtime::{
///     AcpRuntime, AcpRuntimeEnsureInput, AcpRuntimeEvent, AcpRuntimeMode,
///     AcpRuntimeTurnInput, MockAcpRuntime,
/// };
/// let mock = MockAcpRuntime::new(vec![
///     AcpRuntimeEvent::Done { stop_reason: Some("end_turn".into()) },
/// ]);
/// let handle = mock.ensure_session(AcpRuntimeEnsureInput {
///     session_key: "k".into(),
///     agent: "claude".into(),
///     mode: AcpRuntimeMode::Persistent,
///     ..Default::default()
/// }).await.unwrap();
/// let events = mock.run_turn(AcpRuntimeTurnInput {
///     handle,
///     request_id: "r".into(),
///     ..Default::default()
/// }).await;
/// assert_eq!(events.len(), 1);
/// ```
pub struct MockAcpRuntime {
    pub events: Vec<AcpRuntimeEvent>,
    pub next_session_id: std::sync::atomic::AtomicU64,
    capabilities: Option<AcpRuntimeCapabilities>,
}

impl MockAcpRuntime {
    pub fn new(events: Vec<AcpRuntimeEvent>) -> Self {
        Self {
            events,
            next_session_id: std::sync::atomic::AtomicU64::new(1),
            capabilities: None,
        }
    }

    pub fn with_capabilities(mut self, capabilities: AcpRuntimeCapabilities) -> Self {
        self.capabilities = Some(capabilities);
        self
    }
}

#[async_trait]
impl AcpRuntime for MockAcpRuntime {
    async fn ensure_session(
        &self,
        input: AcpRuntimeEnsureInput,
    ) -> Result<AcpRuntimeHandle, AcpRuntimeError> {
        let id = self
            .next_session_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(AcpRuntimeHandle {
            session_key: input.session_key,
            backend: input.agent,
            runtime_session_name: Some(format!("mock-{id}")),
            cwd: input.cwd,
            acpx_record_id: Some(format!("rec-{id}")),
            backend_session_id: Some(format!("backend-{id}")),
            agent_session_id: Some(format!("agent-{id}")),
        })
    }

    fn start_turn(&self, input: AcpRuntimeTurnInput) -> AcpRuntimeTurn {
        let events_vec = self.events.clone();
        let request_id = input.request_id.clone();
        let stream: AcpRuntimeEventStream = Box::pin(futures::stream::iter(events_vec));
        let result_future = Box::pin(async move {
            AcpRuntimeTurnResult::Completed {
                stop_reason: Some("end_turn".into()),
            }
        });
        AcpRuntimeTurn {
            request_id,
            events: stream,
            result: AcpRuntimeTurnResultResolver {
                future: result_future,
            },
        }
    }

    async fn get_capabilities(
        &self,
        _input: AcpRuntimeGetCapabilitiesInput,
    ) -> Option<AcpRuntimeCapabilities> {
        self.capabilities.clone()
    }

    async fn get_status(&self, input: AcpRuntimeGetStatusInput) -> Option<AcpRuntimeStatus> {
        Some(AcpRuntimeStatus {
            summary: Some(format!("mock status for {}", input.handle.session_key)),
            backend_session_id: input.handle.backend_session_id.clone(),
            agent_session_id: input.handle.agent_session_id.clone(),
            ..Default::default()
        })
    }

    async fn doctor(&self) -> Option<AcpRuntimeDoctorReport> {
        Some(AcpRuntimeDoctorReport {
            ok: true,
            message: "mock runtime healthy".into(),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod mock_tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn mock_assigns_incrementing_session_ids() {
        let mock = MockAcpRuntime::new(vec![]);
        let h1 = mock
            .ensure_session(AcpRuntimeEnsureInput {
                session_key: "k1".into(),
                agent: "claude".into(),
                mode: AcpRuntimeMode::Persistent,
                ..Default::default()
            })
            .await
            .unwrap();
        let h2 = mock
            .ensure_session(AcpRuntimeEnsureInput {
                session_key: "k2".into(),
                agent: "codex".into(),
                mode: AcpRuntimeMode::Persistent,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(h1.runtime_session_name, Some("mock-1".into()));
        assert_eq!(h2.runtime_session_name, Some("mock-2".into()));
        assert_ne!(h1.backend_session_id, h2.backend_session_id);
    }

    #[tokio::test]
    async fn mock_emits_configured_events() {
        let mock = MockAcpRuntime::new(vec![
            AcpRuntimeEvent::TextDelta {
                text: "hello".into(),
                stream: Some(AcpRuntimeStream::Output),
                tag: None,
            },
            AcpRuntimeEvent::Done {
                stop_reason: Some("end_turn".into()),
            },
        ]);
        let handle = mock
            .ensure_session(AcpRuntimeEnsureInput {
                session_key: "k".into(),
                agent: "claude".into(),
                mode: AcpRuntimeMode::Persistent,
                ..Default::default()
            })
            .await
            .unwrap();
        let turn = mock.start_turn(AcpRuntimeTurnInput {
            handle: handle.clone(),
            request_id: "r".into(),
            ..Default::default()
        });
        let mut events: Vec<AcpRuntimeEvent> = Vec::new();
        let mut stream = turn.events;
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AcpRuntimeEvent::TextDelta { .. }));
        assert!(matches!(events[1], AcpRuntimeEvent::Done { .. }));
    }

    #[tokio::test]
    async fn mock_capabilities_are_advertised() {
        let mock = MockAcpRuntime::new(vec![]).with_capabilities(AcpRuntimeCapabilities {
            controls: vec![AcpRuntimeControl::SetMode, AcpRuntimeControl::Status],
            config_option_keys: Some(vec!["model".into()]),
        });
        let caps = mock
            .get_capabilities(AcpRuntimeGetCapabilitiesInput::default())
            .await
            .expect("caps");
        assert_eq!(caps.controls.len(), 2);
        assert_eq!(
            caps.config_option_keys.as_deref(),
            Some(&["model".to_string()][..])
        );
    }

    #[tokio::test]
    async fn mock_doctor_reports_ok() {
        let mock = MockAcpRuntime::new(vec![]);
        let report = mock.doctor().await.expect("report");
        assert!(report.ok);
        assert!(!report.message.is_empty());
    }

    #[tokio::test]
    async fn mock_status_copies_handle_fields() {
        let mock = MockAcpRuntime::new(vec![]);
        let handle = AcpRuntimeHandle {
            session_key: "k".into(),
            backend: "claude".into(),
            agent_session_id: Some("agent-1".into()),
            ..Default::default()
        };
        let status = mock
            .get_status(AcpRuntimeGetStatusInput {
                handle: handle.clone(),
            })
            .await
            .expect("status");
        assert_eq!(status.agent_session_id, handle.agent_session_id);
        assert!(status.summary.unwrap().contains("k"));
    }
}

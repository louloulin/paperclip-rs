#![forbid(unsafe_code)]

//! `openclaw_gateway` adapter: WebSocket-based OpenClaw Gateway gateway.
//!
//! 当前阶段（R608 step1）拆分 4 个核心纯函数模块：
//! - `constants` — 协议常量（PROTOCOL_VERSION / scopes / 默认值）
//! - `session_key` — SessionKeyStrategy 枚举 + `resolveSessionKey`
//! - `credentials` — 敏感日志 key 遮蔽 + 设备身份 fingerprint
//! - `host_security` — loopback 检测 + escape hatch
//!
//! 后续 R609+：wire frame codec + WebSocket client + retry policy。

pub mod config_schema;
pub mod constants;
pub mod credentials;
pub mod execute;
pub mod frame_codec;
pub mod host_security;
pub mod parse_stdout;
pub mod retry_policy;
pub mod session_key;
pub mod wake_env;
pub mod wire_client;
pub mod ws_client;

pub use config_schema::{
    describe_adapter as describe_openclaw_adapter, get_config_schema, parse_scopes,
    required_field_keys, AdapterDescriptor as OpenclawAdapterDescriptor, ConfigField, ConfigOption,
    FieldType,
};
pub use constants::{
    frame_types, ADAPTER_LABEL, ADAPTER_TYPE, DEFAULT_CLIENT_ID, DEFAULT_CLIENT_MODE,
    DEFAULT_CLIENT_VERSION, DEFAULT_CONNECT_TIMEOUT_MS, DEFAULT_MAX_HEADER_BYTES,
    DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_REQUEST_TIMEOUT_MS, DEFAULT_ROLE, DEFAULT_SCOPES,
    DEFAULT_SESSION_KEY, DEFAULT_SESSION_KEY_STRATEGY, PERMANENT_GATEWAY_CODES, PROTOCOL_VERSION,
    TRANSIENT_GATEWAY_CODES, VALID_SESSION_KEY_STRATEGIES,
};
pub use credentials::{
    fingerprint_public_key, is_sensitive_log_key, redact_headers, redact_value, summarize_identity,
    DeviceIdentitySource, DeviceIdentitySummary, GatewayDeviceIdentity,
};
pub use frame_codec::{
    event_to_line, frame_id_of, frame_to_value, frame_type_of, parse_any_frame, parse_event_frame,
    parse_request_frame, parse_response_frame, request_to_line, response_to_line, FrameParseError,
    GatewayEventFrame, GatewayFrame, GatewayRequestFrame, GatewayResponseErrorBody,
    GatewayResponseFrame,
};
pub use host_security::{
    allows_insecure_remote_http, is_loopback_host, parse_bool_like,
    remote_plain_http_denied_message, validate_gateway_url, ESCAPE_HATCH_KEY, LOOPBACK_HOSTS,
};
pub use parse_stdout::{
    normalize_stream_line, parse_event_line, parse_stdout_line, EntryKind, StreamSource,
    TranscriptEntry,
};
pub use retry_policy::{
    backoff_with_jitter, classify_gateway_code, should_retry_gateway_error, RetryClass,
};
pub use session_key::{
    is_agent_prefixed, known_strategies, normalize_strategy_string, prefix_session_key_for_agent,
    resolve_session_key, SessionKeyInput, SessionKeyStrategy,
};
pub use wake_env::{
    build_wake_env, describe_env, paperclip_keys, render_paperclip_env_note, EnvDescription,
    WakeEnvInput, WakeEnvOutput,
};
pub use wire_client::{
    build_event, build_ok_response, build_request, make_connect_options, ConnectOptions,
    DynWireClient, FakeWireClient, GatewayError, GatewayHello, GatewayWireClient, ScriptedStep,
};
pub use wire_client::{
    DeviceIdentitySource as _WireDeviceIdentitySource,
    GatewayDeviceIdentity as _WireGatewayDeviceIdentity,
};

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult,
};
use pc_adapter_process::{execute_process_capture, ProcessSpec};

fn default_command(config: &serde_json::Value) -> String {
    config
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("openclaw-gw")
        .to_owned()
}

fn default_model(config: &serde_json::Value) -> Option<String> {
    config
        .get("model")
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Parse adapter-specific JSONL or text output from stdout.
fn parse_stdout(stdout: &str) -> Option<String> {
    // Find the last non-empty line that looks like useful output
    for line in stdout.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Try JSONL parsing for structured events
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(text) = event.get("text").and_then(|v| v.as_str()) {
                return Some(text.to_owned());
            }
            if let Some(item) = event
                .get("item")
                .and_then(|v| v.get("text"))
                .and_then(|v| v.as_str())
            {
                return Some(item.to_owned());
            }
        }
        return Some(trimmed.to_owned());
    }
    None
}

pub struct OpenclawGatewayAdapter;

impl OpenclawGatewayAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpenclawGatewayAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for OpenclawGatewayAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin(ADAPTER_TYPE, ADAPTER_LABEL)
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        let command = default_command(&context.adapter_config);
        let _ = default_model(&context.adapter_config);
        let _ = parse_stdout;
        let spec = ProcessSpec::new(command, std::iter::empty::<String>());
        let execution = execute_process_capture(&spec, &context, events).await?;
        Ok(execution.result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adapter_descriptor_uses_canonical_label() {
        let d = OpenclawGatewayAdapter::new().descriptor();
        assert_eq!(d.adapter_type, ADAPTER_TYPE);
        assert_eq!(d.label, ADAPTER_LABEL);
    }

    #[test]
    fn default_command_falls_back_to_openclaw_gw() {
        let cfg = json!({});
        assert_eq!(default_command(&cfg), "openclaw-gw");
    }

    #[test]
    fn default_command_overrides_when_config_supplied() {
        let cfg = json!({"command": "custom-gw"});
        assert_eq!(default_command(&cfg), "custom-gw");
    }

    #[test]
    fn default_model_returns_some_when_present() {
        let cfg = json!({"model": "gpt-4"});
        assert_eq!(default_model(&cfg).as_deref(), Some("gpt-4"));
    }

    #[test]
    fn default_model_none_when_absent() {
        let cfg = json!({});
        assert!(default_model(&cfg).is_none());
    }

    #[test]
    fn parse_stdout_extracts_text_from_jsonl() {
        let line = r#"{"text":"hello"}"#;
        assert_eq!(parse_stdout(line).as_deref(), Some("hello"));
    }

    #[test]
    fn parse_stdout_falls_back_to_raw_text() {
        assert_eq!(
            parse_stdout(
                "hi
"
            )
            .as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn parse_stdout_returns_none_for_empty_input() {
        assert!(parse_stdout("").is_none());
        assert!(parse_stdout(
            "


"
        )
        .is_none());
    }

    #[test]
    fn parse_stdout_picks_last_nonempty_line() {
        let s = "first\nignored\n{\"text\":\"second\"}\n{\"text\":\"last\"}\n";
        // We pick from bottom up: last valid json with text → "last"
        assert_eq!(parse_stdout(s).as_deref(), Some("last"));
    }
}

/// Public alias for the V2 adapter that uses a mockable transport.
pub use crate::execute::OpenclawGatewayAdapterV2;

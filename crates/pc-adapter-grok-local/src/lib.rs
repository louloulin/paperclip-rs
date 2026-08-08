#![forbid(unsafe_code)]

//! `grok_local` local CLI adapter: spawns `grok`, parses its JSONL
//! output into the shared `AdapterExecutionResult` shape.

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult,
};
use pc_adapter_process::{execute_process_capture, ProcessSpec};
use serde_json::Value;

pub mod grok_jsonl;
pub mod execute_helpers;

pub use execute_helpers::{
    resolve_grok_billing_type, GrokBillingType,
};

pub use grok_jsonl::{is_grok_unknown_session_error, parse_grok_jsonl, ParsedGrokJsonl};

pub const ADAPTER_TYPE: &str = "grok_local";

fn default_command(config: &Value) -> String {
    config
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("grok")
        .to_owned()
}

fn default_model(config: &Value) -> Option<String> {
    config
        .get("model")
        .and_then(|v| v.as_str())
        .map(String::from)
}

pub fn build_grok_exec_args(config: &Value) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    args.push("--output-format".into());
    args.push("stream-json".into());
    if let Some(m) = default_model(config) {
        args.push("--model".into());
        args.push(m);
    }
    if let Some(cwd) = config.get("cwd").and_then(Value::as_str) {
        args.push("--cwd".into());
        args.push(cwd.to_owned());
    }
    if let Some(extra) = config.get("extraArgs").and_then(Value::as_array) {
        for item in extra.iter().filter_map(|v| v.as_str()) {
            args.push(item.to_owned());
        }
    }
    args
}

pub fn parse_grok_output(stdout: &str) -> Option<String> {
    let parsed = parse_grok_jsonl(stdout);
    if !parsed.summary.is_empty() {
        return Some(parsed.summary);
    }
    let mut summary = None;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(trimmed) else {
            summary = Some(trimmed.to_owned());
            continue;
        };
        for candidate in [
            event.pointer("/message/content/0/text").and_then(Value::as_str),
            event.pointer("/part/text").and_then(Value::as_str),
            event.get("text").and_then(Value::as_str),
            event.get("content").and_then(Value::as_str),
            event.get("result").and_then(Value::as_str),
        ] {
            if let Some(text) = candidate {
                summary = Some(text.to_owned());
                break;
            }
        }
    }
    summary
}

pub struct GrokLocalAdapter;

impl GrokLocalAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for GrokLocalAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for GrokLocalAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin(ADAPTER_TYPE, "Grok CLI")
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        let command = default_command(&context.adapter_config);
        let args = build_grok_exec_args(&context.adapter_config);
        let model = default_model(&context.adapter_config);
        let spec = ProcessSpec::new(&command, &args).with_stdin(context.prompt.clone());
        let execution = execute_process_capture(&spec, &context, events).await?;
        let parsed = parse_grok_jsonl(&execution.stdout);
        let mut result = execution.result;
        let billing_type = crate::execute_helpers::resolve_grok_billing_type(&context.env);
        result.provider = Some(ADAPTER_TYPE.into());
        result.model = model;
        result.billing_type = Some(billing_type.as_str().to_owned());
        result.summary = (!parsed.summary.is_empty()).then_some(parsed.summary);
        result.session_id = parsed.session_id;
        result.error_message = parsed
            .error_message
            .or_else(|| (result.exit_code != Some(0)).then(|| execution.stderr.trim().to_owned()).filter(|s| !s.is_empty()));
        result.result_json = Some(serde_json::json!({
            "thought": parsed.thought,
            "stopReason": parsed.stop_reason,
            "requestId": parsed.request_id,
        }));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_returns_correct_type() {
        let adapter = GrokLocalAdapter::new();
        assert_eq!(adapter.descriptor().adapter_type, ADAPTER_TYPE);
    }

    #[test]
    fn default_command_falls_back_to_builtin() {
        let config = serde_json::json!({});
        assert_eq!(default_command(&config), "grok");
    }

    #[test]
    fn build_args_emits_model_flag() {
        let config = serde_json::json!({"model": "grok-4"});
        let args = build_grok_exec_args(&config);
        assert!(args.contains(&"--model".into()));
        assert!(args.contains(&"grok-4".into()));
        assert!(args.contains(&"--output-format".into()));
        assert!(args.contains(&"stream-json".into()));
    }

    #[test]
    fn build_args_appends_extra_args() {
        let config = serde_json::json!({"extraArgs": ["--yolo", "--no-cache"]});
        let args = build_grok_exec_args(&config);
        assert!(args.contains(&"--yolo".into()));
        assert!(args.contains(&"--no-cache".into()));
    }

    #[test]
    fn parse_output_extracts_first_event() {
        let stdout = r#"{"type":"response","content":"Hi from grok"}"#;
        assert_eq!(parse_grok_output(stdout).as_deref(), Some("Hi from grok"));
    }

    #[test]
    fn parse_output_keeps_last_event() {
        let stdout = r#"{"type":"response","content":"Hi from grok"}
{"type":"response","content":"And more"}"#;
        assert_eq!(parse_grok_output(stdout).as_deref(), Some("And more"));
    }

    #[test]
    fn parse_output_empty_returns_none() {
        assert_eq!(parse_grok_output(""), None);
    }

    #[test]
    fn parse_output_plain_text_fallback() {
        let stdout = "log line 1\nfinal answer\n";
        assert_eq!(parse_grok_output(stdout).as_deref(), Some("final answer"));
    }
}

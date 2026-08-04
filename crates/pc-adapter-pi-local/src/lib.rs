#![forbid(unsafe_code)]

//! `pi_local` local CLI adapter: spawns `pi`, parses its JSONL
//! output into the shared `AdapterExecutionResult` shape.

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult,
};
use pc_adapter_process::{execute_process_capture, ProcessSpec};
use serde_json::Value;

pub const ADAPTER_TYPE: &str = "pi_local";

fn default_command(config: &Value) -> String {
    config
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("pi")
        .to_owned()
}

fn default_model(config: &Value) -> Option<String> {
    config.get("model").and_then(|v| v.as_str()).map(String::from)
}

pub fn build_pi_exec_args(config: &Value) -> Vec<String> {
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

pub fn parse_pi_output(stdout: &str) -> Option<String> {
    let mut summary: Option<String> = None;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => {
                summary = Some(trimmed.to_owned());
                continue;
            }
        };
        if let Some(text) = event.pointer("/message/content/0/text").and_then(Value::as_str) {
            summary = Some(text.to_owned());
        } else if let Some(text) = event.pointer("/part/text").and_then(Value::as_str) {
            summary = Some(text.to_owned());
        } else if let Some(text) = event.get("text").and_then(Value::as_str) {
            summary = Some(text.to_owned());
        } else if let Some(content) = event.get("content").and_then(Value::as_str) {
            summary = Some(content.to_owned());
        } else if let Some(result) = event.get("result").and_then(Value::as_str) {
            summary = Some(result.to_owned());
        }
    }
    summary
}

pub struct PiLocalAdapter;

impl PiLocalAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for PiLocalAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for PiLocalAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin(ADAPTER_TYPE, "Pi Coding Agent")
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        let command = default_command(&context.adapter_config);
        let args = build_pi_exec_args(&context.adapter_config);
        let model = default_model(&context.adapter_config);
        let spec = ProcessSpec::new(&command, &args).with_stdin(context.prompt.clone());
        let execution = execute_process_capture(&spec, &context, events).await?;
        let summary = parse_pi_output(&execution.stdout);
        let mut result = execution.result;
        result.provider = Some(ADAPTER_TYPE.into());
        result.model = model;
        result.summary = summary;
        result.error_message = (result.exit_code != Some(0))
            .then(|| execution.stderr.trim().to_owned())
            .filter(|s| !s.is_empty());
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_returns_correct_type() {
        let adapter = PiLocalAdapter::new();
        assert_eq!(adapter.descriptor().adapter_type, ADAPTER_TYPE);
    }

    #[test]
    fn default_command_falls_back_to_builtin() {
        let config = serde_json::json!({});
        assert_eq!(default_command(&config), "pi");
    }

    #[test]
    fn build_args_emits_model_flag() {
        let config = serde_json::json!({"model": "claude-sonnet-4"});
        let args = build_pi_exec_args(&config);
        assert!(args.contains(&"--model".into()));
        assert!(args.contains(&"claude-sonnet-4".into()));
        assert!(args.contains(&"--output-format".into()));
        assert!(args.contains(&"stream-json".into()));
    }

    #[test]
    fn build_args_appends_extra_args() {
        let config = serde_json::json!({"extraArgs": ["--yolo", "--no-cache"]});
        let args = build_pi_exec_args(&config);
        assert!(args.contains(&"--yolo".into()));
        assert!(args.contains(&"--no-cache".into()));
    }

    #[test]
    fn parse_output_extracts_first_event() {
        let stdout = r#"{"type":"message","role":"assistant","content":"hi from pi"}"#;
        assert_eq!(parse_pi_output(stdout).as_deref(), Some("hi from pi"));
    }

    #[test]
    fn parse_output_keeps_last_event() {
        let stdout = r#"{"type":"message","role":"assistant","content":"hi from pi"}
{"type":"message","role":"assistant","content":"and again"}"#;
        assert_eq!(parse_pi_output(stdout).as_deref(), Some("and again"));
    }

    #[test]
    fn parse_output_empty_returns_none() {
        assert_eq!(parse_pi_output(""), None);
    }

    #[test]
    fn parse_output_plain_text_fallback() {
        let stdout = "log line 1\nfinal answer\n";
        assert_eq!(parse_pi_output(stdout).as_deref(), Some("final answer"));
    }
}

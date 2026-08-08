#![forbid(unsafe_code)]

//! `pi_local` local CLI adapter: spawns `pi`, parses its JSONL
//! output into the shared `AdapterExecutionResult` shape.

pub mod execute_helpers;
pub mod pi_stream_json;
pub mod skills;

pub use execute_helpers::{
    cwds_match, model_id, model_provider, normalize_cwd, parse_session_header_cwd,
    resolve_pi_biller, should_clear_session, should_resume,
};
pub use pi_stream_json::{
    is_pi_unknown_session_error, parse_pi_jsonl, to_usage_summary, ParsedPiOutput, PiToolCall,
    PiUsage,
};

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult, UsageSummary,
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
    config
        .get("model")
        .and_then(|v| v.as_str())
        .map(String::from)
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

/// legacy 解析：仅按字段命中取最后一个值，保留以兼容老 fixture。
///
/// 新的权威解析路径是 `parse_pi_jsonl`（完整 Node parity）。
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
        if let Some(text) = event
            .pointer("/message/content/0/text")
            .and_then(Value::as_str)
        {
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
        // 模型 "provider/model" 拆分：provider 用于 billing 归因。
        let provider = match model.as_deref() {
            Some(m) => crate::execute_helpers::model_provider(Some(m)),
            None => None,
        };
        let spec = ProcessSpec::new(&command, &args).with_stdin(context.prompt.clone());
        let execution = execute_process_capture(&spec, &context, events).await?;
        let parsed = parse_pi_jsonl(&execution.stdout);
        let mut result = execution.result;
        // provider：优先用 model 拆出的 provider，否则用 ADAPTER_TYPE（与 Node 一致）。
        result.provider = Some(provider.clone().unwrap_or_else(|| ADAPTER_TYPE.to_owned()));
        result.model = model;
        result.billing_type = Some("unknown".to_owned());
        result.session_id = parsed.session_id.clone();
        result.cost_usd = parsed.usage.cost_usd.filter(|c| *c > 0.0);
        result.usage = Some(UsageSummary {
            input_tokens: parsed.usage.input_tokens,
            output_tokens: parsed.usage.output_tokens,
            cached_input_tokens: parsed.usage.cached_input_tokens,
        });
        // 错误：parser 内部 errors 优先；非零退出且 stderr 非空时也写入。
        let parser_error = (!parsed.errors.is_empty()).then(|| parsed.errors.join("\n"));
        let stderr_error = (result.exit_code != Some(0))
            .then(|| execution.stderr.trim().to_owned())
            .filter(|s| !s.is_empty());
        result.error_message = parser_error.or(stderr_error);
        // summary：final_message 优先，否则拼接 messages。
        let summary_text = parsed
            .final_message
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| parsed.messages.join("\n"));
        result.summary = (!summary_text.is_empty()).then_some(summary_text);
        result.result_json = Some(serde_json::json!({
            "toolCalls": parsed.tool_calls,
            "messages": parsed.messages,
            "errors": parsed.errors,
            "biller": crate::execute_helpers::resolve_pi_biller(&context.env, provider.as_deref()),
        }));
        // clear_session：parser 报错且 is_pi_unknown_session_error 命中时触发。
        result.clear_session = crate::execute_helpers::should_clear_session(
            &execution.stdout,
            &execution.stderr,
        );
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

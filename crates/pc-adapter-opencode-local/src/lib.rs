#![forbid(unsafe_code)]

//! `opencode_local` local CLI adapter: spawns `opencode`, parses its JSONL
//! output into the shared `AdapterExecutionResult` shape.

pub mod execute_helpers;
pub mod skills;

pub use execute_helpers::{claude_skills_home, resolve_opencode_biller};
pub mod opencode_models;
pub mod opencode_stream_json;

pub use opencode_stream_json::{
    is_opencode_unknown_session_error, parse_opencode_stream_json, ParsedOpenCodeStreamJson,
};

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult,
};
use pc_adapter_process::{execute_process_capture, ProcessSpec};
use serde_json::Value;

pub const ADAPTER_TYPE: &str = "opencode_local";

fn default_command(config: &Value) -> String {
    config
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("opencode")
        .to_owned()
}

fn default_model(config: &Value) -> Option<String> {
    config
        .get("model")
        .and_then(|v| v.as_str())
        .map(String::from)
}

pub fn build_opencode_exec_args(config: &Value, resume_session_id: Option<&str>) -> Vec<String> {
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
    if let Some(sid) = resume_session_id.map(str::trim).filter(|s| !s.is_empty()) {
        args.push("--session".into());
        args.push(sid.to_owned());
    }
    args
}

pub fn parse_opencode_output(stdout: &str) -> Option<String> {
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

pub struct OpencodeLocalAdapter;

impl OpencodeLocalAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpencodeLocalAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for OpencodeLocalAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin(ADAPTER_TYPE, "OpenCode CLI")
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        let command = default_command(&context.adapter_config);
        let model = default_model(&context.adapter_config);
        let initial_args =
            build_opencode_exec_args(&context.adapter_config, context.session_id.as_deref());
        let initial_spec =
            ProcessSpec::new(&command, &initial_args).with_stdin(context.prompt.clone());
        let initial_execution =
            execute_process_capture(&initial_spec, &context, events.clone()).await?;
        let initial_parsed = parse_opencode_stream_json(&initial_execution.stdout);

        // 真实重跑：unknown session + 有 resume → 重新构造 args（去掉 --session）。
        let mut retried_after_unknown_session = false;
        let mut clear_session_on_retry = false;
        let mut active_execution = initial_execution;
        let mut active_parsed = initial_parsed;
        if let Some(sid) = context
            .session_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            let initial_failed = !active_execution.result.timed_out
                && (active_execution.result.exit_code.unwrap_or(0) != 0
                    || active_parsed.error_message.is_some());
            if initial_failed
                && is_opencode_unknown_session_error(
                    &active_execution.stdout,
                    &active_execution.stderr,
                )
            {
                let _ = events
                    .clone()
                    .emit(pc_adapter_api::AdapterEvent::stdout(format!(
                        "[paperclip] OpenCode session \"{sid}\" is unavailable; retrying with a fresh session.\n"
                    )))
                    .await;
                let retry_args = build_opencode_exec_args(&context.adapter_config, None);
                let retry_spec =
                    ProcessSpec::new(&command, &retry_args).with_stdin(context.prompt.clone());
                let (retry_sink, _rx) = pc_adapter_api::AdapterEventSink::channel(8);
                let retry_execution =
                    execute_process_capture(&retry_spec, &context, retry_sink).await?;
                let retry_parsed = parse_opencode_stream_json(&retry_execution.stdout);
                active_execution = retry_execution;
                active_parsed = retry_parsed;
                retried_after_unknown_session = true;
                clear_session_on_retry = true;
            }
        }

        let execution = active_execution;
        let parsed = active_parsed;
        let mut result = execution.result;
        let provider = pc_acpx::model_id::parse_model_provider(model.as_deref());
        result.provider = Some(provider.clone().unwrap_or_else(|| ADAPTER_TYPE.to_owned()));
        result.model = model;
        result.billing_type = Some("unknown".to_owned());
        result.summary = (!parsed.summary.is_empty()).then_some(parsed.summary);
        let resolved_session_id = parsed.session_id.clone();
        result.session_id = parsed.session_id;
        result.cost_usd = parsed.cost_usd;
        result.usage = Some(parsed.usage.clone());
        result.error_message = parsed.error_message.or_else(|| {
            (result.exit_code != Some(0))
                .then(|| execution.stderr.trim().to_owned())
                .filter(|s| !s.is_empty())
        });
        let paperclip_env_note =
            pc_acpx::session_config_options::render_paperclip_env_note(&context.env);
        let api_access_note = pc_acpx::session_config_options::render_api_access_note(&context.env);
        result.result_json = Some(serde_json::json!({
            "toolErrors": parsed.tool_errors,
            "biller": crate::execute_helpers::resolve_opencode_biller(&context.env, provider.as_deref()),
            "paperclipEnvNote": paperclip_env_note,
            "apiAccessNote": api_access_note,
            "retriedAfterUnknownSession": retried_after_unknown_session,
        }));
        if clear_session_on_retry && resolved_session_id.is_none() {
            result.clear_session = true;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_returns_correct_type() {
        let adapter = OpencodeLocalAdapter::new();
        assert_eq!(adapter.descriptor().adapter_type, ADAPTER_TYPE);
    }

    #[test]
    fn default_command_falls_back_to_builtin() {
        let config = serde_json::json!({});
        assert_eq!(default_command(&config), "opencode");
    }

    #[test]
    fn build_args_emits_model_flag() {
        let config = serde_json::json!({"model": "anthropic/claude-sonnet-4"});
        let args = build_opencode_exec_args(&config, None);
        assert!(args.contains(&"--model".into()));
        assert!(args.contains(&"anthropic/claude-sonnet-4".into()));
        assert!(args.contains(&"--output-format".into()));
        assert!(args.contains(&"stream-json".into()));
    }

    #[test]
    fn build_args_appends_extra_args() {
        let config = serde_json::json!({"extraArgs": ["--yolo", "--no-cache"]});
        let args = build_opencode_exec_args(&config, None);
        assert!(args.contains(&"--yolo".into()));
        assert!(args.contains(&"--no-cache".into()));
    }

    #[test]
    fn parse_output_extracts_first_event() {
        let stdout = r#"{"type":"text","part":{"text":"hello"}}"#;
        assert_eq!(parse_opencode_output(stdout).as_deref(), Some("hello"));
    }

    #[test]
    fn parse_output_keeps_last_event() {
        let stdout = r#"{"type":"text","part":{"text":"hello"}}
{"type":"text","part":{"text":"updated"}}"#;
        assert_eq!(parse_opencode_output(stdout).as_deref(), Some("updated"));
    }

    #[test]
    fn parse_output_empty_returns_none() {
        assert_eq!(parse_opencode_output(""), None);
    }

    #[test]
    fn parse_output_plain_text_fallback() {
        let stdout = "log line 1\nfinal answer\n";
        assert_eq!(
            parse_opencode_output(stdout).as_deref(),
            Some("final answer")
        );
    }
}

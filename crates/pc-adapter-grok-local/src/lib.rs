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

pub mod execute_helpers;
pub mod grok_jsonl;
pub mod grok_test;
pub mod skills;

pub use execute_helpers::{resolve_grok_billing_type, GrokBillingType};

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

pub fn build_grok_exec_args(config: &Value, resume_session_id: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    // Output format (always stream-json for adapter)
    args.push("--output-format".into());
    args.push("stream-json".into());

    // Model selection
    if let Some(m) = default_model(config) {
        args.push("--model".into());
        args.push(m);
    }

    // Temperature (R870)
    if let Some(t) = config.get("temperature").and_then(Value::as_f64) {
        args.push("--temperature".into());
        args.push(t.to_string());
    }

    // Max tokens (R870)
    if let Some(m) = config.get("maxTokens").and_then(Value::as_u64) {
        args.push("--max-tokens".into());
        args.push(m.to_string());
    }

    // Sandbox mode (R870)
    if let Some(sandbox) = config.get("sandbox").and_then(Value::as_bool) {
        if sandbox {
            args.push("--sandbox".into());
        }
    }

    // Workspace / cwd
    if let Some(cwd) = config.get("cwd").and_then(Value::as_str) {
        args.push("--cwd".into());
        args.push(cwd.to_owned());
    }

    // System prompt (R870)
    if let Some(sp) = config.get("systemPrompt").and_then(Value::as_str) {
        if !sp.is_empty() {
            args.push("--system-prompt".into());
            args.push(sp.to_owned());
        }
    }

    // Append system prompt file (R870)
    if let Some(path) = config.get("appendSystemPromptFile").and_then(Value::as_str) {
        args.push("--append-system-prompt-file".into());
        args.push(path.to_owned());
    }

    // Effort level (R870) — grok-specific
    if let Some(effort) = config.get("effort").and_then(Value::as_str) {
        args.push("--effort".into());
        args.push(effort.to_owned());
    }

    // Extra args (user-supplied, applied last so they can override defaults)
    if let Some(extra) = config.get("extraArgs").and_then(Value::as_array) {
        for item in extra.iter().filter_map(|v| v.as_str()) {
            args.push(item.to_owned());
        }
    }

    // Session resume (always last — it depends on the above args being valid)
    if let Some(sid) = resume_session_id.map(str::trim).filter(|s| !s.is_empty()) {
        args.push("--resume".into());
        args.push(sid.to_owned());
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
            event
                .pointer("/message/content/0/text")
                .and_then(Value::as_str),
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
        let initial_args =
            build_grok_exec_args(&context.adapter_config, context.session_id.as_deref());
        let model = default_model(&context.adapter_config);
        let initial_spec =
            ProcessSpec::new(&command, &initial_args).with_stdin(context.prompt.clone());
        let initial_execution =
            execute_process_capture(&initial_spec, &context, events.clone()).await?;
        let initial_parsed = parse_grok_jsonl(&initial_execution.stdout);

        // 真实重跑：unknown session + 有 resume → 重新构造 args（去掉 --resume）。
        let mut retried_after_unknown_session = false;
        let mut clear_session_on_retry = false;
        let mut active_execution = initial_execution;
        let mut active_parsed = initial_parsed;
        if let Some(sid) = context
            .session_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            if !active_execution.result.timed_out
                && active_execution.result.exit_code.unwrap_or(0) != 0
                && is_grok_unknown_session_error(&active_execution.stdout, &active_execution.stderr)
            {
                let _ = events
                    .clone()
                    .emit(pc_adapter_api::AdapterEvent::stdout(format!(
                        "[paperclip] Grok resume session \"{sid}\" is unavailable; retrying with a fresh session.\n"
                    )))
                    .await;
                let retry_args = build_grok_exec_args(&context.adapter_config, None);
                let retry_spec =
                    ProcessSpec::new(&command, &retry_args).with_stdin(context.prompt.clone());
                let (retry_sink, _rx) = pc_adapter_api::AdapterEventSink::channel(8);
                let retry_execution =
                    execute_process_capture(&retry_spec, &context, retry_sink).await?;
                let retry_parsed = parse_grok_jsonl(&retry_execution.stdout);
                active_execution = retry_execution;
                active_parsed = retry_parsed;
                retried_after_unknown_session = true;
                clear_session_on_retry = true;
            }
        }

        let execution = active_execution;
        let parsed = active_parsed;
        let mut result = execution.result;
        let billing_type = crate::execute_helpers::resolve_grok_billing_type(&context.env);
        result.provider = Some(ADAPTER_TYPE.into());
        result.model = model;
        result.billing_type = Some(billing_type.as_str().to_owned());
        result.summary = (!parsed.summary.is_empty()).then_some(parsed.summary);
        let resolved_session_id = parsed.session_id.clone();
        result.session_id = parsed.session_id;
        result.error_message = parsed.error_message.or_else(|| {
            (result.exit_code != Some(0))
                .then(|| execution.stderr.trim().to_owned())
                .filter(|s| !s.is_empty())
        });
        let paperclip_env_note =
            pc_acpx::session_config_options::render_paperclip_env_note(&context.env);
        let api_access_note = pc_acpx::session_config_options::render_api_access_note(&context.env);
        result.result_json = Some(serde_json::json!({
            "thought": parsed.thought,
            "stopReason": parsed.stop_reason,
            "requestId": parsed.request_id,
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
mod r870_cli_args;

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
        let args = build_grok_exec_args(&config, None);
        assert!(args.contains(&"--model".into()));
        assert!(args.contains(&"grok-4".into()));
        assert!(args.contains(&"--output-format".into()));
        assert!(args.contains(&"stream-json".into()));
    }

    #[test]
    fn build_args_appends_extra_args() {
        let config = serde_json::json!({"extraArgs": ["--yolo", "--no-cache"]});
        let args = build_grok_exec_args(&config, None);
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

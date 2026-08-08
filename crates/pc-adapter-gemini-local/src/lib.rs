#![forbid(unsafe_code)]

//! `gemini_local` local CLI adapter: spawns `gemini`, parses its JSONL
//! output into the shared `AdapterExecutionResult` shape.

pub mod skills;
pub mod gemini_stream_json;
pub mod execute_helpers;

pub use execute_helpers::{
    build_gemini_headless_env, gemini_skills_home, render_api_access_note,
    render_paperclip_env_note, resolve_gemini_billing_type, GeminiBillingType,
};

pub use gemini_stream_json::{
    detect_gemini_auth_required, detect_gemini_quota_exhausted,
    describe_gemini_failure, is_gemini_session_unrecoverable_error,
    is_gemini_transient_network_error, is_gemini_turn_limit_result,
    parse_gemini_stream_json, GeminiQuestion, GeminiQuestionChoice, ParsedGeminiStreamJson,
};

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult,
};
use pc_adapter_process::{execute_process_capture, ProcessSpec};
use serde_json::Value;

pub const ADAPTER_TYPE: &str = "gemini_local";

fn default_command(config: &Value) -> String {
    config
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("gemini")
        .to_owned()
}

fn default_model(config: &Value) -> Option<String> {
    config
        .get("model")
        .and_then(|v| v.as_str())
        .map(String::from)
}

pub fn build_gemini_exec_args(config: &Value, resume_session_id: Option<&str>) -> Vec<String> {
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
        args.push("--resume".into());
        args.push(sid.to_owned());
    }
    args
}

pub fn parse_gemini_output(stdout: &str) -> Option<String> {
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

pub struct GeminiLocalAdapter;

impl GeminiLocalAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for GeminiLocalAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for GeminiLocalAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin(ADAPTER_TYPE, "Gemini CLI")
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        let command = default_command(&context.adapter_config);
        let model = default_model(&context.adapter_config);
        let initial_args =
            build_gemini_exec_args(&context.adapter_config, context.session_id.as_deref());
        let initial_spec = ProcessSpec::new(&command, &initial_args)
            .with_stdin(context.prompt.clone());
        let initial_execution =
            execute_process_capture(&initial_spec, &context, events.clone()).await?;
        let initial_parsed = parse_gemini_stream_json(&initial_execution.stdout);

        // 真实重跑：unknown session + 有 resume → 重新构造 args（去掉 --resume）。
        let mut retried_after_unknown_session = false;
        let mut clear_session_on_retry = false;
        let mut active_execution = initial_execution;
        let mut active_parsed = initial_parsed;
        if let Some(sid) = context.session_id.as_deref().filter(|s| !s.trim().is_empty()) {
            if !active_execution.result.timed_out
                && active_execution.result.exit_code.unwrap_or(0) != 0
                && is_gemini_session_unrecoverable_error(
                    &active_execution.stdout,
                    &active_execution.stderr,
                )
            {
                let _ = events
                    .clone()
                    .emit(pc_adapter_api::AdapterEvent::stdout(format!(
                        "[paperclip] Gemini resume session \"{sid}\" is unavailable; retrying with a fresh session.\n"
                    )))
                    .await;
                let retry_args = build_gemini_exec_args(&context.adapter_config, None);
                let retry_spec = ProcessSpec::new(&command, &retry_args)
                    .with_stdin(context.prompt.clone());
                let (retry_sink, _rx) = pc_adapter_api::AdapterEventSink::channel(8);
                let retry_execution =
                    execute_process_capture(&retry_spec, &context, retry_sink).await?;
                let retry_parsed = parse_gemini_stream_json(&retry_execution.stdout);
                active_execution = retry_execution;
                active_parsed = retry_parsed;
                retried_after_unknown_session = true;
                clear_session_on_retry = true;
            }
        }

        let execution = active_execution;
        let parsed = active_parsed;
        let mut result = execution.result;
        let billing_type = crate::execute_helpers::resolve_gemini_billing_type(&context.env);
        result.provider = Some(ADAPTER_TYPE.into());
        result.model = model;
        result.billing_type = Some(billing_type.as_str().to_owned());
        result.summary = (!parsed.summary.is_empty()).then_some(parsed.summary);
        result.usage = Some(parsed.usage);
        result.error_message = parsed.error_message.or_else(|| (result.exit_code != Some(0))
            .then(|| execution.stderr.trim().to_owned())
            .filter(|s| !s.is_empty()));
        result.session_id = parsed.session_id;
        result.cost_usd = parsed.cost_usd;
        let mut result_json = parsed.result_json.unwrap_or_else(|| serde_json::json!({}));
        if let Value::Object(ref mut map) = result_json {
            map.insert(
                "paperclipEnvNote".to_owned(),
                Value::String(crate::execute_helpers::render_paperclip_env_note(&context.env)),
            );
            map.insert(
                "apiAccessNote".to_owned(),
                Value::String(crate::execute_helpers::render_api_access_note(&context.env)),
            );
            map.insert(
                "retriedAfterUnknownSession".to_owned(),
                Value::Bool(retried_after_unknown_session),
            );
        }
        result.result_json = Some(result_json);
        if clear_session_on_retry && result.session_id.is_none() {
            result.clear_session = true;
        }
        Ok(result)
    }}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_returns_correct_type() {
        let adapter = GeminiLocalAdapter::new();
        assert_eq!(adapter.descriptor().adapter_type, ADAPTER_TYPE);
    }

    #[test]
    fn default_command_falls_back_to_builtin() {
        let config = serde_json::json!({});
        assert_eq!(default_command(&config), "gemini");
    }

    #[test]
    fn build_args_emits_model_flag() {
        let config = serde_json::json!({"model": "gemini-2.5-pro"});
        let args = build_gemini_exec_args(&config, None);
        assert!(args.contains(&"--model".into()));
        assert!(args.contains(&"gemini-2.5-pro".into()));
        assert!(args.contains(&"--output-format".into()));
        assert!(args.contains(&"stream-json".into()));
    }

    #[test]
    fn build_args_appends_extra_args() {
        let config = serde_json::json!({"extraArgs": ["--yolo", "--no-cache"]});
        let args = build_gemini_exec_args(&config, None);
        assert!(args.contains(&"--yolo".into()));
        assert!(args.contains(&"--no-cache".into()));
    }

    #[test]
    fn parse_output_extracts_first_event() {
        let stdout = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hi"}]}}"#;
        assert_eq!(parse_gemini_output(stdout).as_deref(), Some("Hi"));
    }

    #[test]
    fn parse_output_keeps_last_event() {
        let stdout = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hi"}]}}
{"type":"result","subtype":"success","is_error":false,"session_id":"s1","result":"Followup"}"#;
        assert_eq!(parse_gemini_output(stdout).as_deref(), Some("Followup"));
    }

    #[test]
    fn parse_output_empty_returns_none() {
        assert_eq!(parse_gemini_output(""), None);
    }

    #[test]
    fn parse_output_plain_text_fallback() {
        let stdout = "log line 1\nfinal answer\n";
        assert_eq!(parse_gemini_output(stdout).as_deref(), Some("final answer"));
    }
}

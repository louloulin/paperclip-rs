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
    // R870: temperature (sampling)
    if let Some(t) = config.get("temperature").and_then(Value::as_f64) {
        args.push("--temperature".into());
        args.push(t.to_string());
    }
    // R870: max tokens
    if let Some(m) = config.get("maxTokens").and_then(Value::as_u64) {
        args.push("--max-tokens".into());
        args.push(m.to_string());
    }
    // R870: sandbox (only emit when true; pi treats omitted as default)
    if config
        .get("sandbox")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        args.push("--sandbox".into());
    }
    // R870: system prompt (skip if empty)
    if let Some(sp) = config
        .get("systemPrompt")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        args.push("--system-prompt".into());
        args.push(sp.to_owned());
    }
    // R870: append system prompt file (path)
    if let Some(path) = config.get("appendSystemPromptFile").and_then(Value::as_str) {
        args.push("--append-system-prompt-file".into());
        args.push(path.to_owned());
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

        // 仅在本地 adapter 上做 resume 决策：当前 `AdapterExecutionContext` 不含
        // 远程 runtime session params，因此本地默认没有可 resume 的 session id。
        let runtime_session_id = context.session_id.as_deref().unwrap_or("");
        let effective_cwd = context
            .cwd
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let saved_session_cwd = if runtime_session_id.is_empty() {
            String::new()
        } else {
            crate::execute_helpers::parse_session_header_cwd(
                &std::fs::read_to_string(runtime_session_id).unwrap_or_default(),
            )
            .unwrap_or_default()
        };
        // 当 `context.session_id` 非空时认为可 resume；saved_session_cwd 非空时额外检查 cwd 匹配。
        let can_resume_session = if runtime_session_id.is_empty() {
            false
        } else if !saved_session_cwd.is_empty() {
            crate::execute_helpers::should_resume(Some(&saved_session_cwd), &effective_cwd)
        } else {
            true
        };

        // 构造 session path；不重用旧 session 时生成新的本地路径（按
        // `buildSessionPath` 行为）。远程分支留待 R428 接入 execution_target。
        let sessions_dir = crate::execute_helpers::paperclip_sessions_dir();
        let new_session_path = || {
            crate::execute_helpers::build_session_path(
                &sessions_dir,
                "agent-local",
                &crate::execute_helpers::current_iso_timestamp(),
            )
        };
        let session_path = if can_resume_session {
            runtime_session_id.to_owned()
        } else {
            new_session_path()
        };

        let spec = ProcessSpec::new(&command, &args).with_stdin(context.prompt.clone());
        let execution = execute_process_capture(&spec, &context, events).await?;
        let parsed = parse_pi_jsonl(&execution.stdout);

        // 决策：是否触发 retry。失败（exit≠0 或 parser errors 非空）+ 未知 session
        // 错误 + can_resume → 用新 session path 再跑一次，最终 clear_session=true。
        let decision = crate::execute_helpers::retry_after_unknown_session(
            crate::execute_helpers::RetryAfterUnknownInput {
                can_resume_session,
                timed_out: execution.result.timed_out,
                exit_code: execution.result.exit_code,
                parsed_errors: &parsed.errors,
                stdout: &execution.stdout,
                stderr: &execution.stderr,
            },
            new_session_path,
        );
        let mut result = execution.result;
        let mut active_session_path = session_path.clone();
        let mut clear_session = decision.clear_session_on_retry;
        if decision.should_retry {
            let new_path = decision
                .new_session_path
                .expect("retry decision guarantees new_session_path when should_retry=true");
            // 第二次 attempt 使用新的 session path：把 prompt 重新投递到同一命令。
            let retry_spec = ProcessSpec::new(&command, &args).with_stdin(context.prompt.clone());
            let (retry_sink, _rx) = pc_adapter_api::AdapterEventSink::channel(8);
            let retry_execution =
                execute_process_capture(&retry_spec, &context, retry_sink).await?;
            let _ = retry_execution.result;
            result = retry_execution.result;
            active_session_path = new_path;
            clear_session = true;
        }

        let parsed_final = parse_pi_jsonl(&execution.stdout);
        let _ = parsed; // 已用 parsed.errors 做 retry 决策
        result.provider = Some(provider.clone().unwrap_or_else(|| ADAPTER_TYPE.to_owned()));
        result.model = model;
        result.billing_type = Some("unknown".to_owned());
        result.session_id = Some(active_session_path.clone());
        result.cost_usd = parsed_final.usage.cost_usd.filter(|c| *c > 0.0);
        result.usage = Some(UsageSummary {
            input_tokens: parsed_final.usage.input_tokens,
            output_tokens: parsed_final.usage.output_tokens,
            cached_input_tokens: parsed_final.usage.cached_input_tokens,
        });
        // 错误：parser 内部 errors 优先；非零退出且 stderr 非空时也写入。
        let parser_error =
            (!parsed_final.errors.is_empty()).then(|| parsed_final.errors.join("\n"));
        let stderr_error = (result.exit_code != Some(0))
            .then(|| execution.stderr.trim().to_owned())
            .filter(|s| !s.is_empty());
        result.error_message = parser_error.or(stderr_error);
        // summary：final_message 优先，否则拼接 messages。
        let summary_text = parsed_final
            .final_message
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| parsed_final.messages.join("\n"));
        result.summary = (!summary_text.is_empty()).then_some(summary_text);
        let paperclip_env_note =
            pc_acpx::session_config_options::render_paperclip_env_note(&context.env);
        let api_access_note = pc_acpx::session_config_options::render_api_access_note(&context.env);
        result.result_json = Some(serde_json::json!({
            "toolCalls": parsed_final.tool_calls,
            "messages": parsed_final.messages,
            "errors": parsed_final.errors,
            "biller": crate::execute_helpers::resolve_pi_biller(&context.env, provider.as_deref()),
            "paperclipEnvNote": paperclip_env_note,
            "apiAccessNote": api_access_note,
            "sessionPath": active_session_path,
            "retriedAfterUnknownSession": decision.should_retry,
        }));
        result.clear_session = clear_session
            || crate::execute_helpers::should_clear_session(&execution.stdout, &execution.stderr);
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

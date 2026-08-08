#![forbid(unsafe_code)]

pub mod skills;
pub mod codex_errors;
pub mod execute_helpers;

pub use execute_helpers::{
    fallback_mode_uses_fresh_session, fallback_mode_uses_safer_invocation,
    read_codex_transient_fallback_mode, resolve_codex_biller, resolve_codex_billing_type,
    resolve_codex_skills_dir, CodexBillingType, CodexTransientFallbackMode,
};

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult, UsageSummary,
};
use pc_adapter_process::{execute_process_capture, ProcessSpec};
use serde_json::Value;

pub const ADAPTER_TYPE: &str = "codex_local";
pub const DEFAULT_MODEL: &str = "gpt-5.6-sol";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexExecArgs {
    pub args: Vec<String>,
    pub model: String,
    pub fast_mode_requested: bool,
    pub fast_mode_applied: bool,
}

pub fn build_codex_exec_args(
    config: &Value,
    resume_session_id: Option<&str>,
    skip_git_repo_check: bool,
) -> CodexExecArgs {
    let model = normalize_model(string(config, "model"));
    let reasoning = string(config, "modelReasoningEffort")
        .or_else(|| string(config, "reasoningEffort"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let search = boolean(config, "search");
    let fast_mode_requested = boolean(config, "fastMode");
    let fast_mode_applied = fast_mode_requested && supports_fast_mode(&model);
    let bypass = boolean(config, "dangerouslyBypassApprovalsAndSandbox")
        || boolean(config, "dangerouslyBypassSandbox");
    let extra_args = string_array(config, "extraArgs")
        .or_else(|| string_array(config, "args"))
        .unwrap_or_default();

    let mut args = vec!["exec".into(), "--json".into()];
    if skip_git_repo_check && !extra_args.iter().any(|arg| arg == "--skip-git-repo-check") {
        args.push("--skip-git-repo-check".into());
    }
    if search {
        args.insert(0, "--search".into());
    }
    if bypass {
        args.push("--dangerously-bypass-approvals-and-sandbox".into());
    }
    if !model.is_empty() {
        args.extend(["--model".into(), model.clone()]);
    }
    if !reasoning.is_empty() {
        args.extend([
            "-c".into(),
            format!(
                "model_reasoning_effort={}",
                serde_json::to_string(&reasoning).unwrap()
            ),
        ]);
    }
    if fast_mode_applied {
        args.extend([
            "-c".into(),
            "service_tier=\"fast\"".into(),
            "-c".into(),
            "features.fast_mode=true".into(),
        ]);
    }
    args.extend(extra_args);
    if let Some(session_id) = resume_session_id.filter(|value| !value.trim().is_empty()) {
        args.extend(["resume".into(), session_id.into(), "-".into()]);
    } else {
        args.push("-".into());
    }
    CodexExecArgs {
        args,
        model,
        fast_mode_requested,
        fast_mode_applied,
    }
}

fn normalize_model(model: Option<&str>) -> String {
    match model.unwrap_or_default().trim() {
        "gpt-5.6" => DEFAULT_MODEL.into(),
        model => model.into(),
    }
}

fn supports_fast_mode(model: &str) -> bool {
    const KNOWN: &[&str] = &[
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-5.5",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5",
        "o3",
        "o4-mini",
        "gpt-5-mini",
        "gpt-5-nano",
        "o3-mini",
        "codex-mini-latest",
    ];
    const FAST: &[&str] = &[
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-5.5",
        "gpt-5.4",
    ];
    model.is_empty() || FAST.contains(&model) || !KNOWN.contains(&model)
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn boolean(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn string_array(value: &Value, key: &str) -> Option<Vec<String>> {
    Some(
        value
            .get(key)?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodexUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedCodexOutput {
    pub session_id: Option<String>,
    pub summary: String,
    pub usage: CodexUsage,
    pub error_message: Option<String>,
    pub saw_protocol_event: bool,
    pub saw_protocol_terminal_event: bool,
}

pub fn parse_codex_jsonl(stdout: &str) -> ParsedCodexOutput {
    let mut parsed = ParsedCodexOutput::default();
    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !event_type.is_empty() {
            parsed.saw_protocol_event = true;
        }
        if matches!(event_type, "error" | "turn.completed" | "turn.failed") {
            parsed.saw_protocol_terminal_event = true;
        }
        match event_type {
            "thread.started" => {
                if let Some(thread_id) = event.get("thread_id").and_then(Value::as_str) {
                    parsed.session_id = Some(thread_id.into());
                }
            }
            "error" => {
                parsed.error_message = event
                    .get("message")
                    .and_then(Value::as_str)
                    .filter(|message| !message.trim().is_empty())
                    .map(str::to_owned);
            }
            "item.completed" => {
                let item = event.get("item").unwrap_or(&Value::Null);
                if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        parsed.summary = text.trim().into();
                    }
                }
            }
            "turn.completed" => {
                let usage = event.get("usage").unwrap_or(&Value::Null);
                parsed.usage.input_tokens = number(usage, "input_tokens");
                parsed.usage.cached_input_tokens = number(usage, "cached_input_tokens");
                parsed.usage.output_tokens = number(usage, "output_tokens");
            }
            "turn.failed" => {
                parsed.error_message = event
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .filter(|message| !message.trim().is_empty())
                    .map(str::to_owned);
            }
            _ => {}
        }
    }
    parsed
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

#[derive(Debug, Default)]
pub struct CodexLocalAdapter;

impl CodexLocalAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Adapter for CodexLocalAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        let mut descriptor = AdapterDescriptor::builtin(ADAPTER_TYPE, "Codex");
        descriptor.supports_local_agent_jwt = true;
        descriptor.supports_instructions_bundle = true;
        descriptor
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        let command = string(&context.adapter_config, "command").unwrap_or("codex");
        // 首轮 attempt：若 `context.session_id` 非空，传 `resume <sid>`，与 Node
        // `buildArgs(resumeSessionId)` 行为一致。
        let initial_built = build_codex_exec_args(
            &context.adapter_config,
            context.session_id.as_deref(),
            false,
        );
        let initial_spec = ProcessSpec::new(command, &initial_built.args)
            .with_stdin(context.prompt.clone());
        let initial_execution = execute_process_capture(&initial_spec, &context, events.clone()).await?;
        let initial_parsed = parse_codex_jsonl(&initial_execution.stdout);

        // 决策：unknown session + 有 resume id → 真实重跑一轮（不带 resume）。
        let mut retried_after_unknown_session = false;
        let mut clear_session_on_retry = false;
        let mut active_execution = initial_execution;
        let mut active_parsed = initial_parsed;
        let mut active_built = initial_built;
        if let Some(sid) = context.session_id.as_deref().filter(|s| !s.trim().is_empty()) {
            if !active_execution.result.timed_out
                && active_execution.result.exit_code.unwrap_or(0) != 0
                && crate::codex_errors::is_codex_unknown_session_error(
                    &active_execution.stdout,
                    &active_execution.stderr,
                )
            {
                let _ = events
                    .clone()
                    .emit(pc_adapter_api::AdapterEvent::stdout(format!(
                        "[paperclip] Codex resume session \"{sid}\" is unavailable; retrying with a fresh session.\n"
                    )))
                    .await;
                let retry_built = build_codex_exec_args(&context.adapter_config, None, false);
                let retry_spec = ProcessSpec::new(command, &retry_built.args)
                    .with_stdin(context.prompt.clone());
                let (retry_sink, _rx) = pc_adapter_api::AdapterEventSink::channel(8);
                let retry_execution =
                    execute_process_capture(&retry_spec, &context, retry_sink).await?;
                let retry_parsed = parse_codex_jsonl(&retry_execution.stdout);
                active_execution = retry_execution;
                active_parsed = retry_parsed;
                active_built = retry_built;
                retried_after_unknown_session = true;
                clear_session_on_retry = true;
            }
        }

        let execution = active_execution;
        let parsed = active_parsed;
        let built = active_built;
        let mut result = execution.result;
        result.session_id = parsed.session_id;
        let billing_type = crate::execute_helpers::resolve_codex_billing_type(&context.env);
        result.provider = Some("openai".into());
        result.billing_type = Some(billing_type.as_str().to_owned());
        result.model = (!built.model.is_empty()).then_some(built.model);
        result.summary = (!parsed.summary.is_empty()).then_some(parsed.summary);
        result.usage = Some(UsageSummary {
            input_tokens: parsed.usage.input_tokens,
            output_tokens: parsed.usage.output_tokens,
            cached_input_tokens: Some(parsed.usage.cached_input_tokens),
        });
        result.error_message = parsed
            .error_message
            .or_else(|| {
                (result.exit_code != Some(0))
                    .then(|| execution.stderr.trim().to_owned())
                    .filter(|s| !s.is_empty())
            });

        // 错误族 + transient fallback 决策：覆盖首轮与重试后最终结果。
        let decision = crate::execute_helpers::decide_codex_retry(
            crate::execute_helpers::CodexRetryInput {
                session_id: context.session_id.as_deref().unwrap_or(""),
                timed_out: result.timed_out,
                exit_code: result.exit_code,
                stdout: &execution.stdout,
                stderr: &execution.stderr,
                error_message: result.error_message.as_deref(),
                saw_protocol_event: parsed.saw_protocol_event,
                saw_protocol_terminal_event: parsed.saw_protocol_terminal_event,
                now: std::time::SystemTime::now(),
            },
        );
        if clear_session_on_retry || decision.clear_session {
            result.clear_session = true;
        }
        let transient_fallback_mode = decision
            .transient_fallback_mode
            .map(|mode| mode.as_str().to_owned());

        let paperclip_env_note =
            pc_acpx::session_config_options::render_paperclip_env_note(&context.env);
        let api_access_note =
            pc_acpx::session_config_options::render_api_access_note(&context.env);
        let mut result_json = serde_json::json!({
            "sawProtocolEvent": parsed.saw_protocol_event,
            "sawProtocolTerminalEvent": parsed.saw_protocol_terminal_event,
            "fastModeRequested": built.fast_mode_requested,
            "fastModeApplied": built.fast_mode_applied,
            "biller": crate::execute_helpers::resolve_codex_biller(&context.env, billing_type),
            "paperclipEnvNote": paperclip_env_note,
            "apiAccessNote": api_access_note,
            "errorFamily": decision.error_family.as_str(),
            "retriedAfterUnknownSession": retried_after_unknown_session,
        });
        if let Some(mode) = transient_fallback_mode {
            result_json["transientFallbackMode"] = serde_json::Value::String(mode);
        }
        result.result_json = Some(result_json);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use pc_adapter_api::{Adapter, AdapterEventSink, AdapterExecutionContext};

    #[test]
    fn builds_codex_exec_args_with_alias_fast_mode_and_resume() {
        let config = serde_json::json!({
            "model": "gpt-5.6",
            "fastMode": true,
            "search": true,
            "modelReasoningEffort": "high",
            "dangerouslyBypassApprovalsAndSandbox": true,
            "extraArgs": ["--ephemeral"]
        });

        let result = build_codex_exec_args(&config, Some("thread_123"), false);

        assert_eq!(result.model, "gpt-5.6-sol");
        assert_eq!(
            result.args,
            vec![
                "--search",
                "exec",
                "--json",
                "--dangerously-bypass-approvals-and-sandbox",
                "--model",
                "gpt-5.6-sol",
                "-c",
                "model_reasoning_effort=\"high\"",
                "-c",
                "service_tier=\"fast\"",
                "-c",
                "features.fast_mode=true",
                "--ephemeral",
                "resume",
                "thread_123",
                "-"
            ]
        );
        assert!(result.fast_mode_applied);
    }

    #[test]
    fn parses_codex_jsonl_result() {
        let stdout = [
            serde_json::json!({"type":"thread.started","thread_id":"thread_123"}).to_string(),
            serde_json::json!({
                "type":"item.completed",
                "item":{"type":"agent_message","text":"Done"}
            })
            .to_string(),
            serde_json::json!({
                "type":"turn.completed",
                "usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":4}
            })
            .to_string(),
        ]
        .join("\n");

        let result = parse_codex_jsonl(&stdout);

        assert_eq!(result.session_id.as_deref(), Some("thread_123"));
        assert_eq!(result.summary, "Done");
        assert_eq!(result.usage.input_tokens, 10);
        assert_eq!(result.usage.cached_input_tokens, 2);
        assert_eq!(result.usage.output_tokens, 4);
        assert!(result.saw_protocol_terminal_event);
    }

    #[tokio::test]
    async fn codex_adapter_executes_cli_and_returns_protocol_result() {
        let path =
            std::env::temp_dir().join(format!("paperclip-codex-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"thread_fixture\"}' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Fixture done\"}}' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":3,\"cached_input_tokens\":1,\"output_tokens\":2}}'\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        let adapter = CodexLocalAdapter::new();
        let (sink, _receiver) = AdapterEventSink::channel(8);
        let mut context =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "prompt");
        context.adapter_config = serde_json::json!({ "command": path });

        let result = adapter.execute(context, sink).await.unwrap();

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.session_id.as_deref(), Some("thread_fixture"));
        assert_eq!(result.summary.as_deref(), Some("Fixture done"));
        assert_eq!(result.usage.unwrap().output_tokens, 2);
        std::fs::remove_file(path).unwrap();
    }
}

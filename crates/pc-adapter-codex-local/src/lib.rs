#![forbid(unsafe_code)]

pub mod skills;
pub mod codex_errors;
pub mod execute_helpers;
pub mod output_inactivity_monitor;

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
use pc_adapter_process::{
    execute_process_capture, execute_process_capture_with_options, ProcessSpec,
};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
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

/// 输出不活动监控的结果（对齐 Node execute.ts 的 monitor 组装）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorOutcome {
    pub termination_signal: String,
    pub elapsed_ms_since_last_event: u64,
    pub timeout_ms: u64,
}

/// 运行一次 codex 进程并接入输出不活动监控。
///
/// 复刻 Node execute.ts：monitor 触发时设置 kill_flag 终止子进程，
/// 返回 `MonitorOutcome`；未触发返回 `None`。
async fn execute_codex_with_monitor(
    command: &str,
    built: &CodexExecArgs,
    context: &AdapterExecutionContext,
    events: AdapterEventSink,
    monitor_timeout_ms: Option<u64>,
) -> Result<(pc_adapter_process::StreamingProcessExecution, Option<MonitorOutcome>), AdapterError> {
    let spec = ProcessSpec::new(command, &built.args).with_stdin(context.prompt.clone());
    if monitor_timeout_ms.is_none() {
        let execution = execute_process_capture(&spec, context, events).await?;
        return Ok((execution.into_streaming(), None));
    }
    let timeout_ms = monitor_timeout_ms.expect("checked above");
    let kill_flag = Arc::new(AtomicBool::new(false));
    let outcome: Arc<std::sync::Mutex<Option<MonitorOutcome>>> =
        Arc::new(std::sync::Mutex::new(None));
    let outcome_for_monitor = Arc::clone(&outcome);
    let kill_flag_for_monitor = Arc::clone(&kill_flag);
    let monitor = crate::output_inactivity_monitor::spawn_monitor(
        timeout_ms,
        move |state| {
            let elapsed = state
                .fired_at
                .unwrap_or(state.last_event_at)
                .saturating_sub(state.last_event_at);
            *outcome_for_monitor.lock().expect("monitor outcome lock") = Some(MonitorOutcome {
                termination_signal: "SIGTERM".to_owned(),
                elapsed_ms_since_last_event: elapsed,
                timeout_ms,
            });
            kill_flag_for_monitor.store(true, std::sync::atomic::Ordering::SeqCst);
        },
    )
    .map_err(AdapterError::Process)?;

    let monitor_for_chunk = Arc::new(monitor);
    let monitor_for_chunk_cb = Arc::clone(&monitor_for_chunk);
    let on_chunk: Arc<dyn Fn(&str, &str) + Send + Sync> = Arc::new(move |stream, chunk| {
        monitor_for_chunk_cb.note_output_chunk(stream, chunk);
    });

    let execution = match execute_process_capture_with_options(
        &spec,
        context,
        events,
        Some(on_chunk),
        Some(kill_flag),
    )
    .await
    {
        Ok(execution) => execution,
        Err(AdapterError::Process(message)) if message.contains("killed by output inactivity monitor") => {
            let outcome_locked = outcome.lock().expect("monitor outcome lock").clone();
            if outcome_locked.is_some() {
                // monitor 触发终止 → 返回空执行 + outcome（对齐 Node monitor 分支）。
                drop(monitor_for_chunk);
                return Ok((
                    pc_adapter_process::StreamingProcessExecution {
                        result: AdapterExecutionResult {
                            error_message: Some(message),
                            ..AdapterExecutionResult::default()
                        },
                        stdout: String::new(),
                        stderr: String::new(),
                        spawned_pid: None,
                    },
                    outcome_locked,
                ));
            }
            return Err(AdapterError::Process(message));
        }
        Err(error) => return Err(error),
    };
    drop(monitor_for_chunk);

    let outcome = outcome.lock().expect("monitor outcome lock").clone();
    Ok((execution, outcome))
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
        // 输出不活动监控：解析 adapterConfig.outputInactivityTimeoutMs（R433）。
        let monitor_resolution = crate::output_inactivity_monitor::resolve_codex_inactivity_timeout(
            context.adapter_config.get("outputInactivityTimeoutMs"),
        );
        let monitor_timeout_ms = monitor_resolution.timeout_ms();
        if monitor_resolution.is_disabled() {
            let _ = events
                .clone()
                .emit(pc_adapter_api::AdapterEvent::stderr(
                    "[paperclip] Codex output inactivity monitor is DISABLED via adapterConfig.outputInactivityTimeoutMs=null. Hung codex runs will only be detected by the platform-level silent-run safety net.\n".to_owned(),
                ))
                .await;
        }
        // 首轮 attempt：若 `context.session_id` 非空，传 `resume <sid>`，与 Node
        // `buildArgs(resumeSessionId)` 行为一致。
        let initial_built = build_codex_exec_args(
            &context.adapter_config,
            context.session_id.as_deref(),
            false,
        );
        let (initial_execution, initial_monitor) = execute_codex_with_monitor(
            command,
            &initial_built,
            &context,
            events.clone(),
            monitor_timeout_ms,
        )
        .await?;
        let initial_parsed = parse_codex_jsonl(&initial_execution.stdout);

        // 决策：unknown session + 有 resume id → 真实重跑一轮（不带 resume）。
        let mut retried_after_unknown_session = false;
        let mut clear_session_on_retry = false;
        let mut active_execution = initial_execution;
        let mut active_parsed = initial_parsed;
        let mut active_built = initial_built;
        let mut active_monitor = initial_monitor;
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
                let (retry_sink, _rx) = pc_adapter_api::AdapterEventSink::channel(8);
                let (retry_execution, retry_monitor) = execute_codex_with_monitor(
                    command,
                    &retry_built,
                    &context,
                    retry_sink,
                    monitor_timeout_ms,
                )
                .await?;
                let retry_parsed = parse_codex_jsonl(&retry_execution.stdout);
                active_execution = retry_execution;
                active_parsed = retry_parsed;
                active_built = retry_built;
                active_monitor = retry_monitor;
                retried_after_unknown_session = true;
                clear_session_on_retry = true;
            }
        }

        let execution = active_execution;
        let parsed = active_parsed;
        let built = active_built;
        // 输出不活动监控触发 → 组装 `codex_output_inactivity_monitor` 结果（对齐 Node toResult）。
        if let Some(monitor) = active_monitor {
            let error_message = crate::output_inactivity_monitor::
                format_output_inactivity_monitor_error_message(
                    monitor.elapsed_ms_since_last_event,
                );
            let mut monitor_result = AdapterExecutionResult {
                exit_code: None,
                signal: Some(monitor.termination_signal.clone()),
                timed_out: false,
                error_message: Some(error_message.clone()),
                error_code: Some("codex_output_inactivity_monitor".to_owned()),
                provider: Some("openai".to_owned()),
                result_json: Some(serde_json::json!({
                    "stdout": execution.stdout,
                    "stderr": execution.stderr,
                    "outputInactivityMonitor": {
                        "kind": "output_inactivity",
                        "timeoutMs": monitor.timeout_ms,
                        "elapsedMsSinceLastEvent": monitor.elapsed_ms_since_last_event,
                        "terminationSignal": monitor.termination_signal,
                    },
                })),
                ..AdapterExecutionResult::default()
            };
            monitor_result.billing_type =
                Some(crate::execute_helpers::resolve_codex_billing_type(&context.env).as_str().to_owned());
            let paperclip_env_note =
                pc_acpx::session_config_options::render_paperclip_env_note(&context.env);
            let api_access_note =
                pc_acpx::session_config_options::render_api_access_note(&context.env);
            if let Some(result_json) = monitor_result.result_json.as_mut() {
                result_json["paperclipEnvNote"] = serde_json::Value::String(paperclip_env_note);
                result_json["apiAccessNote"] = serde_json::Value::String(api_access_note);
                result_json["errorFamily"] = serde_json::Value::Null;
            }
            let _ = error_message;
            return Ok(monitor_result);
        }
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

#[cfg(test)]
mod monitor_integration_tests {
    use super::*;
    use pc_adapter_api::{AdapterEventSink, AdapterExecutionContext};

    /// 真实进程 + 极短超时：验证 monitor 触发后 kill 子进程并返回 outcome。
    #[tokio::test(flavor = "multi_thread")]
    async fn monitor_fires_and_kills_silent_process() {
        let context = AdapterExecutionContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "ignored prompt",
        );
        let built = CodexExecArgs {
            args: vec!["30".to_owned()],
            model: "gpt-5.6-sol".to_owned(),
            fast_mode_requested: false,
            fast_mode_applied: false,
        };
        let (sink, _rx) = AdapterEventSink::channel(8);
        let (execution, outcome) = execute_codex_with_monitor(
            "sleep",
            &built,
            &context,
            sink,
            Some(300),
        )
        .await
        .expect("execute should complete after monitor kill");

        let outcome = outcome.expect("monitor must fire on silent process");
        assert_eq!(outcome.termination_signal, "SIGTERM");
        assert!(outcome.timeout_ms >= 300);
        // 进程应被终止，不会等到 30s。
        assert!(execution.result.exit_code.is_none() || execution.result.exit_code != Some(0));
        assert_eq!(
            execution.result.error_message.as_deref(),
            Some("killed by output inactivity monitor")
        );
    }

    /// monitor 禁用时不创建监控，正常执行。
    #[tokio::test(flavor = "multi_thread")]
    async fn monitor_disabled_returns_no_outcome() {
        let context = AdapterExecutionContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "ignored",
        );
        let built = CodexExecArgs {
            args: vec![].to_vec(),
            model: "gpt-5.6-sol".to_owned(),
            fast_mode_requested: false,
            fast_mode_applied: false,
        };
        let (sink, _rx) = AdapterEventSink::channel(8);
        let (_execution, outcome) = execute_codex_with_monitor(
            "/bin/echo",
            &built,
            &context,
            sink,
            None,
        )
        .await
        .expect("execute should succeed");
        assert!(outcome.is_none());
    }
}

#![forbid(unsafe_code)]

pub mod skills;
pub mod codex_errors;
pub mod execute_helpers;
pub mod output_inactivity_monitor;
pub mod auth_precedence;
pub mod auth_copyback;
pub mod codex_auth_merge;
pub mod runtime_config;
pub mod codex_home;
pub mod codex_home_staging;
pub mod acp;
pub mod codex_test;
pub mod config_schema;
pub mod codex_remote_workspace;
pub mod codex_bridge_env;
pub mod codex_execution_env;
pub mod codex_session_params;
pub mod codex_session_resume;

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
    /// 当 fast_mode 被请求但未生效时给出原因（对齐 Node
    /// ）。None 表示无原因
    /// （要么未请求，要么已应用）。
    pub fast_mode_ignored_reason: Option<String>,
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
    let fast_mode_ignored_reason = if fast_mode_requested && !fast_mode_applied {
        Some(format_fast_mode_ignored_reason(&model))
    } else {
        None
    };
    CodexExecArgs {
        args,
        model,
        fast_mode_requested,
        fast_mode_applied,
        fast_mode_ignored_reason,
    }
}

fn format_fast_mode_ignored_reason(model: &str) -> String {
    let label = if model.is_empty() { "(default)" } else { model };
    format!(
        "Configured fast mode is currently only supported on {} or manually configured model IDs; Paperclip will ignore it for model {label}.",
        fast_mode_supported_models_label(),
    )
}

fn fast_mode_supported_models_label() -> String {
    const FAST: &[&str] = &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5", "gpt-5.4"];
    FAST.join(", ")
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
///
/// R495：CLI 执行改走 [`pc_acpx::execution_target_process::execute_command_for_target`]，
/// 由其按 target 类型分发（local / ssh / sandbox-fallback），output
/// inactivity monitor 的 kill_flag 透传到新执行器，
/// `killed_by_flag` 信号替代原 `killed by output inactivity monitor`
/// 错误消息字串判断。
///
/// `pub` 暴露给集成测试（如 round495_remote_execute）使用：
/// 真实 sshd fixture + SSH target 验证 codex 路径经新 helper 远端执行。
pub async fn execute_codex_with_monitor(
    command: &str,
    built: &CodexExecArgs,
    context: &AdapterExecutionContext,
    events: AdapterEventSink,
    monitor_timeout_ms: Option<u64>,
) -> Result<(pc_adapter_process::StreamingProcessExecution, Option<MonitorOutcome>), AdapterError> {
    use pc_acpx::execution_target_process::execute_command_for_target;

    let args = built.args.clone();
    let stdin: Option<&str> = if context.prompt.is_empty() { None } else { Some(&context.prompt) };
    // 执行超时独立于 monitor 的不活动窗口：执行超时默认 15min（对齐 Node
    // `runChildProcess` 默认；spec.timeout 在 pc-adapter-process 中也是
    // 15min 默认），monitor 的 `kill_flag` 才是短超时（300ms）触发点。
    let timeout_sec = 900.0_f64;
    let grace_sec = 5.0_f64;
    let cwd_owned = context.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
    let cwd: &str = cwd_owned.as_deref().unwrap_or("");
    let env = context.env.clone();
    let target_json = context.execution_target.clone();

    // 不启用 monitor：纯转发 output chunks + emit events。
    let on_log_no_monitor: Arc<dyn Fn(&str, &str) + Send + Sync> = {
        let events_for_log = events.clone();
        Arc::new(move |stream, chunk| {
            let owned = chunk.to_string();
            let events = events_for_log.clone();
            let label = match stream {
                "stderr" => pc_adapter_api::AdapterEvent::stderr(owned),
                _ => pc_adapter_api::AdapterEvent::stdout(owned),
            };
            tokio::spawn(async move {
                let _ = events.emit(label).await;
            });
        })
    };
    if monitor_timeout_ms.is_none() {
        let result = execute_command_for_target(
            command,
            &args,
            stdin.as_deref(),
            timeout_sec,
            grace_sec,
            &env,
            &cwd,
            target_json.as_ref(),
            Some(on_log_no_monitor),
            None,
        )
        .await
        .map_err(|error| AdapterError::Process(error))?;
        return Ok((run_process_result_to_streaming(result), None));
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
    // monitor 启用：on_log 同时转发 events + 通知 monitor chunk 回调。
    let on_log: Arc<dyn Fn(&str, &str) + Send + Sync> = {
        let events_for_log = events.clone();
        let monitor_for_chunk_outer = Arc::clone(&monitor_for_chunk_cb);
        Arc::new(move |stream, chunk| {
            monitor_for_chunk_outer.note_output_chunk(stream, chunk);
            let owned = chunk.to_string();
            let events = events_for_log.clone();
            let label = match stream {
                "stderr" => pc_adapter_api::AdapterEvent::stderr(owned),
                _ => pc_adapter_api::AdapterEvent::stdout(owned),
            };
            tokio::spawn(async move {
                let _ = events.emit(label).await;
            });
        })
    };

    let result = execute_command_for_target(
        command,
        &args,
        stdin.as_deref(),
        timeout_sec,
        grace_sec,
        &env,
        &cwd,
        target_json.as_ref(),
        Some(on_log),
        Some(Arc::clone(&kill_flag)),
    )
    .await
    .map_err(|error| AdapterError::Process(error))?;

    // monitor 触发终止（kill_flag 已被外部置位）→ 返回 monitor outcome。
    if result.killed_by_flag {
        let outcome_locked = outcome.lock().expect("monitor outcome lock").clone();
        if outcome_locked.is_some() {
            let error_message = crate::output_inactivity_monitor::
                format_output_inactivity_monitor_error_message(
                    outcome_locked.as_ref().unwrap().elapsed_ms_since_last_event,
                );
            drop(monitor_for_chunk_cb);
            return Ok((
                pc_adapter_process::StreamingProcessExecution {
                    result: AdapterExecutionResult {
                        error_message: Some(error_message),
                        ..AdapterExecutionResult::default()
                    },
                    stdout: result.stdout,
                    stderr: result.stderr,
                    spawned_pid: result.spawned_pid,
                },
                outcome_locked,
            ));
        }
    }

    // R438：process-activity-monitor 接线（如有 spawned_pid）。
    let activity_monitor: Option<pc_activity::ProcessActivityMonitorHandle> =
        if let Some(pid) = result.spawned_pid {
            let monitor_for_chunk_outer = Arc::clone(&monitor_for_chunk_cb);
            let monitor = pc_activity::spawn_process_activity_monitor(
                pc_activity::ProcessActivityMonitorOptions {
                    pid,
                    process_group_id: Some(pid),
                    on_activity: Box::new(move || {
                        monitor_for_chunk_outer.note_process_activity();
                    }),
                    interval: None,
                    sample: None,
                },
            );
            Some(monitor)
        } else {
            None
        };

    let outcome = outcome.lock().expect("monitor outcome lock").clone();
    drop(activity_monitor);
    drop(monitor_for_chunk_cb);
    Ok((
        run_process_result_to_streaming(result),
        outcome,
    ))
}

/// `RunProcessResult` → `StreamingProcessExecution`（对齐
/// `execute_process_capture_with_options` 返回结构）。
fn run_process_result_to_streaming(
    result: pc_acpx::execution_target_process::RunProcessResult,
) -> pc_adapter_process::StreamingProcessExecution {
    use std::os::unix::process::ExitStatusExt;
    pc_adapter_process::StreamingProcessExecution {
        result: AdapterExecutionResult {
            exit_code: result.exit_code,
            signal: result.signal.clone(),
            timed_out: result.timed_out,
            ..AdapterExecutionResult::default()
        },
        stdout: result.stdout,
        stderr: result.stderr,
        spawned_pid: result.spawned_pid,
    }
}

#[derive(Debug, Default)]
pub struct CodexLocalAdapter;

impl CodexLocalAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// 组装 codex execute 的 resolvedSessionParams（对齐 Node execute.ts L1342-1357）。
///
/// 当前仅本地执行：仅装配 `sessionId` + `cwd`。
/// `workspaceId` / `repoUrl` / `repoRef` 从 `adapter_config.workspaceContext` 读取
/// （若有），便于后续 resume 时复用。
fn build_resolved_session_params(
    resolved_session_id: Option<&str>,
    cwd: Option<&std::path::Path>,
    adapter_config: &serde_json::Value,
    execution_target: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    // 复用 codex_session_params 模块（对齐 Node resolvedSessionParams：
    // sessionId / cwd / remoteExecution? / workspaceId? / repoUrl? / repoRef?）
    let cwd_str = cwd.as_ref().map(|p| p.to_string_lossy().to_string());
    let target = execution_target.and_then(|v| {
        pc_acpx::execution_target::parse_adapter_execution_target(v)
    });
    let is_remote = pc_acpx::execution_target::adapter_execution_target_is_remote(target.as_ref());
    let identity = if is_remote {
        pc_acpx::execution_target::adapter_execution_target_session_identity(target.as_ref())
            .map(|id| serde_json::to_value(id).ok())
            .flatten()
    } else {
        None
    };
    let workspace_context = adapter_config.get("workspaceContext");
    let input = crate::codex_session_params::ResolvedSessionParamsInput {
        session_id: resolved_session_id,
        cwd: cwd_str.as_deref().unwrap_or(""),
        execution_target_is_remote: is_remote,
        remote_execution_identity: identity,
        workspace_id: workspace_context
            .and_then(|w| w.get("workspaceId").and_then(|v| v.as_str())),
        repo_url: workspace_context
            .and_then(|w| w.get("repoUrl").and_then(|v| v.as_str())),
        repo_ref: workspace_context
            .and_then(|w| w.get("repoRef").and_then(|v| v.as_str())),
    };
    crate::codex_session_params::build_resolved_session_params(&input)
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
        let configured_timeout_sec = context
            .adapter_config
            .get("timeoutSec")
            .and_then(serde_json::Value::as_f64);
        let configured_cwd = string(&context.adapter_config, "cwd");
        let local_fallback_cwd = context
            .cwd
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let agent_command_shell = string(&context.adapter_config, "agentCommand")
            .unwrap_or_default()
            .trim()
            .to_string();
        let execution_target_decision =
            pc_acpx::execution_target::resolve_adapter_execution_target_decision(
                &pc_acpx::execution_target::ResolveAdapterExecutionTargetDecisionInput {
                    execution_target: context.execution_target.as_ref(),
                    legacy_remote_execution: None,
                    environment_id: None,
                    lease_id: None,
                    configured_cwd,
                    local_fallback_cwd: &local_fallback_cwd,
                    configured_timeout_sec,
                    sandbox_runner_available: false,
                    agent_command_shell: Some(&agent_command_shell),
                },
            );
        let timeout_sec = (!execution_target_decision.timeout.is_disabled())
            .then_some(execution_target_decision.timeout.timeout_sec);
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
        // R490+R492：构建执行 env（对齐 Node execute.ts L806-907）。远程 +
        // usesBridge 时合并 paperclip bridge env；R492 起 SSH 远程 target
        // 启动真实 bridge（server/worker + SSH runner），并用真实 bridge
        // env 覆盖 4 键；sandbox target 无 provider runner，保持 env-only
        // 合并；本地原样返回。
        let execution_env = crate::codex_execution_env::build_codex_execution_env(
            &crate::codex_execution_env::CodexExecutionEnvInput {
                run_id: &context.run_id.to_string(),
                base_env: &context.env,
                execution_target: context.execution_target.as_ref(),
                runtime_root_dir: None,
                timeout_sec,
            },
        )
        .map_err(AdapterError::InvalidConfiguration)?;
        if let Some(line) = &execution_env.start_log_line {
            let _ = events
                .clone()
                .emit(pc_adapter_api::AdapterEvent::stdout(line.clone()))
                .await;
        }
        // R492：真实 bridge 启动（对齐 Node execute.ts 的
        // `startAdapterExecutionTargetPaperclipBridge` 分支）。SSH target →
        // 完整启动；sandbox / 本地 → None（保持 env-only）。
        let mut started_bridge: Option<pc_acpx::bridge_executor::StartedAdapterBridge> = None;
        let env = if execution_env.bridge_plan.is_some() {
            let events_for_bridge_log = events.clone();
            match crate::codex_bridge_env::start_codex_execution_bridge(
                &context.run_id.to_string(),
                &context.env,
                context.execution_target.as_ref(),
                timeout_sec,
                Some(Arc::new(move |line: &str| {
                    // 启动日志经 events sink 下发（闭包保持同步发射语义：
                    // 对齐 Node onLog 同步回调）。
                    let sink = events_for_bridge_log.clone();
                    let line = line.to_string();
                    tokio::spawn(async move {
                        let _ = sink
                            .emit(pc_adapter_api::AdapterEvent::stdout(line))
                            .await;
                    });
                })),
            )
            .await
            {
                Ok(Some(bridge)) => {
                    // 真实 bridge env 覆盖（对齐 Node
                    // `Object.assign(env, paperclipBridge.env)`）。
                    let mut env = execution_env.env;
                    for (key, value) in &bridge.env {
                        env.insert(key.clone(), value.clone());
                    }
                    let _ = events
                        .clone()
                        .emit(pc_adapter_api::AdapterEvent::stdout(
                            "[paperclip] Sandbox ACP API callback bridge enabled for this run.\n"
                                .to_owned(),
                        ))
                        .await;
                    started_bridge = Some(bridge);
                    env
                }
                Ok(None) => execution_env.env,
                Err(error) => return Err(AdapterError::InvalidConfiguration(error)),
            }
        } else {
            execution_env.env
        };
        // R493：process session bridge（对齐 Node execute.ts
        // `useRemoteProcessSession` 分支 + `settleRemoteBridgeStarts`）。
        // Rust sandbox target 尚无 provider runner，gate 的
        // `Boolean(executionTarget.runner)` 恒为 false → 不触发启动；
        // 代码路径保留（未来接入 provider runner 后自动生效），启动 env
        // 在 paperclip bridge env 合并后传入（等价于 Node env thunk
        // 求值结果）。
        let mut started_process_session_bridge: Option<
            pc_acpx::process_session_bridge::ProcessSessionBridgeHandle,
        > = None;
        if execution_target_decision.uses_remote_process_session {
            let events_for_bridge_log = events.clone();
            match crate::codex_bridge_env::start_codex_process_session_bridge(
                &context.run_id.to_string(),
                context.execution_target.as_ref(),
                None,
                "codex",
                &agent_command_shell,
                &execution_target_decision.execution_cwd,
                &env,
                timeout_sec,
                None,
                Some(Arc::new(move |line: &str| {
                    let sink = events_for_bridge_log.clone();
                    let line = line.to_string();
                    tokio::spawn(async move {
                        let _ = sink
                            .emit(pc_adapter_api::AdapterEvent::stdout(line))
                            .await;
                    });
                })),
            )
            .await
            {
                Ok(Some(handle)) => started_process_session_bridge = Some(handle),
                Ok(None) => {}
                Err(error) => return Err(AdapterError::InvalidConfiguration(error)),
            }
        }
        let execution_context = AdapterExecutionContext {
            env,
            ..context.clone()
        };
        let outcome: Result<AdapterExecutionResult, AdapterError> = async {
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
            &execution_context,
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
                    &execution_context,
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
        // 组装 resolvedSessionParams，对齐 Node codex execute.ts L1342-1357：
        // { sessionId, cwd, remoteExecution?, workspaceId?, repoUrl?, repoRef? }
        // 当前 codex-local 仅本地执行，remoteExecution 暂不装配；
        // workspace 字段从 adapter_config.workspaceContext 读取（若有）。
        result.session_params = build_resolved_session_params(
            result.session_id.as_deref(),
            (!execution_target_decision.execution_cwd.is_empty())
                .then(|| std::path::Path::new(&execution_target_decision.execution_cwd)),
            &context.adapter_config,
            context.execution_target.as_ref(),
        );
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
    .await;
    // R492+R493 teardown：双 bridge 在所有出口停止（对齐 Node
    // `cleanupRemoteBridges` 的
    // `Promise.allSettled([processSessionBridge?.stop(), paperclipBridge?.stop()])`：
    // 先停 process session bridge，再停 paperclip bridge，全部 best-effort）。
    if let Some(bridge) = &started_process_session_bridge {
        bridge.stop().await;
    }
    if let Some(bridge) = &started_bridge {
        bridge.stop().await;
    }
    outcome
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;
    use pc_adapter_api::{Adapter, AdapterEventSink, AdapterExecutionContext};

    #[test]
    fn build_resolved_session_params_only_session_id_and_cwd() {
        let config = serde_json::json!({});
        let params = build_resolved_session_params(
            Some("thread_123"),
            Some(std::path::Path::new("/repo")),
            &config,
            None,
        )
        .expect("params should be present when session_id resolves");
        assert_eq!(
            params.get("sessionId").and_then(|v| v.as_str()),
            Some("thread_123")
        );
        assert_eq!(params.get("cwd").and_then(|v| v.as_str()), Some("/repo"));
        assert!(params.get("workspaceId").is_none());
        assert!(params.get("repoUrl").is_none());
        assert!(params.get("repoRef").is_none());
    }

    #[test]
    fn build_resolved_session_params_includes_workspace_context() {
        let config = serde_json::json!({
            "workspaceContext": {
                "workspaceId": "ws_1",
                "repoUrl": "git@github.com:foo/bar.git",
                "repoRef": "main",
            }
        });
        let params = build_resolved_session_params(
            Some("thread_x"),
            Some(std::path::Path::new("/work")),
            &config,
            None,
        )
        .expect("params should be present");
        assert_eq!(params.get("workspaceId").and_then(|v| v.as_str()), Some("ws_1"));
        assert_eq!(
            params.get("repoUrl").and_then(|v| v.as_str()),
            Some("git@github.com:foo/bar.git")
        );
        assert_eq!(params.get("repoRef").and_then(|v| v.as_str()), Some("main"));
    }

    #[test]
    fn build_resolved_session_params_skips_empty_workspace_fields() {
        let config = serde_json::json!({
            "workspaceContext": {
                "workspaceId": "ws_1",
                "repoUrl": "",
                "repoRef": "main",
            }
        });
        let params = build_resolved_session_params(
            Some("thread_x"),
            None,
            &config,
            None,
        )
        .expect("params should be present");
        assert_eq!(params.get("workspaceId").and_then(|v| v.as_str()), Some("ws_1"));
        assert_eq!(params.get("repoRef").and_then(|v| v.as_str()), Some("main"));
        assert!(params.get("repoUrl").is_none());
        // Node codex resolvedSessionParams 始终写 cwd（cwd: effectiveExecutionCwd）
        assert_eq!(params.get("cwd").and_then(|v| v.as_str()), Some(""));
    }

    #[test]
    fn build_resolved_session_params_none_when_session_id_missing() {
        let config = serde_json::json!({});
        let params = build_resolved_session_params(
            None,
            Some(std::path::Path::new("/repo")),
            &config,
            None,
        );
        assert!(params.is_none());
    }

    #[test]
    fn build_resolved_session_params_includes_remote_execution_identity() {
        // 对齐 Node：远程执行时 sessionParams 装配 remoteExecution identity
        let config = serde_json::json!({
            "workspaceContext": {
                "workspaceId": "ws_1",
                "repoUrl": "git@github.com:foo/bar.git",
                "repoRef": "main",
            }
        });
        let target = serde_json::json!({
            "kind": "remote",
            "transport": "ssh",
            "remoteCwd": "/remote/workspace/.paperclip-runtime/runs/run-1/workspace",
            "spec": {
                "host": "127.0.0.1",
                "port": 2222,
                "username": "fixture",
                "remoteWorkspacePath": "/remote/workspace",
                "remoteCwd": "/remote/workspace/.paperclip-runtime/runs/run-1/workspace",
                "privateKey": "PRIVATE KEY",
                "knownHosts": "[127.0.0.1]:2222 ssh-ed25519 AAAA",
                "strictHostKeyChecking": true,
            }
        });
        let params = build_resolved_session_params(
            Some("thread_remote"),
            Some(std::path::Path::new("/remote/workspace/.paperclip-runtime/runs/run-1/workspace")),
            &config,
            Some(&target),
        )
        .expect("params should be present");
        let remote = params.get("remoteExecution").expect("remoteExecution present");
        assert_eq!(remote.get("transport").and_then(|v| v.as_str()), Some("ssh"));
        assert_eq!(remote.get("host").and_then(|v| v.as_str()), Some("127.0.0.1"));
        assert_eq!(remote.get("username").and_then(|v| v.as_str()), Some("fixture"));
        assert_eq!(remote.get("port").and_then(|v| v.as_u64()), Some(2222));
        assert_eq!(params.get("workspaceId").and_then(|v| v.as_str()), Some("ws_1"));
    }

    #[test]
    fn build_resolved_session_params_omits_remote_execution_for_local() {
        let config = serde_json::json!({});
        let params = build_resolved_session_params(
            Some("thread_local"),
            Some(std::path::Path::new("/repo")),
            &config,
            None,
        )
        .expect("params should be present");
        assert!(params.get("remoteExecution").is_none());
    }

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

    /// 真实 codex JSONL fixture 跑通后，`result.session_params` 应携带
    /// `{ sessionId, cwd, workspaceId?, repoUrl?, repoRef? }`（对齐 Node
    /// codex execute.ts L1342-1357 的 resolvedSessionParams）。
    #[tokio::test]
    async fn codex_adapter_populates_session_params() {
        let path =
            std::env::temp_dir().join(format!("paperclip-codex-params-{}", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"thread_params\"}' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"ok\"}}' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":0,\"cached_input_tokens\":0,\"output_tokens\":0}}'\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        let adapter = CodexLocalAdapter::new();
        let (sink, _receiver) = AdapterEventSink::channel(8);
        let mut context =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "p");
        context.cwd = Some(std::env::temp_dir());
        context.adapter_config = serde_json::json!({
            "command": path,
            "workspaceContext": {
                "workspaceId": "ws_params",
                "repoUrl": "git@github.com:foo/bar.git",
                "repoRef": "main",
            },
        });

        let result = adapter.execute(context, sink).await.unwrap();

        let params = result
            .session_params
            .as_ref()
            .expect("session_params should be populated");
        assert_eq!(
            params.get("sessionId").and_then(|v| v.as_str()),
            Some("thread_params")
        );
        let expected_cwd = std::env::temp_dir().to_string_lossy().to_string();
        assert_eq!(
            params.get("cwd").and_then(|v| v.as_str()),
            Some(expected_cwd.as_str())
        );
        assert_eq!(
            params.get("workspaceId").and_then(|v| v.as_str()),
            Some("ws_params")
        );
        assert_eq!(
            params.get("repoUrl").and_then(|v| v.as_str()),
            Some("git@github.com:foo/bar.git")
        );
        assert_eq!(params.get("repoRef").and_then(|v| v.as_str()), Some("main"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn fast_mode_ignored_reason_set_when_requested_but_not_applied() {
        let config = serde_json::json!({
            "model": "o3",
            "fastMode": true,
        });
        let built = build_codex_exec_args(&config, None, false);
        assert!(built.fast_mode_requested);
        assert!(!built.fast_mode_applied);
        let reason = built.fast_mode_ignored_reason.expect("reason expected");
        assert!(reason.contains("Configured fast mode is currently only supported on"));
        assert!(reason.contains("will ignore it for model o3"));
    }

    #[test]
    fn fast_mode_ignored_reason_none_when_applied() {
        let config = serde_json::json!({
            "model": "gpt-5.6-sol",
            "fastMode": true,
        });
        let built = build_codex_exec_args(&config, None, false);
        assert!(built.fast_mode_requested);
        assert!(built.fast_mode_applied);
        assert!(built.fast_mode_ignored_reason.is_none());
    }

    #[test]
    fn fast_mode_ignored_reason_none_when_not_requested() {
        let config = serde_json::json!({
            "model": "o3",
            "fastMode": false,
        });
        let built = build_codex_exec_args(&config, None, false);
        assert!(!built.fast_mode_requested);
        assert!(!built.fast_mode_applied);
        assert!(built.fast_mode_ignored_reason.is_none());
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
            fast_mode_ignored_reason: None,
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
        // R495：错误消息对齐 Node formatOutputInactivityMonitorErrorMessage：
        // "monitor: no codex activity (output or process) for {m}m {s}s"。
        // elapsed 取决于 timer jitter，使用前缀匹配，避免对实际 ms 数敏感。
        let error_message = execution
            .result
            .error_message
            .as_deref()
            .expect("monitor kill must populate error_message");
        assert!(
            error_message.starts_with("monitor: no codex activity (output or process) for "),
            "unexpected monitor error message: {error_message}"
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
            fast_mode_ignored_reason: None,
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

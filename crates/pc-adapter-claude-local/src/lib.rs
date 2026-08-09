#![forbid(unsafe_code)]

//! `claude_local` adapter — 调用本地 Claude Code CLI（`@anthropic-ai/claude-code`）。
//!
//! 协议要点（对齐 Node `@paperclipai/adapter-claude-local/server.ts`）：
//! - 必选 flag：`--print` / `--output-format stream-json` / `--verbose`
//! - 可选 flag：`--model <model>` / `--add-dir <cwd>` / `--append-system-prompt-file <file>` /
//!   `--mcp-config <json>` / `--effort <level>` / `--dangerously-skip-permissions`
//! - session id：JSONL 中 `thread.started.thread_id` 或首条 `session_id`
//! - prompt：stdin（与 `--print` 一致）
//!
//! 注意：实际 `claude` CLI 的能力探测通过 `claudeCommandSupportsEffortFlag` 在执行期检测，
//! 本适配器在 adapter_config 提供 `effort` 时尝试加 `--effort <level>`，被 CLI 拒绝时
//! 由 stderr 反映，agent.run 会回退处理（与 Node 行为对齐）。

pub mod skills;
pub mod claude_stream_json;
pub mod claude_test;
pub mod claude_config;
pub mod claude_models;
pub mod claude_permissions;
pub mod claude_prompt_cache;
pub mod cli_capabilities;
pub mod claude_errors;
pub mod execute_helpers;
pub mod acp;
pub mod config_schema;
pub mod claude_session_resume;

pub use claude_stream_json::{
    claude_model_usage_totals, detect_claude_login_required, extract_claude_login_url,
    is_claude_image_processing_error, is_claude_unknown_session_error,
    parse_claude_stream_json, ParsedClaudeStreamJson,
};
pub use execute_helpers::{
    claude_session_cwd_matches_execution_target, is_bedrock_auth, resolve_claude_billing_type,
    ClaudeBillingType,
};

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult,
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub const ADAPTER_TYPE: &str = "claude_local";

/// Claude CLI 启动参数（用于测试 + 实际执行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeExecArgs {
    pub args: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub add_dir: Option<String>,
    pub append_system_prompt_file: Option<String>,
    pub mcp_config: Option<String>,
    pub dangerously_skip_permissions: bool,
}

/// 从 adapter_config 构造 Claude CLI 启动参数。
pub fn build_claude_exec_args(config: &Value) -> ClaudeExecArgs {
    let model = config
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let effort = config
        .get("effort")
        .or_else(|| config.get("modelReasoningEffort"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let add_dir = config
        .get("addDir")
        .or_else(|| config.get("cwd"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let append_system_prompt_file = config
        .get("appendSystemPromptFile")
        .or_else(|| config.get("instructionsFile"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mcp_config = config
        .get("mcpConfig")
        .or_else(|| config.get("mcp_config"))
        .map(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Object(_) | Value::Array(_) => Some(v.to_string()),
            _ => None,
        })
        .flatten();
    let dangerously_skip_permissions = config
        .get("dangerouslySkipPermissions")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let extra_args: Vec<String> = config
        .get("extraArgs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut args: Vec<String> = Vec::new();
    // 必选协议 flag（按 Node 顺序）
    args.push("--print".into());
    args.push("--output-format".into());
    args.push("stream-json".into());
    args.push("--verbose".into());
    if let Some(m) = &model {
        args.push("--model".into());
        args.push(m.clone());
    }
    if let Some(d) = &add_dir {
        args.push("--add-dir".into());
        args.push(d.clone());
    }
    if let Some(f) = &append_system_prompt_file {
        args.push("--append-system-prompt-file".into());
        args.push(f.clone());
    }
    if let Some(m) = &mcp_config {
        args.push("--mcp-config".into());
        args.push(m.clone());
    }
    if let Some(e) = &effort {
        args.push("--effort".into());
        args.push(e.clone());
    }
    if dangerously_skip_permissions {
        args.push("--dangerously-skip-permissions".into());
    }
    args.extend(extra_args);

    ClaudeExecArgs {
        args,
        model,
        effort,
        add_dir,
        append_system_prompt_file,
        mcp_config,
        dangerously_skip_permissions,
    }
}

/// 从 adapter_config 构造 Claude CLI 启动参数（v2 版本，使用 `build_claude_args_v2` 完整逻辑）。
///
/// 与 `build_claude_exec_args` 的区别：
/// - args 顺序严格对齐 Node `buildClaudeArgs`
/// - 支持 `--chrome` / `--max-turns` / `--strict-mcp-config`
/// - Bedrock auth 模式下 gating `--model`
/// - resume 时跳过 `--append-system-prompt-file`
///
/// 当前默认入口仍是 `build_claude_exec_args`（向后兼容）。v2 用于需要完整 Node 语义的场景。
#[must_use]
pub fn build_claude_exec_args_v2(
    config: &Value,
    effective_execution_cwd: &str,
    resume_session_id: Option<&str>,
    is_bedrock_auth: bool,
) -> ClaudeExecArgs {
    let model = config
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let effort = config
        .get("effort")
        .or_else(|| config.get("modelReasoningEffort"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let add_dir = config
        .get("addDir")
        .or_else(|| config.get("cwd"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let append_system_prompt_file = config
        .get("appendSystemPromptFile")
        .or_else(|| config.get("instructionsFile"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mcp_config = config
        .get("mcpConfig")
        .or_else(|| config.get("mcp_config"))
        .map(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Object(_) | Value::Array(_) => Some(v.to_string()),
            _ => None,
        })
        .flatten();
    let chrome = config
        .get("chrome")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_turns = config
        .get("maxTurns")
        .and_then(|v| v.as_i64())
        .map(|n| n as i32)
        .unwrap_or(0);
    let dangerously_skip_permissions = config
        .get("dangerouslySkipPermissions")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let extra_args: Vec<String> = config
        .get("extraArgs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let input = crate::claude_cli_args::build_claude_args_input_from_context(
        config,
        effective_execution_cwd,
        effort.as_deref(),
        mcp_config.as_deref(),
        add_dir.as_deref(),
        max_turns,
        resume_session_id,
        append_system_prompt_file.as_deref(),
        &extra_args,
        is_bedrock_auth,
        false,
    );
    let args = crate::claude_cli_args::build_claude_args_v2(&input);

    ClaudeExecArgs {
        args,
        model,
        effort,
        add_dir,
        append_system_prompt_file,
        mcp_config,
        dangerously_skip_permissions,
    }
}


fn default_command(config: &Value) -> String {
    config
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("claude")
        .to_owned()
}

/// 解析 Claude CLI 的 stdout（stream-json 输出）。
///
/// 期望事件：
/// - `{"type":"thread.started","thread_id":"..."}` → session_id
/// - `{"type":"item.completed","item":{"type":"agent_message","text":"..."}}` → summary
/// - `{"type":"turn.completed","usage":{"input_tokens":...,"output_tokens":...,"cache_read_input_tokens":...}}` → usage
/// - `{"type":"result","subtype":"...","result":"...","is_error":true,"session_id":"..."}` → error
#[derive(Debug, Default, Clone)]
pub struct ParsedClaudeOutput {
    pub session_id: Option<String>,
    pub summary: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub error_message: Option<String>,
    pub saw_protocol_terminal_event: bool,
    pub saw_protocol_event: bool,
    pub model: Option<String>,
    pub stop_reason: Option<String>,
}

pub fn parse_claude_jsonl(stdout: &str) -> ParsedClaudeOutput {
    let mut out = ParsedClaudeOutput::default();
    let mut last_summary_event: Option<String> = None;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let val: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let event_type = val
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !event_type.is_empty() {
            out.saw_protocol_event = true;
        }
        match event_type.as_str() {
            "thread.started" => {
                if out.session_id.is_none() {
                    out.session_id = val
                        .get("thread_id")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
            }
            "item.completed" => {
                let item_type = val
                    .get("item")
                    .and_then(|i| i.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                if item_type == "agent_message" {
                    if let Some(text) = val
                        .get("item")
                        .and_then(|i| i.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        last_summary_event = Some(text.to_owned());
                    }
                }
            }
            "turn.completed" => {
                out.saw_protocol_terminal_event = true;
                if let Some(usage) = val.get("usage") {
                    out.input_tokens = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    out.output_tokens = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    out.cache_read_tokens = usage
                        .get("cache_read_input_tokens")
                        .or_else(|| usage.get("cached_input_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                }
            }
            "result" => {
                out.saw_protocol_terminal_event = true;
                let is_error = val
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_error {
                    if let Some(msg) = val
                        .get("result")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        out.error_message = Some(msg.to_owned());
                    } else if let Some(msg) = val
                        .get("subtype")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        out.error_message = Some(format!("error_subtype={msg}"));
                    }
                } else if let Some(text) = val
                    .get("result")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    last_summary_event = Some(text.to_owned());
                }
                // result.session_id 总是覆盖之前的 thread.started.thread_id
                if let Some(sid) = val.get("session_id").and_then(|v| v.as_str()) {
                    out.session_id = Some(sid.to_owned());
                }
                if let Some(model) = val.get("model").and_then(|v| v.as_str()) {
                    out.model = Some(model.to_owned());
                }
                if let Some(reason) = val.get("stop_reason").and_then(|v| v.as_str()) {
                    out.stop_reason = Some(reason.to_owned());
                }
                // 解析 result.usage（Anthropic SDK + CLI 都可能把 usage 嵌在 result 事件）
                if let Some(usage) = val.get("usage") {
                    if let Some(t) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                        out.input_tokens = t;
                    }
                    if let Some(t) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                        out.output_tokens = t;
                    }
                    if let Some(t) = usage
                        .get("cache_read_input_tokens")
                        .or_else(|| usage.get("cached_input_tokens"))
                        .and_then(|v| v.as_u64())
                    {
                        out.cache_read_tokens = t;
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(s) = last_summary_event {
        out.summary = s;
    }
    out
}

pub struct ClaudeLocalAdapter;

impl ClaudeLocalAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// v2 execute：集成 R461 模块（session resume 重试循环 + 错误族 + session_params）。
    ///
    /// 与 `execute` 的差别：
    /// - 调用 `claude_session_resume::decide_claude_session_resume` 决策是否 resume
    /// - 第一次 attempt 失败 + 是 session 错误 → 自动 fresh 重试
    /// - 通过 `claude_result_builder::assemble_claude_result` 整合最终结果
    /// - session_params 由 `claude_session_params::build_resolved_session_params` 组装
    ///
    /// 远程执行路径：bridge env 合并已实现（R490）；真实 bridge
    /// server/worker 执行器与 materializeRemoteClaudeConfig 留待后续。
    pub async fn execute_with_resume_retry(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        let command = default_command(&context.adapter_config);
        let configured_timeout_sec = context
            .adapter_config
            .get("timeoutSec")
            .and_then(Value::as_f64);
        let configured_cwd = context
            .adapter_config
            .get("cwd")
            .and_then(Value::as_str);
        let local_fallback_cwd = context
            .cwd
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let agent_command_shell = context
            .adapter_config
            .get("agentCommand")
            .and_then(Value::as_str)
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
        // R490+R492：构建执行 env（对齐 Node claude execute.ts L679-692）。
        // 远程 + usesBridge 时合并 paperclip bridge env；R492 起 SSH 远程
        // target 启动真实 bridge（server/worker + SSH runner），并用真实
        // bridge env 覆盖 4 键；sandbox target 无 provider runner，保持
        // env-only 合并；本地原样返回。
        let execution_env = crate::claude_execution_env::build_claude_execution_env(
            &crate::claude_execution_env::ClaudeExecutionEnvInput {
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
        // R492：真实 bridge 启动（对齐 Node claude execute.ts 的
        // `startAdapterExecutionTargetPaperclipBridge` 分支）。SSH target →
        // 完整启动；sandbox / 本地 → None（保持 env-only）。
        let mut started_bridge: Option<pc_acpx::bridge_executor::StartedAdapterBridge> = None;
        let env = if execution_env.bridge_plan.is_some() {
            let events_for_bridge_log = events.clone();
            match crate::claude_remote_workspace::start_claude_execution_bridge(
                &context.run_id.to_string(),
                &context.env,
                context.execution_target.as_ref(),
                timeout_sec,
                Some(Arc::new(move |line: &str| {
                    // 启动日志经 events sink 下发（对齐 Node onLog 同步回调）。
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
            match crate::claude_remote_workspace::start_claude_process_session_bridge(
                &context.run_id.to_string(),
                context.execution_target.as_ref(),
                None,
                "claude",
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
        // 第一步：用 v2 构造 base args（不含 --resume）
        let is_bedrock_auth = crate::claude_models::is_bedrock_env(&context.env);
        let effective_execution_cwd_pre = context
            .cwd
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let built = build_claude_exec_args_v2(
            &context.adapter_config,
            &effective_execution_cwd_pre,
            None,
            is_bedrock_auth,
        );

        let runtime_session_id = context.session_id.as_deref().unwrap_or("");
        let runtime_session_cwd = context
            .cwd
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let runtime_session_params = context.session_params.as_ref();
        let (
            runtime_prompt_bundle_key,
            runtime_mcp_server_identity,
            runtime_remote_execution,
        ) = extract_runtime_session_params(runtime_session_params);

        let prompt_bundle_key =
            compute_prompt_bundle_key(&context.prompt, &context.adapter_config);
        let mcp_server_identity = String::new();
        let effective_execution_cwd = execution_target_decision.execution_cwd.clone();

        // 从 context.execution_target 解析远程状态（对齐 Node execute.ts）：
        // - execution_target_is_remote：远程 target（SSH / Sandbox）为 true
        // - execution_target_session_identity：远程时才装配 identity
        let execution_target_is_remote = execution_target_decision.is_remote;
        let execution_target: Option<&Value> = context.execution_target.as_ref();
        let execution_target_session_identity_owned: Option<Value> = if execution_target_is_remote {
            execution_target_decision
                .remote_execution_identity
                .as_ref()
                .and_then(|identity| serde_json::to_value(identity).ok())
        } else {
            None
        };
        let execution_target_session_identity: Option<&Value> =
            execution_target_session_identity_owned.as_ref();

        let decision = crate::claude_session_resume::decide_claude_session_resume(
            &crate::claude_session_resume::SessionResumeInput {
                runtime_session_id,
                runtime_session_cwd: &runtime_session_cwd,
                runtime_remote_execution,
                runtime_prompt_bundle_key: &runtime_prompt_bundle_key,
                runtime_mcp_server_identity: &runtime_mcp_server_identity,
                effective_execution_cwd: &effective_execution_cwd,
                current_prompt_bundle_key: &prompt_bundle_key,
                current_mcp_server_identity: &mcp_server_identity,
                execution_target_is_remote,
                execution_target,
            },
        );
        for log in &decision.log_lines {
            let _ = events
                .clone()
                .emit(pc_adapter_api::AdapterEvent::stdout(log.clone()))
                .await;
        }
        let resume_session_id = decision
            .resume_session_id(runtime_session_id)
            .map(str::to_owned);

        let loop_input = crate::claude_resume_loop::ResumeRetryInput {
            context: &execution_context,
            events: events.clone(),
            command: &command,
            base_args: &built.args,
            resume_session_id: resume_session_id.as_deref(),
            runtime_session_id,
            effective_execution_cwd: &effective_execution_cwd,
            prompt_bundle_key: &prompt_bundle_key,
            mcp_server_identity: &mcp_server_identity,
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
            execution_target_is_remote,
            execution_target_session_identity,
            config_model: built.model.as_deref().unwrap_or(""),
            is_bedrock_auth: crate::claude_models::is_bedrock_env(&context.env),
            now: std::time::SystemTime::now(),
        };
        let result = crate::claude_resume_loop::run_resume_retry_loop(&loop_input).await;
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
        result
    }
}

/// content-addressed prompt bundle key：基于 prompt + adapter_config 的 SHA-256 hex（前 16 字节）。
pub fn compute_prompt_bundle_key(prompt: &str, adapter_config: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    hasher.update(adapter_config.to_string().as_bytes());
    let digest = hasher.finalize();
    hex_encode(&digest[..16])
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

fn extract_runtime_session_params(
    params: Option<&Value>,
) -> (String, String, Option<&Value>) {
    let Some(value) = params else {
        return (String::new(), String::new(), None);
    };
    let bundle_key = value
        .get("promptBundleKey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let mcp_identity = value
        .get("mcpServerIdentity")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let remote = value.get("remoteExecution");
    (bundle_key, mcp_identity, remote)
}

impl Default for ClaudeLocalAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for ClaudeLocalAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        let mut descriptor = AdapterDescriptor::builtin(ADAPTER_TYPE, "Claude Code");
        descriptor.supports_local_agent_jwt = true;
        descriptor.supports_instructions_bundle = true;
        descriptor
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        let command = default_command(&context.adapter_config);
        let built = build_claude_exec_args(&context.adapter_config);
        // R496：CLI 执行改走 `execute_command_for_target` 三分支 dispatch
        // （local / ssh / sandbox-fallback），让 Claude 主路径也享受远程
        // target 支持。`execute_claude_attempt_for_target` 复用 R495 的
        // helper：on_log → AdapterEventSink，timeout/grace 对齐 Node
        // `runChildProcess` 默认（15min / 5s）。
        let stdin = if context.prompt.is_empty() {
            None
        } else {
            Some(context.prompt.as_str())
        };
        let execution = crate::claude_resume_loop::execute_claude_attempt_for_target(
            &command,
            &built.args,
            stdin,
            &context,
            events,
        )
        .await?;
        let parsed = parse_claude_stream_json(&execution.stdout);
        let mut result = execution.result;
        result.session_id = parsed.session_id.clone();
        result.provider = Some("claude_local".into());
        result.model = parsed.model.clone().or_else(|| built.model.clone());
        result.billing_type = Some(
            crate::execute_helpers::resolve_claude_billing_type(&context.env)
                .as_str()
                .to_owned(),
        );
        result.summary = (!parsed.summary.is_empty()).then_some(parsed.summary.clone());
        result.usage = parsed.usage.clone();
        result.error_message = parsed.error_message.clone().or_else(|| {
            (result.exit_code != Some(0))
                .then(|| execution.stderr.trim().to_owned())
                .filter(|s| !s.is_empty())
        });
        let paperclip_env_note =
            pc_acpx::session_config_options::render_paperclip_env_note(&context.env);
        let api_access_note =
            pc_acpx::session_config_options::render_api_access_note(&context.env);
        let decision = crate::execute_helpers::decide_retry(
            crate::execute_helpers::ClaudeRetryInput {
                session_id: result.session_id.as_deref().unwrap_or(""),
                timed_out: result.timed_out,
                exit_code: result.exit_code,
                parsed: parsed.result_json.as_ref(),
                stdout: &execution.stdout,
                stderr: &execution.stderr,
                error_message: result.error_message.as_deref(),
            },
        );
        let mut stop_reason = parsed.stop_reason.clone();
        if matches!(
            decision.error_family,
            crate::execute_helpers::ClaudeErrorFamily::MaxTurns
        ) {
            stop_reason = Some("max_turns_exhausted".to_owned());
        } else if matches!(
            decision.error_family,
            crate::execute_helpers::ClaudeErrorFamily::PoisonedPreviousMessageId
        ) {
            stop_reason = Some("claude_poisoned_previous_message_id".to_owned());
        } else if matches!(
            decision.error_family,
            crate::execute_helpers::ClaudeErrorFamily::Refusal
        ) {
            stop_reason = Some("refusal".to_owned());
        }
        let mut merged = json!({
            "sawProtocolEvent": true,
            "sawProtocolTerminalEvent": parsed.result_json.is_some(),
            "stopReason": stop_reason,
            "costUsd": parsed.cost_usd,
            "claudeResult": parsed.result_json,
            "effortRequested": built.effort,
            "addDir": built.add_dir,
            "appendSystemPromptFile": built.append_system_prompt_file,
            "mcpConfigProvided": built.mcp_config.is_some(),
            "dangerouslySkipPermissions": built.dangerously_skip_permissions,
            "paperclipEnvNote": paperclip_env_note,
            "apiAccessNote": api_access_note,
            "errorFamily": decision.error_family.as_str(),
        });
        if decision.provider_quota || decision.transient_upstream {
            merged["transientUpstream"] = json!(decision.transient_upstream);
        }
        result.result_json = Some(merged);
        if decision.clear_session {
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
        let adapter = ClaudeLocalAdapter::new();
        assert_eq!(adapter.descriptor().adapter_type, "claude_local");
        assert!(adapter.descriptor().supports_local_agent_jwt);
        assert!(adapter.descriptor().supports_instructions_bundle);
    }

    #[test]
    fn default_command_falls_back_to_builtin() {
        let config = serde_json::json!({});
        assert_eq!(default_command(&config), "claude");
    }

    #[test]
    fn default_command_reads_config() {
        let config = serde_json::json!({"command": "/custom/path"});
        assert_eq!(default_command(&config), "/custom/path");
    }

    #[test]
    fn builds_minimal_args() {
        let built = build_claude_exec_args(&serde_json::json!({}));
        assert_eq!(
            built.args,
            vec!["--print", "--output-format", "stream-json", "--verbose"]
        );
        assert!(built.model.is_none());
        assert!(built.effort.is_none());
        assert!(!built.dangerously_skip_permissions);
    }

    #[test]
    fn builds_full_args_with_effort_and_mcp() {
        let config = serde_json::json!({
            "model": "claude-opus-4-7",
            "effort": "high",
            "addDir": "/workspace",
            "appendSystemPromptFile": "/tmp/instructions.md",
            "mcpConfig": {"mcpServers": {}},
            "dangerouslySkipPermissions": true,
            "extraArgs": ["--resume", "abc123"]
        });
        let built = build_claude_exec_args(&config);
        assert_eq!(built.model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(built.effort.as_deref(), Some("high"));
        assert_eq!(built.add_dir.as_deref(), Some("/workspace"));
        assert_eq!(
            built.append_system_prompt_file.as_deref(),
            Some("/tmp/instructions.md")
        );
        assert!(built.mcp_config.is_some());
        assert!(built.dangerously_skip_permissions);
        assert!(built.args.contains(&"--print".to_string()));
        assert!(built.args.contains(&"--model".to_string()));
        let model_idx = built.args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(built.args[model_idx + 1], "claude-opus-4-7");
        assert!(built.args.contains(&"--effort".to_string()));
        assert!(built
            .args
            .contains(&"--dangerously-skip-permissions".to_string()));
        // extraArgs 追加在末尾
        let resume_idx = built.args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(built.args[resume_idx + 1], "abc123");
    }

    #[test]
    fn effort_alias_model_reasoning_effort_works() {
        let config = serde_json::json!({ "modelReasoningEffort": "low" });
        let built = build_claude_exec_args(&config);
        assert_eq!(built.effort.as_deref(), Some("low"));
        assert!(built.args.contains(&"--effort".to_string()));
    }

    #[test]
    fn mcp_config_accepts_object() {
        let config = serde_json::json!({
            "mcpConfig": { "mcpServers": { "x": {"command": "node"} } }
        });
        let built = build_claude_exec_args(&config);
        let mcp = built.mcp_config.expect("mcp_config present");
        assert!(mcp.contains("mcpServers"));
    }

    #[test]
    fn parse_claude_jsonl_thread_started_and_result() {
        let stdout = [
            json!({"type":"thread.started","thread_id":"thread_abc"}).to_string(),
            json!({
                "type":"item.completed",
                "item":{"type":"agent_message","text":"partial"}
            })
            .to_string(),
            json!({
                "type":"result",
                "is_error": false,
                "result": "Final reply",
                "session_id": "sess_xyz",
                "model": "claude-opus-4-7",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 11, "output_tokens": 7, "cache_read_input_tokens": 4}
            })
            .to_string(),
        ]
        .join("\n");

        let p = parse_claude_jsonl(&stdout);
        assert_eq!(p.session_id.as_deref(), Some("sess_xyz"));
        assert_eq!(p.summary, "Final reply");
        assert_eq!(p.input_tokens, 11);
        assert_eq!(p.output_tokens, 7);
        assert_eq!(p.cache_read_tokens, 4);
        assert_eq!(p.model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(p.stop_reason.as_deref(), Some("end_turn"));
        assert!(p.saw_protocol_terminal_event);
    }

    #[test]
    fn parse_claude_jsonl_error_result() {
        let stdout = json!({
            "type": "result",
            "is_error": true,
            "subtype": "invalid_tool_input",
            "result": "tool X missing",
            "session_id": "s1",
        })
        .to_string();
        let p = parse_claude_jsonl(&stdout);
        assert_eq!(p.error_message.as_deref(), Some("tool X missing"));
        assert!(p.saw_protocol_terminal_event);
    }

    #[test]
    fn parse_claude_jsonl_skips_non_json_lines() {
        let stdout = "random stdout noise\n{\"type\":\"thread.started\",\"thread_id\":\"t1\"}\n";
        let p = parse_claude_jsonl(stdout);
        assert_eq!(p.session_id.as_deref(), Some("t1"));
    }

    #[tokio::test]
    async fn claude_adapter_executes_cli_fixture() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("paperclip-claude-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"sess_fixture\"}' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"hi\"}}' '{\"type\":\"result\",\"is_error\":false,\"result\":\"done\",\"session_id\":\"sess_fixture\",\"model\":\"claude-opus-4-7\",\"stop_reason\":\"end_turn\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3,\"cache_read_input_tokens\":1}}'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        let adapter = ClaudeLocalAdapter::new();
        let (sink, _receiver) = AdapterEventSink::channel(8);
        let mut context =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "prompt");
        context.adapter_config = serde_json::json!({
            "command": path,
            "model": "claude-opus-4-7",
            "dangerouslySkipPermissions": true,
        });
        let result = adapter.execute(context, sink).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.session_id.as_deref(), Some("sess_fixture"));
        assert_eq!(result.summary.as_deref(), Some("done"));
        assert_eq!(result.usage.unwrap().output_tokens, 3);
        assert_eq!(result.model.as_deref(), Some("claude-opus-4-7"));
        std::fs::remove_file(path).unwrap();
    }

    /// v2 execute：单 attempt 成功路径（用真实 fixture）。
    #[tokio::test(flavor = "multi_thread")]
    async fn execute_with_resume_retry_happy_path() {
        let path = copy_fixture_to_temp("claude_happy_path.sh");

        let adapter = ClaudeLocalAdapter::new();
        let (sink, _receiver) = AdapterEventSink::channel(8);
        let mut context =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "prompt");
        context.adapter_config = serde_json::json!({
            "command": path.to_string_lossy(),
            "model": "claude-opus-4-7",
            "dangerouslySkipPermissions": true,
        });

        let result = adapter.execute_with_resume_retry(context, sink).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.session_id.as_deref(), Some("v2_sess"));
        assert_eq!(result.summary.as_deref(), Some("v2 done"));
        assert_eq!(result.model.as_deref(), Some("claude-opus-4-7"));
        let usage = result.usage.expect("usage present");
        assert_eq!(usage.output_tokens, 3);
        assert!(!result.clear_session);
        let _ = std::fs::remove_file(&path);
    }

    /// v2 execute：第一次 attempt 返回 unknown session → 自动 fresh 重试 → 成功。
    #[tokio::test(flavor = "multi_thread")]
    async fn execute_with_resume_retry_unknown_session_triggers_fresh_retry() {
        let counter_path = std::env::temp_dir()
            .join(format!("paperclip-claude-retry-{}.counter", uuid::Uuid::new_v4()));
        let path = copy_fixture_to_temp("claude_retry_unknown_session.sh");

        let adapter = ClaudeLocalAdapter::new();
        let (sink, _receiver) = AdapterEventSink::channel(8);
        let mut context = AdapterExecutionContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "prompt",
        );
        context.adapter_config = serde_json::json!({
            "command": path.to_string_lossy(),
            "model": "claude-opus-4-7",
        });
        context.env.insert(
            "PAPERCLIP_RETRY_COUNTER".to_owned(),
            counter_path.to_string_lossy().to_string(),
        );
        // 显式传 session_id 触发 resume 路径
        context.session_id = Some("550e8400-e29b-41d4-a716-446655440000".to_owned());

        let result = adapter.execute_with_resume_retry(context, sink).await.unwrap();
        // 重试后应该 fresh session
        assert_eq!(result.session_id.as_deref(), Some("fresh_sess"));
        assert_eq!(result.summary.as_deref(), Some("fresh done"));
        // 对齐 Node L1202：resolvedSessionId 有值时 clearSession=false
        // 重试成功产生新 session，server 端持久化的旧 sid 由调用方根据 session_id 切换自行清理
        assert!(!result.clear_session);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&counter_path);
    }

    /// v2 execute：session_id 不是合法 UUID → 不传 --resume，单 attempt。
    #[tokio::test(flavor = "multi_thread")]
    async fn execute_with_resume_retry_invalid_uuid_skips_resume() {
        let path = copy_fixture_to_temp("claude_invalid_uuid.sh");

        let adapter = ClaudeLocalAdapter::new();
        let (sink, _receiver) = AdapterEventSink::channel(8);
        let mut context =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "prompt");
        context.adapter_config = serde_json::json!({
            "command": path.to_string_lossy(),
        });
        context.session_id = Some("not-a-uuid".to_owned());

        let result = adapter.execute_with_resume_retry(context, sink).await.unwrap();
        assert_eq!(result.session_id.as_deref(), Some("sess1"));
        let _ = std::fs::remove_file(&path);
    }

    /// 把 tests/fixtures/<name> 复制到 /tmp/paperclip-claude-<uuid>-<name> 并设可执行权限。
    fn copy_fixture_to_temp(name: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src = manifest_dir.join("tests").join("fixtures").join(name);
        let dest = std::env::temp_dir()
            .join(format!("paperclip-claude-{}-{}", uuid::Uuid::new_v4(), name));
        std::fs::copy(&src, &dest).expect("copy fixture");
        let mut perms = std::fs::metadata(&dest).expect("stat").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms).expect("chmod");
        dest
    }

    #[test]
    fn compute_prompt_bundle_key_stable_for_same_input() {
        let cfg = serde_json::json!({"model": "x"});
        let k1 = compute_prompt_bundle_key("hello", &cfg);
        let k2 = compute_prompt_bundle_key("hello", &cfg);
        assert_eq!(k1, k2);
    }

    #[test]
    fn compute_prompt_bundle_key_differs_for_different_prompt() {
        let cfg = serde_json::json!({});
        let k1 = compute_prompt_bundle_key("hello", &cfg);
        let k2 = compute_prompt_bundle_key("world", &cfg);
        assert_ne!(k1, k2);
    }

    #[test]
    fn compute_prompt_bundle_key_differs_for_different_config() {
        let cfg1 = serde_json::json!({"model": "a"});
        let cfg2 = serde_json::json!({"model": "b"});
        let k1 = compute_prompt_bundle_key("hello", &cfg1);
        let k2 = compute_prompt_bundle_key("hello", &cfg2);
        assert_ne!(k1, k2);
    }

    #[test]
    fn compute_prompt_bundle_key_returns_32_hex_chars() {
        let k = compute_prompt_bundle_key("hello", &serde_json::json!({}));
        assert_eq!(k.len(), 32);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn extract_runtime_session_params_handles_missing() {
        let (b, m, r) = extract_runtime_session_params(None);
        assert_eq!(b, "");
        assert_eq!(m, "");
        assert!(r.is_none());
    }

    #[test]
    fn extract_runtime_session_params_extracts_all_fields() {
        let params = serde_json::json!({
            "promptBundleKey": "bundle-x",
            "mcpServerIdentity": r#"[{"name":"a"}]"#,
            "remoteExecution": {"id": "ssh-1"},
        });
        let (b, m, r) = extract_runtime_session_params(Some(&params));
        assert_eq!(b, "bundle-x");
        assert_eq!(m, r#"[{"name":"a"}]"#);
        assert!(r.is_some());
    }

    #[test]
    fn build_claude_exec_args_v2_minimal() {
        let config = serde_json::json!({});
        let built = build_claude_exec_args_v2(&config, "/cwd", None, false);
        assert_eq!(
            built.args,
            vec!["--print", "--output-format", "stream-json", "--verbose"]
        );
        assert!(built.model.is_none());
        assert!(built.effort.is_none());
    }

    #[test]
    fn build_claude_exec_args_v2_full_features() {
        let config = serde_json::json!({
            "model": "claude-opus-4-7",
            "chrome": true,
            "effort": "high",
            "maxTurns": 50,
            "addDir": "/workspace",
            "mcpConfig": "/mcp.json",
            "dangerouslySkipPermissions": true,
        });
        let built = build_claude_exec_args_v2(&config, "/workspace", None, false);
        // 验证 args 顺序对齐 Node
        let expected = vec![
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--dangerously-skip-permissions",
            "--chrome",
            "--model",
            "claude-opus-4-7",
            "--effort",
            "high",
            "--max-turns",
            "50",
            "--mcp-config",
            "/mcp.json",
            "--strict-mcp-config",
            "--add-dir",
            "/workspace",
        ];
        assert_eq!(built.args, expected);
    }

    #[test]
    fn build_claude_exec_args_v2_with_resume_skips_instructions() {
        let config = serde_json::json!({
            "appendSystemPromptFile": "/instr.md",
            "model": "claude-opus-4-7",
        });
        let built = build_claude_exec_args_v2(&config, "/cwd", Some("abc-123"), false);
        // resume 时不应传 --append-system-prompt-file
        assert!(!built.args.contains(&"--append-system-prompt-file".to_owned()));
        // 但应传 --resume
        let idx = built.args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(built.args[idx + 1], "abc-123");
    }

    #[test]
    fn build_claude_exec_args_v2_bedrock_auth_skips_anthropic_short_model() {
        let config = serde_json::json!({"model": "claude-opus-4-6"});
        let built = build_claude_exec_args_v2(&config, "/cwd", None, true);
        assert!(!built.args.contains(&"--model".to_owned()));
    }

    #[test]
    fn build_claude_exec_args_v2_bedrock_auth_keeps_bedrock_native() {
        let config = serde_json::json!({"model": "us.anthropic.claude-opus-4-8-v1"});
        let built = build_claude_exec_args_v2(&config, "/cwd", None, true);
        assert!(built.args.contains(&"--model".to_owned()));
        assert!(built.args.iter().any(|a| a.contains("us.anthropic")));
    }

    #[test]
    fn extract_runtime_session_params_omits_missing_fields() {
        let params = serde_json::json!({
            "promptBundleKey": "bundle-x",
        });
        let (b, m, r) = extract_runtime_session_params(Some(&params));
        assert_eq!(b, "bundle-x");
        assert_eq!(m, "");
        assert!(r.is_none());
    }
}
pub mod claude_cli_args;
pub mod claude_session_params;
pub mod claude_result_builder;
pub mod claude_prompt_sections;
pub mod claude_mcp_config;
pub mod claude_session_cleanup;
pub mod claude_resume_loop;
pub mod claude_remote_workspace;
pub mod claude_execution_env;
pub mod claude_quota;

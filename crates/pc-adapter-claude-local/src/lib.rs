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
pub mod claude_errors;
pub mod execute_helpers;

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
use pc_adapter_process::{execute_process_capture, ProcessSpec};
use serde_json::{json, Value};

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
        let spec = ProcessSpec::new(&command, &built.args).with_stdin(context.prompt.clone());
        let execution = execute_process_capture(&spec, &context, events).await?;
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
        result.result_json = Some(json!({
            "sawProtocolEvent": true,
            "sawProtocolTerminalEvent": parsed.result_json.is_some(),
            "stopReason": parsed.stop_reason,
            "costUsd": parsed.cost_usd,
            "claudeResult": parsed.result_json,
            "effortRequested": built.effort,
            "addDir": built.add_dir,
            "appendSystemPromptFile": built.append_system_prompt_file,
            "mcpConfigProvided": built.mcp_config.is_some(),
            "dangerouslySkipPermissions": built.dangerously_skip_permissions,
            "paperclipEnvNote": paperclip_env_note,
            "apiAccessNote": api_access_note,
        }));
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
}

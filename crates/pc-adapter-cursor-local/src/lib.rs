#![forbid(unsafe_code)]

//! `cursor_local` adapter — 调用本地 `cursor-agent` CLI。
//!
//! 协议要点（对齐 Node `@paperclipai/adapter-cursor-local/server.ts`）：
//! - 必选 flag：`--print` / `--output-format stream-json` / `--stream-partial-output`
//! - 可选 flag：`--model <model>` / `--workspace <path>` / `--sandbox` / `--force`
//! - session id：JSONL 中 `session_id` 字段（顶层或 `result.session_id`）
//! - prompt：stdin（与 `--print` 一致）
//!
//! 注意：`cursor-agent` 是 Cursor 官方 CLI（`@cursor/cli`），命令名可能为
//! `cursor-agent`、`cursor`、或通过 adapter_config.command 自定义。

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult,
};
use pc_adapter_process::{execute_process_capture, ProcessSpec};
use serde_json::{json, Value};

pub mod cursor_stream_json;
pub mod execute_helpers;

pub use execute_helpers::{
    cursor_skills_home, normalize_mode, resolve_cursor_biller, resolve_cursor_billing_type,
    resolve_provider_from_model, CursorBillingType, CursorMode,
};

pub use cursor_stream_json::{
    is_cursor_unknown_session_error, normalize_cursor_stream_line,
    parse_cursor_stream_json, ParsedCursorStreamJson,
};

pub const ADAPTER_TYPE: &str = "cursor_local";

/// Cursor CLI 启动参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorExecArgs {
    pub args: Vec<String>,
    pub model: Option<String>,
    pub workspace: Option<String>,
    pub sandbox: bool,
    pub force: bool,
}

pub fn build_cursor_exec_args(config: &Value) -> CursorExecArgs {
    let model = config
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let workspace = config
        .get("workspace")
        .or_else(|| config.get("cwd"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let sandbox = config
        .get("sandbox")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let force = config
        .get("force")
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
    args.push("--stream-partial-output".into());
    if let Some(m) = &model {
        args.push("--model".into());
        args.push(m.clone());
    }
    if let Some(w) = &workspace {
        args.push("--workspace".into());
        args.push(w.clone());
    }
    if sandbox {
        args.push("--sandbox".into());
    }
    if force {
        args.push("--force".into());
    }
    args.extend(extra_args);

    CursorExecArgs {
        args,
        model,
        workspace,
        sandbox,
        force,
    }
}

fn default_command(config: &Value) -> String {
    config
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("cursor-agent")
        .to_owned()
}

/// 解析 Cursor CLI 的 stdout（stream-json 输出）。
///
/// 事件：
/// - `{"type":"system","session_id":"..."}` → session_id
/// - `{"type":"assistant","message":{"content":[{"type":"text","text":"..."}]}}` → summary
/// - `{"type":"result","subtype":"success","session_id":"...","usage":{...}}` → terminal
/// - `{"type":"result","is_error":true,"result":"..."}` → error
#[derive(Debug, Default, Clone)]
pub struct ParsedCursorOutput {
    pub session_id: Option<String>,
    pub summary: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub error_message: Option<String>,
    pub saw_protocol_terminal_event: bool,
    pub saw_protocol_event: bool,
    pub model: Option<String>,
}

pub fn parse_cursor_jsonl(stdout: &str) -> ParsedCursorOutput {
    let mut out = ParsedCursorOutput::default();
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
            "system" => {
                if out.session_id.is_none() {
                    out.session_id = val
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
                if let Some(model) = val.get("model").and_then(|v| v.as_str()) {
                    out.model = Some(model.to_owned());
                }
            }
            "assistant" => {
                if let Some(content) = val
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        if let Some(text) = block
                            .get("text")
                            .and_then(|t| t.as_str())
                            .filter(|s| !s.is_empty())
                        {
                            last_summary_event = Some(text.to_owned());
                        }
                    }
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
                    } else if let Some(sub) = val
                        .get("subtype")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        out.error_message = Some(format!("error_subtype={sub}"));
                    }
                } else if let Some(text) = val
                    .get("result")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    last_summary_event = Some(text.to_owned());
                }
                // result.session_id 总是覆盖之前的 system.session_id
                if let Some(sid) = val.get("session_id").and_then(|v| v.as_str()) {
                    out.session_id = Some(sid.to_owned());
                }
                if let Some(model) = val.get("model").and_then(|v| v.as_str()) {
                    out.model = Some(model.to_owned());
                }
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

pub struct CursorLocalAdapter;

impl CursorLocalAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for CursorLocalAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for CursorLocalAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        let mut descriptor = AdapterDescriptor::builtin(ADAPTER_TYPE, "Cursor Local");
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
        let built = build_cursor_exec_args(&context.adapter_config);
        let spec = ProcessSpec::new(&command, &built.args).with_stdin(context.prompt.clone());
        let execution = execute_process_capture(&spec, &context, events).await?;
        let parsed = parse_cursor_stream_json(&execution.stdout);
        let mut result = execution.result;
        result.session_id = parsed.session_id.clone();
        result.provider = Some("cursor_local".into());
        result.model = parsed.model.clone().or_else(|| built.model.clone());
        let billing_type = crate::execute_helpers::resolve_cursor_billing_type(&context.env);
        let model_for_provider = context.adapter_config.get("model").and_then(Value::as_str);
        let provider = crate::execute_helpers::resolve_provider_from_model(
            model_for_provider.unwrap_or(""),
        );
        result.billing_type = Some(billing_type.as_str().to_owned());
        result.result_json = Some(serde_json::json!({
            "biller": crate::execute_helpers::resolve_cursor_biller(
                &context.env,
                billing_type,
                provider.as_deref(),
            ),
        }));
        result.summary = (!parsed.summary.is_empty()).then_some(parsed.summary.clone());
        result.usage = Some(parsed.usage.clone());
        result.error_message = parsed.error_message.clone().or_else(|| {
            (result.exit_code != Some(0))
                .then(|| execution.stderr.trim().to_owned())
                .filter(|s| !s.is_empty())
        });
        result.cost_usd = parsed.cost_usd;
        result.result_json = Some(json!({
            "cursorResult": parsed.result_json,
            "workspace": built.workspace,
            "sandbox": built.sandbox,
            "force": built.force,
        }));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_returns_correct_type() {
        let adapter = CursorLocalAdapter::new();
        assert_eq!(adapter.descriptor().adapter_type, "cursor_local");
        assert!(adapter.descriptor().supports_local_agent_jwt);
        assert!(adapter.descriptor().supports_instructions_bundle);
    }

    #[test]
    fn default_command_falls_back_to_builtin() {
        assert_eq!(default_command(&serde_json::json!({})), "cursor-agent");
        assert_eq!(
            default_command(&serde_json::json!({"command": "cursor"})),
            "cursor"
        );
    }

    #[test]
    fn builds_minimal_args() {
        let built = build_cursor_exec_args(&serde_json::json!({}));
        assert_eq!(
            built.args,
            vec![
                "--print",
                "--output-format",
                "stream-json",
                "--stream-partial-output",
            ]
        );
        assert!(built.model.is_none());
        assert!(!built.sandbox);
        assert!(!built.force);
    }

    #[test]
    fn builds_full_args_with_workspace_and_sandbox() {
        let config = serde_json::json!({
            "model": "claude-3-5-sonnet",
            "workspace": "/tmp/ws",
            "sandbox": true,
            "force": true,
            "extraArgs": ["--resume", "abc"]
        });
        let built = build_cursor_exec_args(&config);
        assert_eq!(built.model.as_deref(), Some("claude-3-5-sonnet"));
        assert_eq!(built.workspace.as_deref(), Some("/tmp/ws"));
        assert!(built.sandbox);
        assert!(built.force);
        let model_idx = built.args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(built.args[model_idx + 1], "claude-3-5-sonnet");
        assert!(built.args.contains(&"--workspace".to_string()));
        assert!(built.args.contains(&"--sandbox".to_string()));
        assert!(built.args.contains(&"--force".to_string()));
        let resume_idx = built.args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(built.args[resume_idx + 1], "abc");
    }

    #[test]
    fn parse_cursor_jsonl_full_lifecycle() {
        let stdout = [
            json!({"type":"system","session_id":"sess_a","model":"claude-3-5-sonnet"}).to_string(),
            json!({
                "type":"assistant",
                "message":{"content":[{"type":"text","text":"partial"}]}
            })
            .to_string(),
            json!({
                "type":"result",
                "is_error": false,
                "result": "final reply",
                "session_id": "sess_a",
                "usage": {"input_tokens": 12, "output_tokens": 8, "cache_read_input_tokens": 3}
            })
            .to_string(),
        ]
        .join("\n");
        let p = parse_cursor_jsonl(&stdout);
        assert_eq!(p.session_id.as_deref(), Some("sess_a"));
        assert_eq!(p.summary, "final reply");
        assert_eq!(p.input_tokens, 12);
        assert_eq!(p.output_tokens, 8);
        assert_eq!(p.cache_read_tokens, 3);
        assert_eq!(p.model.as_deref(), Some("claude-3-5-sonnet"));
        assert!(p.saw_protocol_terminal_event);
    }

    #[test]
    fn parse_cursor_jsonl_error_result() {
        let stdout = json!({
            "type":"result",
            "is_error": true,
            "subtype": "rate_limit",
            "result": "too many requests",
            "session_id": "sess_b"
        })
        .to_string();
        let p = parse_cursor_jsonl(&stdout);
        assert_eq!(p.error_message.as_deref(), Some("too many requests"));
        assert!(p.saw_protocol_terminal_event);
    }

    #[test]
    fn parse_cursor_jsonl_skips_non_json() {
        let stdout = "noise\n{\"type\":\"system\",\"session_id\":\"t1\"}\n";
        let p = parse_cursor_jsonl(stdout);
        assert_eq!(p.session_id.as_deref(), Some("t1"));
    }

    #[tokio::test]
    async fn cursor_adapter_executes_cli_fixture() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("paperclip-cursor-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"system\",\"session_id\":\"sess_fix\",\"model\":\"claude-3-5-sonnet\"}' '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}' '{\"type\":\"result\",\"is_error\":false,\"result\":\"done\",\"session_id\":\"sess_fix\",\"usage\":{\"input_tokens\":7,\"output_tokens\":4,\"cache_read_input_tokens\":2}}'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        let adapter = CursorLocalAdapter::new();
        let (sink, _receiver) = AdapterEventSink::channel(8);
        let mut context =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "prompt");
        context.adapter_config = serde_json::json!({
            "command": path,
            "model": "claude-3-5-sonnet",
            "sandbox": true,
        });
        let result = adapter.execute(context, sink).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.session_id.as_deref(), Some("sess_fix"));
        assert_eq!(result.summary.as_deref(), Some("done"));
        assert_eq!(result.usage.unwrap().output_tokens, 4);
        assert_eq!(result.model.as_deref(), Some("claude-3-5-sonnet"));
        std::fs::remove_file(path).unwrap();
    }
}

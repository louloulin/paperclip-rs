#![forbid(unsafe_code)]

//! Claude execute 整合（session resume 重试循环 + 错误族 + session_params 组装）。
//!
//! 对齐 Node `execute.ts` L1189-1267 的 resume retry 主循环：
//! 1. 用 `decide_claude_session_resume` 决定是否 resume
//! 2. 第一次 attempt：执行 CLI，解析 stdout
//! 3. 如果 session 错误（unknown / poisoned / image），自动重试 fresh session
//! 4. 用 `assemble_claude_result` 组装最终 AdapterExecutionResult
//!
//! 本模块提供：
//! - `detect_session_error_kind` — 检查 stdout+parsed 是否是 unknown/poisoned/image session 错误
//! - `build_resume_claude_args` — 构造带/不带 --resume 的 CLI args
//! - `run_resume_retry_loop` — 整合流程（同步执行两次 attempt）
//!
//! 注：远程执行路径（executionTargetIsRemote + bridge）暂不实现，留待后续 R461.7。

use crate::claude_errors::{
    is_claude_poisoned_previous_message_id_error, is_claude_unknown_session_error,
};
use crate::claude_stream_json::is_claude_image_processing_error;
use crate::claude_result_builder::{assemble_claude_result, AssembleInput};
use crate::claude_session_params::ResolvedSessionParamsInput;
use crate::claude_session_resume::{
    decide_claude_session_resume, SessionResumeDecision, SessionResumeInput,
};
use crate::claude_stream_json::{parse_claude_stream_json, ParsedClaudeStreamJson};
use crate::execute_helpers::resolve_claude_billing_type;
use pc_adapter_api::{AdapterExecutionContext, AdapterExecutionResult, AdapterEventSink};
use pc_adapter_process::{execute_process_capture, ProcessSpec};
use serde_json::{json, Value};
use std::time::SystemTime;

/// session resume 错误类型（对齐 Node L1212-1222）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionErrorKind {
    Unknown,
    Poisoned,
    Image,
}

impl SessionErrorKind {
    /// 决策 attempt 是否应重试。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            SessionErrorKind::Unknown => "unknown",
            SessionErrorKind::Poisoned => "poisoned",
            SessionErrorKind::Image => "image",
        }
    }
}

/// 检测 stdout / parsed 是否包含 session 错误（Node L1192-1200）。
#[must_use]
pub fn detect_session_error_kind(
    stdout: &str,
    parsed: Option<&Value>,
    exit_code: Option<i32>,
) -> Option<SessionErrorKind> {
    // 对齐 Node L1194：只有 exit_code != 0 时才检测 session error
    if exit_code.unwrap_or(0) == 0 {
        return None;
    }
    if let Some(parsed) = parsed {
        if is_claude_unknown_session_error(Some(parsed)) {
            return Some(SessionErrorKind::Unknown);
        }
        if is_claude_poisoned_previous_message_id_error(parsed) {
            return Some(SessionErrorKind::Poisoned);
        }
        if is_claude_image_processing_error(parsed) {
            return Some(SessionErrorKind::Image);
        }
    }
    // 退而求其次：从 stdout 字符串扫描（用于 stream-json 解析失败时）
    let lower = stdout.to_ascii_lowercase();
    if lower.contains("no conversation found") || lower.contains("session not found") || lower.contains("unknown session") {
        return Some(SessionErrorKind::Unknown);
    }
    if lower.contains("previous_message_id") && lower.contains("starts with `msg_`") {
        return Some(SessionErrorKind::Poisoned);
    }
    if lower.contains("could not process image") {
        return Some(SessionErrorKind::Image);
    }
    None
}

/// 构造带/不带 --resume 的 Claude CLI args（Node L831-870 简化版）。
///
/// 当前实现只关注 resume flag；permission / chrome / model / effort 等其他参数
/// 由 `build_claude_exec_args` 提供，本函数只追加 --resume。
#[must_use]
pub fn build_resume_claude_args(base_args: &[String], resume_session_id: Option<&str>) -> Vec<String> {
    let mut args = base_args.to_vec();
    if let Some(sid) = resume_session_id {
        args.push("--resume".to_owned());
        args.push(sid.to_owned());
    }
    args
}

/// 把 JSON 结果对象 + attempt 输入打包成 AdapterExecutionResult（复用 assemble_claude_result）。
#[must_use]
pub fn build_result_from_attempt(
    parsed: &Value,
    parsed_stream: &ParsedClaudeStreamJson,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    config_model: &str,
    is_bedrock_auth: bool,
    effective_execution_cwd: &str,
    prompt_bundle_key: &str,
    mcp_server_identity: &str,
    workspace_id: Option<&str>,
    repo_url: Option<&str>,
    repo_ref: Option<&str>,
    execution_target_is_remote: bool,
    execution_target_session_identity: Option<&Value>,
    login_required: bool,
    login_url: Option<&str>,
    fallback_session_id: Option<&str>,
    clear_session_on_missing_session: bool,
    terminal_result_cleanup: Option<Value>,
    now: SystemTime,
) -> AdapterExecutionResult {
    let input = AssembleInput {
        parsed,
        stdout,
        stderr,
        exit_code,
        login_required,
        login_url,
        error_message: None,
        fallback_session_id,
        config_model,
        config_billing_type: resolve_claude_billing_type_env(is_bedrock_auth),
        is_bedrock_auth,
        effective_execution_cwd,
        prompt_bundle_key,
        mcp_server_identity,
        workspace_id,
        repo_url,
        repo_ref,
        execution_target_is_remote,
        execution_target_session_identity,
        clear_session_on_missing_session,
        parsed_stream_session_id: parsed_stream.session_id.as_deref(),
        parsed_stream_model: parsed_stream.model.as_deref(),
        parsed_stream_usage: parsed_stream.usage.clone(),
        parsed_stream_summary: &parsed_stream.summary,
        parsed_stream_cost_usd: parsed_stream.cost_usd,
        terminal_result_cleanup,
        now,
    };
    assemble_claude_result(&input)
}

/// 简化的 billing type 解析（基于 is_bedrock_auth）。
fn resolve_claude_billing_type_env(is_bedrock_auth: bool) -> &'static str {
    if is_bedrock_auth {
        "metered_api"
    } else {
        "api"
    }
}

/// Resume retry 循环的输入。
#[derive(Debug, Clone)]
pub struct ResumeRetryInput<'a> {
    pub context: &'a AdapterExecutionContext,
    pub events: AdapterEventSink,
    pub command: &'a str,
    pub base_args: &'a [String],
    /// 是否 resume（由 `decide_claude_session_resume` 给出）
    pub resume_session_id: Option<&'a str>,
    /// runtime session id（用于 fallback）
    pub runtime_session_id: &'a str,
    pub effective_execution_cwd: &'a str,
    pub prompt_bundle_key: &'a str,
    pub mcp_server_identity: &'a str,
    pub workspace_id: Option<&'a str>,
    pub repo_url: Option<&'a str>,
    pub repo_ref: Option<&'a str>,
    pub execution_target_is_remote: bool,
    pub execution_target_session_identity: Option<&'a Value>,
    pub config_model: &'a str,
    pub is_bedrock_auth: bool,
    pub now: SystemTime,
}

/// 执行 resume retry 循环（不含远程 execution target / bridge 启动）。
///
/// 流程：
/// 1. 第一次 attempt（带 --resume 如有）
/// 2. 检测 session 错误
/// 3. 如有错误：第二次 attempt（不带 --resume，clear_session_on_missing_session=true）
/// 4. 组装并返回 AdapterExecutionResult
pub async fn run_resume_retry_loop(
    input: &ResumeRetryInput<'_>,
) -> Result<AdapterExecutionResult, pc_adapter_api::AdapterError> {
    let args = build_resume_claude_args(input.base_args, input.resume_session_id);
    let spec = ProcessSpec::new(input.command, &args).with_stdin(input.context.prompt.clone());
    let initial_execution = execute_process_capture(&spec, input.context, input.events.clone()).await?;
    let initial_stdout = initial_execution.stdout.clone();
    let initial_stderr = initial_execution.stderr.clone();
    let initial_exit = initial_execution.result.exit_code;
    let initial_parsed_stream = parse_claude_stream_json(&initial_stdout);
    drop(initial_execution);
    let initial_parsed_json = initial_parsed_stream.result_json.clone().unwrap_or(json!({}));

    let session_error = detect_session_error_kind(
        &initial_stdout,
        initial_parsed_stream.result_json.as_ref(),
        initial_exit,
    );

    let (final_stdout, final_stderr, final_exit, final_parsed_stream, clear_on_missing) = if session_error.is_some() && input.resume_session_id.is_some() {
        // 重试：fresh session
        let retry_args = build_resume_claude_args(input.base_args, None);
        let retry_spec = ProcessSpec::new(input.command, &retry_args).with_stdin(input.context.prompt.clone());
        let retry_execution = execute_process_capture(&retry_spec, input.context, input.events.clone()).await?;
        let retry_stdout = retry_execution.stdout.clone();
        let retry_stderr = retry_execution.stderr.clone();
        let retry_exit = retry_execution.result.exit_code;
        let retry_parsed_stream = parse_claude_stream_json(&retry_execution.stdout);
        // Note: retry_execution is dropped here, no longer needed
        (
            retry_stdout,
            retry_stderr,
            retry_exit,
            retry_parsed_stream,
            true,
        )
    } else {
        (
            initial_stdout,
            initial_stderr,
            initial_exit,
            initial_parsed_stream,
            false,
        )
    };

    let final_parsed_json = final_parsed_stream.result_json.clone().unwrap_or(json!({}));

    // 检测 login_required
    let (login_required, login_url) = detect_login(&final_stdout, &final_stderr, final_parsed_stream.result_json.as_ref());

    let fallback_session_id = if session_error.is_some() {
        None
    } else {
        // 第一次 attempt 成功且有 resume 时保留 runtime_session_id 作 fallback
        if input.resume_session_id.is_some() {
            Some(input.runtime_session_id)
        } else {
            None
        }
    };

    let result = build_result_from_attempt(
        &final_parsed_json,
        &final_parsed_stream,
        &final_stdout,
        &final_stderr,
        final_exit,
        input.config_model,
        input.is_bedrock_auth,
        input.effective_execution_cwd,
        input.prompt_bundle_key,
        input.mcp_server_identity,
        input.workspace_id,
        input.repo_url,
        input.repo_ref,
        input.execution_target_is_remote,
        input.execution_target_session_identity,
        login_required,
        login_url.as_deref(),
        fallback_session_id,
        clear_on_missing,
        None,
        input.now,
    );

    Ok(result)
}

fn detect_login(stdout: &str, stderr: &str, parsed: Option<&Value>) -> (bool, Option<String>) {
    use crate::claude_stream_json::detect_claude_login_required;
    use crate::claude_stream_json::extract_claude_login_url;
    let required = detect_claude_login_required(parsed, stdout, stderr);
    let url = if required {
        extract_claude_login_url(stdout)
            .or_else(|| extract_claude_login_url(stderr))
            .or_else(|| parsed.and_then(|p| crate::claude_stream_json::extract_claude_login_url(p.get("message").and_then(|m| m.as_str()).unwrap_or(""))))
    } else {
        None
    };
    (required, url)
}

/// 不调用真实进程的 resume decision 包装（用于测试）。
///
/// 接受外部传入的 stdout + parsed + session_id decision，
/// 复用 resume decision 的日志逻辑。
pub fn format_session_resume_log(
    decision: &SessionResumeDecision,
) -> Vec<String> {
    decision.log_lines.clone()
}

/// resume decision 工厂（用于测试场景或 lib.rs 暴露给路由层）。
pub fn make_session_resume_decision(input: &SessionResumeInput<'_>) -> SessionResumeDecision {
    decide_claude_session_resume(input)
}

/// 给定 runtime session params + 当前执行上下文，构造 `ResolvedSessionParamsInput`。
///
/// 调用方负责提供所有字段（这层不做 I/O）。
#[must_use]
pub fn make_resolved_session_params_input<'a>(
    session_id: Option<&'a str>,
    effective_execution_cwd: &'a str,
    prompt_bundle_key: &'a str,
    mcp_server_identity: &'a str,
    workspace_id: Option<&'a str>,
    repo_url: Option<&'a str>,
    repo_ref: Option<&'a str>,
    execution_target_is_remote: bool,
    execution_target_session_identity: Option<&'a Value>,
) -> ResolvedSessionParamsInput<'a> {
    ResolvedSessionParamsInput {
        session_id,
        cwd: effective_execution_cwd,
        prompt_bundle_key,
        mcp_server_identity,
        execution_target_is_remote,
        execution_target_session_identity,
        workspace_id,
        repo_url,
        repo_ref,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detect_session_error_kind_unknown() {
        let parsed = json!({"errors": [{"message": "No conversation found with session ID: abc"}]});
        assert_eq!(
            detect_session_error_kind("", Some(&parsed), Some(1)),
            Some(SessionErrorKind::Unknown)
        );
    }

    #[test]
    fn detect_session_error_kind_poisoned() {
        let parsed = json!({"errors": [{"message": "diagnostics.previous_message_id 'x' starts with `msg_` invalid"}]});
        assert_eq!(
            detect_session_error_kind("", Some(&parsed), Some(1)),
            Some(SessionErrorKind::Poisoned)
        );
    }

    #[test]
    fn detect_session_error_kind_image() {
        let parsed = json!({"errors": [{"message": "could not process image (unprocessable entity)"}]});
        assert_eq!(
            detect_session_error_kind("", Some(&parsed), Some(1)),
            Some(SessionErrorKind::Image)
        );
    }

    #[test]
    fn detect_session_error_kind_none_for_zero_exit() {
        // exit_code = 0 表示 CLI 报告成功，跳过 session error 检测
        let parsed = json!({"subtype": "success"});
        assert_eq!(
            detect_session_error_kind("", Some(&parsed), Some(0)),
            None
        );
    }

    #[test]
    fn detect_session_error_kind_none_for_nonzero_but_no_marker() {
        // exit_code != 0 但 parsed 没有 unknown/poisoned/image marker
        let parsed = json!({"errors": [{"message": "something else"}]});
        assert_eq!(
            detect_session_error_kind("", Some(&parsed), Some(1)),
            None
        );
    }

    #[test]
    fn detect_session_error_kind_from_stdout_only() {
        // 没有 parsed 但 stdout 包含 "No conversation found"
        assert_eq!(
            detect_session_error_kind("Error: No conversation found with session ID", None, Some(1)),
            Some(SessionErrorKind::Unknown)
        );
    }

    #[test]
    fn build_resume_claude_args_no_resume() {
        let args = build_resume_claude_args(&["--print".to_owned()], None);
        assert_eq!(args, vec!["--print"]);
    }

    #[test]
    fn build_resume_claude_args_with_resume() {
        let args = build_resume_claude_args(
            &["--print".to_owned(), "--verbose".to_owned()],
            Some("session-1"),
        );
        assert_eq!(args, vec!["--print", "--verbose", "--resume", "session-1"]);
    }

    #[test]
    fn build_resume_claude_args_empty() {
        let args = build_resume_claude_args(&[], Some("session-1"));
        assert_eq!(args, vec!["--resume", "session-1"]);
    }

    #[test]
    fn format_session_resume_log_returns_logs() {
        let input = SessionResumeInput {
            runtime_session_id: "550e8400-e29b-41d4-a716-446655440000",
            runtime_session_cwd: "/old",
            runtime_remote_execution: None,
            runtime_prompt_bundle_key: "bundle-a",
            runtime_mcp_server_identity: "[{\"name\":\"a\"}]",
            effective_execution_cwd: "/new",
            current_prompt_bundle_key: "bundle-a",
            current_mcp_server_identity: "[{\"name\":\"a\"}]",
            execution_target_is_remote: false,
            execution_target: None,
        };
        let decision = decide_claude_session_resume(&input);
        let logs = format_session_resume_log(&decision);
        assert!(!logs.is_empty());
    }

    #[test]
    fn make_resolved_session_params_input_minimal() {
        let input = make_resolved_session_params_input(
            Some("sid"),
            "/cwd",
            "bundle",
            "[]",
            None,
            None,
            None,
            false,
            None,
        );
        assert_eq!(input.session_id, Some("sid"));
        assert_eq!(input.cwd, "/cwd");
        assert!(!input.execution_target_is_remote);
    }

    #[test]
    fn make_resolved_session_params_input_remote_with_identity() {
        let identity = json!({"id": "ssh-1", "port": 22});
        let input = make_resolved_session_params_input(
            Some("sid"),
            "/remote/cwd",
            "bundle",
            "[]",
            Some("ws-1"),
            Some("git@github.com:foo/bar.git"),
            Some("main"),
            true,
            Some(&identity),
        );
        assert!(input.execution_target_is_remote);
        assert_eq!(input.execution_target_session_identity, Some(&identity));
        assert_eq!(input.workspace_id, Some("ws-1"));
    }
}

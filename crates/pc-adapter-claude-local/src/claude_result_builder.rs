#![forbid(unsafe_code)]

//! Claude `toAdapterResult` 纯函数版本（对齐 Node `execute.ts` L957-1199）。
//!
//! 把 Node 端 `toAdapterResult` 中的**决策逻辑**抽出为可独立测试的纯函数，
//! 不依赖具体的 process / events / 远程 runtime。
//!
//! 提供：
//! - `parse_fallback_error_message` — CLI 不可解析时 fallback 错误信息
//! - `decide_parsed_success` — subtype=success + is_error=false 判定
//! - `decide_failed` — failed 综合判定（succeeded + exit_code + is_error）
//! - `clear_session_for_max_turns` / `clear_session_for_poison` — clear-on-error 单条件
//! - `decide_clear_session` — 多原因 OR 整合
//! - `resolve_session_id_with_poison_drop` — poisoned sessionId 直接丢弃
//! - `resolve_error_code` — 优先级链解析错误码
//! - `decide_error_family` — errorFamily 字段解析
//! - `merge_result_json` — stopReason / errorFamily / retryNotBefore 合并
//! - `resolve_usage` — usage 优先级计算（parsed_stream → modelUsage → parsed.usage）
//! - `assemble_claude_result` — 顶层整合，返回完整 AdapterExecutionResult
//!
//! 错误族判定（provider_quota / transient_upstream / claude_refusal）依赖
//! `pc-adapter-claude-local::claude_errors` 中的现有函数，本模块不重复实现。
//!
//! Bedrock 模型过滤依赖 `pc-adapter-claude-local::claude_models::is_bedrock_model_id`。

use crate::claude_errors::{
    describe_claude_failure, extract_claude_retry_not_before, is_claude_max_turns_result,
    is_claude_model_not_found_error, is_claude_poisoned_previous_message_id_error,
    is_claude_provider_quota_error, is_claude_refusal_result, is_claude_transient_upstream_error,
};
use crate::claude_models::is_bedrock_model_id;
use pc_adapter_api::{AdapterExecutionResult, UsageSummary};
use serde_json::{Map, Value};
use std::time::SystemTime;

/// `parseFallbackErrorMessage`（Node execute.ts L867-878）的 Rust 版本。
#[must_use]
pub fn parse_fallback_error_message(stderr: &str, exit_code: Option<i32>) -> String {
    let stderr_line = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_owned();
    let exit_code = exit_code.unwrap_or(-1);
    if exit_code == 0 {
        return "Failed to parse claude JSON output".to_owned();
    }
    if stderr_line.is_empty() {
        format!("Claude exited with code {exit_code}")
    } else {
        format!("Claude exited with code {exit_code}: {stderr_line}")
    }
}

/// 决策 parsed JSON 是否算"成功"（对齐 Node L1031-1039）。
#[must_use]
pub fn decide_parsed_success(parsed: &Value) -> bool {
    let subtype = parsed
        .get("subtype")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_error = parsed
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    subtype == "success" && !is_error
}

/// `decide_parsed_success` + 退出码 → failed 标志。
#[must_use]
pub fn decide_failed(parsed: &Value, exit_code: Option<i32>) -> bool {
    let succeeded = decide_parsed_success(parsed);
    let parsed_is_error = parsed
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    !succeeded && (exit_code.unwrap_or(0) != 0 || parsed_is_error)
}

/// 是否因 max_turns 触发 clear_session（与 Node L1158 一致）。
#[must_use]
pub fn clear_session_for_max_turns(parsed: &Value) -> bool {
    is_claude_max_turns_result(Some(parsed))
}

/// 是否因 poisoned previous_message_id 触发 clear_session（与 Node L1161 一致）。
#[must_use]
pub fn clear_session_for_poison(parsed: &Value) -> bool {
    is_claude_poisoned_previous_message_id_error(parsed)
}

/// 综合决策 clear_session（Node L1158-1175）。
#[must_use]
pub fn decide_clear_session(
    parsed: &Value,
    clear_session_on_missing_session: bool,
    resolved_session_id: Option<&str>,
) -> bool {
    clear_session_for_max_turns(parsed)
        || clear_session_for_poison(parsed)
        || (clear_session_on_missing_session && resolved_session_id.is_none())
}

/// Resolve session_id with poison-drop guard（对齐 Node L1049-1050）。
#[must_use]
pub fn resolve_session_id_with_poison_drop(
    raw_session_id: Option<&str>,
    parsed: &Value,
) -> Option<String> {
    if clear_session_for_poison(parsed) {
        return None;
    }
    raw_session_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// 错误族分类（对齐 Node L1041-1058 / L1133-1137）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorFamily {
    #[default]
    None,
    ProviderQuota,
    TransientUpstream,
    ModelRefusal,
}

impl ErrorFamily {
    #[must_use]
    pub const fn as_str(self) -> Option<&'static str> {
        match self {
            ErrorFamily::None => None,
            ErrorFamily::ProviderQuota => Some("provider_quota"),
            ErrorFamily::TransientUpstream => Some("transient_upstream"),
            ErrorFamily::ModelRefusal => Some("model_refusal"),
        }
    }
}

/// provider_quota 检测（对齐 Node L1041-1049）。
#[must_use]
pub fn is_provider_quota(
    failed: bool,
    login_required: bool,
    max_turns: bool,
    poisoned: bool,
    parsed: &Value,
    stdout: &str,
    stderr: &str,
    error_message: Option<&str>,
) -> bool {
    failed
        && !login_required
        && !max_turns
        && !poisoned
        && is_claude_provider_quota_error(Some(parsed), stdout, stderr, error_message)
}

/// transient_upstream 检测（对齐 Node L1051-1058）。
#[must_use]
pub fn is_transient_upstream(
    failed: bool,
    login_required: bool,
    max_turns: bool,
    poisoned: bool,
    provider_quota: bool,
    parsed: &Value,
    stdout: &str,
    stderr: &str,
    error_message: Option<&str>,
) -> bool {
    failed
        && !login_required
        && !max_turns
        && !poisoned
        && !provider_quota
        && is_claude_transient_upstream_error(Some(parsed), stdout, stderr, error_message)
}

/// 错误码解析（对齐 Node L1119-1131）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResolvedErrorCode {
    #[default]
    None,
    ClaudeAuthRequired,
    ModelNotFound,
    MaxTurnsExhausted,
    ClaudePoisonedPreviousMessageId,
    ProviderQuota,
    ClaudeTransientUpstream,
    ClaudeRefusal,
}

impl ResolvedErrorCode {
    #[must_use]
    pub const fn as_str(self) -> Option<&'static str> {
        match self {
            ResolvedErrorCode::None => None,
            ResolvedErrorCode::ClaudeAuthRequired => Some("claude_auth_required"),
            ResolvedErrorCode::ModelNotFound => Some("model_not_found"),
            ResolvedErrorCode::MaxTurnsExhausted => Some("max_turns_exhausted"),
            ResolvedErrorCode::ClaudePoisonedPreviousMessageId => {
                Some("claude_poisoned_previous_message_id")
            }
            ResolvedErrorCode::ProviderQuota => Some("provider_quota"),
            ResolvedErrorCode::ClaudeTransientUpstream => Some("claude_transient_upstream"),
            ResolvedErrorCode::ClaudeRefusal => Some("claude_refusal"),
        }
    }
}

/// 解析最终错误码（Node L1119-1131 的链式 if-else）。
#[must_use]
pub fn resolve_error_code(
    login_required: bool,
    failed: bool,
    parsed: &Value,
    stdout: &str,
    stderr: &str,
    error_message: Option<&str>,
    max_turns: bool,
    poisoned: bool,
    provider_quota: bool,
    transient_upstream: bool,
    claude_refusal: bool,
) -> ResolvedErrorCode {
    if login_required {
        return ResolvedErrorCode::ClaudeAuthRequired;
    }
    if failed
        && is_claude_model_not_found_error(Some(parsed), stdout, stderr, error_message)
    {
        return ResolvedErrorCode::ModelNotFound;
    }
    if failed && max_turns {
        return ResolvedErrorCode::MaxTurnsExhausted;
    }
    if failed && poisoned {
        return ResolvedErrorCode::ClaudePoisonedPreviousMessageId;
    }
    if provider_quota {
        return ResolvedErrorCode::ProviderQuota;
    }
    if transient_upstream {
        return ResolvedErrorCode::ClaudeTransientUpstream;
    }
    if claude_refusal {
        return ResolvedErrorCode::ClaudeRefusal;
    }
    ResolvedErrorCode::None
}

/// 解析 errorFamily（Node L1133-1137）。
#[must_use]
pub fn decide_error_family(
    provider_quota: bool,
    transient_upstream: bool,
    claude_refusal: bool,
) -> ErrorFamily {
    if provider_quota {
        ErrorFamily::ProviderQuota
    } else if transient_upstream {
        ErrorFamily::TransientUpstream
    } else if claude_refusal {
        ErrorFamily::ModelRefusal
    } else {
        ErrorFamily::None
    }
}

/// 合并 result_json（对齐 Node L1147-1156）。
#[must_use]
pub fn merge_result_json(
    parsed: &Value,
    failed: bool,
    max_turns: bool,
    poisoned: bool,
    claude_refusal: bool,
    error_family: ErrorFamily,
    retry_not_before: Option<SystemTime>,
    provider_quota: bool,
    terminal_result_cleanup: Option<&Value>,
) -> Map<String, Value> {
    let mut map: Map<String, Value> = match parsed.as_object() {
        Some(obj) => obj.clone(),
        None => Map::new(),
    };
    if failed && max_turns {
        map.insert(
            "stopReason".to_owned(),
            Value::String("max_turns_exhausted".to_owned()),
        );
    }
    if failed && poisoned {
        map.insert(
            "stopReason".to_owned(),
            Value::String("claude_poisoned_previous_message_id".to_owned()),
        );
    }
    if claude_refusal {
        map.insert("stopReason".to_owned(), Value::String("refusal".to_owned()));
        map.insert(
            "errorFamily".to_owned(),
            Value::String("model_refusal".to_owned()),
        );
    }
    if let Some(family) = error_family.as_str() {
        map.insert("errorFamily".to_owned(), Value::String(family.to_owned()));
    }
    if let Some(retry) = retry_not_before {
        let iso = system_time_to_iso(retry);
        map.insert("retryNotBefore".to_owned(), Value::String(iso.clone()));
        map.insert(
            "transientRetryNotBefore".to_owned(),
            Value::String(iso.clone()),
        );
        if provider_quota {
            map.insert(
                "providerQuotaRetryNotBefore".to_owned(),
                Value::String(iso),
            );
        }
    }
    if let Some(cleanup) = terminal_result_cleanup {
        map.insert("unmanagedBackgroundTask".to_owned(), cleanup.clone());
    }
    map
}

/// SystemTime → ISO 8601 字符串（与 Node `Date.toISOString()` 格式一致）。
#[must_use]
pub fn system_time_to_iso(t: SystemTime) -> String {
    let duration = t
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::from_secs(0));
    let secs = duration.as_secs() as i64;
    let nanos = duration.subsec_nanos();
    format_iso8601(secs, nanos)
}

fn format_iso8601(secs: i64, nanos: u32) -> String {
    let days_since_epoch = secs.div_euclid(86_400);
    let secs_in_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days_since_epoch);
    let hour = secs_in_day / 3600;
    let minute = (secs_in_day % 3600) / 60;
    let second = secs_in_day % 60;
    let millis = nanos / 1_000_000;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
    )
}

// Howard Hinnant 的 civil_from_days 算法（与 chrono 一致）。
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// 计算 usage（对齐 Node L1019-1035）。
#[must_use]
pub fn resolve_usage(parsed: &Value, parsed_stream_usage: Option<&UsageSummary>) -> UsageResolution {
    if let Some(usage) = parsed_stream_usage {
        return UsageResolution {
            usage: Some(usage.clone()),
            basis: UsageBasis::PerRun,
        };
    }
    if let Some(totals) = crate::claude_stream_json::claude_model_usage_totals(
        parsed.get("modelUsage"),
    ) {
        return UsageResolution {
            usage: Some(totals),
            basis: UsageBasis::PerRun,
        };
    }
    let usage_obj = parsed.get("usage").and_then(|v| v.as_object());
    if let Some(obj) = usage_obj {
        let input = obj
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cached = obj
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = obj
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return UsageResolution {
            usage: Some(UsageSummary {
                input_tokens: input,
                cached_input_tokens: Some(cached),
                output_tokens: output,
            }),
            basis: UsageBasis::Unknown,
        };
    }
    UsageResolution::default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageBasis {
    #[default]
    Unknown,
    PerRun,
}

#[derive(Debug, Clone, Default)]
pub struct UsageResolution {
    pub usage: Option<UsageSummary>,
    pub basis: UsageBasis,
}

/// 顶部整合输入参数。
#[derive(Debug, Clone)]
pub struct AssembleInput<'a> {
    pub parsed: &'a Value,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub exit_code: Option<i32>,
    pub login_required: bool,
    pub login_url: Option<&'a str>,
    pub error_message: Option<&'a str>,
    pub fallback_session_id: Option<&'a str>,
    pub config_model: &'a str,
    pub config_billing_type: &'a str,
    pub is_bedrock_auth: bool,
    pub effective_execution_cwd: &'a str,
    pub prompt_bundle_key: &'a str,
    pub mcp_server_identity: &'a str,
    pub workspace_id: Option<&'a str>,
    pub repo_url: Option<&'a str>,
    pub repo_ref: Option<&'a str>,
    pub execution_target_is_remote: bool,
    pub execution_target_session_identity: Option<&'a Value>,
    pub clear_session_on_missing_session: bool,
    pub parsed_stream_session_id: Option<&'a str>,
    pub parsed_stream_model: Option<&'a str>,
    pub parsed_stream_usage: Option<UsageSummary>,
    pub parsed_stream_summary: &'a str,
    pub parsed_stream_cost_usd: Option<f64>,
    pub terminal_result_cleanup: Option<Value>,
    pub now: SystemTime,
}

impl Default for AssembleInput<'_> {
    fn default() -> Self {
        Self {
            parsed: &serde_json::Value::Null,
            stdout: "",
            stderr: "",
            exit_code: None,
            login_required: false,
            login_url: None,
            error_message: None,
            fallback_session_id: None,
            config_model: "",
            config_billing_type: "",
            is_bedrock_auth: false,
            effective_execution_cwd: "",
            prompt_bundle_key: "",
            mcp_server_identity: "",
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            clear_session_on_missing_session: false,
            parsed_stream_session_id: None,
            parsed_stream_model: None,
            parsed_stream_usage: None,
            parsed_stream_summary: "",
            parsed_stream_cost_usd: None,
            terminal_result_cleanup: None,
            now: SystemTime::now(),
        }
    }
}



/// 顶部整合入口（Node toAdapterResult L959-1199 的纯函数版本）。
#[must_use]
pub fn assemble_claude_result(input: &AssembleInput<'_>) -> AdapterExecutionResult {
    let parsed = input.parsed;
    let stdout = input.stdout;
    let stderr = input.stderr;
    let exit_code = input.exit_code;
    let login_required = input.login_required;
    let error_message = input.error_message;

    let succeeded = decide_parsed_success(parsed);
    let failed = decide_failed(parsed, exit_code);
    let max_turns = clear_session_for_max_turns(parsed);
    let poisoned = clear_session_for_poison(parsed);
    let claude_refusal = is_claude_refusal_result(Some(parsed));

    let provider_quota = is_provider_quota(
        failed,
        login_required,
        max_turns,
        poisoned,
        parsed,
        stdout,
        stderr,
        error_message,
    );
    let transient_upstream = is_transient_upstream(
        failed,
        login_required,
        max_turns,
        poisoned,
        provider_quota,
        parsed,
        stdout,
        stderr,
        error_message,
    );
    let retry_not_before = if provider_quota || transient_upstream {
        let mut combined = String::with_capacity(stdout.len() + stderr.len() + 32);
        combined.push_str(stdout);
        combined.push('\n');
        combined.push_str(stderr);
        if let Some(msg) = error_message {
            combined.push('\n');
            combined.push_str(msg);
        }
        extract_claude_retry_not_before(&combined, input.now)
    } else {
        None
    };

    let error_family = decide_error_family(provider_quota, transient_upstream, claude_refusal);
    let error_code = resolve_error_code(
        login_required,
        failed,
        parsed,
        stdout,
        stderr,
        error_message,
        max_turns,
        poisoned,
        provider_quota,
        transient_upstream,
        claude_refusal,
    );

    let raw_session_id = input
        .parsed_stream_session_id
        .map(str::to_owned)
        .or_else(|| input.fallback_session_id.map(str::to_owned))
        .or_else(|| {
            parsed
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        });
    let resolved_session_id =
        resolve_session_id_with_poison_drop(raw_session_id.as_deref(), parsed);

    let resolved_session_params = if let Some(sid) = resolved_session_id.as_deref() {
        let params_input = crate::claude_session_params::ResolvedSessionParamsInput {
            session_id: Some(sid),
            cwd: input.effective_execution_cwd,
            prompt_bundle_key: input.prompt_bundle_key,
            mcp_server_identity: input.mcp_server_identity,
            execution_target_is_remote: input.execution_target_is_remote,
            execution_target_session_identity: input.execution_target_session_identity,
            workspace_id: input.workspace_id,
            repo_url: input.repo_url,
            repo_ref: input.repo_ref,
        };
        crate::claude_session_params::build_resolved_session_params(&params_input)
    } else {
        None
    };

    let final_error_message = if failed {
        describe_claude_failure(parsed)
            .or_else(|| error_message.map(str::to_owned))
            .unwrap_or_else(|| format!("Claude exited with code {}", exit_code.unwrap_or(-1)))
    } else if let Some(msg) = error_message {
        msg.to_owned()
    } else {
        String::new()
    };

    let biller = if input.is_bedrock_auth {
        "aws_bedrock"
    } else {
        "anthropic"
    };

    let model_resolution = input
        .parsed_stream_model
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            parsed
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| input.config_model.to_owned());
    let final_model = if !input.is_bedrock_auth || is_bedrock_model_id(&model_resolution) {
        model_resolution
    } else {
        input.config_model.to_owned()
    };

    let usage_resolution = resolve_usage(parsed, input.parsed_stream_usage.as_ref());

    let merged_json = merge_result_json(
        parsed,
        failed,
        max_turns,
        poisoned,
        claude_refusal,
        error_family,
        retry_not_before,
        provider_quota,
        input.terminal_result_cleanup.as_ref(),
    );

    // AdapterExecutionResult 不含 biller 字段；放入 result_json 中。
    let mut merged_json = merged_json;
    merged_json.insert("biller".to_owned(), Value::String(biller.to_owned()));

    let clear_session = decide_clear_session(
        parsed,
        input.clear_session_on_missing_session,
        resolved_session_id.as_deref(),
    );

    let summary = if !input.parsed_stream_summary.is_empty() {
        input.parsed_stream_summary.to_owned()
    } else {
        parsed
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned()
    };

    AdapterExecutionResult {
        exit_code,
        signal: None,
        timed_out: false,
        error_message: if final_error_message.is_empty() {
            None
        } else {
            Some(final_error_message)
        },
        error_code: error_code.as_str().map(str::to_owned),
        provider: Some("anthropic".to_owned()),
        model: Some(final_model),
        billing_type: Some(input.config_billing_type.to_owned()),
        cost_usd: input.parsed_stream_cost_usd,
        result_json: Some(Value::Object(merged_json)),
        usage: usage_resolution.usage,
        session_id: resolved_session_id.clone(),
        session_params: resolved_session_params,
        session_display_id: resolved_session_id,
        summary: if summary.is_empty() {
            None
        } else {
            Some(summary)
        },
        clear_session,
        ..AdapterExecutionResult::default()
    }
}

/// 简化入口：登录元数据（loginUrl）。
#[must_use]
pub fn build_login_error_meta(login_url: Option<&str>) -> Option<Map<String, Value>> {
    login_url.map(|url| {
        let mut map = Map::new();
        map.insert("loginUrl".to_owned(), Value::String(url.to_owned()));
        map
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_parsed_success() -> Value {
        json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "session_id": "abc-123",
            "result": "Hello world",
            "model": "claude-opus-4-7",
            "usage": {
                "input_tokens": 100,
                "cache_read_input_tokens": 50,
                "output_tokens": 200,
            },
        })
    }

    #[test]
    fn fallback_error_message_zero_exit_says_failed_to_parse() {
        let msg = parse_fallback_error_message("noise", Some(0));
        assert!(msg.contains("Failed to parse"));
    }

    #[test]
    fn fallback_error_message_nonzero_exit_includes_stderr() {
        let msg = parse_fallback_error_message("boom\n", Some(1));
        assert!(msg.contains("Claude exited with code 1"));
        assert!(msg.contains("boom"));
    }

    #[test]
    fn fallback_error_message_no_stderr() {
        let msg = parse_fallback_error_message("", Some(2));
        assert_eq!(msg, "Claude exited with code 2");
    }

    #[test]
    fn parsed_success_when_subtype_success_and_no_error() {
        let parsed = sample_parsed_success();
        assert!(decide_parsed_success(&parsed));
        assert!(!decide_failed(&parsed, Some(0)));
    }

    #[test]
    fn parsed_not_success_when_is_error() {
        let mut parsed = sample_parsed_success();
        parsed["is_error"] = json!(true);
        assert!(!decide_parsed_success(&parsed));
    }

    #[test]
    fn parsed_not_success_when_subtype_failure() {
        let mut parsed = sample_parsed_success();
        parsed["subtype"] = json!("failure");
        assert!(!decide_parsed_success(&parsed));
    }

    #[test]
    fn parsed_not_failed_when_exit_nonzero_but_subtype_success() {
        // 对齐 Node L1038：subtype=success + !is_error 时 failed=false，即使 exit_code != 0。
        let parsed = sample_parsed_success();
        assert!(!decide_failed(&parsed, Some(1)));
    }

    #[test]
    fn parsed_failed_when_subtype_failure_and_is_error() {
        let mut parsed = sample_parsed_success();
        parsed["subtype"] = json!("failure");
        parsed["is_error"] = json!(true);
        assert!(decide_failed(&parsed, Some(0)));
    }

    #[test]
    fn clear_session_for_max_turns_true_when_marker_present() {
        let parsed = json!({"subtype": "max_turns_exhausted", "is_error": true});
        assert!(clear_session_for_max_turns(&parsed));
    }

    #[test]
    fn clear_session_for_poison_true_on_marker() {
        let parsed = json!({
            "errors": [{
                "message": "invalid request: diagnostics.previous_message_id 'abc' starts with `msg_` invalid",
            }],
        });
        assert!(clear_session_for_poison(&parsed));
    }

    #[test]
    fn decide_clear_session_max_turns() {
        let parsed = json!({"subtype": "max_turns_exhausted", "is_error": true});
        assert!(decide_clear_session(&parsed, false, Some("session-1")));
    }

    #[test]
    fn decide_clear_session_poison() {
        let parsed = json!({
            "errors": [{
                "message": "diagnostics.previous_message_id 'x' starts with `msg_` invalid",
            }],
        });
        assert!(decide_clear_session(&parsed, false, Some("session-1")));
    }

    #[test]
    fn decide_clear_session_on_missing() {
        let parsed = json!({"subtype": "success"});
        assert!(decide_clear_session(&parsed, true, None));
    }

    #[test]
    fn decide_clear_session_false_when_no_reasons() {
        let parsed = json!({"subtype": "success"});
        assert!(!decide_clear_session(&parsed, false, Some("session-1")));
    }

    #[test]
    fn resolve_session_id_keeps_when_not_poison() {
        let parsed = json!({"subtype": "success"});
        assert_eq!(
            resolve_session_id_with_poison_drop(Some("session-1"), &parsed),
            Some("session-1".to_owned())
        );
    }

    #[test]
    fn resolve_session_id_drops_when_poison() {
        let parsed = json!({
            "errors": [{
                "message": "diagnostics.previous_message_id 'x' starts with `msg_` invalid",
            }],
        });
        assert_eq!(
            resolve_session_id_with_poison_drop(Some("session-1"), &parsed),
            None
        );
    }

    #[test]
    fn resolve_session_id_none_input_returns_none() {
        let parsed = json!({"subtype": "success"});
        assert_eq!(
            resolve_session_id_with_poison_drop(None, &parsed),
            None
        );
    }

    #[test]
    fn resolve_error_code_login_required_takes_priority() {
        assert_eq!(
            resolve_error_code(
                true, true, &json!({}), "", "", Some("err"), false, false, false, false, false,
            ),
            ResolvedErrorCode::ClaudeAuthRequired
        );
    }

    #[test]
    fn resolve_error_code_model_not_found() {
        assert_eq!(
            resolve_error_code(
                false, true, &json!({}), "", "model not found: x", Some("err"),
                false, false, false, false, false,
            ),
            ResolvedErrorCode::ModelNotFound
        );
    }

    #[test]
    fn resolve_error_code_max_turns() {
        let parsed = json!({"is_error": true, "subtype": "max_turns_exhausted"});
        assert_eq!(
            resolve_error_code(
                false, true, &parsed, "", "", Some("err"),
                true, false, false, false, false,
            ),
            ResolvedErrorCode::MaxTurnsExhausted
        );
    }

    #[test]
    fn resolve_error_code_poisoned() {
        let parsed = json!({"errors": [{"message": "diagnostics.previous_message_id 'x' starts with `msg_` invalid"}]});
        assert_eq!(
            resolve_error_code(
                false, true, &parsed, "", "", Some("err"),
                false, true, false, false, false,
            ),
            ResolvedErrorCode::ClaudePoisonedPreviousMessageId
        );
    }

    #[test]
    fn resolve_error_code_provider_quota() {
        let parsed = json!({"is_error": true, "errors": [{"message": "weekly limit reached"}]});
        assert_eq!(
            resolve_error_code(
                false, true, &parsed, "", "weekly limit reached", Some("err"),
                false, false, true, false, false,
            ),
            ResolvedErrorCode::ProviderQuota
        );
    }

    #[test]
    fn resolve_error_code_transient_upstream() {
        let parsed = json!({"is_error": true, "errors": [{"message": "529 overloaded"}]});
        assert_eq!(
            resolve_error_code(
                false, true, &parsed, "", "529 overloaded", Some("err"),
                false, false, false, true, false,
            ),
            ResolvedErrorCode::ClaudeTransientUpstream
        );
    }

    #[test]
    fn resolve_error_code_refusal() {
        let parsed = json!({"subtype": "refusal", "is_error": false});
        assert_eq!(
            resolve_error_code(
                false, false, &parsed, "", "", None,
                false, false, false, false, true,
            ),
            ResolvedErrorCode::ClaudeRefusal
        );
    }

    #[test]
    fn resolve_error_code_none_when_no_match() {
        assert_eq!(
            resolve_error_code(
                false, false, &json!({}), "", "", None,
                false, false, false, false, false,
            ),
            ResolvedErrorCode::None
        );
    }

    #[test]
    fn decide_error_family_provider_quota() {
        assert_eq!(
            decide_error_family(true, false, false),
            ErrorFamily::ProviderQuota
        );
    }

    #[test]
    fn decide_error_family_transient_upstream() {
        assert_eq!(
            decide_error_family(false, true, false),
            ErrorFamily::TransientUpstream
        );
    }

    #[test]
    fn decide_error_family_model_refusal() {
        assert_eq!(
            decide_error_family(false, false, true),
            ErrorFamily::ModelRefusal
        );
    }

    #[test]
    fn decide_error_family_priority_provider_quota_first() {
        assert_eq!(
            decide_error_family(true, true, true),
            ErrorFamily::ProviderQuota
        );
    }

    #[test]
    fn merge_result_json_keeps_parsed_fields() {
        let parsed = json!({"session_id": "x", "model": "y"});
        let merged = merge_result_json(
            &parsed, false, false, false, false, ErrorFamily::None, None, false, None,
        );
        assert_eq!(merged.get("session_id").and_then(|v| v.as_str()), Some("x"));
        assert_eq!(merged.get("model").and_then(|v| v.as_str()), Some("y"));
    }

    #[test]
    fn merge_result_json_includes_max_turns_stop_reason() {
        let parsed = json!({});
        let merged = merge_result_json(
            &parsed, true, true, false, false, ErrorFamily::None, None, false, None,
        );
        assert_eq!(
            merged.get("stopReason").and_then(|v| v.as_str()),
            Some("max_turns_exhausted")
        );
    }

    #[test]
    fn merge_result_json_includes_poisoned_stop_reason() {
        let parsed = json!({});
        let merged = merge_result_json(
            &parsed, true, false, true, false, ErrorFamily::None, None, false, None,
        );
        assert_eq!(
            merged.get("stopReason").and_then(|v| v.as_str()),
            Some("claude_poisoned_previous_message_id")
        );
    }

    #[test]
    fn merge_result_json_refusal_includes_stop_reason_and_family() {
        let parsed = json!({});
        let merged = merge_result_json(
            &parsed, false, false, false, true, ErrorFamily::None, None, false, None,
        );
        assert_eq!(
            merged.get("stopReason").and_then(|v| v.as_str()),
            Some("refusal")
        );
        assert_eq!(
            merged.get("errorFamily").and_then(|v| v.as_str()),
            Some("model_refusal")
        );
    }

    #[test]
    fn merge_result_json_includes_retry_not_before_for_provider_quota() {
        let parsed = json!({});
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let merged = merge_result_json(
            &parsed,
            true,
            false,
            false,
            false,
            ErrorFamily::ProviderQuota,
            Some(now),
            true,
            None,
        );
        assert!(merged.get("retryNotBefore").is_some());
        assert!(merged.get("transientRetryNotBefore").is_some());
        assert!(merged.get("providerQuotaRetryNotBefore").is_some());
    }

    #[test]
    fn merge_result_json_omits_retry_not_before_for_transient_only() {
        let parsed = json!({});
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let merged = merge_result_json(
            &parsed,
            true,
            false,
            false,
            false,
            ErrorFamily::TransientUpstream,
            Some(now),
            false,
            None,
        );
        assert!(merged.get("retryNotBefore").is_some());
        assert!(merged.get("providerQuotaRetryNotBefore").is_none());
    }

    #[test]
    fn merge_result_json_includes_terminal_cleanup() {
        let parsed = json!({});
        let cleanup = json!({"reason": "shell_left_running"});
        let merged = merge_result_json(
            &parsed,
            false,
            false,
            false,
            false,
            ErrorFamily::None,
            None,
            false,
            Some(&cleanup),
        );
        assert_eq!(
            merged
                .get("unmanagedBackgroundTask")
                .and_then(|v| v.get("reason"))
                .and_then(|v| v.as_str()),
            Some("shell_left_running")
        );
    }

    #[test]
    fn resolve_usage_uses_parsed_stream_first() {
        let parsed = json!({"usage": {"input_tokens": 100}});
        let usage = UsageSummary {
            input_tokens: 1,
            output_tokens: 2,
            cached_input_tokens: Some(3),
        };
        let resolution = resolve_usage(&parsed, Some(&usage));
        assert_eq!(resolution.usage.as_ref().unwrap().input_tokens, 1);
        assert_eq!(resolution.basis, UsageBasis::PerRun);
    }

    #[test]
    fn resolve_usage_falls_back_to_model_usage() {
        let parsed = json!({
            "modelUsage": {
                "claude-opus-4-7": {
                    "inputTokens": 10,
                    "outputTokens": 20,
                }
            }
        });
        let resolution = resolve_usage(&parsed, None);
        assert!(resolution.usage.is_some());
        assert_eq!(resolution.basis, UsageBasis::PerRun);
    }

    #[test]
    fn resolve_usage_falls_back_to_parsed_usage() {
        let parsed =
            json!({"usage": {"input_tokens": 5, "output_tokens": 6, "cache_read_input_tokens": 1}});
        let resolution = resolve_usage(&parsed, None);
        let usage = resolution.usage.expect("usage");
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 6);
        assert_eq!(usage.cached_input_tokens, Some(1));
    }

    #[test]
    fn resolve_usage_returns_none_when_no_data() {
        let parsed = json!({});
        let resolution = resolve_usage(&parsed, None);
        assert!(resolution.usage.is_none());
    }

    #[test]
    fn build_login_error_meta_includes_url() {
        let meta = build_login_error_meta(Some("https://example.com/login"));
        let meta = meta.expect("Some");
        assert_eq!(
            meta.get("loginUrl").and_then(|v| v.as_str()),
            Some("https://example.com/login")
        );
    }

    #[test]
    fn build_login_error_meta_none_when_no_url() {
        assert!(build_login_error_meta(None).is_none());
    }

    #[test]
    fn assemble_success_result() {
        let parsed = sample_parsed_success();
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let input = AssembleInput {
            parsed: &parsed,
            stdout: "",
            stderr: "",
            exit_code: Some(0),
            login_required: false,
            login_url: None,
            error_message: None,
            fallback_session_id: None,
            config_model: "claude-opus-4-7",
            config_billing_type: "api",
            is_bedrock_auth: false,
            effective_execution_cwd: "/repo",
            prompt_bundle_key: "bundle-a",
            mcp_server_identity: "[]",
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            clear_session_on_missing_session: false,
            parsed_stream_session_id: Some("abc-123"),
            parsed_stream_model: Some("claude-opus-4-7"),
            parsed_stream_usage: Some(UsageSummary {
                input_tokens: 100,
                output_tokens: 200,
                cached_input_tokens: Some(50),
            }),
            parsed_stream_summary: "Hello world",
            parsed_stream_cost_usd: Some(0.05),
            terminal_result_cleanup: None,
            now,
        };
        let result = assemble_claude_result(&input);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.error_message.is_none() || result.error_message.as_deref() == Some(""));
        assert_eq!(result.session_id.as_deref(), Some("abc-123"));
        assert_eq!(result.provider.as_deref(), Some("anthropic"));
        assert_eq!(result.result_json.as_ref().and_then(|v| v.get("biller")).and_then(|v| v.as_str()), Some("anthropic"));
        assert_eq!(result.model.as_deref(), Some("claude-opus-4-7"));
        assert!(result.session_params.is_some());
        assert!(!result.clear_session);
    }

    #[test]
    fn assemble_max_turns_emits_error_code_and_clear_session() {
        let parsed = json!({
            "subtype": "max_turns_exhausted",
            "is_error": true,
            "session_id": "abc-123",
        });
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let input = AssembleInput {
            parsed: &parsed,
            stdout: "",
            stderr: "",
            exit_code: Some(0),
            login_required: false,
            login_url: None,
            error_message: None,
            fallback_session_id: None,
            config_model: "claude-opus-4-7",
            config_billing_type: "api",
            is_bedrock_auth: false,
            effective_execution_cwd: "/repo",
            prompt_bundle_key: "bundle-a",
            mcp_server_identity: "[]",
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            clear_session_on_missing_session: false,
            parsed_stream_session_id: Some("abc-123"),
            parsed_stream_model: None,
            parsed_stream_usage: None,
            parsed_stream_summary: "",
            parsed_stream_cost_usd: None,
            terminal_result_cleanup: None,
            now,
        };
        let result = assemble_claude_result(&input);
        assert_eq!(result.error_code.as_deref(), Some("max_turns_exhausted"));
        assert!(result.clear_session);
    }

    #[test]
    fn assemble_poisoned_session_drops_session_id() {
        let parsed = json!({
            "errors": [{"message": "diagnostics.previous_message_id 'x' starts with `msg_` invalid"}],
            "session_id": "abc-123",
        });
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let input = AssembleInput {
            parsed: &parsed,
            stdout: "",
            stderr: "",
            exit_code: Some(1),
            login_required: false,
            login_url: None,
            error_message: None,
            fallback_session_id: None,
            config_model: "claude-opus-4-7",
            config_billing_type: "api",
            is_bedrock_auth: false,
            effective_execution_cwd: "/repo",
            prompt_bundle_key: "bundle-a",
            mcp_server_identity: "[]",
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            clear_session_on_missing_session: false,
            parsed_stream_session_id: Some("abc-123"),
            parsed_stream_model: None,
            parsed_stream_usage: None,
            parsed_stream_summary: "",
            parsed_stream_cost_usd: None,
            terminal_result_cleanup: None,
            now,
        };
        let result = assemble_claude_result(&input);
        assert!(result.session_id.is_none());
        assert!(result.clear_session);
        assert_eq!(
            result.error_code.as_deref(),
            Some("claude_poisoned_previous_message_id")
        );
    }

    #[test]
    fn assemble_provider_quota_emits_error_family() {
        let parsed = json!({
            "is_error": true,
            "errors": [{"message": "weekly limit reached for claude"}],
            "session_id": "abc-123",
        });
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let input = AssembleInput {
            parsed: &parsed,
            stdout: "",
            stderr: "weekly limit reached",
            exit_code: Some(1),
            login_required: false,
            login_url: None,
            error_message: Some("weekly limit reached"),
            fallback_session_id: None,
            config_model: "claude-opus-4-7",
            config_billing_type: "api",
            is_bedrock_auth: false,
            effective_execution_cwd: "/repo",
            prompt_bundle_key: "bundle-a",
            mcp_server_identity: "[]",
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            clear_session_on_missing_session: false,
            parsed_stream_session_id: Some("abc-123"),
            parsed_stream_model: None,
            parsed_stream_usage: None,
            parsed_stream_summary: "",
            parsed_stream_cost_usd: None,
            terminal_result_cleanup: None,
            now,
        };
        let result = assemble_claude_result(&input);
        assert_eq!(result.error_code.as_deref(), Some("provider_quota"));
        let result_json = result.result_json.expect("result_json");
        assert_eq!(
            result_json.get("errorFamily").and_then(|v| v.as_str()),
            Some("provider_quota")
        );
    }

    #[test]
    fn assemble_login_required_takes_priority() {
        let parsed = json!({});
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let input = AssembleInput {
            parsed: &parsed,
            stdout: "Please login first",
            stderr: "",
            exit_code: Some(0),
            login_required: true,
            login_url: Some("https://example.com/login"),
            error_message: None,
            fallback_session_id: None,
            config_model: "claude-opus-4-7",
            config_billing_type: "api",
            is_bedrock_auth: false,
            effective_execution_cwd: "/repo",
            prompt_bundle_key: "bundle-a",
            mcp_server_identity: "[]",
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            clear_session_on_missing_session: false,
            parsed_stream_session_id: None,
            parsed_stream_model: None,
            parsed_stream_usage: None,
            parsed_stream_summary: "",
            parsed_stream_cost_usd: None,
            terminal_result_cleanup: None,
            now,
        };
        let result = assemble_claude_result(&input);
        assert_eq!(result.error_code.as_deref(), Some("claude_auth_required"));
    }

    #[test]
    fn assemble_bedrock_sets_aws_bedrock_biller() {
        let parsed = sample_parsed_success();
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let input = AssembleInput {
            parsed: &parsed,
            stdout: "",
            stderr: "",
            exit_code: Some(0),
            login_required: false,
            login_url: None,
            error_message: None,
            fallback_session_id: None,
            config_model: "us.anthropic.claude-opus-4-7",
            config_billing_type: "metered_api",
            is_bedrock_auth: true,
            effective_execution_cwd: "/repo",
            prompt_bundle_key: "bundle-a",
            mcp_server_identity: "[]",
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            clear_session_on_missing_session: false,
            parsed_stream_session_id: Some("abc-123"),
            parsed_stream_model: Some("us.anthropic.claude-opus-4-7"),
            parsed_stream_usage: None,
            parsed_stream_summary: "",
            parsed_stream_cost_usd: None,
            terminal_result_cleanup: None,
            now,
        };
        let result = assemble_claude_result(&input);
        assert_eq!(result.result_json.as_ref().and_then(|v| v.get("biller")).and_then(|v| v.as_str()), Some("aws_bedrock"));
    }

    #[test]
    fn assemble_remote_session_includes_remote_execution() {
        let parsed = sample_parsed_success();
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let identity = json!({"id": "ssh-1", "port": 22});
        let input = AssembleInput {
            parsed: &parsed,
            stdout: "",
            stderr: "",
            exit_code: Some(0),
            login_required: false,
            login_url: None,
            error_message: None,
            fallback_session_id: None,
            config_model: "claude-opus-4-7",
            config_billing_type: "api",
            is_bedrock_auth: false,
            effective_execution_cwd: "/remote/repo",
            prompt_bundle_key: "bundle-a",
            mcp_server_identity: "[]",
            workspace_id: Some("ws-1"),
            repo_url: Some("git@github.com:foo/bar.git"),
            repo_ref: Some("main"),
            execution_target_is_remote: true,
            execution_target_session_identity: Some(&identity),
            clear_session_on_missing_session: false,
            parsed_stream_session_id: Some("abc-123"),
            parsed_stream_model: Some("claude-opus-4-7"),
            parsed_stream_usage: None,
            parsed_stream_summary: "",
            parsed_stream_cost_usd: None,
            terminal_result_cleanup: None,
            now,
        };
        let result = assemble_claude_result(&input);
        let params = result.session_params.expect("session_params");
        assert_eq!(params["remoteExecution"], identity);
        assert_eq!(params["workspaceId"], "ws-1");
    }

    #[test]
    fn assemble_no_session_id_returns_no_session_params() {
        let parsed = json!({"subtype": "success"});
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let input = AssembleInput {
            parsed: &parsed,
            stdout: "",
            stderr: "",
            exit_code: Some(0),
            login_required: false,
            login_url: None,
            error_message: None,
            fallback_session_id: None,
            config_model: "claude-opus-4-7",
            config_billing_type: "api",
            is_bedrock_auth: false,
            effective_execution_cwd: "/repo",
            prompt_bundle_key: "bundle-a",
            mcp_server_identity: "[]",
            workspace_id: None,
            repo_url: None,
            repo_ref: None,
            execution_target_is_remote: false,
            execution_target_session_identity: None,
            clear_session_on_missing_session: false,
            parsed_stream_session_id: None,
            parsed_stream_model: None,
            parsed_stream_usage: None,
            parsed_stream_summary: "",
            parsed_stream_cost_usd: None,
            terminal_result_cleanup: None,
            now,
        };
        let result = assemble_claude_result(&input);
        assert!(result.session_id.is_none());
        assert!(result.session_params.is_none());
    }

    #[test]
    fn system_time_to_iso_format() {
        let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let iso = system_time_to_iso(t);
        assert!(iso.starts_with("2023-"));
        assert!(iso.ends_with("Z"));
    }
}

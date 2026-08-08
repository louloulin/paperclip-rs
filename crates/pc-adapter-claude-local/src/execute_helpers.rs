//! Claude-local execute 助手函数。
//!
//! 完整复刻 Node `packages/adapters/claude-local/src/server/execute.ts`
//! 中与 session cwd 比对、Bedrock 认证检测、billing 类型解析相关的纯函数。

use std::collections::BTreeMap;

use pc_acpx::env_helpers::has_non_empty_env_value;

/// Claude 的 billing 模式。
///
/// Node 等价：`resolveClaudeBillingType` 的返回类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeBillingType {
    /// 直接调用 Anthropic API（环境中有 `ANTHROPIC_API_KEY`）。
    Api,
    /// Claude Pro/Max 订阅（无 API key，但走 claude.ai 登录）。
    Subscription,
    /// 通过 AWS Bedrock 中转（`CLAUDE_CODE_USE_BEDROCK=1` 或 `ANTHROPIC_BEDROCK_BASE_URL`）。
    MeteredApi,
}

impl ClaudeBillingType {
    /// 转为 wire-format 字符串（Node 一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            ClaudeBillingType::Api => "api",
            ClaudeBillingType::Subscription => "subscription",
            ClaudeBillingType::MeteredApi => "metered_api",
        }
    }
}

/// 判断环境变量是否启用 AWS Bedrock 认证。
///
/// Node 等价：`isBedrockAuth`。触发条件：
/// - `CLAUDE_CODE_USE_BEDROOK == "1"` 或 `"true"`，或
/// - `ANTHROPIC_BEDROCK_BASE_URL` 非空。
pub fn is_bedrock_auth(env: &BTreeMap<String, String>) -> bool {
    matches!(
        env.get("CLAUDE_CODE_USE_BEDROCK").map(String::as_str),
        Some("1") | Some("true")
    ) || has_non_empty_env_value(env, "ANTHROPIC_BEDROCK_BASE_URL")
}

/// 解析 Claude 的 billing 类型。
///
/// Node 等价：`resolveClaudeBillingType`。
/// - Bedrock 优先 → `MeteredApi`
/// - `ANTHROPIC_API_KEY` 非空 → `Api`
/// - 否则 → `Subscription`
pub fn resolve_claude_billing_type(env: &BTreeMap<String, String>) -> ClaudeBillingType {
    if is_bedrock_auth(env) {
        ClaudeBillingType::MeteredApi
    } else if has_non_empty_env_value(env, "ANTHROPIC_API_KEY") {
        ClaudeBillingType::Api
    } else {
        ClaudeBillingType::Subscription
    }
}

/// 判断 runtime session cwd 是否与 effective execution cwd 匹配。
///
/// Node 等价：`claudeSessionCwdMatchesExecutionTarget`。
/// - 远程执行：永远返回 `true`（cwd 由 execution target 决定）。
/// - runtime session cwd 为空：返回 `true`（无 cwd 信息，宽松通过）。
/// - 否则：规范化后字符串比较。
pub fn claude_session_cwd_matches_execution_target(
    runtime_session_cwd: &str,
    effective_execution_cwd: &str,
    execution_target_is_remote: bool,
) -> bool {
    if execution_target_is_remote || runtime_session_cwd.trim().is_empty() {
        return true;
    }
    pc_acpx::paths::cwds_match(runtime_session_cwd, effective_execution_cwd)
}

/// Claude adapter 单一 attempt 的"错误族"分类。
///
/// Node 等价：`errorFamily` 字段在 Node 里有 `provider_quota` / `transient_upstream`
/// 两个值。Rust 端我们用枚举表达相同概念。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeErrorFamily {
    None,
    ProviderQuota,
    TransientUpstream,
    MaxTurns,
    PoisonedPreviousMessageId,
    Refusal,
    ModelRefusal,
    UnknownSession,
}

impl ClaudeErrorFamily {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClaudeErrorFamily::None => "",
            ClaudeErrorFamily::ProviderQuota => "provider_quota",
            ClaudeErrorFamily::TransientUpstream => "transient_upstream",
            ClaudeErrorFamily::MaxTurns => "max_turns",
            ClaudeErrorFamily::PoisonedPreviousMessageId => "claude_poisoned_previous_message_id",
            ClaudeErrorFamily::Refusal => "refusal",
            ClaudeErrorFamily::ModelRefusal => "model_refusal",
            ClaudeErrorFamily::UnknownSession => "unknown_session",
        }
    }
}

/// Claude adapter 的 retry 决策输入快照。
#[derive(Debug, Clone)]
pub struct ClaudeRetryInput<'a> {
    pub session_id: &'a str,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub parsed: Option<&'a crate::claude_stream_json::ParsedClaudeStreamJson>,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub error_message: Option<&'a str>,
}

/// Claude adapter 一次 attempt 的综合分类结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeRetryDecision {
    pub error_family: ClaudeErrorFamily,
    pub clear_session: bool,
    pub provider_quota: bool,
    pub transient_upstream: bool,
}

/// 在 `execute()` 主循环里复用：把首轮 attempt 多种判断压缩成单一决策，
/// 与 Node `errorFamily` + `retryNotBefore` + `clearSession` 三者等价。
pub fn decide_retry(input: ClaudeRetryInput<'_>) -> ClaudeRetryDecision {
    use crate::claude_stream_json::ParsedClaudeStreamJson;
    let failed = !input.timed_out && input.exit_code.unwrap_or(0) != 0;
    let parsed = input.parsed;
    let unknown_session = parsed
        .map(|p| crate::claude_stream_json::is_claude_unknown_session_error(p))
        .unwrap_or(false)
        && !input.session_id.is_empty();
    let max_turns = parsed
        .map(crate::claude_stream_json::is_claude_max_turns_result)
        .unwrap_or(false);
    let poisoned = parsed
        .map(crate::claude_stream_json::is_claude_poisoned_previous_message_id_error)
        .unwrap_or(false);
    let refusal = parsed
        .map(crate::claude_stream_json::is_claude_refusal_result)
        .unwrap_or(false);
    let provider_quota = failed
        && !max_turns
        && !poisoned
        && parsed
            .map(|p| crate::claude_stream_json::is_claude_provider_quota_error(
                p,
                input.stdout,
                input.stderr,
                input.error_message.unwrap_or(""),
            ))
            .unwrap_or(false);
    let transient_upstream = failed
        && !max_turns
        && !poisoned
        && !provider_quota
        && parsed
            .map(|p| crate::claude_stream_json::is_claude_transient_upstream_error(
                p,
                input.stdout,
                input.stderr,
                input.error_message.unwrap_or(""),
            ))
            .unwrap_or(false);

    let error_family = if refusal {
        ClaudeErrorFamily::Refusal
    } else if max_turns {
        ClaudeErrorFamily::MaxTurns
    } else if poisoned {
        ClaudeErrorFamily::PoisonedPreviousMessageId
    } else if provider_quota {
        ClaudeErrorFamily::ProviderQuota
    } else if transient_upstream {
        ClaudeErrorFamily::TransientUpstream
    } else if unknown_session {
        ClaudeErrorFamily::UnknownSession
    } else {
        ClaudeErrorFamily::None
    };

    let clear_session = max_turns || poisoned || unknown_session;
    ClaudeRetryDecision {
        error_family,
        clear_session,
        provider_quota,
        transient_upstream,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn bedrock_auth_use_bedrock_1() {
        let env = env_from(&[("CLAUDE_CODE_USE_BEDROCK", "1")]);
        assert!(is_bedrock_auth(&env));
    }

    #[test]
    fn bedrock_auth_use_bedrock_true() {
        let env = env_from(&[("CLAUDE_CODE_USE_BEDROCK", "true")]);
        assert!(is_bedrock_auth(&env));
    }

    #[test]
    fn bedrock_auth_use_bedrock_其他值不触发() {
        let env = env_from(&[("CLAUDE_CODE_USE_BEDROCK", "0")]);
        assert!(!is_bedrock_auth(&env));
        let env = env_from(&[("CLAUDE_CODE_USE_BEDROCK", "false")]);
        assert!(!is_bedrock_auth(&env));
        let env = env_from(&[("CLAUDE_CODE_USE_BEDROCK", "")]);
        assert!(!is_bedrock_auth(&env));
    }

    #[test]
    fn bedrock_auth_base_url非空() {
        let env = env_from(&[("ANTHROPIC_BEDROCK_BASE_URL", "https://bedrock.example")]);
        assert!(is_bedrock_auth(&env));
    }

    #[test]
    fn bedrock_auth_base_url空白不触发() {
        let env = env_from(&[("ANTHROPIC_BEDROCK_BASE_URL", "   ")]);
        assert!(!is_bedrock_auth(&env));
    }

    #[test]
    fn bedrock_auth_都不存在() {
        let env = env_from(&[]);
        assert!(!is_bedrock_auth(&env));
    }

    #[test]
    fn billing_type_metered_api优先() {
        // 即使有 API key，Bedrock 仍优先。
        let env = env_from(&[
            ("ANTHROPIC_API_KEY", "sk-test"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
        ]);
        assert_eq!(
            resolve_claude_billing_type(&env),
            ClaudeBillingType::MeteredApi
        );
    }

    #[test]
    fn billing_type_api有api_key() {
        let env = env_from(&[("ANTHROPIC_API_KEY", "sk-test")]);
        assert_eq!(
            resolve_claude_billing_type(&env),
            ClaudeBillingType::Api
        );
    }

    #[test]
    fn billing_type_api_key空白当subscription() {
        let env = env_from(&[("ANTHROPIC_API_KEY", "")]);
        assert_eq!(
            resolve_claude_billing_type(&env),
            ClaudeBillingType::Subscription
        );
    }

    #[test]
    fn billing_type_subscription默认() {
        let env = env_from(&[]);
        assert_eq!(
            resolve_claude_billing_type(&env),
            ClaudeBillingType::Subscription
        );
    }

    #[test]
    fn billing_type_as_str_映射() {
        assert_eq!(ClaudeBillingType::Api.as_str(), "api");
        assert_eq!(ClaudeBillingType::Subscription.as_str(), "subscription");
        assert_eq!(ClaudeBillingType::MeteredApi.as_str(), "metered_api");
    }

    #[test]
    fn session_cwd_remote_总是true() {
        assert!(claude_session_cwd_matches_execution_target(
            "/any/where",
            "/current/here",
            true,
        ));
    }

    #[test]
    fn session_cwd_空cwd_总是true() {
        assert!(claude_session_cwd_matches_execution_target(
            "",
            "/current/here",
            false,
        ));
        assert!(claude_session_cwd_matches_execution_target(
            "   ",
            "/current/here",
            false,
        ));
    }

    #[test]
    fn session_cwd_一致() {
        assert!(claude_session_cwd_matches_execution_target(
            "/home/u/proj",
            "/home/u/proj",
            false,
        ));
        // 规范化后一致
        assert!(claude_session_cwd_matches_execution_target(
            "/home/u/proj/.",
            "/home/u/proj",
            false,
        ));
    }

    #[test]
    fn session_cwd_不一致() {
        assert!(!claude_session_cwd_matches_execution_target(
            "/home/u/proj",
            "/home/u/other",
            false,
        ));
        assert!(!claude_session_cwd_matches_execution_target(
            "/home/u/proj",
            "/home/u/proj/sub",
            false,
        ));
    }
}

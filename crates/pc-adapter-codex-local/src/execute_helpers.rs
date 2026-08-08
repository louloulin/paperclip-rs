//! Codex-local execute 助手函数。
//!
//! 完整复刻 Node `packages/adapters/codex-local/src/server/execute.ts`
//! 中与 billing 解析、skills 路径、transient fallback mode 解析相关的纯函数。

use std::collections::BTreeMap;

use pc_acpx::billing::infer_openai_compatible_biller;
use pc_acpx::env_helpers::has_non_empty_env_value;

/// Codex 的 billing 模式。
///
/// Node 等价：`resolveCodexBillingType` 的返回类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexBillingType {
    /// OpenAI API key 认证（环境中有 `OPENAI_API_KEY`）。
    Api,
    /// ChatGPT 订阅登录（无 API key，走本地登录/session）。
    Subscription,
}

impl CodexBillingType {
    pub fn as_str(&self) -> &'static str {
        match self {
            CodexBillingType::Api => "api",
            CodexBillingType::Subscription => "subscription",
        }
    }
}

/// 解析 Codex 的 billing 类型。
///
/// Node 等价：`resolveCodexBillingType`。Codex 不支持 Bedrock，
/// 仅区分 API key 与 ChatGPT 订阅。
pub fn resolve_codex_billing_type(env: &BTreeMap<String, String>) -> CodexBillingType {
    if has_non_empty_env_value(env, "OPENAI_API_KEY") {
        CodexBillingType::Api
    } else {
        CodexBillingType::Subscription
    }
}

/// 解析 Codex 的 biller（成本归属）。
///
/// Node 等价：`resolveCodexBiller`。
/// - OpenRouter 检测到 → `"openrouter"`。
/// - 否则：subscription → `"chatgpt"`；api → OpenAI-compatible 或 `"openai"`。
pub fn resolve_codex_biller(
    env: &BTreeMap<String, String>,
    billing_type: CodexBillingType,
) -> String {
    let openai_compatible = infer_openai_compatible_biller(env, Some("openai"));
    if openai_compatible.as_deref() == Some("openrouter") {
        return "openrouter".to_owned();
    }
    match billing_type {
        CodexBillingType::Subscription => "chatgpt".to_owned(),
        CodexBillingType::Api => openai_compatible.unwrap_or_else(|| "openai".to_owned()),
    }
}

/// 解析 Codex 临时回退模式（用于 transient 错误时的执行策略）。
///
/// Node 等价：`readCodexTransientFallbackMode`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexTransientFallbackMode {
    /// 不变 session 重试。
    SameSession,
    /// 使用更安全的调用参数。
    SaferInvocation,
    /// 启用全新 session。
    FreshSession,
    /// 同时启用 fresh session + safer invocation。
    FreshSessionSaferInvocation,
}

impl CodexTransientFallbackMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            CodexTransientFallbackMode::SameSession => "same_session",
            CodexTransientFallbackMode::SaferInvocation => "safer_invocation",
            CodexTransientFallbackMode::FreshSession => "fresh_session",
            CodexTransientFallbackMode::FreshSessionSaferInvocation => {
                "fresh_session_safer_invocation"
            }
        }
    }
}

/// 从 context 中读取 transient fallback mode。
///
/// Node 等价：`readCodexTransientFallbackMode`。
/// - 取 `context.codexTransientFallbackMode` 字段（trim 后）。
/// - 匹配四个合法值之一，否则返回 `None`。
pub fn read_codex_transient_fallback_mode(
    context: &serde_json::Value,
) -> Option<CodexTransientFallbackMode> {
    let raw = context
        .get("codexTransientFallbackMode")
        .and_then(serde_json::Value::as_str)?
        .trim();
    match raw {
        "same_session" => Some(CodexTransientFallbackMode::SameSession),
        "safer_invocation" => Some(CodexTransientFallbackMode::SaferInvocation),
        "fresh_session" => Some(CodexTransientFallbackMode::FreshSession),
        "fresh_session_safer_invocation" => Some(CodexTransientFallbackMode::FreshSessionSaferInvocation),
        _ => None,
    }
}

/// transient fallback mode 是否启用"更安全调用"。
///
/// Node 等价：`fallbackModeUsesSaferInvocation`。
pub fn fallback_mode_uses_safer_invocation(mode: Option<CodexTransientFallbackMode>) -> bool {
    matches!(
        mode,
        Some(
            CodexTransientFallbackMode::SaferInvocation
                | CodexTransientFallbackMode::FreshSessionSaferInvocation
        )
    )
}

/// transient fallback mode 是否启用"全新 session"。
///
/// Node 等价：`fallbackModeUsesFreshSession`。
pub fn fallback_mode_uses_fresh_session(mode: Option<CodexTransientFallbackMode>) -> bool {
    matches!(
        mode,
        Some(
            CodexTransientFallbackMode::FreshSession
                | CodexTransientFallbackMode::FreshSessionSaferInvocation
        )
    )
}

/// 解析 Codex skills 目录路径。
///
/// Node 等价：`resolveCodexSkillsDir`（底层 `path.join(codexHome, "skills")`）。
/// - `codex_home = "/x"` → `"/x/skills"`
/// - `codex_home = "/x/"` → `"/x/skills"`（trim 尾随 `/`）
/// - `codex_home = "/"` → `"/skills"`（保留根 `/`）
/// - `codex_home = ""` → `"skills"`
pub fn resolve_codex_skills_dir(codex_home: &str) -> String {
    if codex_home == "/" {
        return "/skills".to_owned();
    }
    let trimmed = codex_home.trim_end_matches('/');
    if trimmed.is_empty() {
        "skills".to_owned()
    } else {
        format!("{trimmed}/skills")
    }
}

/// Codex adapter 的错误族分类。
///
/// Node 等价：`errorFamily` 字段 + `transientFallbackMode` 决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexErrorFamily {
    None,
    ProviderQuota,
    TransientUpstream,
    RefreshTokenReused,
    RefreshTokenExpired,
    RefreshTokenInvalidated,
    HarnessCrash,
    UnknownSession,
}

impl CodexErrorFamily {
    pub fn as_str(&self) -> &'static str {
        match self {
            CodexErrorFamily::None => "",
            CodexErrorFamily::ProviderQuota => "provider_quota",
            CodexErrorFamily::TransientUpstream => "transient_upstream",
            CodexErrorFamily::RefreshTokenReused => "refresh_token_reused",
            CodexErrorFamily::RefreshTokenExpired => "refresh_token_expired",
            CodexErrorFamily::RefreshTokenInvalidated => "refresh_token_invalidated",
            CodexErrorFamily::HarnessCrash => "codex_harness_crash",
            CodexErrorFamily::UnknownSession => "unknown_session",
        }
    }
}

/// Codex 一次 attempt 的综合分类结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexRetryDecision {
    pub error_family: CodexErrorFamily,
    pub clear_session: bool,
    pub transient_fallback_mode: Option<CodexTransientFallbackMode>,
}

/// Codex adapter 的 retry 决策输入快照。
#[derive(Debug, Clone, Copy)]
pub struct CodexRetryInput<'a> {
    pub session_id: &'a str,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub error_message: Option<&'a str>,
    pub saw_protocol_event: bool,
    pub saw_protocol_terminal_event: bool,
    pub now: std::time::SystemTime,
}

/// 整合多个 Codex 错误分类器，返回单一决策。
///
/// Node 等价：`execute.ts` L1300-1450 的 family + retryNotBefore + clearSession 三元组。
pub fn decide_codex_retry(input: CodexRetryInput<'_>) -> CodexRetryDecision {
    use crate::codex_errors::{
        classify_codex_auth_refresh_failure, is_codex_harness_crash,
        is_codex_provider_quota_error, is_codex_transient_upstream_error,
        is_codex_unknown_session_error, CodexAuthRefreshFailureClass,
        CodexProtocolState,
    };
    let failed = !input.timed_out && input.exit_code.unwrap_or(0) != 0;
    let now = input.now;
    let provider_quota = failed
        && is_codex_provider_quota_error(
            Some(input.stdout),
            Some(input.stderr),
            input.error_message,
            now,
        );
    let transient_upstream = !provider_quota
        && failed
        && is_codex_transient_upstream_error(
            Some(input.stdout),
            Some(input.stderr),
            input.error_message,
            now,
        );
    let auth_failure = if failed {
        classify_codex_auth_refresh_failure(
            Some(input.stdout),
            Some(input.stderr),
            input.error_message,
        )
    } else {
        None
    };
    let harness_crash = is_codex_harness_crash(CodexProtocolState {
        exit_code: input.exit_code,
        saw_protocol_event: input.saw_protocol_event,
        saw_protocol_terminal_event: input.saw_protocol_terminal_event,
    });
    let unknown_session = failed
        && !input.session_id.is_empty()
        && is_codex_unknown_session_error(input.stdout, input.stderr);

    let error_family = if provider_quota {
        CodexErrorFamily::ProviderQuota
    } else if transient_upstream {
        CodexErrorFamily::TransientUpstream
    } else if matches!(auth_failure, Some(CodexAuthRefreshFailureClass::RefreshTokenReused)) {
        CodexErrorFamily::RefreshTokenReused
    } else if matches!(auth_failure, Some(CodexAuthRefreshFailureClass::RefreshTokenExpired)) {
        CodexErrorFamily::RefreshTokenExpired
    } else if matches!(auth_failure, Some(CodexAuthRefreshFailureClass::RefreshTokenInvalidated)) {
        CodexErrorFamily::RefreshTokenInvalidated
    } else if harness_crash {
        CodexErrorFamily::HarnessCrash
    } else if unknown_session {
        CodexErrorFamily::UnknownSession
    } else {
        CodexErrorFamily::None
    };

    // transient 上游错误 → 同 session + 更安全调用；quota → fresh session + 更安全调用。
    let transient_fallback_mode = if provider_quota {
        Some(CodexTransientFallbackMode::FreshSessionSaferInvocation)
    } else if transient_upstream {
        Some(CodexTransientFallbackMode::SaferInvocation)
    } else {
        None
    };
    let clear_session = unknown_session || matches!(auth_failure, Some(_));

    CodexRetryDecision {
        error_family,
        clear_session,
        transient_fallback_mode,
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

    // -----------------------------------------------------------------
    // resolve_codex_billing_type
    // -----------------------------------------------------------------

    #[test]
    fn billing_api_key有值_Api() {
        let env = env_from(&[("OPENAI_API_KEY", "sk-test")]);
        assert_eq!(
            resolve_codex_billing_type(&env),
            CodexBillingType::Api
        );
    }

    #[test]
    fn billing_api_key空白_Subscription() {
        let env = env_from(&[("OPENAI_API_KEY", "   ")]);
        assert_eq!(
            resolve_codex_billing_type(&env),
            CodexBillingType::Subscription
        );
    }

    #[test]
    fn billing默认_Subscription() {
        assert_eq!(
            resolve_codex_billing_type(&env_from(&[])),
            CodexBillingType::Subscription
        );
    }

    #[test]
    fn billing_as_str_映射() {
        assert_eq!(CodexBillingType::Api.as_str(), "api");
        assert_eq!(CodexBillingType::Subscription.as_str(), "subscription");
    }

    // -----------------------------------------------------------------
    // resolve_codex_biller
    // -----------------------------------------------------------------

    #[test]
    fn biller_openrouter_优先() {
        let env = env_from(&[
            ("OPENAI_API_KEY", "sk-test"),
            ("OPENROUTER_API_KEY", "sk-or-test"),
        ]);
        assert_eq!(
            resolve_codex_biller(&env, CodexBillingType::Api),
            "openrouter"
        );
    }

    #[test]
    fn biller_subscription_chatgpt() {
        let env = env_from(&[]);
        assert_eq!(
            resolve_codex_biller(&env, CodexBillingType::Subscription),
            "chatgpt"
        );
    }

    #[test]
    fn biller_api_openai_fallback() {
        let env = env_from(&[("OPENAI_API_KEY", "sk-test")]);
        assert_eq!(
            resolve_codex_biller(&env, CodexBillingType::Api),
            "openai"
        );
    }

    #[test]
    fn biller_api_openrouter_via_base_url() {
        // OpenRouter 也可通过 base url 命中。
        let env = env_from(&[
            ("OPENAI_API_KEY", "sk-test"),
            ("OPENAI_BASE_URL", "https://openrouter.ai/api/v1"),
        ]);
        assert_eq!(
            resolve_codex_biller(&env, CodexBillingType::Api),
            "openrouter"
        );
    }

    // -----------------------------------------------------------------
    // read_codex_transient_fallback_mode
    // -----------------------------------------------------------------

    #[test]
    fn fallback_mode_四种合法值() {
        assert_eq!(
            read_codex_transient_fallback_mode(
                &serde_json::json!({"codexTransientFallbackMode": "same_session"})
            ),
            Some(CodexTransientFallbackMode::SameSession)
        );
        assert_eq!(
            read_codex_transient_fallback_mode(
                &serde_json::json!({"codexTransientFallbackMode": "safer_invocation"})
            ),
            Some(CodexTransientFallbackMode::SaferInvocation)
        );
        assert_eq!(
            read_codex_transient_fallback_mode(
                &serde_json::json!({"codexTransientFallbackMode": "fresh_session"})
            ),
            Some(CodexTransientFallbackMode::FreshSession)
        );
        assert_eq!(
            read_codex_transient_fallback_mode(
                &serde_json::json!({"codexTransientFallbackMode": "fresh_session_safer_invocation"})
            ),
            Some(CodexTransientFallbackMode::FreshSessionSaferInvocation)
        );
    }

    // -----------------------------------------------------------------
    // decide_codex_retry
    // -----------------------------------------------------------------

    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    fn fixed_now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(22 * 3600)
    }
    fn base() -> super::CodexRetryInput<'static> {
        super::CodexRetryInput {
            session_id: "thread-1",
            timed_out: false,
            exit_code: Some(1),
            stdout: "",
            stderr: "",
            error_message: Some(""),
            saw_protocol_event: true,
            saw_protocol_terminal_event: true,
            now: fixed_now(),
        }
    }

    #[test]

    #[test]
    fn decide_codex_retry_provider_quota_with_retry_not_before() {
        let mut input = base();
        input.error_message = Some("You've hit your usage limit for gpt-5, try again at 3:30 PM (UTC)");
        let decision = super::decide_codex_retry(input);
        assert_eq!(decision.error_family, super::CodexErrorFamily::ProviderQuota);
        assert!(!decision.clear_session);
        assert_eq!(
            decision.transient_fallback_mode,
            Some(super::CodexTransientFallbackMode::FreshSessionSaferInvocation)
        );
    }

    #[test]
    fn decide_codex_retry_transient_upstream_returns_safer_invocation() {
        let mut input = base();
        input.stderr = "high demand temporary errors";
        let decision = super::decide_codex_retry(input);
        assert_eq!(decision.error_family, super::CodexErrorFamily::TransientUpstream);
        assert_eq!(
            decision.transient_fallback_mode,
            Some(super::CodexTransientFallbackMode::SaferInvocation)
        );
    }

    #[test]
    fn decide_codex_retry_refresh_token_reused_clears_session() {
        let mut input = base();
        input.stdout = "refresh_token_reused detected";
        let decision = super::decide_codex_retry(input);
        assert_eq!(decision.error_family, super::CodexErrorFamily::RefreshTokenReused);
        assert!(decision.clear_session);
    }

    #[test]
    fn decide_codex_retry_unknown_session_clears_session() {
        let mut input = base();
        input.stderr = "unknown session id: thread-1";
        let decision = super::decide_codex_retry(input);
        assert_eq!(decision.error_family, super::CodexErrorFamily::UnknownSession);
        assert!(decision.clear_session);
    }

    #[test]
    fn decide_codex_retry_harness_crash_when_protocol_started_nonterminal() {
        let mut input = base();
        input.saw_protocol_terminal_event = false;
        let decision = super::decide_codex_retry(input);
        assert_eq!(decision.error_family, super::CodexErrorFamily::HarnessCrash);
    }

    #[test]
    fn decide_codex_retry_exit_0_返回None() {
        let mut input = base();
        input.exit_code = Some(0);
        let decision = super::decide_codex_retry(input);
        assert_eq!(decision.error_family, super::CodexErrorFamily::None);
        assert!(!decision.clear_session);
        assert_eq!(decision.transient_fallback_mode, None);
    }

        fn fallback_mode_非法值_None() {
        assert_eq!(
            read_codex_transient_fallback_mode(
                &serde_json::json!({"codexTransientFallbackMode": "invalid_mode"})
            ),
            None
        );
        assert_eq!(
            read_codex_transient_fallback_mode(
                &serde_json::json!({"codexTransientFallbackMode": ""})
            ),
            None
        );
        assert_eq!(
            read_codex_transient_fallback_mode(&serde_json::json!({})),
            None
        );
        assert_eq!(
            read_codex_transient_fallback_mode(&serde_json::Value::Null),
            None
        );
    }

    #[test]
    fn fallback_mode_uses_safer_invocation_判断() {
        assert!(fallback_mode_uses_safer_invocation(Some(
            CodexTransientFallbackMode::SaferInvocation
        )));
        assert!(fallback_mode_uses_safer_invocation(Some(
            CodexTransientFallbackMode::FreshSessionSaferInvocation
        )));
        assert!(!fallback_mode_uses_safer_invocation(Some(
            CodexTransientFallbackMode::SameSession
        )));
        assert!(!fallback_mode_uses_safer_invocation(Some(
            CodexTransientFallbackMode::FreshSession
        )));
        assert!(!fallback_mode_uses_safer_invocation(None));
    }

    #[test]
    fn fallback_mode_uses_fresh_session_判断() {
        assert!(fallback_mode_uses_fresh_session(Some(
            CodexTransientFallbackMode::FreshSession
        )));
        assert!(fallback_mode_uses_fresh_session(Some(
            CodexTransientFallbackMode::FreshSessionSaferInvocation
        )));
        assert!(!fallback_mode_uses_fresh_session(Some(
            CodexTransientFallbackMode::SameSession
        )));
        assert!(!fallback_mode_uses_fresh_session(Some(
            CodexTransientFallbackMode::SaferInvocation
        )));
        assert!(!fallback_mode_uses_fresh_session(None));
    }

    // -----------------------------------------------------------------
    // resolve_codex_skills_dir
    // -----------------------------------------------------------------

    #[test]
    fn skills_dir_基本拼接() {
        assert_eq!(
            resolve_codex_skills_dir("/home/u/.codex"),
            "/home/u/.codex/skills"
        );
        assert_eq!(
            resolve_codex_skills_dir("/home/u/.codex/"),
            "/home/u/.codex/skills"
        );
    }

    #[test]
    fn skills_dir_空codex_home() {
        assert_eq!(resolve_codex_skills_dir(""), "skills");
        assert_eq!(resolve_codex_skills_dir("/"), "/skills");
    }
}

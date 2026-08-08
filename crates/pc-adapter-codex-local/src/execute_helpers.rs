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

    #[test]
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

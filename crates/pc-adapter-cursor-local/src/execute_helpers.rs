//! Cursor-local execute 助手函数。
//!
//! 完整复刻 Node `packages/adapters/cursor-local/src/server/execute.ts`
//! 中与 billing 解析、model provider 启发式、mode 规范化、skills 路径相关的纯函数。
//!
//! 通用工具 `render_paperclip_env_note` / `render_api_access_note` 复用
//! `pc_acpx::session_config_options`（R408+），不重复实现。

use std::collections::BTreeMap;

use pc_acpx::billing::infer_openai_compatible_biller;
use pc_acpx::env_helpers::has_non_empty_env_value;

/// Cursor 的 billing 模式。
///
/// Node 等价：`resolveCursorBillingType` 的返回类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorBillingType {
    /// API key 认证（`CURSOR_API_KEY` 或 `OPENAI_API_KEY`）。
    Api,
    /// Cursor 订阅。
    Subscription,
}

impl CursorBillingType {
    pub fn as_str(&self) -> &'static str {
        match self {
            CursorBillingType::Api => "api",
            CursorBillingType::Subscription => "subscription",
        }
    }
}

/// 解析 Cursor 的 billing 类型。
///
/// Node 等价：`resolveCursorBillingType`。
pub fn resolve_cursor_billing_type(env: &BTreeMap<String, String>) -> CursorBillingType {
    if has_non_empty_env_value(env, "CURSOR_API_KEY")
        || has_non_empty_env_value(env, "OPENAI_API_KEY")
    {
        CursorBillingType::Api
    } else {
        CursorBillingType::Subscription
    }
}

/// 解析 Cursor 的 biller（成本归属）。
///
/// Node 等价：`resolveCursorBiller`。
/// - OpenRouter 检测到 → `"openrouter"`。
/// - subscription → `"cursor"`。
/// - 否则 → provider（如果有）否则 `"cursor"`。
pub fn resolve_cursor_biller(
    env: &BTreeMap<String, String>,
    billing_type: CursorBillingType,
    provider: Option<&str>,
) -> String {
    let openai_compatible = infer_openai_compatible_biller(env, None);
    if openai_compatible.as_deref() == Some("openrouter") {
        return "openrouter".to_owned();
    }
    if billing_type == CursorBillingType::Subscription {
        return "cursor".to_owned();
    }
    provider.map(str::to_owned).unwrap_or_else(|| "cursor".to_owned())
}

/// Cursor 的执行模式。
///
/// Node 等价：`normalizeMode` 的返回类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    Plan,
    Ask,
}

impl CursorMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            CursorMode::Plan => "plan",
            CursorMode::Ask => "ask",
        }
    }
}

/// 规范化 Cursor 模式字符串。
///
/// Node 等价：`normalizeMode`：
/// - 接受 `"plan"` / `"ask"`（trim + lowercase）。
/// - 其他值返回 `None`。
pub fn normalize_mode(raw_mode: &str) -> Option<CursorMode> {
    let mode = raw_mode.trim().to_ascii_lowercase();
    match mode.as_str() {
        "plan" => Some(CursorMode::Plan),
        "ask" => Some(CursorMode::Ask),
        _ => None,
    }
}

/// 从 model 字符串推断 provider。
///
/// Node 等价：`resolveProviderFromModel`。
/// - `provider/model` 拆分（取首字符为非空 provider）。
/// - 含 `"sonnet"` 或 `"claude"` → `"anthropic"`。
/// - 以 `"gpt"` 或 `"o"` 开头 → `"openai"`。
/// - 都不匹配 → `None`。
pub fn resolve_provider_from_model(model: &str) -> Option<String> {
    let trimmed = model.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(slash) = trimmed.find('/') {
        if slash > 0 {
            let prefix = trimmed[..slash].trim();
            if !prefix.is_empty() {
                return Some(prefix.to_owned());
            }
        }
    }
    if trimmed.contains("sonnet") || trimmed.contains("claude") {
        return Some("anthropic".to_owned());
    }
    if trimmed.starts_with("gpt") || trimmed.starts_with("o") {
        return Some("openai".to_owned());
    }
    None
}

/// 解析 Cursor skills 目录路径。
///
/// Node 等价：`cursorSkillsHome`。返回 `<homedir>/.cursor/skills`。
pub fn cursor_skills_home(homedir: &str) -> String {
    let home_trimmed = homedir.trim_end_matches('/');
    if home_trimmed.is_empty() || home_trimmed == "/" {
        return "/.cursor/skills".to_owned();
    }
    format!("{home_trimmed}/.cursor/skills")
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
    // resolve_cursor_billing_type
    // -----------------------------------------------------------------

    #[test]
    fn billing_默认subscription() {
        assert_eq!(
            resolve_cursor_billing_type(&env_from(&[])),
            CursorBillingType::Subscription
        );
    }

    #[test]
    fn billing_cursor_key_Api() {
        assert_eq!(
            resolve_cursor_billing_type(&env_from(&[("CURSOR_API_KEY", "key")])),
            CursorBillingType::Api
        );
    }

    #[test]
    fn billing_openai_key_Api() {
        assert_eq!(
            resolve_cursor_billing_type(&env_from(&[("OPENAI_API_KEY", "key")])),
            CursorBillingType::Api
        );
    }

    #[test]
    fn billing_空白_Subscription() {
        let env = env_from(&[("CURSOR_API_KEY", "   ")]);
        assert_eq!(
            resolve_cursor_billing_type(&env),
            CursorBillingType::Subscription
        );
    }

    #[test]
    fn billing_as_str_映射() {
        assert_eq!(CursorBillingType::Api.as_str(), "api");
        assert_eq!(CursorBillingType::Subscription.as_str(), "subscription");
    }

    // -----------------------------------------------------------------
    // resolve_cursor_biller
    // -----------------------------------------------------------------

    #[test]
    fn biller_openrouter_优先() {
        let env = env_from(&[
            ("CURSOR_API_KEY", "key"),
            ("OPENROUTER_API_KEY", "or-key"),
        ]);
        assert_eq!(
            resolve_cursor_biller(&env, CursorBillingType::Api, Some("anthropic")),
            "openrouter"
        );
    }

    #[test]
    fn biller_subscription_默认cursor() {
        let env = env_from(&[]);
        assert_eq!(
            resolve_cursor_biller(&env, CursorBillingType::Subscription, None),
            "cursor"
        );
    }

    #[test]
    fn biller_api_用provider() {
        let env = env_from(&[("CURSOR_API_KEY", "key")]);
        assert_eq!(
            resolve_cursor_biller(&env, CursorBillingType::Api, Some("anthropic")),
            "anthropic"
        );
    }

    #[test]
    fn biller_api_无provider_fallback_cursor() {
        let env = env_from(&[("CURSOR_API_KEY", "key")]);
        assert_eq!(
            resolve_cursor_biller(&env, CursorBillingType::Api, None),
            "cursor"
        );
    }

    // -----------------------------------------------------------------
    // normalize_mode
    // -----------------------------------------------------------------

    #[test]
    fn mode_plan合法() {
        assert_eq!(normalize_mode("plan"), Some(CursorMode::Plan));
        assert_eq!(normalize_mode("PLAN"), Some(CursorMode::Plan));
        assert_eq!(normalize_mode("  plan  "), Some(CursorMode::Plan));
    }

    #[test]
    fn mode_ask合法() {
        assert_eq!(normalize_mode("ask"), Some(CursorMode::Ask));
        assert_eq!(normalize_mode("ASK"), Some(CursorMode::Ask));
    }

    #[test]
    fn mode_非法_None() {
        assert_eq!(normalize_mode("agent"), None);
        assert_eq!(normalize_mode(""), None);
        assert_eq!(normalize_mode("  "), None);
    }

    #[test]
    fn mode_as_str() {
        assert_eq!(CursorMode::Plan.as_str(), "plan");
        assert_eq!(CursorMode::Ask.as_str(), "ask");
    }

    // -----------------------------------------------------------------
    // resolve_provider_from_model
    // -----------------------------------------------------------------

    #[test]
    fn provider_斜杠拆分() {
        assert_eq!(
            resolve_provider_from_model("anthropic/claude-sonnet-4"),
            Some("anthropic".to_owned())
        );
        assert_eq!(
            resolve_provider_from_model("openai/gpt-4"),
            Some("openai".to_owned())
        );
    }

    #[test]
    fn provider_大小写不敏感() {
        assert_eq!(
            resolve_provider_from_model("ANTHROPIC/CLAUDE"),
            Some("anthropic".to_owned())
        );
    }

    #[test]
    fn provider_含sonnet() {
        assert_eq!(
            resolve_provider_from_model("claude-3-sonnet-20240229"),
            Some("anthropic".to_owned())
        );
        assert_eq!(
            resolve_provider_from_model("my-sonnet-x"),
            Some("anthropic".to_owned())
        );
    }

    #[test]
    fn provider_含claude() {
        assert_eq!(
            resolve_provider_from_model("claude-opus-4"),
            Some("anthropic".to_owned())
        );
    }

    #[test]
    fn provider_gpt开头() {
        assert_eq!(
            resolve_provider_from_model("gpt-4o"),
            Some("openai".to_owned())
        );
        assert_eq!(
            resolve_provider_from_model("gpt-3.5-turbo"),
            Some("openai".to_owned())
        );
    }

    #[test]
    fn provider_o开头() {
        assert_eq!(
            resolve_provider_from_model("o1-preview"),
            Some("openai".to_owned())
        );
        assert_eq!(
            resolve_provider_from_model("o3-mini"),
            Some("openai".to_owned())
        );
    }

    #[test]
    fn provider_无法识别() {
        assert_eq!(resolve_provider_from_model(""), None);
        assert_eq!(resolve_provider_from_model("   "), None);
        assert_eq!(resolve_provider_from_model("gemini-pro"), None);
        // `/` 开头 → slash=0, 返回 None
        assert_eq!(resolve_provider_from_model("/model"), None);
    }

    // -----------------------------------------------------------------
    // cursor_skills_home
    // -----------------------------------------------------------------

    #[test]
    fn skills_home_标准路径() {
        assert_eq!(
            cursor_skills_home("/home/u"),
            "/home/u/.cursor/skills"
        );
    }

    #[test]
    fn skills_home_尾斜杠() {
        assert_eq!(
            cursor_skills_home("/home/u/"),
            "/home/u/.cursor/skills"
        );
    }

    #[test]
    fn skills_home_根路径() {
        assert_eq!(cursor_skills_home("/"), "/.cursor/skills");
    }

    #[test]
    fn skills_home_空输入() {
        assert_eq!(cursor_skills_home(""), "/.cursor/skills");
    }
}
